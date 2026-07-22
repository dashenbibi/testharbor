//! Trust confirmation + test execution + structured AcceptPass evaluation.

use crate::{inspect_project, AppState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};
use testharbor_agent_drivers::{
    analyze_result, review_result, AgentMode, AnalyzeRequest, AnalyzeResult,
    ResultReviewAgentResult, ResultReviewRequest,
};
use testharbor_domain::{
    can_accept_pass, can_start_execution, case_approval_context_hash, AcceptPassContext,
    GuardReason, ResultCompleteness, ResultConsistency, RunId, StartExecutionContext,
    WorkflowState,
};
use testharbor_process_supervisor::{
    new_task_id, read_log_tail, CommandSpec, ProcessError, ProcessOutcome, TaskPhase,
};
use testharbor_result_normalizer::{
    collect_go_inventory, make_intended_manifest, observe_intended, parse_go_test_json_with_exit,
    parse_junit_xml_reports_with_exit, CaseStatus, IntendedObservation, IntendedTestManifest,
    NormalizedStructuredResult, TestIdentity, TestInventory,
};
use testharbor_storage::StorageError;
use testharbor_test_drivers::{self as drivers, GenericTestResult};
use testharbor_trust::{create_grant, CommandIdentity, TestProfileDraft, TrustGrant};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Process(#[from] testharbor_process_supervisor::ProcessError),
    #[error(transparent)]
    Driver(#[from] drivers::DriverError),
    #[error(transparent)]
    Inventory(#[from] testharbor_result_normalizer::InventoryError),
    #[error("invalid run id")]
    InvalidRunId,
    #[error("trust grant required: confirm test profile first")]
    TrustRequired,
    #[error("no active execution")]
    NoActiveExecution,
    #[error("no structured result for run")]
    NoStructuredResult,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ExecutionError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSuggestion {
    pub profile: TestProfileDraft,
    pub trust_scope_hash: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmedTrust {
    pub grant: TrustGrant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartExecutionResult {
    pub execution_id: String,
    pub task_id: String,
    pub outcome: ProcessOutcome,
    pub parser: String,
    pub ui_warning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPollResult {
    pub execution_id: String,
    pub task_id: String,
    pub outcome: ProcessOutcome,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub generic_result: Option<GenericTestResult>,
    pub structured: Option<NormalizedStructuredResult>,
    pub intended_observation: Option<IntendedObservation>,
    pub accept_pass: Option<AcceptPassView>,
    /// Analyzer Agent output (once execution finished).
    pub analysis: Option<AnalyzeResult>,
    /// Result Reviewer Agent output (once execution finished).
    pub result_review: Option<ResultReviewAgentResult>,
    /// Codex Analyzer / Result Reviewer failure. No mock result is substituted.
    pub agent_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastExecutionRecord {
    pub execution_id: String,
    pub parser: String,
    pub outcome: ProcessOutcome,
    pub generic_result: GenericTestResult,
    pub structured: Option<NormalizedStructuredResult>,
    pub intended_observation: Option<IntendedObservation>,
    pub accept_pass: Option<AcceptPassView>,
    pub analysis: Option<AnalyzeResult>,
    pub result_review: Option<ResultReviewAgentResult>,
    pub agent_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptPassView {
    pub enabled: bool,
    pub reasons: Vec<String>,
    pub reason_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryView {
    pub inventory: TestInventory,
    pub intended: Option<IntendedTestManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileProbeStage {
    pub stage: String,
    pub outcome: ProcessOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileProbeResult {
    pub probe_id: String,
    pub base_commit: String,
    pub profile_hash: String,
    pub trust_scope_hash: String,
    pub env_fingerprint: String,
    pub status: String,
    pub stale: bool,
    pub stages: Vec<ProfileProbeStage>,
}

fn parse_run_id(s: &str) -> Result<RunId> {
    Uuid::parse_str(s)
        .map(RunId)
        .map_err(|_| ExecutionError::InvalidRunId)
}

fn parser_kind(parser: &str) -> &str {
    parser
        .split_once(':')
        .map(|(kind, _)| kind)
        .unwrap_or(parser)
}

fn parser_spec(parser: &str) -> Option<&str> {
    parser.split_once(':').map(|(_, spec)| spec)
}

fn runtime_env_for_profile(
    state: &AppState,
    run_id: &str,
    profile: &TestProfileDraft,
) -> Result<std::collections::HashMap<String, String>> {
    let supplied = state
        .get_test_environment(run_id)
        .map(|environment| environment.agent_environment().variables)
        .unwrap_or_default();
    let mut resolved = std::collections::HashMap::new();
    for policy in &profile.env_policy {
        let value = match policy.source {
            testharbor_trust::EnvValueSource::InheritAllowed => std::env::var(&policy.key).ok(),
            testharbor_trust::EnvValueSource::PromptSecret
            | testharbor_trust::EnvValueSource::FixedNonSecret => {
                supplied.get(&policy.key).cloned()
            }
        };
        if let Some(value) = value {
            resolved.insert(policy.key.clone(), value);
        }
    }
    if let Some(service) = &profile.service {
        let missing = service
            .required_env_keys
            .iter()
            .filter(|key| !resolved.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(ExecutionError::Other(format!(
                "missing required runtime environment keys: {}",
                missing.join(", ")
            )));
        }
    }
    Ok(resolved)
}

fn wait_for_service(
    state: &AppState,
    task_id: &str,
    readiness: &testharbor_trust::TcpReadiness,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(readiness.timeout_ms);
    let address = format!("{}:{}", readiness.host, readiness.port);
    while Instant::now() < deadline {
        let outcome = state.supervisor().poll(task_id)?;
        if !matches!(outcome.phase, TaskPhase::Running | TaskPhase::Starting) {
            return Err(ExecutionError::Other(format!(
                "service exited before readiness: {:?}",
                outcome.phase
            )));
        }
        if address
            .to_socket_addrs()
            .ok()
            .into_iter()
            .flatten()
            .any(|socket| TcpStream::connect_timeout(&socket, Duration::from_millis(200)).is_ok())
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(ExecutionError::Other(format!(
        "service readiness timed out: {address}"
    )))
}

/// Suggest a runnable profile for the project based on detected repo markers.
pub fn suggest_profile(state: &AppState, run_id: &str) -> Result<ProfileSuggestion> {
    let run_id = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(run_id))?;
    let project = state.with_store(|s| s.get_project(run.project_id))?;
    let root = PathBuf::from(&project.root_path);
    if let Some(grant) = state.get_trust_grant(&run_id.to_string()) {
        let profile = grant.profile.normalize().map_err(ExecutionError::Other)?;
        let scope = testharbor_trust::profile_trust_scope_hash(&project.root_path, &profile);
        if scope == grant.trust_scope_hash {
            return Ok(ProfileSuggestion {
                profile,
                trust_scope_hash: scope,
                notes: vec!["使用 Agent 已选择并绑定的测试命令".into()],
            });
        }
    }
    let inspection = inspect_project(&root);
    let profile = inspection
        .profile
        .ok_or_else(|| {
            ExecutionError::Other(
                "未找到可执行的测试命令。请先让 Agent CLI 从仓库配置、README 和测试工具中返回 test_runner；标准项目会自动复用已绑定的命令。".into(),
            )
        })?
        .normalize()
        .map_err(ExecutionError::Other)?;

    let scope = testharbor_trust::profile_trust_scope_hash(&project.root_path, &profile);
    Ok(ProfileSuggestion {
        profile,
        trust_scope_hash: scope,
        notes: inspection.notes,
    })
}

pub fn confirm_profile(
    state: &AppState,
    run_id: &str,
    profile: TestProfileDraft,
) -> Result<ConfirmedTrust> {
    let run_id = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(run_id))?;
    let project = state.with_store(|s| s.get_project(run.project_id))?;
    if run.legacy_read_only {
        return Err(ExecutionError::Other(
            "该旧版 Run 为只读；请创建 successor Run".into(),
        ));
    }
    let profile = profile.normalize().map_err(ExecutionError::Other)?;
    profile
        .validate(Path::new(&project.root_path))
        .map_err(ExecutionError::Other)?;

    let proposed = create_grant(&project.id.to_string(), &project.root_path, profile);
    let grant = state
        .with_store(|store| {
            store.find_trust_grant_payload(&project.id.to_string(), &proposed.trust_scope_hash)
        })?
        .and_then(|payload| serde_json::from_str::<TrustGrant>(&payload).ok())
        .filter(|cached| {
            serde_json::to_value(&cached.profile).ok()
                == serde_json::to_value(&proposed.profile).ok()
        })
        .unwrap_or(proposed);
    if let Some(snapshot_json) = state.with_store(|store| store.get_run_snapshot_json(run.id))? {
        let snapshot: testharbor_domain::RunSnapshot = serde_json::from_str(&snapshot_json)
            .map_err(|error| ExecutionError::Other(error.to_string()))?;
        if snapshot.required_trust_scope_hash != grant.trust_scope_hash {
            return Err(ExecutionError::Other(
                "RunSnapshot 已冻结；Profile/Trust scope 变化必须创建 successor Run".into(),
            ));
        }
    }
    let previous_grant = state.get_trust_grant(&run_id.to_string());
    let scope_changed = previous_grant
        .as_ref()
        .map(|previous| previous.trust_scope_hash != grant.trust_scope_hash)
        .unwrap_or(false);
    if scope_changed && state.get_case_approval(&run_id.to_string()).is_some() {
        state.clear_case_approval(&run_id.to_string())?;
        state.clear_result_decision(&run_id.to_string())?;
        state.clear_last_execution(&run_id.to_string())?;
        state.with_store(|store| {
            store.update_run_worktree(
                run.id,
                run.worktree_path.as_deref(),
                WorkflowState::PendingTestReview,
            )?;
            store.append_event(
                Some(run.id),
                Some(project.id),
                "trust_scope_changed",
                &serde_json::json!({
                    "previous_scope_hash": previous_grant
                        .as_ref()
                        .map(|previous| previous.trust_scope_hash.clone()),
                    "new_scope_hash": grant.trust_scope_hash,
                    "approval_invalidated": true,
                }),
            )?;
            Ok::<(), StorageError>(())
        })?;
    }
    state.set_trust_grant(run_id.to_string(), grant.clone())?;

    state.with_store(|s| {
        s.append_event(Some(run_id), Some(project.id), "trust_granted", &grant)?;
        s.upsert_trust_grant(
            &grant.id,
            &project.id.to_string(),
            &grant.trust_scope_hash,
            &serde_json::to_string(&grant).unwrap_or_default(),
        )?;
        Ok::<(), StorageError>(())
    })?;

    Ok(ConfirmedTrust { grant })
}

pub fn probe_profile_readiness(
    state: &AppState,
    run_id: &str,
    include_baseline: bool,
) -> Result<ProfileProbeResult> {
    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|store| store.get_run(rid))?;
    if run.legacy_read_only {
        return Err(ExecutionError::Other("legacy run is read-only".into()));
    }
    let project = state.with_store(|store| store.get_project(run.project_id))?;
    let grant = state
        .get_trust_grant(run_id)
        .ok_or(ExecutionError::TrustRequired)?;
    let current_scope =
        testharbor_trust::profile_trust_scope_hash(&project.root_path, &grant.profile);
    if current_scope != grant.trust_scope_hash {
        return Err(ExecutionError::Other("TrustGrant scope is stale".into()));
    }
    let probe_id = Uuid::new_v4().to_string();
    let worktree = state
        .paths()
        .worktrees_dir
        .join(format!("probe-{probe_id}"));
    testharbor_git_adapter::add_detached_worktree(
        Path::new(&project.root_path),
        &worktree,
        &run.base_commit,
    )
    .map_err(|error| ExecutionError::Other(error.to_string()))?;
    let log_root = state.paths().root.join("probes").join(&probe_id);
    let mut stages = Vec::new();
    let mut commands = Vec::new();
    if let Some(command) = &grant.profile.collect {
        commands.push(("collect", command));
    }
    if let Some(command) = &grant.profile.compile {
        commands.push(("compile", command));
    }
    if include_baseline {
        commands.push(("baseline", &grant.profile.command));
    }
    let run_result = (|| -> Result<()> {
        for (stage, command) in commands {
            let task_id = new_task_id();
            let started = state.supervisor().start(
                &task_id,
                &CommandSpec {
                    executable: command.executable.clone(),
                    args: command.args.clone(),
                    cwd: worktree.clone(),
                    env: Default::default(),
                    timeout_ms: Some(grant.profile.timeout_ms),
                },
                &log_root.join(stage),
            )?;
            let outcome = if matches!(started.phase, TaskPhase::Running | TaskPhase::Starting) {
                state.supervisor().wait(&task_id, 25)?
            } else {
                started
            };
            let succeeded = outcome.phase == TaskPhase::Exited && outcome.exit_code == Some(0);
            stages.push(ProfileProbeStage {
                stage: stage.into(),
                outcome,
            });
            if !succeeded {
                break;
            }
        }
        Ok(())
    })();
    let cleanup =
        testharbor_git_adapter::remove_worktree(Path::new(&project.root_path), &worktree, true);
    run_result?;
    cleanup.map_err(|error| ExecutionError::Other(error.to_string()))?;
    let status = if stages
        .iter()
        .all(|stage| stage.outcome.phase == TaskPhase::Exited && stage.outcome.exit_code == Some(0))
    {
        "passed"
    } else {
        "failed"
    };
    let profile_json = serde_json::to_vec(&grant.profile)
        .map_err(|error| ExecutionError::Other(error.to_string()))?;
    let env_fingerprint = testharbor_domain::canonical_hash(&[
        std::env::consts::OS,
        std::env::consts::ARCH,
        &grant.profile.all_command_snapshots_hash(),
    ]);
    let result = ProfileProbeResult {
        probe_id: probe_id.clone(),
        base_commit: run.base_commit.clone(),
        profile_hash: testharbor_domain::sha256_hex(&profile_json),
        trust_scope_hash: grant.trust_scope_hash.clone(),
        env_fingerprint,
        status: status.into(),
        stale: false,
        stages,
    };
    state.with_store(|store| {
        store.insert_profile_probe(
            &probe_id,
            project.id,
            &run.base_commit,
            &result.profile_hash,
            &grant.trust_scope_hash,
            if include_baseline {
                "full"
            } else {
                "readiness"
            },
            status,
            &result,
        )
    })?;
    Ok(result)
}

#[derive(Debug, Clone)]
struct CandidateTestSelection {
    package_patterns: Vec<String>,
    test_names: Vec<String>,
}

fn case_id_from_marker(line: &str) -> Option<String> {
    let marker = line.find("TESTHARBOR_CASE")?;
    let json_start = line[marker..].find('{')? + marker;
    let json_end = line[json_start..].find('}')? + json_start;
    let value = serde_json::from_str::<serde_json::Value>(&line[json_start..=json_end]).ok()?;
    value
        .get("id")
        .or_else(|| value.get("case_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn parse_go_test_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in source.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("func ") else {
            continue;
        };
        let Some(name) = ["Test", "Example", "Benchmark"]
            .iter()
            .find_map(|prefix| rest.strip_prefix(prefix).map(|suffix| (*prefix, suffix)))
        else {
            continue;
        };
        let mut ident = name.0.to_string();
        for ch in name.1.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ident.push(ch);
            } else {
                break;
            }
        }
        if ident.len() > name.0.len() {
            names.push(ident);
        }
    }
    names.sort();
    names.dedup();
    names
}

fn package_pattern_for_test_path(path: &str) -> String {
    let dir = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    if dir.is_empty() {
        ".".into()
    } else {
        format!("./{dir}")
    }
}

fn candidate_test_selection(state: &AppState, run_id: &str) -> Option<CandidateTestSelection> {
    let candidate = state.get_candidate(run_id)?;
    let run = parse_run_id(run_id)
        .ok()
        .and_then(|id| state.with_store(|store| store.get_run(id)).ok())?;
    let worktree = run.worktree_path.as_ref().map(PathBuf::from)?;
    if !worktree.is_dir() {
        return None;
    }

    let mut by_pattern: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for changed_path in candidate.changed_paths {
        if !changed_path.ends_with("_test.go") {
            continue;
        }
        let content = std::fs::read_to_string(worktree.join(&changed_path)).ok()?;
        let names = parse_go_test_names(&content);
        if names.is_empty() {
            continue;
        }
        let pattern = package_pattern_for_test_path(&changed_path);
        by_pattern.entry(pattern).or_default().extend(names);
    }

    if by_pattern.is_empty() {
        return None;
    }
    let mut test_names: Vec<String> = by_pattern
        .values()
        .flat_map(|names| names.iter().cloned())
        .collect();
    test_names.sort();
    test_names.dedup();
    Some(CandidateTestSelection {
        package_patterns: by_pattern.keys().cloned().collect(),
        test_names,
    })
}

fn candidate_case_test_selection(
    state: &AppState,
    run_id: &str,
    case_id: &str,
) -> Result<CandidateTestSelection> {
    let candidate = state
        .get_candidate(run_id)
        .ok_or_else(|| ExecutionError::Other("current candidate revision required".into()))?;
    let run = parse_run_id(run_id).and_then(|id| {
        state
            .with_store(|store| store.get_run(id))
            .map_err(ExecutionError::from)
    })?;
    let worktree = run
        .worktree_path
        .as_ref()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .ok_or_else(|| ExecutionError::Other("approved worktree is unavailable".into()))?;

    let mut by_pattern: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut case_ids_by_test: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for changed_path in candidate.changed_paths {
        if !changed_path.ends_with("_test.go") {
            continue;
        }
        let content = std::fs::read_to_string(worktree.join(&changed_path))
            .map_err(|error| ExecutionError::Other(error.to_string()))?;
        let mut current_test = None;
        for line in content.lines() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("func ") {
                if let Some(name) = ["Test", "Example", "Benchmark"]
                    .iter()
                    .find_map(|prefix| rest.strip_prefix(prefix).map(|suffix| (*prefix, suffix)))
                {
                    let mut ident = name.0.to_string();
                    for ch in name.1.chars() {
                        if ch.is_ascii_alphanumeric() || ch == '_' {
                            ident.push(ch);
                        } else {
                            break;
                        }
                    }
                    if ident.len() > name.0.len() {
                        current_test = Some(ident);
                    }
                }
            }
            if let Some(marker_case_id) = case_id_from_marker(line) {
                if let Some(test_name) = current_test.clone() {
                    case_ids_by_test
                        .entry(test_name.clone())
                        .or_default()
                        .insert(marker_case_id.clone());
                    if marker_case_id == case_id {
                        by_pattern
                            .entry(package_pattern_for_test_path(&changed_path))
                            .or_default()
                            .insert(test_name);
                    }
                }
            }
        }
    }

    if case_ids_by_test.values().any(|case_ids| case_ids.len() > 1) {
        return Err(ExecutionError::Other(format!(
            "用例 {case_id} 与其他业务用例共用一个 Go 测试入口，请执行全部用例"
        )));
    }

    let package_patterns: Vec<String> = by_pattern.keys().cloned().collect();
    let test_names: Vec<String> = by_pattern
        .values()
        .flat_map(|names| names.iter().cloned())
        .collect();
    if package_patterns.is_empty() || test_names.is_empty() {
        return Err(ExecutionError::Other(format!(
            "用例 {case_id} 没有映射到独立 Go 测试入口，请执行全部用例"
        )));
    }
    Ok(CandidateTestSelection {
        package_patterns,
        test_names,
    })
}

fn go_test_run_regex(test_names: &[String]) -> String {
    format!("^({})$", test_names.join("|"))
}

fn narrow_go_profile_to_candidate(
    profile: &mut TestProfileDraft,
    selection: &CandidateTestSelection,
) {
    if selection.test_names.is_empty() || selection.package_patterns.is_empty() {
        return;
    }
    let mut args = vec![
        "test".to_string(),
        "-json".to_string(),
        "-count=1".to_string(),
        "-run".to_string(),
        go_test_run_regex(&selection.test_names),
    ];
    args.extend(selection.package_patterns.iter().cloned());
    profile.command.args = args;
}

/// Collect Go inventory in worktree or project root; optionally select all as intended.
pub fn collect_inventory(
    state: &AppState,
    run_id: &str,
    select_all_as_intended: bool,
) -> Result<InventoryView> {
    let run_id = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(run_id))?;
    let project = state.with_store(|s| s.get_project(run.project_id))?;
    let cwd = run
        .worktree_path
        .as_ref()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from(&project.root_path));

    let run_id_text = run_id.to_string();
    let selection = candidate_test_selection(state, &run_id_text);
    let trust_parser = state
        .get_trust_grant(&run_id_text)
        .map(|grant| grant.profile.command.parser)
        .unwrap_or_else(|| "go-test-json".into());
    if parser_kind(&trust_parser) != "go-test-json" {
        let inventory = TestInventory {
            identities: Vec::new(),
            hash: "inventory-unavailable".into(),
            package_count: 0,
        };
        return Ok(InventoryView {
            inventory,
            intended: state.get_intended(&run_id_text),
        });
    }
    let package_patterns = selection
        .as_ref()
        .map(|selection| selection.package_patterns.clone())
        .unwrap_or_else(|| vec!["./...".into()]);
    let package_pattern_refs = package_patterns
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>();

    let inventory = collect_go_inventory(&cwd, &package_pattern_refs)?;
    let intended = if select_all_as_intended
        && !inventory.identities.is_empty()
        && state.get_case_approval(&run_id_text).is_none()
    {
        let selected = if let Some(selection) = &selection {
            let names = selection
                .test_names
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            inventory
                .identities
                .iter()
                .filter(|identity| names.contains(&identity.name))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            inventory.identities.clone()
        };
        if selected.is_empty() {
            state.get_intended(&run_id.to_string())
        } else {
            let m = make_intended_manifest(&inventory, selected)?;
            state.set_intended(run_id.to_string(), m.clone())?;
            state.with_store(|s| {
                s.append_event(Some(run_id), Some(project.id), "intended_manifest_set", &m)
            })?;
            Some(m)
        }
    } else {
        state.get_intended(&run_id.to_string())
    };

    state.set_inventory(run_id.to_string(), inventory.clone())?;
    state.with_store(|s| {
        s.append_event(
            Some(run_id),
            Some(project.id),
            "inventory_collected",
            &inventory,
        )
    })?;

    Ok(InventoryView {
        inventory,
        intended,
    })
}

/// Set intended identities by package/name pairs (must be in last inventory).
pub fn set_intended_tests(
    state: &AppState,
    run_id: &str,
    identities: Vec<TestIdentity>,
) -> Result<IntendedTestManifest> {
    let run_id = parse_run_id(run_id)?;
    if state.get_case_approval(&run_id.to_string()).is_some() {
        return Err(ExecutionError::Other(
            "用例已批准，不能再修改 intended 测试清单".into(),
        ));
    }
    let inventory = state
        .get_inventory(&run_id.to_string())
        .ok_or_else(|| ExecutionError::Other("collect inventory first".into()))?;
    let m = make_intended_manifest(&inventory, identities)?;
    state.set_intended(run_id.to_string(), m.clone())?;
    state.with_store(|s| s.append_event(Some(run_id), None, "intended_manifest_set", &m))?;
    Ok(m)
}

pub fn start_test_execution(
    state: &AppState,
    run_id: &str,
    case_id: Option<&str>,
) -> Result<StartExecutionResult> {
    state.reconcile_process_slots()?;
    let run_id = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(run_id))?;
    if run.legacy_read_only {
        return Err(ExecutionError::Other(
            "该旧版 Run 为只读，不能继续执行".into(),
        ));
    }
    let project = state.with_store(|s| s.get_project(run.project_id))?;
    let purpose = if case_id.is_some() {
        ExecutionPurpose::Diagnostic
    } else {
        ExecutionPurpose::Formal
    };

    let grant = state
        .get_trust_grant(&run_id.to_string())
        .ok_or(ExecutionError::TrustRequired)?;
    let candidate = state
        .get_candidate(&run_id.to_string())
        .ok_or_else(|| ExecutionError::Other("current candidate revision required".into()))?;
    let approval = state
        .get_case_approval(&run_id.to_string())
        .ok_or_else(|| ExecutionError::Other("current candidate is not approved".into()))?;
    let review = state
        .get_case_review(&run_id.to_string())
        .ok_or_else(|| ExecutionError::Other("case review state required".into()))?;
    let case_scope = state
        .get_case_scope(&run_id.to_string())
        .ok_or_else(|| ExecutionError::Other("case approval scope required".into()))?;

    let worktree = run
        .worktree_path
        .as_ref()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .ok_or_else(|| ExecutionError::Other("approved worktree is unavailable".into()))?;
    let handle = testharbor_workspace_manager::WorkspaceHandle {
        repo_root: PathBuf::from(&project.root_path),
        worktree_path: worktree.clone(),
        base_commit: run.base_commit.clone(),
        run_id: run.id.to_string(),
    };
    let tree_unchanged =
        testharbor_workspace_manager::verify_integrity(&handle, &candidate.candidate_tree)
            .map_err(|error| ExecutionError::Other(error.to_string()))?;
    let worktree_changes = if tree_unchanged {
        Vec::new()
    } else {
        testharbor_git_adapter::list_changed_paths(&worktree)
            .map_err(|error| ExecutionError::Other(error.to_string()))?
    };
    if !tree_unchanged {
        let changed_summary = if worktree_changes.is_empty() {
            "HEAD 或候选 tree 已被切换".to_string()
        } else {
            worktree_changes
                .iter()
                .map(|change| format!("{} {}", change.status, change.path))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let has_tracked_changes = worktree_changes.iter().any(|change| change.status != "??");
        if has_tracked_changes || worktree_changes.is_empty() {
            state.clear_case_approval(&run_id.to_string())?;
            state.clear_result_decision(&run_id.to_string())?;
            state.clear_last_execution(&run_id.to_string())?;
            state.with_store(|store| {
                store.update_run_worktree(
                    run.id,
                    run.worktree_path.as_deref(),
                    WorkflowState::PendingTestReview,
                )?;
                store.append_event(
                    Some(run.id),
                    Some(project.id),
                    "execution_blocked_candidate_changed",
                    &serde_json::json!({
                        "candidate_tree": candidate.candidate_tree.clone(),
                        "changed_paths": worktree_changes,
                    }),
                )?;
                Ok::<(), StorageError>(())
            })?;
        }
        let next_step = if has_tracked_changes || worktree_changes.is_empty() {
            "已回到用例评审，请重新确认候选后再执行"
        } else {
            "清理临时文件后可以再次执行"
        };
        return Err(ExecutionError::Other(format!(
            "start execution blocked: CandidateTreeMismatch; 隔离 worktree 在批准后发生变化：{changed_summary}。{next_step}。"
        )));
    }
    let recomputed_context =
        case_approval_context_hash(&case_scope, &review.disposition_snapshot_hash);
    let intended_matches = approval.intended_manifest_hash
        == state
            .get_intended(&run_id.to_string())
            .map(|manifest| manifest.hash);
    if !intended_matches {
        state.clear_case_approval(&run_id.to_string())?;
        state.clear_result_decision(&run_id.to_string())?;
        state.clear_last_execution(&run_id.to_string())?;
        state.with_store(|store| {
            store.update_run_worktree(
                run.id,
                run.worktree_path.as_deref(),
                WorkflowState::PendingTestReview,
            )?;
            store.append_event(
                Some(run.id),
                Some(project.id),
                "execution_blocked_intended_changed",
                &serde_json::json!({
                    "approved_intended_manifest_hash": approval.intended_manifest_hash,
                    "current_intended_manifest_hash": state
                        .get_intended(&run_id.to_string())
                        .map(|manifest| manifest.hash),
                }),
            )?;
            Ok::<(), StorageError>(())
        })?;
        return Err(ExecutionError::Other(
            "批准后的测试清单发生变化，已回到用例评审；请重新批准后再执行".into(),
        ));
    }
    let guard = can_start_execution(&StartExecutionContext {
        workflow_state: run.workflow_state,
        approved_revision_id: Some(format!("r{}", candidate.sequence)),
        current_revision_id: format!("r{}", candidate.sequence),
        latest_human_test_decision: Some(approval.decision),
        approval_context_hash: approval.case_approval_context_hash.clone(),
        recomputed_approval_context_hash: recomputed_context,
        candidate_tree: approval.candidate_tree.clone(),
        recomputed_candidate_tree: candidate.candidate_tree.clone(),
        trust_grant_valid: !grant.trust_scope_hash.is_empty(),
        has_active_execution: state.get_active_execution(&run_id.to_string()).is_some(),
    });
    if !guard.ok {
        return Err(ExecutionError::Other(format!(
            "start execution blocked: {}",
            guard
                .reasons
                .iter()
                .map(|reason| format!("{reason:?}"))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }

    let cwd = worktree.display().to_string();

    let mut profile = grant.profile.clone();
    profile.command.cwd = cwd;
    if let Some(command) = &mut profile.format {
        command.cwd = worktree.display().to_string();
    }
    if let Some(command) = &mut profile.collect {
        command.cwd = worktree.display().to_string();
    }
    if let Some(command) = &mut profile.compile {
        command.cwd = worktree.display().to_string();
    }
    if let Some(service) = &mut profile.service {
        service.startup.cwd = worktree.display().to_string();
    }
    if case_id.is_some() && parser_kind(&profile.command.parser) != "go-test-json" {
        return Err(ExecutionError::Other(
            "当前测试驱动还不能按业务用例单独重跑，请执行全部用例".into(),
        ));
    }
    if parser_kind(&profile.command.parser) == "go-test-json" {
        let selection = match case_id {
            Some(case_id) => Some(candidate_case_test_selection(
                state,
                &run_id.to_string(),
                case_id,
            )?),
            // Formal executions always run the frozen Profile's complete Test
            // command. Candidate-only selection is diagnostic evidence and must
            // never unlock AcceptPass or Apply.
            None => None,
        };
        if let Some(selection) = selection {
            narrow_go_profile_to_candidate(&mut profile, &selection);
        }
    }

    let execution_id = Uuid::new_v4().to_string();
    let execution_scope_hash = testharbor_domain::canonical_hash(&[
        &candidate.candidate_tree,
        &testharbor_trust::command_snapshot_hash(&profile.command),
        &grant.trust_scope_hash,
        &run.base_commit,
    ]);
    let task_id = new_task_id();
    let planned_service_task_id =
        (purpose == ExecutionPurpose::Formal && profile.service.is_some()).then(new_task_id);
    let required_process_slots = 1 + usize::from(planned_service_task_id.is_some());
    let log_dir = state
        .paths()
        .root
        .join("runs")
        .join(run_id.to_string())
        .join("executions")
        .join(&execution_id);
    std::fs::create_dir_all(&log_dir).map_err(|e| ExecutionError::Other(e.to_string()))?;
    if parser_kind(&profile.command.parser) == "junit-xml" {
        let spec = parser_spec(&profile.command.parser).unwrap_or(".testharbor/junit.xml");
        prepare_junit_report_target(&worktree, spec)?;
    }
    if purpose == ExecutionPurpose::Formal {
        state.clear_result_decision(&run_id.to_string())?;
        state.clear_last_execution(&run_id.to_string())?;
    }
    let runtime_env = runtime_env_for_profile(state, &run_id.to_string(), &profile)?;

    state.with_store_transaction(|s| {
        s.ensure_process_capacity(required_process_slots)?;
        s.insert_execution(
            &execution_id,
            run.id,
            Some(&format!("r{}", candidate.sequence)),
            if purpose == ExecutionPurpose::Formal {
                "formal"
            } else {
                "diagnostic"
            },
            "starting",
            &execution_scope_hash,
            &serde_json::json!({
                "command": profile.command,
                "candidate_tree": candidate.candidate_tree,
            }),
        )?;
        s.insert_process_intent(
            &task_id,
            run.id,
            Some(&execution_id),
            "foreground",
            &testharbor_trust::command_snapshot_hash(&profile.command),
            &serde_json::json!({"command": profile.command}),
        )?;
        if let (Some(service_task_id), Some(service)) = (&planned_service_task_id, &profile.service)
        {
            s.insert_process_intent(
                service_task_id,
                run.id,
                Some(&execution_id),
                "service",
                &testharbor_trust::command_snapshot_hash(&service.startup),
                &serde_json::json!({"command": service.startup, "readiness": service.readiness}),
            )?;
        }
        if purpose == ExecutionPurpose::Formal {
            s.update_run_worktree(
                run.id,
                run.worktree_path.as_deref(),
                WorkflowState::ExecutingTests,
            )?;
        }
        s.append_event(
            Some(run.id),
            Some(project.id),
            if purpose == ExecutionPurpose::Formal {
                "execution_started"
            } else {
                "diagnostic_execution_started"
            },
            &serde_json::json!({
                "execution_id": execution_id,
                "task_id": task_id,
                "command": profile.command,
                "purpose": purpose,
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;

    let service_task_id = if purpose == ExecutionPurpose::Formal {
        if let Some(service) = &profile.service {
            let service_task_id = planned_service_task_id
                .clone()
                .ok_or_else(|| ExecutionError::Other("service task intent missing".into()))?;
            let service_outcome = match state.supervisor().start_with_identity_callback(
                &service_task_id,
                &CommandSpec {
                    executable: service.startup.executable.clone(),
                    args: service.startup.args.clone(),
                    cwd: worktree.clone(),
                    env: runtime_env.clone(),
                    timeout_ms: Some(service.timeout_ms),
                },
                &log_dir.join("service"),
                |identity| {
                    state
                        .with_store(|store| {
                            store.set_process_identity(
                                &service_task_id,
                                "waiting_for_go",
                                identity.pid,
                                identity.pgid,
                                identity.start_token.as_deref(),
                                identity,
                            )
                        })
                        .map_err(|error| ProcessError::Other(error.to_string()))
                },
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    state.with_store(|store| {
                        store.mark_active_process_tasks_for_run(run.id, "spawn_failed")?;
                        store.update_run_worktree(
                            run.id,
                            run.worktree_path.as_deref(),
                            WorkflowState::NeedsHuman,
                        )
                    })?;
                    return Err(error.into());
                }
            };
            if service_outcome.phase == TaskPhase::Failed {
                state.with_store(|store| {
                    store.mark_active_process_tasks_for_run(run.id, "spawn_failed")?;
                    store.update_run_worktree(
                        run.id,
                        run.worktree_path.as_deref(),
                        WorkflowState::NeedsHuman,
                    )
                })?;
                return Err(ExecutionError::Other(
                    "service process failed to start".into(),
                ));
            }
            state.with_store(|store| {
                store.set_process_identity(
                    &service_task_id,
                    "running",
                    service_outcome.pid,
                    service_outcome.pgid,
                    service_outcome.start_token.as_deref(),
                    &service_outcome,
                )
            })?;
            if let Err(error) = wait_for_service(state, &service_task_id, &service.readiness) {
                let _ = state.supervisor().cancel(&service_task_id);
                state.with_store(|store| {
                    store.mark_active_process_tasks_for_run(run.id, "readiness_failed")?;
                    store.update_run_worktree(
                        run.id,
                        run.worktree_path.as_deref(),
                        WorkflowState::NeedsHuman,
                    )
                })?;
                return Err(error);
            }
            Some(service_task_id)
        } else {
            None
        }
    } else {
        None
    };
    let outcome = match drivers::start_generic_with_env_and_identity(
        state.supervisor(),
        &task_id,
        &profile,
        &log_dir,
        runtime_env,
        |identity| {
            state
                .with_store(|store| {
                    store.set_process_identity(
                        &task_id,
                        "waiting_for_go",
                        identity.pid,
                        identity.pgid,
                        identity.start_token.as_deref(),
                        identity,
                    )
                })
                .map_err(|error| ProcessError::Other(error.to_string()))
        },
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(service_task_id) = &service_task_id {
                let _ = state.supervisor().cancel(service_task_id);
            }
            state.with_store(|store| {
                store.mark_active_process_tasks_for_run(run.id, "spawn_failed")?;
                store.set_process_identity(
                    &task_id,
                    "spawn_failed",
                    None,
                    None,
                    None,
                    &serde_json::json!({"error": error.to_string()}),
                )?;
                if purpose == ExecutionPurpose::Formal {
                    store.update_run_worktree(
                        run.id,
                        run.worktree_path.as_deref(),
                        WorkflowState::NeedsHuman,
                    )?;
                }
                store.append_event(
                    Some(run.id),
                    Some(project.id),
                    "execution_start_failed",
                    &serde_json::json!({
                        "execution_id": execution_id,
                        "task_id": task_id,
                        "error": error.to_string(),
                    }),
                )?;
                Ok::<(), StorageError>(())
            })?;
            return Err(error.into());
        }
    };
    if outcome.phase == TaskPhase::Failed {
        if let Some(service_task_id) = &service_task_id {
            let _ = state.supervisor().cancel(service_task_id);
        }
        let stderr = read_log_tail(&outcome.stderr_path, 32_000).unwrap_or_default();
        let detail = stderr.trim();
        let message = if detail.is_empty() {
            "test command failed to start".to_string()
        } else {
            detail.to_string()
        };
        state.with_store(|store| {
            store.mark_active_process_tasks_for_run(run.id, "spawn_failed")?;
            store.set_process_identity(
                &task_id,
                "spawn_failed",
                outcome.pid,
                outcome.pgid,
                outcome.start_token.as_deref(),
                &outcome,
            )?;
            if purpose == ExecutionPurpose::Formal {
                store.update_run_worktree(
                    run.id,
                    run.worktree_path.as_deref(),
                    WorkflowState::NeedsHuman,
                )?;
            }
            store.append_event(
                Some(run.id),
                Some(project.id),
                "execution_start_failed",
                &serde_json::json!({
                    "execution_id": execution_id,
                    "task_id": task_id,
                    "error": message,
                    "stderr_path": outcome.stderr_path,
                }),
            )?;
            Ok::<(), StorageError>(())
        })?;
        return Err(ExecutionError::Other(message));
    }
    state.with_store(|store| {
        store.set_process_identity(
            &task_id,
            "running",
            outcome.pid,
            outcome.pgid,
            outcome.start_token.as_deref(),
            &outcome,
        )
    })?;

    let warning = if parser_kind(&profile.command.parser) == "go-test-json" {
        "Go Test JSON：结束后解析用例树；仅当 Complete 且 intended 全观察通过才可 AcceptPass。"
            .into()
    } else if parser_kind(&profile.command.parser) == "junit-xml" {
        "JUnit XML：结束后读取结构化报告并给出结果评审；当前仍不能严格 AcceptPass。".into()
    } else {
        "Generic/exit-code：仅命令级结果，不能 AcceptPass。".into()
    };

    state.set_active_execution(
        run_id.to_string(),
        ActiveExecution {
            execution_id: execution_id.clone(),
            task_id: task_id.clone(),
            parser: profile.command.parser.clone(),
            outcome: outcome.clone(),
            purpose,
            service_task_id,
        },
    )?;

    Ok(StartExecutionResult {
        execution_id,
        task_id,
        outcome,
        parser: profile.command.parser,
        ui_warning: warning,
    })
}

pub fn poll_execution(state: &AppState, run_id: &str) -> Result<ExecutionPollResult> {
    let run_id = parse_run_id(run_id)?;
    let active = state
        .get_active_execution(&run_id.to_string())
        .ok_or(ExecutionError::NoActiveExecution)?;

    let outcome = state.supervisor().poll(&active.task_id)?;
    let secrets = runtime_secret_values(state, &run_id.to_string());
    let stdout_tail = testharbor_evidence::redact_and_bound_log(
        &read_log_tail(&outcome.stdout_path, 128_000).unwrap_or_default(),
        &secrets,
        16_384,
        64_000,
    );
    let stderr_tail = testharbor_evidence::redact_and_bound_log(
        &read_log_tail(&outcome.stderr_path, 64_000).unwrap_or_default(),
        &secrets,
        16_384,
        32_000,
    );

    let finished = matches!(
        outcome.phase,
        TaskPhase::Exited | TaskPhase::TimedOut | TaskPhase::Cancelled | TaskPhase::Failed
    );

    let mut generic_result = None;
    let mut structured = None;
    let mut intended_observation = None;
    let mut accept_pass = None;
    let mut analysis = None;
    let mut result_review = None;
    let mut agent_error = None;

    if finished {
        if let Some(service_task_id) = &active.service_task_id {
            if let Ok(service_outcome) = state.supervisor().cancel(service_task_id) {
                let _ = state.with_store(|store| {
                    store.set_process_identity(
                        service_task_id,
                        "terminated",
                        service_outcome.pid,
                        service_outcome.pgid,
                        service_outcome.start_token.as_deref(),
                        &service_outcome,
                    )
                });
            }
        }
        state.with_store(|store| {
            store.set_process_identity(
                &active.task_id,
                match outcome.phase {
                    TaskPhase::Exited => "exited",
                    TaskPhase::Cancelled => "cancelled",
                    TaskPhase::TimedOut => "timed_out",
                    _ => "failed",
                },
                outcome.pid,
                outcome.pgid,
                outcome.start_token.as_deref(),
                &outcome,
            )
        })?;
        let raw_stdout = read_full_log(&outcome.stdout_path);
        let raw_stderr = read_full_log(&outcome.stderr_path);
        let full_stdout = testharbor_evidence::redact_and_bound_log(
            &raw_stdout,
            &secrets,
            64 * 1024,
            2 * 1024 * 1024,
        );
        let full_stderr = testharbor_evidence::redact_and_bound_log(
            &raw_stderr,
            &secrets,
            64 * 1024,
            1024 * 1024,
        );
        sanitize_finalized_log(&outcome.stdout_path, &full_stdout);
        sanitize_finalized_log(&outcome.stderr_path, &full_stderr);

        if active.purpose == ExecutionPurpose::Diagnostic {
            let diagnostic_structured = (parser_kind(&active.parser) == "go-test-json")
                .then(|| parse_go_test_json_with_exit(&raw_stdout, outcome.exit_code));
            let diagnostic_generic = drivers::normalize_generic(&active.task_id, outcome.clone());
            state.clear_active_execution(&run_id.to_string())?;
            state.with_store(|store| {
                store.finalize_execution(
                    &active.execution_id,
                    match outcome.phase {
                        TaskPhase::Exited => "complete",
                        TaskPhase::Cancelled => "cancelled",
                        TaskPhase::TimedOut => "timed_out",
                        _ => "failed",
                    },
                    &serde_json::json!({
                        "outcome": outcome,
                        "structured": diagnostic_structured,
                    }),
                )?;
                store.append_event(
                    Some(run_id),
                    None,
                    "diagnostic_execution_finished",
                    &serde_json::json!({
                        "execution_id": active.execution_id,
                        "outcome": outcome,
                        "structured": diagnostic_structured,
                    }),
                )
            })?;
            return Ok(ExecutionPollResult {
                execution_id: active.execution_id,
                task_id: active.task_id,
                outcome,
                stdout_tail,
                stderr_tail,
                generic_result: Some(diagnostic_generic),
                structured: diagnostic_structured,
                intended_observation: None,
                accept_pass: None,
                analysis: None,
                result_review: None,
                agent_error: None,
            });
        }

        if parser_kind(&active.parser) == "go-test-json" {
            let norm = parse_go_test_json_with_exit(&raw_stdout, outcome.exit_code);
            if let Some(intended) = state.get_intended(&run_id.to_string()) {
                let obs = observe_intended(&intended, &norm);
                intended_observation = Some(obs.clone());
                accept_pass = Some(build_accept_pass_view(
                    state,
                    &outcome,
                    &norm,
                    &intended,
                    &obs,
                    &run_id.to_string(),
                    &active.execution_id,
                ));
            } else {
                // No intended: still expose structured result; AcceptPass blocked (count 0)
                accept_pass = Some(AcceptPassView {
                    enabled: false,
                    reasons: vec![
                        "未设置 intended manifest（请先收集 Inventory 并选择 intended）".into(),
                    ],
                    reason_codes: vec!["INTENDED_COUNT_ZERO".into()],
                });
            }
            structured = Some(norm.clone());
            state.set_structured(run_id.to_string(), norm)?;
        } else if parser_kind(&active.parser) == "junit-xml" {
            let run = state.with_store(|store| store.get_run(run_id))?;
            let project = state.with_store(|store| store.get_project(run.project_id))?;
            let cwd = run
                .worktree_path
                .as_ref()
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
                .unwrap_or_else(|| PathBuf::from(&project.root_path));
            let spec = parser_spec(&active.parser).unwrap_or(".testharbor/junit.xml");
            let reports = collect_junit_reports(&cwd, spec);
            let norm = parse_junit_xml_reports_with_exit(&reports, outcome.exit_code);
            accept_pass = Some(AcceptPassView {
                enabled: false,
                reasons: vec![
                    "当前 JUnit XML 链路缺少稳定 intended/inventory 绑定，不能严格 AcceptPass。"
                        .into(),
                ],
                reason_codes: vec!["STRUCTURED_WITHOUT_INTENDED_BINDING".into()],
            });
            structured = Some(norm.clone());
            state.set_structured(run_id.to_string(), norm)?;
        }

        let gr = drivers::normalize_generic(&active.task_id, outcome.clone());
        // For go-test-json, refine summary
        let gr = if let Some(ref s) = structured {
            GenericTestResult {
                summary: format!(
                    "{} | structured: total={} pass={} fail={} empty={} complete={:?} consistent={:?}",
                    if s.empty {
                        "Empty run (0 tests)"
                    } else if s.failed > 0 {
                        "有失败用例"
                    } else if s.completeness == ResultCompleteness::Complete
                        && s.consistency == ResultConsistency::Consistent
                        && outcome.exit_code == Some(0)
                    {
                        "结构化结果完整且命令 exit 0（仍需 intended 观察才能 AcceptPass）"
                    } else {
                        "结构化结果已解析"
                    },
                    s.total,
                    s.passed,
                    s.failed,
                    s.empty,
                    s.completeness,
                    s.consistency
                ),
                completeness: if parser_kind(&active.parser) == "go-test-json"
                    || parser_kind(&active.parser) == "junit-xml"
                {
                    s.completeness
                } else {
                    gr.completeness
                },
                ..gr
            }
        } else {
            gr
        };
        generic_result = Some(gr.clone());

        // Every exited formal execution requires Analyzer and an independent
        // Result Reviewer, including deterministic green results.
        if outcome.phase == TaskPhase::Exited {
            let (an, rr, error) = run_result_agents(
                state,
                &run_id.to_string(),
                &active.parser,
                &outcome,
                structured.as_ref(),
                intended_observation.as_ref(),
                accept_pass.as_ref(),
                &full_stdout,
                &full_stderr,
            );
            analysis = an;
            result_review = rr;
            agent_error = error;
            if agent_error.is_some() {
                accept_pass = Some(AcceptPassView {
                    enabled: false,
                    reasons: vec![
                        "Analyzer 或 Result Reviewer 未完成，必须修复环境并重试结果分析".into(),
                    ],
                    reason_codes: vec!["RESULT_AGENT_REVIEW_REQUIRED".into()],
                });
            }
        }

        let last_execution = LastExecutionRecord {
            execution_id: active.execution_id.clone(),
            parser: active.parser.clone(),
            outcome: outcome.clone(),
            generic_result: gr.clone(),
            structured: structured.clone(),
            intended_observation: intended_observation.clone(),
            accept_pass: accept_pass.clone(),
            analysis: analysis.clone(),
            result_review: result_review.clone(),
            agent_error: agent_error.clone(),
        };
        state.set_last_execution(run_id.to_string(), last_execution)?;

        if state.take_active_execution(&run_id.to_string()).is_some() {
            state.clear_active_execution(&run_id.to_string())?;
            state.with_store_transaction(|s| {
                let next = match outcome.phase {
                    TaskPhase::Exited if agent_error.is_none() => {
                        WorkflowState::PendingResultReview
                    }
                    TaskPhase::Exited => WorkflowState::NeedsHuman,
                    TaskPhase::Cancelled | TaskPhase::TimedOut => WorkflowState::NeedsHuman,
                    _ => WorkflowState::Failed,
                };
                let run = s.get_run(run_id)?;
                s.update_run_worktree(run.id, run.worktree_path.as_deref(), next)?;
                s.finalize_execution(
                    &active.execution_id,
                    match outcome.phase {
                        TaskPhase::Exited => "complete",
                        TaskPhase::Cancelled => "cancelled",
                        TaskPhase::TimedOut => "timed_out",
                        _ => "failed",
                    },
                    &serde_json::json!({
                        "generic": gr,
                        "structured": structured,
                        "intended_observation": intended_observation,
                        "analysis": analysis,
                        "result_review": result_review,
                        "agent_error": agent_error,
                    }),
                )?;
                s.append_event(
                    Some(run_id),
                    None,
                    "execution_finished",
                    &serde_json::json!({
                        "generic": gr,
                        "structured": structured,
                        "intended_observation": intended_observation,
                        "analysis": analysis,
                        "result_review": result_review,
                        "agent_error": agent_error,
                    }),
                )?;
                Ok::<(), StorageError>(())
            })?;
        }
    }

    Ok(ExecutionPollResult {
        execution_id: active.execution_id,
        task_id: active.task_id,
        outcome,
        stdout_tail,
        stderr_tail,
        generic_result,
        structured,
        intended_observation,
        accept_pass,
        analysis,
        result_review,
        agent_error,
    })
}

fn prepare_junit_report_target(cwd: &Path, spec: &str) -> Result<()> {
    let Some(target) = safe_report_target(cwd, spec) else {
        return Err(ExecutionError::Other(
            "invalid junit report path in parser profile".into(),
        ));
    };
    if looks_like_xml_file(&target) {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ExecutionError::Other(format!("failed to create junit report dir: {error}"))
            })?;
        }
        if target.exists() {
            std::fs::remove_file(&target).map_err(|error| {
                ExecutionError::Other(format!("failed to remove stale junit report: {error}"))
            })?;
        }
        return Ok(());
    }

    remove_xml_reports_under(&target)
        .map_err(|error| ExecutionError::Other(format!("failed to clear junit reports: {error}")))
}

fn collect_junit_reports(cwd: &Path, spec: &str) -> Vec<(String, String)> {
    let Some(target) = safe_report_target(cwd, spec) else {
        return Vec::new();
    };
    if target.is_file() {
        return std::fs::read_to_string(&target)
            .map(|content| vec![(target.display().to_string(), content)])
            .unwrap_or_default();
    }
    if target.is_dir() {
        let mut reports = Vec::new();
        let mut stack = vec![target.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    stack.push(path);
                } else if file_type.is_file()
                    && path.extension().and_then(|value| value.to_str()) == Some("xml")
                {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        reports.push((path.display().to_string(), content));
                    }
                }
            }
        }
        reports.sort_by(|left, right| left.0.cmp(&right.0));
        return reports;
    }
    Vec::new()
}

fn safe_report_target(cwd: &Path, spec: &str) -> Option<PathBuf> {
    let spec_path = Path::new(spec);
    if spec_path.is_absolute() {
        return None;
    }
    if spec_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    Some(cwd.join(spec_path))
}

fn looks_like_xml_file(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some("xml")
}

fn remove_xml_reports_under(target: &Path) -> std::io::Result<()> {
    if target.is_file() {
        if looks_like_xml_file(target) {
            std::fs::remove_file(target)?;
        }
        return Ok(());
    }
    if !target.is_dir() {
        return Ok(());
    }
    let mut stack = vec![target.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() && looks_like_xml_file(&path) {
                std::fs::remove_file(path)?;
            }
        }
    }
    Ok(())
}

fn agent_mode_for_run(state: &AppState, run_id: &str) -> AgentMode {
    match state
        .get_intent_meta(run_id)
        .and_then(|m| m.agent_mode)
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("mock") => AgentMode::Mock,
        Some("claude") => AgentMode::Claude,
        Some("grok") => AgentMode::Grok,
        _ => AgentMode::Codex,
    }
}

fn excerpt(s: &str, max: usize) -> String {
    let chars = s.chars().collect::<Vec<_>>();
    if chars.len() <= max {
        return s.to_string();
    }
    let head = max / 2;
    let tail = max.saturating_sub(head);
    format!(
        "{}\n…[中间日志已省略]…\n{}",
        chars[..head].iter().collect::<String>(),
        chars[chars.len() - tail..].iter().collect::<String>()
    )
}

fn failure_focused_excerpt(
    stdout: &str,
    structured: Option<&NormalizedStructuredResult>,
    max: usize,
) -> String {
    let lines = stdout.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }

    let mut selected = BTreeSet::new();
    let failed_cases = structured
        .map(|result| {
            result
                .packages
                .iter()
                .flat_map(|package| {
                    package
                        .cases
                        .iter()
                        .filter(|case| case.status == CaseStatus::Fail)
                        .map(|case| (case.package.clone(), case.name.clone()))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if !failed_cases.is_empty() {
        for (index, line) in lines.iter().enumerate() {
            for (package, test) in &failed_cases {
                if line.contains(package) && line.contains(test) {
                    let start = index.saturating_sub(6);
                    let end = (index + 8).min(lines.len());
                    for candidate in start..end {
                        selected.insert(candidate);
                    }
                }
            }
        }
    }

    for (index, line) in lines.iter().enumerate() {
        let lower = line.to_lowercase();
        let interesting = lower.contains("\"action\":\"fail\"")
            || lower.contains("--- fail:")
            || lower.contains("fail\\t")
            || lower.contains("failure")
            || lower.contains("assert")
            || lower.contains("expected")
            || lower.contains("actual")
            || lower.contains("\"success\":\"fail\"")
            || lower.contains("\\\"success\\\":\\\"fail\\\"")
            || lower.contains("\"code\":-1")
            || lower.contains("\\\"code\\\":-1")
            || lower.contains("publish release:");
        if interesting && (failed_cases.is_empty() || selected.contains(&index)) {
            let start = index.saturating_sub(4);
            let end = (index + 5).min(lines.len());
            for candidate in start..end {
                selected.insert(candidate);
            }
        }
    }

    if selected.is_empty() {
        return excerpt(stdout, max);
    }

    fn compact_line(line: &str, max: usize) -> String {
        let chars = line.chars().collect::<Vec<_>>();
        if chars.len() <= max {
            return line.to_string();
        }
        let head = max / 2;
        let tail = max.saturating_sub(head);
        format!(
            "{}...[long line omitted]...{}",
            chars[..head].iter().collect::<String>(),
            chars[chars.len() - tail..].iter().collect::<String>()
        )
    }

    fn belongs_to_other_go_case(line: &str, failed_cases: &[(String, String)]) -> bool {
        if failed_cases.is_empty() {
            return false;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            return false;
        };
        let Some(test) = value.get("Test").and_then(|v| v.as_str()) else {
            return false;
        };
        let package = value.get("Package").and_then(|v| v.as_str()).unwrap_or("");
        !failed_cases
            .iter()
            .any(|(failed_package, failed_test)| failed_package == package && failed_test == test)
    }

    let mut out = String::new();
    let mut previous = None;
    for index in selected {
        if belongs_to_other_go_case(lines[index], &failed_cases) {
            continue;
        }
        if previous.is_some_and(|prev| index > prev + 1) {
            out.push_str("\n...[unrelated passing output omitted]...\n");
        }
        out.push_str(&compact_line(lines[index], 900));
        out.push('\n');
        previous = Some(index);
    }
    excerpt(out.trim_end(), max)
}

/// Run Analyzer then Result Reviewer after execution finishes. Non-fatal on agent errors.
#[allow(clippy::too_many_arguments)]
fn run_result_agents(
    state: &AppState,
    run_id: &str,
    parser: &str,
    outcome: &ProcessOutcome,
    structured: Option<&NormalizedStructuredResult>,
    intended_obs: Option<&IntendedObservation>,
    accept_pass: Option<&AcceptPassView>,
    full_stdout: &str,
    full_stderr: &str,
) -> (
    Option<AnalyzeResult>,
    Option<ResultReviewAgentResult>,
    Option<String>,
) {
    let mode = agent_mode_for_run(state, run_id);
    let meta = state.get_intent_meta(run_id);
    let objective = meta
        .as_ref()
        .map(|m| m.objective.clone())
        .unwrap_or_else(|| "(no objective recorded)".into());
    let criteria = meta
        .as_ref()
        .map(|m| m.acceptance_criteria.clone())
        .unwrap_or_default();
    let intent_hash = meta
        .as_ref()
        .map(|m| m.test_intent_hash.clone())
        .unwrap_or_else(|| "unknown".into());
    let agent_model = meta.as_ref().and_then(|m| m.agent_model.clone());
    let (total, passed, failed, skipped, empty, completeness, consistency, failure_phase) =
        if let Some(s) = structured {
            (
                s.total,
                s.passed,
                s.failed,
                s.skipped,
                s.empty,
                format!("{:?}", s.completeness).to_lowercase(),
                format!("{:?}", s.consistency).to_lowercase(),
                s.failure_phase
                    .as_ref()
                    .map(|p| format!("{p:?}").to_lowercase()),
            )
        } else {
            (0, 0, 0, 0, true, "opaque".into(), "unknown".into(), None)
        };

    let (intended_count, intended_observed, intended_failed, intended_missing) =
        if let Some(o) = intended_obs {
            (
                o.intended_count,
                o.observed_count,
                o.failed_count,
                o.missing.len() as u32,
            )
        } else {
            (0, 0, 0, 0)
        };

    let candidate_tree = state
        .get_candidate(run_id)
        .map(|candidate| candidate.candidate_tree)
        .unwrap_or_else(|| "missing-candidate".into());
    let command_hash = state
        .get_trust_grant(run_id)
        .map(|grant| testharbor_trust::command_snapshot_hash(&grant.profile.command))
        .unwrap_or_else(|| "missing-command".into());
    let execution_scope_hash = testharbor_domain::canonical_hash(&[
        &intent_hash,
        &candidate_tree,
        &command_hash,
        parser,
        outcome
            .executable_identity
            .as_deref()
            .unwrap_or("missing-identity"),
    ]);
    let evidence_refs = vec![
        format!("stdout:{}", outcome.stdout_path.display()),
        format!("stderr:{}", outcome.stderr_path.display()),
        format!(
            "process-start:{}",
            outcome.start_token.as_deref().unwrap_or("missing")
        ),
    ];

    let analyze_req = AnalyzeRequest {
        // Result analysis is a separate, small task. Reusing the generation
        // transcript would replay all repository exploration and tool output.
        session_id: None,
        agent_model: agent_model.clone(),
        test_intent_hash: intent_hash.clone(),
        scope_hash: execution_scope_hash.clone(),
        evidence_refs: evidence_refs.clone(),
        objective: objective.clone(),
        acceptance_criteria: criteria,
        exit_code: outcome.exit_code,
        parser: parser.to_string(),
        completeness: completeness.clone(),
        consistency: consistency.clone(),
        total,
        passed,
        failed,
        skipped,
        empty,
        intended_count,
        intended_observed,
        intended_failed,
        intended_missing,
        stdout_excerpt: failure_focused_excerpt(full_stdout, structured, 3600),
        stderr_excerpt: excerpt(full_stderr, 1200),
        failure_phase,
    };

    let mut agent_error = None;
    let analysis = match analyze_result(mode, &analyze_req) {
        Ok(a) => {
            let _ = state.set_analysis(run_id.to_string(), a.clone());
            Some(a)
        }
        Err(e) => {
            agent_error = Some(format!("Codex 结果分析失败：{e}"));
            let _ = state.with_store(|s| {
                s.append_event(
                    Some(RunId(uuid::Uuid::parse_str(run_id).unwrap_or_default())),
                    None,
                    "analyzer_failed",
                    &serde_json::json!({"error": e.to_string()}),
                )
            });
            None
        }
    };

    let result_review = if let Some(ref an) = analysis {
        let ap_enabled = accept_pass.map(|a| a.enabled).unwrap_or(false);
        let ap_reasons = accept_pass.map(|a| a.reasons.clone()).unwrap_or_default();
        let analyzer_hash =
            testharbor_domain::sha256_hex(&serde_json::to_vec(an).unwrap_or_else(|_| Vec::new()));
        let result_review_scope =
            testharbor_domain::canonical_hash(&[&execution_scope_hash, &analyzer_hash]);
        let mut result_review_evidence = evidence_refs;
        result_review_evidence.push(format!("analyzer:sha256:{analyzer_hash}"));
        let rr_req = ResultReviewRequest {
            agent_model,
            test_intent_hash: intent_hash,
            scope_hash: result_review_scope,
            evidence_refs: result_review_evidence,
            objective,
            analyzer_verdict: an.suggested_verdict.clone(),
            analyzer_summary: an.summary.clone(),
            analyzer_confidence: an.confidence.clone(),
            exit_code: outcome.exit_code,
            completeness,
            consistency,
            total,
            failed,
            empty,
            intended_observed,
            intended_count,
            accept_pass_enabled: ap_enabled,
            accept_pass_reasons: ap_reasons,
        };
        match review_result(mode, &rr_req) {
            Ok(r) => {
                let _ = state.set_result_agent_review(run_id.to_string(), r.clone());
                Some(r)
            }
            Err(e) => {
                agent_error = Some(format!("Codex 独立结果复核失败：{e}"));
                let _ = state.with_store(|s| {
                    s.append_event(
                        Some(RunId(uuid::Uuid::parse_str(run_id).unwrap_or_default())),
                        None,
                        "result_reviewer_failed",
                        &serde_json::json!({"error": e.to_string()}),
                    )
                });
                None
            }
        }
    } else {
        None
    };

    (analysis, result_review, agent_error)
}

fn read_full_log(path: &std::path::Path) -> String {
    let candidates = [
        path.to_path_buf(),
        std::path::PathBuf::from(path.to_string_lossy().trim_end_matches(".partial")),
    ];
    for p in candidates {
        if let Ok(s) = std::fs::read_to_string(p) {
            return s;
        }
    }
    String::new()
}

fn runtime_secret_values(state: &AppState, run_id: &str) -> Vec<String> {
    let Some(environment) = state.get_test_environment(run_id) else {
        return Vec::new();
    };
    let mut values = environment
        .variables
        .values()
        .filter(|value| !value.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    values.extend(
        [environment.username, environment.password]
            .into_iter()
            .flatten()
            .filter(|value| !value.is_empty()),
    );
    values.sort();
    values.dedup();
    values
}

fn sanitize_finalized_log(partial_path: &Path, redacted: &str) {
    let final_path = partial_path
        .to_str()
        .and_then(|path| path.strip_suffix(".partial"))
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(|| partial_path.to_path_buf());
    if final_path.is_file() {
        let _ = std::fs::write(final_path, redacted);
    }
}

fn build_accept_pass_view(
    state: &AppState,
    outcome: &ProcessOutcome,
    norm: &NormalizedStructuredResult,
    intended: &IntendedTestManifest,
    obs: &IntendedObservation,
    run_id: &str,
    execution_id: &str,
) -> AcceptPassView {
    let mut workflow_state = uuid::Uuid::parse_str(run_id)
        .ok()
        .map(RunId)
        .and_then(|id| state.with_store(|store| store.get_run(id)).ok())
        .map(|run| run.workflow_state)
        .unwrap_or(WorkflowState::NeedsHuman);
    if workflow_state == WorkflowState::ExecutingTests
        && matches!(
            outcome.phase,
            TaskPhase::Exited | TaskPhase::TimedOut | TaskPhase::Cancelled | TaskPhase::Failed
        )
    {
        workflow_state = WorkflowState::PendingResultReview;
    }
    let current_execution_id = state
        .get_last_execution(run_id)
        .map(|execution| execution.execution_id)
        .unwrap_or_else(|| execution_id.to_string());
    // Bind real candidate / approval hashes when case was approved (S5).
    let approval = state.get_case_approval(run_id);
    let candidate = state.get_candidate(run_id);
    let (rev_id, tree, ctx_hash) = match (&approval, &candidate) {
        (Some(a), Some(c)) => (
            format!("r{}", c.sequence),
            c.candidate_tree.clone(),
            a.case_approval_context_hash.clone(),
        ),
        (Some(a), None) => (
            "approved".into(),
            a.candidate_tree.clone(),
            a.case_approval_context_hash.clone(),
        ),
        _ => {
            return AcceptPassView {
                enabled: false,
                reasons: vec!["用例尚未人工批准（需要 generate → review → approve）".into()],
                reason_codes: vec!["CASE_NOT_APPROVED".into()],
            };
        }
    };

    let intended_hash = approval
        .as_ref()
        .and_then(|a| a.intended_manifest_hash.clone())
        .unwrap_or_else(|| intended.hash.clone());

    let ctx = AcceptPassContext {
        workflow_state,
        execution_id: execution_id.to_string(),
        current_execution_id,
        execution_revision_id: rev_id.clone(),
        approved_revision_id: rev_id,
        execution_candidate_tree: tree.clone(),
        current_candidate_tree: tree,
        approval_context_hash: ctx_hash.clone(),
        recomputed_approval_context_hash: ctx_hash,
        execution_exited: outcome.phase == TaskPhase::Exited,
        process_exit_code: outcome.exit_code,
        completeness: norm.completeness,
        consistency: norm.consistency,
        result_manifest_hash: norm.result_manifest_hash.clone(),
        recomputed_result_manifest_hash: norm.result_manifest_hash.clone(),
        intended_manifest_hash: intended.hash.clone(),
        approved_intended_manifest_hash: intended_hash,
        intended_test_count: obs.intended_count,
        intended_failed: obs.failed_count,
        intended_observed: obs.observed_count,
        unexplained_skipped: 0,
        total_failed: norm.failed,
        total_errors: if norm.failure_phase
            == Some(testharbor_result_normalizer::FailurePhase::Build)
        {
            1
        } else {
            0
        },
        every_started_suite_has_terminal: norm.every_started_has_terminal,
    };

    // Empty run: force fail accept
    let mut result = can_accept_pass(&ctx);
    if norm.empty {
        result.ok = false;
        result.reasons.push(GuardReason::IntendedCountZero);
    }

    AcceptPassView {
        enabled: result.ok,
        reasons: result.reasons.iter().map(|r| format!("{r:?}")).collect(),
        reason_codes: result.reasons.iter().map(reason_code).collect(),
    }
}

fn reason_code(r: &GuardReason) -> String {
    match r {
        GuardReason::IntendedNotObserved => "INTENDED_NOT_OBSERVED".into(),
        GuardReason::ResultOpaque => "RESULT_OPAQUE".into(),
        GuardReason::ResultNotComplete => "RESULT_NOT_COMPLETE".into(),
        GuardReason::ResultContradictory => "RESULT_CONTRADICTORY".into(),
        GuardReason::IntendedCountZero => "INTENDED_COUNT_ZERO".into(),
        GuardReason::RegressionFailed { .. } => "REGRESSION_FAILED".into(),
        GuardReason::ProcessNotSucceeded => "PROCESS_NOT_SUCCEEDED".into(),
        other => format!("{other:?}"),
    }
}

pub fn cancel_execution(state: &AppState, run_id: &str) -> Result<ExecutionPollResult> {
    let rid = parse_run_id(run_id)?;
    let active = state
        .get_active_execution(&rid.to_string())
        .ok_or(ExecutionError::NoActiveExecution)?;
    let _ = state.supervisor().cancel(&active.task_id)?;
    poll_execution(state, run_id)
}

pub fn get_last_execution_result(state: &AppState, run_id: &str) -> Result<ExecutionPollResult> {
    let _ = parse_run_id(run_id)?;
    let record = state
        .get_last_execution(run_id)
        .ok_or_else(|| ExecutionError::Other("no persisted execution result".into()))?;
    Ok(ExecutionPollResult {
        execution_id: record.execution_id,
        task_id: "persisted".into(),
        outcome: record.outcome.clone(),
        stdout_tail: read_log_tail(&record.outcome.stdout_path, 64_000).unwrap_or_default(),
        stderr_tail: read_log_tail(&record.outcome.stderr_path, 32_000).unwrap_or_default(),
        generic_result: Some(record.generic_result),
        structured: record.structured,
        intended_observation: record.intended_observation,
        accept_pass: record.accept_pass,
        analysis: record.analysis,
        result_review: record.result_review,
        agent_error: record.agent_error,
    })
}

pub fn confirm_custom_command(
    state: &AppState,
    run_id: &str,
    executable: String,
    args: Vec<String>,
    timeout_ms: u64,
) -> Result<ConfirmedTrust> {
    let run_id = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(run_id))?;
    let project = state.with_store(|s| s.get_project(run.project_id))?;
    let cwd = run
        .worktree_path
        .clone()
        .filter(|p| PathBuf::from(p).is_dir())
        .unwrap_or_else(|| project.root_path.clone());

    let profile = TestProfileDraft {
        name: "custom".into(),
        format: None,
        collect: None,
        compile: None,
        command: CommandIdentity {
            executable,
            args,
            cwd,
            parser: "exit-code".into(),
            executable_version: None,
            executable_identity: None,
        },
        timeout_ms,
        service: None,
        env_policy: vec![],
        network_unrestricted: false,
    };
    confirm_profile(state, &run_id.to_string(), profile)
}

/// Change only the execution timeout while preserving the selected command,
/// parser, and approved candidate.
pub fn set_execution_timeout(
    state: &AppState,
    run_id: &str,
    timeout_ms: u64,
) -> Result<ConfirmedTrust> {
    if timeout_ms == 0 {
        return Err(ExecutionError::Other(
            "execution timeout must be greater than zero".into(),
        ));
    }
    let profile = match state.get_trust_grant(run_id) {
        Some(grant) => grant.profile,
        None => suggest_profile(state, run_id)?.profile,
    };
    let mut profile = profile;
    profile.timeout_ms = timeout_ms;
    confirm_profile(state, run_id, profile)
}

/// Re-evaluate AcceptPass from last structured result (after setting intended).
pub fn reevaluate_accept_pass(state: &AppState, run_id: &str) -> Result<AcceptPassView> {
    let record = state
        .get_last_execution(run_id)
        .ok_or_else(|| ExecutionError::Other("no persisted execution result".into()))?;
    let structured = record
        .structured
        .as_ref()
        .ok_or(ExecutionError::NoStructuredResult)?;
    let intended = state
        .get_intended(run_id)
        .ok_or_else(|| ExecutionError::Other("no intended manifest".into()))?;
    let obs = record
        .intended_observation
        .unwrap_or_else(|| observe_intended(&intended, structured));
    Ok(build_accept_pass_view(
        state,
        &record.outcome,
        structured,
        &intended,
        &obs,
        run_id,
        &record.execution_id,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPurpose {
    #[default]
    Formal,
    Diagnostic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveExecution {
    pub execution_id: String,
    pub task_id: String,
    pub parser: String,
    pub outcome: ProcessOutcome,
    #[serde(default)]
    pub purpose: ExecutionPurpose,
    #[serde(default)]
    pub service_task_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::open_project;
    use crate::AppState;
    use std::process::Command;

    fn init_repo(dir: &std::path::Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@e.com"])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(dir)
            .status()
            .unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "i"])
            .current_dir(dir)
            .status()
            .unwrap();
    }

    #[test]
    fn go_candidate_selection_narrows_profile_to_changed_tests() {
        let names = parse_go_test_names(
            r#"
package demo

func helper() {}
func TestGeneratedFlow(t *testing.T) {}
func TestAnother_Case(t *testing.T) {}
func ExampleGenerated() {}
"#,
        );
        assert_eq!(
            names,
            vec![
                "ExampleGenerated".to_string(),
                "TestAnother_Case".to_string(),
                "TestGeneratedFlow".to_string()
            ]
        );

        let selection = CandidateTestSelection {
            package_patterns: vec!["./module/demo".into()],
            test_names: names,
        };
        let mut profile = TestProfileDraft {
            name: "go-test-json".into(),
            format: None,
            collect: None,
            compile: None,
            command: CommandIdentity {
                executable: "go".into(),
                args: vec![
                    "test".into(),
                    "-json".into(),
                    "-count=1".into(),
                    "./...".into(),
                ],
                cwd: ".".into(),
                parser: "go-test-json".into(),
                executable_version: None,
                executable_identity: None,
            },
            timeout_ms: 60_000,
            service: None,
            env_policy: vec![],
            network_unrestricted: false,
        };
        narrow_go_profile_to_candidate(&mut profile, &selection);

        assert_eq!(
            profile.command.args,
            vec![
                "test",
                "-json",
                "-count=1",
                "-run",
                "^(ExampleGenerated|TestAnother_Case|TestGeneratedFlow)$",
                "./module/demo"
            ]
        );
    }

    #[test]
    fn case_marker_extracts_stable_id() {
        assert_eq!(
            case_id_from_marker(
                r#"t.Log(`TESTHARBOR_CASE {"id":"publish-happy","title":"发布成功"}`)"#,
            ),
            Some("publish-happy".into())
        );
        assert_eq!(
            case_id_from_marker(r#"TESTHARBOR_CASE {"case_id":"legacy-case"}"#),
            Some("legacy-case".into())
        );
    }

    #[test]
    fn result_analysis_excerpt_focuses_on_failed_go_cases() {
        let stdout = r#"{"Time":"2026-07-20T01:00:00Z","Action":"run","Package":"demo/pkg","Test":"TestPassingCase"}
{"Time":"2026-07-20T01:00:01Z","Action":"output","Package":"demo/pkg","Test":"TestPassingCase","Output":"PASSING_TEST_SHOULD_NOT_BE_INCLUDED with a very large successful response body\n"}
{"Time":"2026-07-20T01:00:02Z","Action":"pass","Package":"demo/pkg","Test":"TestPassingCase","Elapsed":0.1}
{"Time":"2026-07-20T01:00:03Z","Action":"run","Package":"demo/pkg","Test":"TestFailedCase"}
{"Time":"2026-07-20T01:00:04Z","Action":"output","Package":"demo/pkg","Test":"TestFailedCase","Output":"TESTHARBOR_STEP {\"name\":\"发布服务版本\",\"type\":\"api\"}\n"}
{"Time":"2026-07-20T01:00:05Z","Action":"output","Package":"demo/pkg","Test":"TestFailedCase","Output":"publish release: expected success, actual {\"success\":\"fail\",\"code\":-1}\n"}
{"Time":"2026-07-20T01:00:06Z","Action":"fail","Package":"demo/pkg","Test":"TestFailedCase","Elapsed":0.2}
{"Time":"2026-07-20T01:00:07Z","Action":"fail","Package":"demo/pkg","Elapsed":0.3}
"#;
        let structured = parse_go_test_json_with_exit(stdout, Some(1));

        let focused = failure_focused_excerpt(stdout, Some(&structured), 3600);

        assert!(focused.contains("TestFailedCase"));
        assert!(focused.contains("publish release"));
        assert!(!focused.contains("PASSING_TEST_SHOULD_NOT_BE_INCLUDED"));
    }

    #[test]
    fn junit_report_target_is_scoped_and_clears_stale_xml() {
        let dir = tempfile::tempdir().unwrap();
        let reports = dir.path().join("target").join("surefire-reports");
        std::fs::create_dir_all(&reports).unwrap();
        std::fs::write(reports.join("TEST-old.xml"), "<testsuite />").unwrap();
        std::fs::write(reports.join("keep.txt"), "diagnostic").unwrap();

        prepare_junit_report_target(dir.path(), "target/surefire-reports").unwrap();

        assert!(!reports.join("TEST-old.xml").exists());
        assert!(reports.join("keep.txt").exists());
        assert!(prepare_junit_report_target(dir.path(), "../outside.xml").is_err());

        prepare_junit_report_target(dir.path(), ".testharbor/junit.xml").unwrap();
        assert!(dir.path().join(".testharbor").is_dir());
    }

    #[test]
    fn execution_without_approved_candidate_is_blocked() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;
        state
            .set_intent_meta(
                run_id.clone(),
                crate::generate::IntentMeta {
                    objective: "test echo execution".into(),
                    acceptance_criteria: vec!["execution completes".into()],
                    test_intent_hash: "test-intent".into(),
                    test_strategy: "e2e_api_flow".into(),
                    cleanup_test_data: true,
                    agent_mode: Some("mock".into()),
                    reuse_agent_session: false,
                    agent_model: None,
                    environment: None,
                },
            )
            .unwrap();

        let profile = TestProfileDraft {
            name: "echo-smoke".into(),
            format: None,
            collect: None,
            compile: None,
            command: CommandIdentity {
                executable: "/bin/echo".into(),
                args: vec!["testharbor-ok".into()],
                cwd: repo.path().display().to_string(),
                parser: "exit-code".into(),
                executable_version: None,
                executable_identity: None,
            },
            timeout_ms: 10_000,
            service: None,
            env_policy: vec![],
            network_unrestricted: false,
        };
        confirm_profile(&state, &run_id, profile).unwrap();

        let error = start_test_execution(&state, &run_id, None).unwrap_err();
        assert!(error.to_string().contains("candidate"));
    }
}
