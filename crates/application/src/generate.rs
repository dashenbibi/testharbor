//! Generate tests → freeze/review → automatic major-issue revision loop → human approve.

use crate::{inspect_project, AppState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;
use testharbor_agent_drivers::{
    generate, is_session_resume_error, review_tests, AgentFinding, AgentMode, AgentTestRunner,
    GenerateRequest, ReviewRequest,
};
use testharbor_domain::{
    case_approval_context_hash, content_body_hash, sha256_hex, test_case_scope_hash,
    test_intent_hash as compute_test_intent_hash, truncate_utf8, try_transition,
    AcceptanceCriterion, FindingDisposition, HumanTestDecision, RevisionStatus, RunId, RunSnapshot,
    SourceAnchor, SpecAttachmentId, SpecAttachmentKind, SpecAttachmentSnapshot, SpecHashPart,
    TestIntentHashInput, TestIntentSnapshot, WorkflowEvent, WorkflowState,
    SPEC_MAX_BYTES_PER_ATTACHMENT, SPEC_MAX_BYTES_TOTAL,
};
use testharbor_process_supervisor::{new_task_id, CommandSpec, TaskPhase};
use testharbor_result_normalizer::{collect_go_inventory, make_intended_manifest};
use testharbor_review::{
    evaluate_can_approve, is_major_severity, write_review_bundle, CaseApprovalRecord,
    CaseReviewState, ReviewFinding,
};
use testharbor_storage::{write_content_addressed, ProjectRecord, RunRecord, StorageError};
use testharbor_workspace_manager::{self as ws, CandidateRevision};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum GenerateError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Workspace(#[from] ws::WorkspaceError),
    #[error(transparent)]
    Agent(#[from] testharbor_agent_drivers::AgentError),
    #[error(transparent)]
    Process(#[from] testharbor_process_supervisor::ProcessError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid run id")]
    InvalidRunId,
    #[error("worktree required: prepare worktree first")]
    WorktreeRequired,
    #[error("no candidate revision")]
    NoCandidate,
    #[error("no case review state")]
    NoReview,
    #[error("approve blocked: {0}")]
    ApproveBlocked(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, GenerateError>;

const DEFAULT_MAX_AUTO_REVIEW_REVISIONS: u32 = 3;

fn max_auto_review_revisions() -> u32 {
    std::env::var("TESTHARBOR_MAX_AUTO_REVIEW_REVISIONS")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(DEFAULT_MAX_AUTO_REVIEW_REVISIONS)
        .min(3)
}

fn project_language_hint(root: &Path) -> &'static str {
    if root.join("go.mod").is_file() {
        "go"
    } else if root.join("Cargo.toml").is_file() {
        "rust"
    } else if root.join("pyproject.toml").is_file() || root.join("pytest.ini").is_file() {
        "python"
    } else if root.join("composer.json").is_file()
        || root.join("test.php").is_file()
        || root.join("Test.php").is_file()
    {
        "php"
    } else if root.join("pom.xml").is_file()
        || root.join("build.gradle").is_file()
        || root.join("build.gradle.kts").is_file()
    {
        "java"
    } else if root.join("package.json").is_file() {
        "node"
    } else if root.join("Makefile").is_file() {
        "make"
    } else {
        "unknown"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GenerateTestsRequest {
    pub run_id: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    /// Optional code symbols/files confirmed by the user.
    #[serde(default)]
    pub source_anchors: Vec<SourceAnchor>,
    /// Writable repository-relative glob scope. Empty uses the product default.
    #[serde(default)]
    pub generation_scope: Vec<String>,
    /// Frozen pasted text or base-commit repository Markdown specifications.
    #[serde(default)]
    pub spec_attachments: Vec<SpecAttachmentInput>,
    #[serde(default)]
    pub test_strategy: Option<String>,
    #[serde(default)]
    pub cleanup_test_data: Option<bool>,
    /// Start a fresh generation from the run base instead of freezing partial files.
    #[serde(default)]
    pub force_regenerate: bool,
    /// Internal test hook. Production callers always use "codex".
    pub agent_mode: Option<String>,
    /// Reuse the task's AI session only for generation and automatic revisions.
    #[serde(default)]
    pub reuse_agent_session: bool,
    /// Optional model for the selected agent. Grok falls back to its CLI default.
    #[serde(default)]
    pub agent_model: Option<String>,
    /// Runtime environment supplied by the user for black-box E2E testing.
    #[serde(default)]
    pub environment: Option<TestEnvironmentInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecAttachmentInput {
    pub kind: SpecAttachmentKind,
    /// Paste label or repository-relative Markdown path.
    pub label_or_path: String,
    /// Required for pasted text; ignored for repository Markdown.
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TestEnvironmentInput {
    /// Free-form environment prompt shown to the selected CLI.
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub health_url: Option<String>,
    #[serde(default)]
    pub login_url: Option<String>,
    #[serde(default)]
    pub startup_command: Option<String>,
    #[serde(default)]
    pub startup_cwd: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TestEnvironmentConfig {
    #[serde(default)]
    pub context: String,
    pub base_url: String,
    pub health_url: Option<String>,
    pub login_url: Option<String>,
    pub startup_command: Option<String>,
    pub startup_cwd: Option<String>,
    pub credentials_provided: bool,
    pub variable_names: Vec<String>,
    #[serde(default)]
    pub masked_inputs: Vec<MaskedRuntimeInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaskedRuntimeInput {
    pub key: String,
    pub source: String,
    pub hmac_fingerprint: String,
}

impl TestEnvironmentInput {
    fn config(&self, state: &AppState) -> Result<TestEnvironmentConfig> {
        let mut variable_names = self.variables.keys().cloned().collect::<Vec<_>>();
        variable_names.sort();
        let mut masked_inputs = self
            .variables
            .iter()
            .map(|(key, value)| {
                Ok(MaskedRuntimeInput {
                    key: key.clone(),
                    source: "runtime_variable".into(),
                    hmac_fingerprint: runtime_input_hmac(state, key, value)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        for (key, value) in [
            ("TESTHARBOR_TEST_USERNAME", self.username.as_deref()),
            ("TESTHARBOR_TEST_PASSWORD", self.password.as_deref()),
        ] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                masked_inputs.push(MaskedRuntimeInput {
                    key: key.into(),
                    source: "masked_prompt".into(),
                    hmac_fingerprint: runtime_input_hmac(state, key, value)?,
                });
            }
        }
        masked_inputs.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(TestEnvironmentConfig {
            context: self.context.trim().to_string(),
            base_url: self.base_url.trim().to_string(),
            health_url: self
                .health_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            login_url: self
                .login_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            startup_command: None,
            startup_cwd: None,
            credentials_provided: self
                .username
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                || self
                    .password
                    .as_deref()
                    .is_some_and(|value| !value.is_empty()),
            variable_names,
            masked_inputs,
        })
    }

    pub(crate) fn agent_environment(&self) -> testharbor_agent_drivers::TestEnvironment {
        let mut variables = self.variables.clone();
        if let Some(value) = self.username.as_deref().filter(|value| !value.is_empty()) {
            variables.insert("TESTHARBOR_TEST_USERNAME".into(), value.to_string());
        }
        if let Some(value) = self.password.as_deref().filter(|value| !value.is_empty()) {
            variables.insert("TESTHARBOR_TEST_PASSWORD".into(), value.to_string());
        }
        if !self.context.trim().is_empty() {
            variables.insert(
                "TESTHARBOR_ENVIRONMENT_CONTEXT".into(),
                self.context.trim().to_string(),
            );
        }
        testharbor_agent_drivers::TestEnvironment {
            context: self.context.trim().to_string(),
            base_url: self.base_url.trim().to_string(),
            health_url: self.health_url.clone(),
            login_url: self.login_url.clone(),
            variables,
        }
    }
}

fn runtime_input_hmac(state: &AppState, key_name: &str, value: &str) -> Result<String> {
    use std::io::Read;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let key_path = state.paths().root.join("runtime-input-hmac.key");
    let secret = if key_path.is_file() {
        std::fs::read(&key_path)?
    } else {
        let mut secret = vec![0u8; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut secret)?;
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&key_path) {
            Ok(mut file) => {
                file.write_all(&secret)?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                secret = std::fs::read(&key_path)?;
            }
            Err(error) => return Err(error.into()),
        }
        secret
    };
    let mut block = [0u8; 64];
    if secret.len() > block.len() {
        block[..32].copy_from_slice(&Sha256::digest(&secret));
    } else {
        block[..secret.len()].copy_from_slice(&secret);
    }
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for index in 0..64 {
        inner_pad[index] ^= block[index];
        outer_pad[index] ^= block[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(key_name.as_bytes());
    inner.update([0xff]);
    inner.update(value.as_bytes());
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_hash);
    Ok(format!("hmac-sha256:{}", hex::encode(outer.finalize())))
}

fn environment_with_context_defaults(environment: TestEnvironmentInput) -> TestEnvironmentInput {
    environment
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateTestsResult {
    pub agent_summary: String,
    pub written_files: Vec<String>,
    pub candidate: CandidateRevision,
    pub review: CaseReviewState,
    pub agent_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationProgress {
    pub stdout_tail: String,
    pub stderr_tail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingChangeRequest {
    pub message: String,
    pub previous_revision_sequence: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationRecovery {
    pub target: WorkflowState,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApproveCaseResult {
    pub approval: CaseApprovalRecord,
    pub workflow_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectCaseResult {
    pub workflow_state: String,
    pub message: String,
}

struct RunActivityGuard<'a> {
    state: &'a AppState,
    run_id: RunId,
    owner_id: String,
}

impl Drop for RunActivityGuard<'_> {
    fn drop(&mut self) {
        let _ = self.state.release_run_activity(self.run_id, &self.owner_id);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentCaseResult {
    pub workflow_state: String,
    pub message: String,
}

fn parse_run_id(s: &str) -> Result<RunId> {
    Uuid::parse_str(s)
        .map(RunId)
        .map_err(|_| GenerateError::InvalidRunId)
}

fn agent_mode(s: Option<&str>) -> AgentMode {
    match s.map(|x| x.to_lowercase()).as_deref() {
        Some("mock") => AgentMode::Mock,
        Some("claude") => AgentMode::Claude,
        Some("grok") => AgentMode::Grok,
        Some("codex") => AgentMode::Codex,
        None => AgentMode::from_env(),
        _ => AgentMode::Codex,
    }
}

fn normalize_test_strategy(strategy: Option<&str>) -> String {
    match strategy.map(|value| value.trim().to_lowercase()).as_deref() {
        Some("unit") | Some("unit_test") => "unit".into(),
        Some("integration") | Some("integration_test") => "integration".into(),
        _ => "e2e_api_flow".into(),
    }
}

fn build_run_snapshot(
    state: &AppState,
    run: &RunRecord,
    project: &ProjectRecord,
    req: &GenerateTestsRequest,
    mode: AgentMode,
) -> Result<RunSnapshot> {
    let profile = if let Some(grant) = state.get_trust_grant(&run.id.to_string()) {
        Some(grant.profile)
    } else {
        let recommended = inspect_project(Path::new(&project.root_path))
            .profile
            .map(|profile| profile.normalize().map_err(GenerateError::Other))
            .transpose()?;
        if let Some(recommended) = recommended {
            let scope =
                testharbor_trust::profile_trust_scope_hash(&project.root_path, &recommended);
            let cached = state
                .with_store(|store| {
                    store.find_trust_grant_payload(&project.id.to_string(), &scope)
                })?
                .and_then(|payload| {
                    serde_json::from_str::<testharbor_trust::TrustGrant>(&payload).ok()
                })
                .filter(|grant| {
                    grant.profile.all_command_snapshots_hash()
                        == recommended.all_command_snapshots_hash()
                });
            let cached = cached.or_else(|| {
                // The detected standard runner is now an internal default;
                // the UI does not require a separate Profile/Trust form.
                Some(testharbor_trust::create_grant(
                    &project.id.to_string(),
                    &project.root_path,
                    recommended.clone(),
                ))
            });
            if let Some(cached) = cached {
                state.set_trust_grant(run.id.to_string(), cached.clone())?;
                Some(cached.profile)
            } else {
                None
            }
        } else {
            // Non-standard projects let the Agent choose the runner and return
            // it in the generation envelope. Do not add language-specific
            // execution branches here.
            None
        }
    };
    if let Some(snapshot_json) = state.with_store(|store| store.get_run_snapshot_json(run.id))? {
        let existing: RunSnapshot = serde_json::from_str(&snapshot_json)
            .map_err(|error| GenerateError::Other(error.to_string()))?;
        let requested_criteria = req
            .acceptance_criteria
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        let existing_criteria = existing
            .test_intent
            .acceptance_criteria
            .iter()
            .map(|value| value.text.as_str())
            .collect::<Vec<_>>();
        let current_agent_hash = testharbor_domain::canonical_hash(&[
            &format!("{mode:?}").to_lowercase(),
            req.agent_model.as_deref().unwrap_or("provider-default"),
        ]);
        let immutable_fields_match = existing.test_intent.objective == req.objective.trim()
            && existing_criteria == requested_criteria
            && (req.source_anchors.is_empty()
                || existing.test_intent.source_anchors == req.source_anchors)
            && (req.generation_scope.is_empty()
                || existing.test_intent.generation_scope == req.generation_scope)
            && profile.as_ref().is_none_or(|profile| {
                let command_hash = profile.all_command_snapshots_hash();
                let trust_scope_hash =
                    testharbor_trust::profile_trust_scope_hash(&project.root_path, profile);
                (existing.all_command_snapshots_hash == command_hash
                    || crate::is_deferred_runner_hash(&existing.all_command_snapshots_hash))
                    && (existing.required_trust_scope_hash == trust_scope_hash
                        || crate::is_deferred_runner_hash(&existing.required_trust_scope_hash))
            })
            && existing.agent_profile_hash == current_agent_hash;
        if !immutable_fields_match || !req.spec_attachments.is_empty() {
            return Err(GenerateError::Other(
                "RunSnapshot 已冻结；Intent、Profile、Provider 或附件变化必须创建 successor Run"
                    .into(),
            ));
        }
        return Ok(existing);
    }
    let generation_scope = if req.generation_scope.is_empty() {
        ws::default_writable_patterns()
    } else {
        req.generation_scope.clone()
    };
    if generation_scope.iter().any(|value| value.trim().is_empty()) {
        return Err(GenerateError::Other(
            "generation scope 不能包含空路径规则".into(),
        ));
    }

    let mut attachment_snapshots = Vec::new();
    let mut attachment_bodies = Vec::new();
    let mut remaining_total = SPEC_MAX_BYTES_TOTAL as usize;
    for input in &req.spec_attachments {
        let label = input.label_or_path.trim();
        if label.is_empty() {
            return Err(GenerateError::Other("附件名称或路径不能为空".into()));
        }
        let (git_blob_oid, raw_body) = match input.kind {
            SpecAttachmentKind::PastedText => (
                None,
                input
                    .body
                    .clone()
                    .ok_or_else(|| GenerateError::Other("粘贴附件缺少正文".into()))?,
            ),
            SpecAttachmentKind::RepoMarkdown => {
                let (oid, body) = testharbor_git_adapter::read_text_blob_at(
                    Path::new(&project.root_path),
                    &run.base_commit,
                    label,
                )
                .map_err(|error| GenerateError::Other(error.to_string()))?;
                (Some(oid), body)
            }
        };
        let normalized = raw_body.replace("\r\n", "\n").replace('\r', "\n");
        let allowed = remaining_total.min(SPEC_MAX_BYTES_PER_ATTACHMENT as usize);
        let (body, truncated) = truncate_utf8(&normalized, allowed);
        remaining_total = remaining_total.saturating_sub(body.len());
        let content_sha256 = content_body_hash(&body);
        let (artifact_hash, _) =
            write_content_addressed(&state.paths().artifacts_dir, body.as_bytes())?;
        attachment_bodies.push(body);
        attachment_snapshots.push(SpecAttachmentSnapshot {
            id: SpecAttachmentId::new(),
            kind: input.kind,
            label_or_path: label.to_string(),
            content_sha256,
            byte_len: attachment_bodies.last().map_or(0, |body| body.len() as u64),
            truncated,
            git_blob_oid,
            artifact_ref: format!("sha256:{artifact_hash}"),
        });
    }

    let criteria = req
        .acceptance_criteria
        .iter()
        .enumerate()
        .filter_map(|(index, text)| {
            let text = text.trim();
            (!text.is_empty()).then(|| AcceptanceCriterion {
                id: format!("ac-{}", index + 1),
                text: text.to_string(),
            })
        })
        .collect::<Vec<_>>();
    let objective = req.objective.trim().to_string();
    if objective.is_empty() || criteria.is_empty() {
        return Err(GenerateError::Other(
            "TestIntent 必须包含目标和至少一条验收项".into(),
        ));
    }
    let criterion_refs = criteria
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>();
    let attachment_hash_parts = attachment_snapshots
        .iter()
        .map(|item| SpecHashPart {
            kind: match item.kind {
                SpecAttachmentKind::PastedText => "pasted_text",
                SpecAttachmentKind::RepoMarkdown => "repo_markdown",
            },
            label_or_path: &item.label_or_path,
            content_sha256: &item.content_sha256,
            truncated: item.truncated,
            git_blob_oid: item.git_blob_oid.as_deref(),
        })
        .collect::<Vec<_>>();
    let anchors = req
        .source_anchors
        .iter()
        .map(|anchor| format!("{}:{}", anchor.kind.trim(), anchor.value.trim()))
        .collect::<Vec<_>>();
    let anchor_refs = anchors.iter().map(String::as_str).collect::<Vec<_>>();
    let generation_refs = generation_scope
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let execution_scope_summary = profile
        .as_ref()
        .map(|profile| {
            format!(
                "{} {} (cwd={}, parser={})",
                profile.command.executable,
                profile.command.args.join(" "),
                profile.command.cwd,
                profile.command.parser
            )
        })
        .unwrap_or_else(|| "agent-selected repository test runner".into());
    let pass_policy_summary = if profile
        .as_ref()
        .is_some_and(|profile| profile.command.parser == "go-test-json")
    {
        "strict: full command exits 0; result complete; all intended identities observed and passed"
            .to_string()
    } else {
        "reviewable only unless the selected driver provides stable inventory and completeness"
            .to_string()
    };
    let intent_hash = compute_test_intent_hash(&TestIntentHashInput {
        objective: &objective,
        acceptance_criteria: &criterion_refs,
        spec_attachments: &attachment_hash_parts,
        source_anchors: &anchor_refs,
        git_diff_snapshot_hash: None,
        generation_scope: &generation_refs,
        execution_scope_summary: &execution_scope_summary,
        pass_policy_summary: &pass_policy_summary,
    });
    let test_intent = TestIntentSnapshot {
        objective,
        acceptance_criteria: criteria,
        spec_attachments: attachment_snapshots,
        source_anchors: req.source_anchors.clone(),
        git_diff_snapshot_hash: None,
        generation_scope,
        execution_scope_summary,
        pass_policy_summary,
        hash: intent_hash,
    };
    let profile_json = profile
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|error| GenerateError::Other(error.to_string()))?;
    let project_config_hash = testharbor_git_adapter::read_utf8_blob_at(
        Path::new(&project.root_path),
        &run.base_commit,
        ".testharbor.yaml",
    )
    .map(|(_, body)| content_body_hash(&body))
    .unwrap_or_else(|_| testharbor_domain::canonical_hash(&["no-project-config"]));
    let agent_profile_hash = testharbor_domain::canonical_hash(&[
        &format!("{mode:?}").to_lowercase(),
        req.agent_model.as_deref().unwrap_or("provider-default"),
    ]);
    let path_policy_json = serde_json::to_vec(&test_intent.generation_scope)
        .map_err(|error| GenerateError::Other(error.to_string()))?;
    let env_policy_json = serde_json::to_vec(
        &profile
            .as_ref()
            .map(|profile| profile.env_policy.clone())
            .unwrap_or_default(),
    )
    .map_err(|error| GenerateError::Other(error.to_string()))?;
    let all_command_snapshots_hash = profile
        .as_ref()
        .map(|profile| profile.all_command_snapshots_hash())
        .unwrap_or_else(crate::deferred_runner_hash);
    let required_trust_scope_hash = profile
        .as_ref()
        .map(|profile| testharbor_trust::profile_trust_scope_hash(&project.root_path, profile))
        .unwrap_or_else(crate::deferred_runner_hash);
    Ok(RunSnapshot {
        test_intent,
        base_commit: run.base_commit.clone(),
        project_config_hash,
        test_profile_hash: profile_json
            .as_deref()
            .map(sha256_hex)
            .unwrap_or_else(crate::deferred_runner_hash),
        all_command_snapshots_hash,
        agent_profile_hash,
        path_policy_hash: sha256_hex(&path_policy_json),
        env_policy_hash: sha256_hex(&env_policy_json),
        required_trust_scope_hash,
    })
}

fn generation_agent_contract(snapshot: &RunSnapshot) -> (String, Vec<String>) {
    let scope_hash = testharbor_domain::canonical_hash(&[
        &snapshot.test_intent.hash,
        &snapshot.base_commit,
        &snapshot.path_policy_hash,
        &snapshot.agent_profile_hash,
    ]);
    let mut evidence_refs = vec![format!("base-commit:{}", snapshot.base_commit)];
    evidence_refs.extend(
        snapshot
            .test_intent
            .spec_attachments
            .iter()
            .map(|attachment| attachment.artifact_ref.clone()),
    );
    (scope_hash, evidence_refs)
}

fn generation_progress_dir(state: &AppState, run_id: &str) -> PathBuf {
    state.paths().root.join("runs").join(run_id).join("agent")
}

fn service_endpoint(base_url: &str) -> Option<String> {
    let value = base_url.trim();
    let authority = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .unwrap_or(value)
        .split('/')
        .next()?
        .trim();
    if authority.is_empty() {
        return None;
    }
    if authority.starts_with('[') {
        let end = authority.find(']')?;
        let host = &authority[..=end];
        let port = authority[end + 1..]
            .strip_prefix(':')
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or_else(|| {
                if value.starts_with("https://") {
                    443
                } else {
                    80
                }
            });
        return Some(format!("{host}:{port}"));
    }
    if authority
        .rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
    {
        return Some(authority.to_string());
    }
    let port = value.starts_with("https://").then_some(443).unwrap_or(80);
    Some(format!("{authority}:{port}"))
}

fn service_reachable(base_url: &str) -> bool {
    let Some(endpoint) = service_endpoint(base_url) else {
        return false;
    };
    endpoint
        .to_socket_addrs()
        .ok()
        .into_iter()
        .flatten()
        .any(|address| TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok())
}

fn ensure_test_environment(environment: &TestEnvironmentInput, progress_path: &Path) -> Result<()> {
    let base_url = environment.base_url.trim();
    if base_url.is_empty() {
        let message = if environment.context.trim().is_empty() {
            "未提供固定服务地址；生成器不会推断或执行服务启动命令"
        } else {
            "已提供环境说明；将由 CLI 判断服务地址、登录方式和启动方式"
        };
        append_progress(progress_path, "testharbor.status", message)?;
        return Ok(());
    }
    if service_reachable(base_url) {
        append_progress(progress_path, "testharbor.status", "测试服务已运行")?;
        return Ok(());
    }
    append_progress(
        progress_path,
        "testharbor.status",
        "未检测到指定服务；请配置并确认受控 ServiceProfile 后重试",
    )?;
    Ok(())
}

fn is_major_agent_finding(finding: &AgentFinding) -> bool {
    is_major_severity(&finding.severity)
}

fn needs_auto_revision(recommendation: &str, findings: &[AgentFinding]) -> bool {
    recommendation.trim().eq_ignore_ascii_case("block")
        || findings.iter().any(is_major_agent_finding)
}

fn build_ai_revision_request(
    recommendation: &str,
    summary: &str,
    findings: &[AgentFinding],
) -> String {
    let mut request = format!(
        "这是 AI 评审后的自动修订。请修复重大问题后再提交测试文件。评审建议：{}。评审摘要：{}。",
        recommendation.trim(),
        summary.trim()
    );
    for finding in findings
        .iter()
        .filter(|finding| is_major_agent_finding(finding))
    {
        request.push_str(&format!(
            "\n- [{}] {}{}：{}",
            finding.severity,
            finding.file.as_deref().unwrap_or("未指定文件"),
            finding
                .line
                .map(|line| format!(":{line}"))
                .unwrap_or_default(),
            finding.message
        ));
        if let Some(suggestion) = finding
            .suggestion
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            request.push_str(&format!(" 建议：{suggestion}"));
        }
    }
    request.push_str(
        "\n修订必须覆盖上述问题，并保持测试目标、验收标准、真实业务链路和测试数据清理策略不变。",
    );
    request
}

fn codex_session_from_progress(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let event_type = value.get("type").and_then(|v| v.as_str());
        let is_session_event = event_type == Some("thread.started")
            || event_type == Some("session.started")
            || event_type == Some("end");
        if !is_session_event {
            continue;
        }
        if let Some(id) = value
            .get("thread_id")
            .or_else(|| value.get("session_id"))
            .or_else(|| value.get("sessionId"))
            .and_then(|v| v.as_str())
        {
            return Some(id.to_string());
        }
    }
    None
}

fn append_progress(path: &Path, event_type: &str, message: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let event = serde_json::json!({
        "type": event_type,
        "message": message,
    });
    writeln!(file, "{event}")?;
    file.flush()
}

fn no_candidate_changes_error(agent_summary: &str) -> GenerateError {
    let summary = agent_summary.trim();
    if summary.is_empty() {
        GenerateError::Other(
            "生成服务完成但没有产生任何测试候选变更。请查看实时输出中的阻塞原因。".into(),
        )
    } else {
        GenerateError::Other(format!(
            "生成服务完成但没有产生任何测试候选变更。\n\n{summary}"
        ))
    }
}

fn run_validation_command(
    state: &AppState,
    run_id: &str,
    stage: &str,
    command: &testharbor_trust::CommandIdentity,
    cwd: &Path,
    timeout_ms: u64,
) -> Result<serde_json::Value> {
    let task_id = new_task_id();
    let log_dir = state
        .paths()
        .root
        .join("runs")
        .join(run_id)
        .join("validation")
        .join(stage);
    let started = state.supervisor().start(
        &task_id,
        &CommandSpec {
            executable: command.executable.clone(),
            args: command.args.clone(),
            cwd: cwd.to_path_buf(),
            env: Default::default(),
            timeout_ms: Some(timeout_ms),
        },
        &log_dir,
    )?;
    let outcome = if matches!(started.phase, TaskPhase::Starting | TaskPhase::Running) {
        state.supervisor().wait(&task_id, 25)?
    } else {
        started
    };
    let manifest = serde_json::json!({
        "stage": stage,
        "command_hash": testharbor_trust::command_snapshot_hash(command),
        "phase": outcome.phase,
        "exit_code": outcome.exit_code,
        "stdout": outcome.stdout_path,
        "stderr": outcome.stderr_path,
    });
    if outcome.phase != TaskPhase::Exited || outcome.exit_code != Some(0) {
        return Err(GenerateError::Other(format!(
            "{stage} validation failed: phase={:?}, exit={:?}",
            outcome.phase, outcome.exit_code
        )));
    }
    Ok(manifest)
}

fn agent_runner_profile(
    project: &ProjectRecord,
    runner: &AgentTestRunner,
) -> Result<testharbor_trust::TestProfileDraft> {
    let root = Path::new(&project.root_path)
        .canonicalize()
        .map_err(|error| GenerateError::Other(format!("项目根目录无效: {error}")))?;
    let cwd = runner.cwd.as_deref().unwrap_or(".");
    if Path::new(cwd).is_absolute() {
        return Err(GenerateError::Other(
            "Agent runner cwd 必须是仓库相对路径".into(),
        ));
    }
    let cwd_path = root
        .join(cwd)
        .canonicalize()
        .map_err(|error| GenerateError::Other(format!("Agent runner cwd 无效: {error}")))?;
    if !cwd_path.starts_with(&root) {
        return Err(GenerateError::Other("Agent runner cwd 越出仓库目录".into()));
    }

    let executable = if Path::new(&runner.executable).is_absolute()
        || runner.executable.contains('/')
        || runner.executable.contains('\\')
    {
        let candidate = if Path::new(&runner.executable).is_absolute() {
            PathBuf::from(&runner.executable)
        } else {
            cwd_path.join(&runner.executable)
        };
        candidate.canonicalize().map_err(|error| {
            GenerateError::Other(format!("Agent runner executable 无效: {error}"))
        })?
    } else {
        testharbor_trust::resolve_executable(&runner.executable).ok_or_else(|| {
            GenerateError::Other(format!(
                "Agent runner executable 未安装: {}",
                runner.executable
            ))
        })?
    };
    if !executable.is_file() {
        return Err(GenerateError::Other(format!(
            "Agent runner executable 不是文件: {}",
            executable.display()
        )));
    }

    let parser = runner.parser.trim();
    if parser != "exit-code" && parser != "go-test-json" && !parser.starts_with("junit-xml:") {
        return Err(GenerateError::Other(format!(
            "Agent runner parser 不受支持: {parser}"
        )));
    }
    if let Some(report) = parser.strip_prefix("junit-xml:") {
        if report.trim().is_empty() || Path::new(report).is_absolute() {
            return Err(GenerateError::Other(
                "Agent runner 的 JUnit 报告路径必须是非空相对路径".into(),
            ));
        }
    }

    Ok(testharbor_trust::TestProfileDraft {
        name: "agent-selected".into(),
        format: None,
        collect: None,
        compile: None,
        command: testharbor_trust::CommandIdentity {
            executable: executable.display().to_string(),
            args: runner.args.clone(),
            cwd: cwd_path.display().to_string(),
            parser: parser.to_string(),
            executable_version: testharbor_trust::probe_version(&executable),
            executable_identity: None,
        },
        timeout_ms: runner.timeout_ms.unwrap_or(3_600_000).clamp(1, 3_600_000),
        service: None,
        env_policy: vec![],
        network_unrestricted: false,
    })
}

/// Keep generation permissive for repositories whose test tooling is not
/// covered by the built-in project inspector. These are best-effort command
/// candidates only; the CLI remains the primary source of the repository's
/// native runner.
fn fallback_agent_runner(project: &ProjectRecord) -> Option<AgentTestRunner> {
    let root = Path::new(&project.root_path);
    let candidates: &[(&str, &str, &[&str])] = &[
        ("test.php", "php", &["test.php"]),
        ("test.sh", "sh", &["test.sh"]),
        ("test.py", "python3", &["test.py"]),
        ("pyproject.toml", "python3", &["-m", "pytest"]),
        ("package.json", "npm", &["test"]),
        ("Makefile", "make", &["test"]),
        ("Cargo.toml", "cargo", &["test"]),
        ("go.mod", "go", &["test", "-json", "./..."]),
        ("pom.xml", "mvn", &["test"]),
    ];
    for (marker, executable, args) in candidates {
        if root.join(marker).is_file() && testharbor_trust::resolve_executable(executable).is_some()
        {
            return Some(AgentTestRunner {
                executable: (*executable).into(),
                args: args.iter().map(|arg| (*arg).into()).collect(),
                cwd: Some(".".into()),
                parser: "exit-code".into(),
                timeout_ms: Some(3_600_000),
            });
        }
    }
    let phpunit = root.join("vendor/bin/phpunit");
    if phpunit.is_file() {
        return Some(AgentTestRunner {
            executable: "vendor/bin/phpunit".into(),
            args: vec![],
            cwd: Some(".".into()),
            parser: "exit-code".into(),
            timeout_ms: Some(3_600_000),
        });
    }
    None
}

fn bind_agent_runner(
    state: &AppState,
    run: &RunRecord,
    project: &ProjectRecord,
    runner: Option<AgentTestRunner>,
) -> Result<Option<testharbor_trust::TestProfileDraft>> {
    if runner.is_none() {
        if let Some(existing) = state.get_trust_grant(&run.id.to_string()) {
            return Ok(Some(existing.profile));
        }
    }
    let Some(runner) = runner.or_else(|| fallback_agent_runner(project)) else {
        return Ok(None);
    };
    let profile = agent_runner_profile(project, &runner)?
        .normalize()
        .map_err(GenerateError::Other)?;
    if let Some(existing) = state.get_trust_grant(&run.id.to_string()) {
        if existing.profile.all_command_snapshots_hash() != profile.all_command_snapshots_hash() {
            return Err(GenerateError::Other(
                "Agent 返回的测试命令与已绑定 Profile 不一致；请重新生成 successor Run".into(),
            ));
        }
        return Ok(Some(existing.profile));
    }

    crate::confirm_profile(state, &run.id.to_string(), profile)
        .map_err(|error| GenerateError::Other(error.to_string()))?;
    Ok(state
        .get_trust_grant(&run.id.to_string())
        .map(|grant| grant.profile))
}

pub(crate) fn freeze_and_review_worktree(
    state: &AppState,
    run: &RunRecord,
    project: &ProjectRecord,
    worktree_path: &str,
    objective: &str,
    acceptance_criteria: &[String],
    test_intent_hash: &str,
    pending_change: Option<PendingChangeRequest>,
    mode: AgentMode,
    agent_summary: String,
    written_files: Vec<String>,
    proposed_intended_names: Vec<String>,
    test_runner: Option<AgentTestRunner>,
    progress_stdout: &Path,
    progress_dir: &Path,
    auto_revision_count: u32,
) -> Result<GenerateTestsResult> {
    let max_auto_revisions = max_auto_review_revisions();
    let handle = ws::WorkspaceHandle {
        repo_root: PathBuf::from(&project.root_path),
        worktree_path: PathBuf::from(worktree_path),
        base_commit: run.base_commit.clone(),
        run_id: run.id.to_string(),
    };
    let profile = bind_agent_runner(state, run, project, test_runner)?;
    if profile.is_none() {
        append_progress(
            progress_stdout,
            "testharbor.status",
            "未能自动确定测试命令，已保留候选进入用例评审；执行前需配置测试命令",
        )?;
    }
    let mut validation_stages = Vec::new();
    if let Some(profile) = profile.as_ref() {
        if let Some(format) = &profile.format {
            validation_stages.push(run_validation_command(
                state,
                &run.id.to_string(),
                "format",
                format,
                &handle.worktree_path,
                profile.timeout_ms,
            )?);
        }
    }

    let snapshot_json = state
        .with_store(|store| store.get_run_snapshot_json(run.id))?
        .ok_or_else(|| GenerateError::Other("RunSnapshot is required before validation".into()))?;
    let snapshot: RunSnapshot = serde_json::from_str(&snapshot_json)
        .map_err(|error| GenerateError::Other(error.to_string()))?;
    if snapshot.test_intent.hash != test_intent_hash {
        return Err(GenerateError::Other(
            "persisted TestIntent hash does not match generation context".into(),
        ));
    }
    let patterns = snapshot.test_intent.generation_scope.clone();
    if ws::list_changes(&handle, &patterns)?.is_empty() {
        append_progress(
            progress_stdout,
            "testharbor.status",
            "生成服务未产生测试候选变更",
        )?;
        state.with_store(|store| {
            store.append_event(
                Some(run.id),
                Some(project.id),
                "agent_generated_no_candidate_changes",
                &serde_json::json!({
                    "agent_summary": agent_summary,
                    "written_files": written_files,
                }),
            )
        })?;
        return Err(no_candidate_changes_error(&agent_summary));
    }
    let seq = state.next_revision_seq(&run.id.to_string())?;
    let candidate = ws::freeze_candidate(
        &handle,
        seq,
        &patterns,
        &format!("testharbor generate r{seq}"),
    )?;
    if let Some(profile) = profile.as_ref() {
        for (stage, command) in [
            ("collect", profile.collect.as_ref()),
            ("compile", profile.compile.as_ref()),
        ] {
            if let Some(command) = command {
                validation_stages.push(run_validation_command(
                    state,
                    &run.id.to_string(),
                    stage,
                    command,
                    &handle.worktree_path,
                    profile.timeout_ms,
                )?);
                if !ws::verify_integrity(&handle, &candidate.candidate_tree)? {
                    return Err(GenerateError::Other(format!(
                        "{stage} changed the frozen candidate source tree"
                    )));
                }
            }
        }
    }
    if profile
        .as_ref()
        .is_some_and(|profile| profile.command.parser == "go-test-json")
    {
        let inventory = collect_go_inventory(&handle.worktree_path, &["./..."])
            .map_err(|error| GenerateError::Other(error.to_string()))?;
        if proposed_intended_names.is_empty() {
            return Err(GenerateError::Other(
                "Generator did not provide intended test identities".into(),
            ));
        }
        let proposed = proposed_intended_names
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        let identities = inventory
            .identities
            .iter()
            .filter(|identity| proposed.contains(&identity.name))
            .cloned()
            .collect::<Vec<_>>();
        if identities.len() != proposed.len() {
            let found = identities
                .iter()
                .map(|identity| identity.name.clone())
                .collect::<std::collections::BTreeSet<_>>();
            let missing = proposed
                .into_iter()
                .filter(|name| !found.contains(*name))
                .cloned()
                .collect::<Vec<_>>();
            return Err(GenerateError::Other(format!(
                "Generator intended identities are absent from Inventory: {}",
                missing.join(", ")
            )));
        }
        let intended = make_intended_manifest(&inventory, identities)
            .map_err(|error| GenerateError::Other(error.to_string()))?;
        state.set_inventory(run.id.to_string(), inventory.clone())?;
        state.set_intended(run.id.to_string(), intended.clone())?;
        validation_stages.push(serde_json::json!({
            "stage": "inventory",
            "inventory_hash": inventory.hash,
            "intended_manifest_hash": intended.hash,
            "intended_count": intended.identities.len(),
        }));
    }

    let bundle_dir = state
        .paths()
        .root
        .join("runs")
        .join(run.id.to_string())
        .join("reviews")
        .join(format!("r{seq}"));
    let patch = testharbor_git_adapter::display_patch(
        &handle.worktree_path,
        &candidate.base_commit,
        &candidate.candidate_commit,
    )
    .unwrap_or_default();
    let validation_summary = serde_json::json!({
        "policy": if profile.is_some() { "passed" } else { "deferred" },
        "runner_status": if profile.is_some() { "bound" } else { "unconfigured" },
        "writable": true,
        "stages": validation_stages,
        "candidate_tree": candidate.candidate_tree,
        "all_command_snapshots_hash": profile
            .as_ref()
            .map(|profile| profile.all_command_snapshots_hash())
            .unwrap_or_else(crate::deferred_runner_hash),
        "human_change_request": pending_change.as_ref().map(|request| request.message.clone()),
    })
    .to_string();
    write_review_bundle(
        &bundle_dir,
        objective,
        acceptance_criteria,
        test_intent_hash,
        &candidate.changed_paths,
        &patch,
        &validation_summary,
    )?;
    if let Some(snapshot_json) = state.with_store(|store| store.get_run_snapshot_json(run.id))? {
        let snapshot: RunSnapshot = serde_json::from_str(&snapshot_json)
            .map_err(|error| GenerateError::Other(error.to_string()))?;
        std::fs::write(
            bundle_dir.join("test-intent.json"),
            serde_json::to_vec_pretty(&snapshot.test_intent)
                .map_err(|error| GenerateError::Other(error.to_string()))?,
        )?;
        let attachment_dir = bundle_dir.join("attachments");
        std::fs::create_dir_all(&attachment_dir)?;
        for attachment in &snapshot.test_intent.spec_attachments {
            let hash = attachment
                .artifact_ref
                .strip_prefix("sha256:")
                .ok_or_else(|| GenerateError::Other("invalid attachment artifact ref".into()))?;
            if hash.len() < 2 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(GenerateError::Other(
                    "invalid attachment artifact hash".into(),
                ));
            }
            let artifact = state
                .paths()
                .artifacts_dir
                .join("objects")
                .join(&hash[..2])
                .join(hash);
            let bytes = std::fs::read(&artifact)?;
            if sha256_hex(&bytes) != hash {
                return Err(GenerateError::Other(format!(
                    "attachment artifact is missing or corrupt: {}",
                    attachment.label_or_path
                )));
            }
            std::fs::write(attachment_dir.join(format!("{}.md", attachment.id)), bytes)?;
        }
    }

    let validation_hash = sha256_hex(validation_summary.as_bytes());
    let scope_hash = testharbor_domain::canonical_hash(&[
        test_intent_hash,
        &candidate.candidate_tree,
        &profile
            .as_ref()
            .map(|profile| profile.all_command_snapshots_hash())
            .unwrap_or_else(crate::deferred_runner_hash),
        &validation_hash,
    ]);
    let evidence_refs = vec![
        format!("candidate-tree:{}", candidate.candidate_tree),
        format!("validation:sha256:{validation_hash}"),
        format!("bundle:test-intent:sha256:{test_intent_hash}"),
    ];
    let review_req = ReviewRequest {
        run_id: Some(run.id.to_string()),
        test_intent_hash: test_intent_hash.to_string(),
        scope_hash,
        evidence_refs,
        objective: objective.to_string(),
        acceptance_criteria: acceptance_criteria.to_vec(),
        changed_paths: candidate.changed_paths.clone(),
        patch_excerpt: patch.chars().take(8000).collect(),
        change_request: pending_change
            .as_ref()
            .map(|request| request.message.clone()),
        bundle_dir: bundle_dir.display().to_string(),
        progress_dir: Some(progress_dir.display().to_string()),
        // A review must not inherit the generation transcript. That transcript
        // contains repository exploration and tool output, and resuming it here
        // makes every review request replay an ever-growing context. Session
        // reuse is limited to the generation/revision phase below.
        session_id: None,
        agent_model: state
            .get_intent_meta(&run.id.to_string())
            .and_then(|intent| intent.agent_model),
    };

    append_progress(progress_stdout, "testharbor.status", "正在评审生成结果")?;
    let agent_review = review_tests(mode, &review_req).map_err(GenerateError::from)?;
    if !ws::verify_integrity(&handle, &candidate.candidate_tree)? {
        return Err(GenerateError::Other(
            "Reviewer 返回后 candidate tree 已变化，评审结果作废".into(),
        ));
    }

    let auto_revision_needed =
        needs_auto_revision(&agent_review.recommendation, &agent_review.findings);
    if auto_revision_needed && auto_revision_count < max_auto_revisions {
        let next_revision = auto_revision_count + 1;
        let revision_request = build_ai_revision_request(
            &agent_review.recommendation,
            &agent_review.summary,
            &agent_review.findings,
        );
        append_progress(
            progress_stdout,
            "testharbor.status",
            &format!(
                "AI 评审发现重大问题，正在自动修订（第 {next_revision}/{max_auto_revisions} 次）"
            ),
        )?;
        state.with_store(|store| {
            store.append_event(
                Some(run.id),
                Some(project.id),
                "agent_auto_revision_requested",
                &serde_json::json!({
                    "revision": candidate.sequence,
                    "next_revision": next_revision,
                    "recommendation": agent_review.recommendation,
                    "summary": agent_review.summary,
                    "findings": agent_review.findings,
                    "change_request": revision_request,
                }),
            )
        })?;

        let intent = state
            .get_intent_meta(&run.id.to_string())
            .ok_or_else(|| GenerateError::Other("persisted TestIntent is unavailable".into()))?;
        let language_hint = project_language_hint(Path::new(&project.root_path));
        let snapshot_json = state
            .with_store(|store| store.get_run_snapshot_json(run.id))?
            .ok_or_else(|| GenerateError::Other("RunSnapshot is unavailable".into()))?;
        let snapshot: RunSnapshot = serde_json::from_str(&snapshot_json)
            .map_err(|error| GenerateError::Other(error.to_string()))?;
        let (generation_scope_hash, generation_evidence_refs) =
            generation_agent_contract(&snapshot);
        let generated = generate(
            mode,
            &GenerateRequest {
                run_id: Some(run.id.to_string()),
                objective: objective.to_string(),
                acceptance_criteria: acceptance_criteria.to_vec(),
                test_strategy: Some(intent.test_strategy),
                cleanup_test_data: Some(intent.cleanup_test_data),
                change_request: Some(revision_request),
                test_intent_hash: test_intent_hash.to_string(),
                scope_hash: generation_scope_hash,
                evidence_refs: generation_evidence_refs,
                worktree_path: worktree_path.to_string(),
                base_commit: run.base_commit.clone(),
                language_hint: language_hint.into(),
                progress_dir: Some(progress_dir.display().to_string()),
                session_id: state.get_codex_generation_session(&run.id.to_string()),
                agent_model: intent.agent_model,
                environment: state
                    .get_test_environment(&run.id.to_string())
                    .map(|environment| environment.agent_environment()),
            },
        )?;
        if let Some(session_id) = generated.session_id.clone() {
            state.set_codex_generation_session(run.id.to_string(), session_id)?;
        }
        return freeze_and_review_worktree(
            state,
            run,
            project,
            worktree_path,
            objective,
            acceptance_criteria,
            test_intent_hash,
            pending_change,
            mode,
            generated.summary,
            generated.written_files,
            generated.proposed_intended_names,
            generated.test_runner,
            progress_stdout,
            progress_dir,
            next_revision,
        );
    }

    if auto_revision_needed {
        append_progress(
            progress_stdout,
            "testharbor.status",
            "AI 自动修订次数已达上限，保留当前结果进入人工评审",
        )?;
        state.with_store(|store| {
            store.append_event(
                Some(run.id),
                Some(project.id),
                "agent_auto_revision_limit_reached",
                &serde_json::json!({
                    "revision": candidate.sequence,
                    "max_auto_revisions": max_auto_revisions,
                    "recommendation": agent_review.recommendation,
                    "findings": agent_review.findings,
                }),
            )
        })?;
    } else {
        append_progress(
            progress_stdout,
            "testharbor.status",
            "AI 评审未发现重大问题，进入人工评审",
        )?;
    }

    let mut review_state = CaseReviewState::from_agent_review(
        agent_review.recommendation,
        agent_review.summary.clone(),
        agent_review.findings,
    );
    if let Some(request) = &pending_change {
        review_state.findings.push(ReviewFinding {
            id: Uuid::new_v4().to_string(),
            severity: "info".into(),
            category: "human_request_followup".into(),
            file: None,
            line: None,
            message: format!(
                "请确认 r{} 已满足人工修改要求：{}",
                request.previous_revision_sequence.unwrap_or_default(),
                request.message
            ),
            suggestion: Some("核对新 revision 后将此项标记为 Resolved 或 MustFix".into()),
            disposition: FindingDisposition::Open,
            risk_reason: None,
        });
        review_state.sort_findings();
        review_state.recompute_disposition_hash();
    }

    let intended_hash = state
        .get_intended(&run.id.to_string())
        .map(|m| m.hash)
        .unwrap_or_else(|| "no-intended".into());

    let scope = test_case_scope_hash(
        &candidate.candidate_tree,
        &candidate.base_commit,
        test_intent_hash,
        &intended_hash,
        "cmd",
        "cfg",
        "path",
        state
            .get_trust_grant(&run.id.to_string())
            .map(|g| g.trust_scope_hash)
            .unwrap_or_else(crate::deferred_runner_hash)
            .as_str(),
    );

    let run_id = run.id.to_string();
    state.set_candidate(run_id.clone(), candidate.clone())?;
    state.set_case_review(run_id.clone(), review_state.clone())?;
    state.set_case_scope(run_id.clone(), scope.clone())?;
    state.clear_case_approval(&run_id)?;
    state.clear_result_decision(&run_id)?;
    state.clear_last_execution(&run_id)?;
    state.clear_pending_change(&run_id)?;
    state.clear_generation_recovery(&run_id)?;

    state.with_store_transaction(|s| {
        let revision_id = Uuid::new_v4().to_string();
        s.insert_test_revision(
            &revision_id,
            run.id,
            candidate.sequence,
            &candidate.candidate_tree,
            &scope,
            &candidate,
        )?;
        s.insert_review(
            &Uuid::new_v4().to_string(),
            run.id,
            "test_revision",
            &revision_id,
            "agent",
            &scope,
            &review_state,
        )?;
        s.update_run_objective(run.id, objective)?;
        s.update_run_worktree(
            run.id,
            Some(worktree_path),
            WorkflowState::PendingTestReview,
        )?;
        s.append_event(
            Some(run.id),
            Some(project.id),
            "generated_and_reviewed",
            &serde_json::json!({
                "candidate": candidate,
                "review": review_state,
                "agent_summary": agent_summary,
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;
    append_progress(progress_stdout, "testharbor.status", "测试候选已生成")?;

    Ok(GenerateTestsResult {
        agent_summary,
        written_files,
        candidate,
        review: review_state,
        agent_mode: format!("{:?}", mode).to_lowercase(),
    })
}

pub fn get_generation_progress(state: &AppState, run_id: &str) -> Result<GenerationProgress> {
    let _ = parse_run_id(run_id)?;
    let dir = generation_progress_dir(state, run_id);
    Ok(GenerationProgress {
        stdout_tail: testharbor_process_supervisor::read_log_tail(
            &dir.join("generation.stdout.jsonl"),
            128_000,
        )
        .unwrap_or_default(),
        stderr_tail: testharbor_process_supervisor::read_log_tail(
            &dir.join("generation.stderr.log"),
            64_000,
        )
        .unwrap_or_default(),
    })
}

/// Ask the active Codex generation/review process for a Run to stop.
/// The worker completes the transition to the recoverable checkpoint after it
/// observes the cancellation signal.
pub fn cancel_generation(state: &AppState, run_id: &str) -> Result<bool> {
    let id = parse_run_id(run_id)?;
    let run = state.with_store(|store| store.get_run(id))?;
    if run.workflow_state != WorkflowState::GeneratingTests {
        return Ok(false);
    }
    let progress_dir = generation_progress_dir(state, run_id);
    std::fs::create_dir_all(&progress_dir)?;
    let cancel_file = progress_dir.join("generation.cancel");
    let file_requested = std::fs::write(&cancel_file, b"cancelled\n").is_ok();
    let cancelled = testharbor_agent_drivers::cancel_generation(run_id) || file_requested;
    if cancelled {
        state.with_store(|store| {
            store.append_event(
                Some(run.id),
                Some(run.project_id),
                "generation_cancel_requested",
                &serde_json::json!({ "run_id": run_id }),
            )
        })?;
    }
    Ok(cancelled)
}

/// Full S5 pipeline: ensure worktree → Codex generate → freeze → Codex review.
pub fn generate_and_review(
    state: &AppState,
    req: GenerateTestsRequest,
) -> Result<GenerateTestsResult> {
    if req.objective.trim().is_empty()
        || !req
            .acceptance_criteria
            .iter()
            .any(|criterion| !criterion.trim().is_empty())
    {
        return Err(GenerateError::Other(
            "objective and at least one acceptance criterion are required".into(),
        ));
    }
    let run_id = req.run_id.clone();
    let parsed_run_id = parse_run_id(&run_id)?;
    let owner_id = state
        .acquire_run_activity(parsed_run_id, "generation")
        .map_err(|error| GenerateError::Other(error.to_string()))?;
    let _activity_guard = RunActivityGuard {
        state,
        run_id: parsed_run_id,
        owner_id,
    };
    match generate_and_review_inner(state, req) {
        Ok(result) => Ok(result),
        Err(error) => {
            if let Ok(id) = parse_run_id(&run_id) {
                if let Ok(run) = state.with_store(|store| store.get_run(id)) {
                    let reason = error.to_string();
                    let _ = state.set_generation_recovery(
                        run_id.clone(),
                        GenerationRecovery {
                            target: WorkflowState::GeneratingTests,
                            reason: reason.clone(),
                        },
                    );
                    let _ = state.with_store(|store| {
                        store.update_run_worktree(
                            run.id,
                            run.worktree_path.as_deref(),
                            WorkflowState::NeedsHuman,
                        )?;
                        store.append_event(
                            Some(run.id),
                            Some(run.project_id),
                            "generation_failed_needs_human",
                            &serde_json::json!({
                                "error": reason,
                                "retry_target": "generating_tests",
                            }),
                        )?;
                        Ok::<(), StorageError>(())
                    });
                }
            }
            Err(error)
        }
    }
}

fn generate_and_review_inner(
    state: &AppState,
    req: GenerateTestsRequest,
) -> Result<GenerateTestsResult> {
    let run_id = parse_run_id(&req.run_id)?;
    let run = state.with_store(|s| s.get_run(run_id))?;
    if run.legacy_read_only {
        return Err(GenerateError::Other(
            "该 Run 来自旧版存储，只能查看、导出或清理；请创建 successor Run".into(),
        ));
    }
    let project = state.with_store(|s| s.get_project(run.project_id))?;

    // Ensure worktree
    let worktree_path = if let Some(p) = &run.worktree_path {
        if PathBuf::from(p).is_dir() {
            p.clone()
        } else {
            crate::prepare_worktree(state, &req.run_id)
                .map_err(|e| GenerateError::Other(e.to_string()))?
                .worktree_path
        }
    } else {
        crate::prepare_worktree(state, &req.run_id)
            .map_err(|e| GenerateError::Other(e.to_string()))?
            .worktree_path
    };

    let run = state.with_store(|s| s.get_run(run_id))?; // refresh
    let base_commit = run.base_commit.clone();
    let test_strategy = normalize_test_strategy(req.test_strategy.as_deref());
    let cleanup_test_data = req.cleanup_test_data.unwrap_or(true);
    let environment = req
        .environment
        .clone()
        .or_else(|| state.get_test_environment(&req.run_id))
        .map(environment_with_context_defaults);
    if let Some(environment) = environment.clone() {
        state.set_test_environment(req.run_id.clone(), environment);
    }
    let mode = agent_mode(req.agent_mode.as_deref());
    let snapshot = build_run_snapshot(state, &run, &project, &req, mode)?;
    let snapshot_json =
        serde_json::to_vec(&snapshot).map_err(|error| GenerateError::Other(error.to_string()))?;
    let snapshot_hash = sha256_hex(&snapshot_json);
    let ih = snapshot.test_intent.hash.clone();
    state.with_store(|store| {
        store.set_run_snapshot(run.id, &ih, &snapshot_hash, &snapshot)?;
        store.append_event(
            Some(run.id),
            Some(project.id),
            "run_snapshot_frozen",
            &serde_json::json!({
                "snapshot_hash": snapshot_hash,
                "test_intent_hash": ih,
                "attachment_count": snapshot.test_intent.spec_attachments.len(),
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;
    let mode_key = format!("{mode:?}").to_lowercase();
    let provider_changed = state
        .get_intent_meta(&req.run_id)
        .and_then(|intent| intent.agent_mode)
        .is_some_and(|previous| previous != mode_key);
    if provider_changed {
        state.clear_codex_generation_session(&req.run_id)?;
    }
    let progress_dir = generation_progress_dir(state, &req.run_id);
    std::fs::create_dir_all(&progress_dir)?;
    let progress_stdout = progress_dir.join("generation.stdout.jsonl");
    let progress_stderr = progress_dir.join("generation.stderr.log");
    let generation_session_id = if req.force_regenerate || provider_changed {
        None
    } else if !req.reuse_agent_session {
        state.clear_codex_generation_session(&req.run_id)?;
        None
    } else {
        state
            .get_codex_generation_session(&req.run_id)
            .or_else(|| codex_session_from_progress(&progress_stdout))
    };
    if let Some(session_id) = generation_session_id.clone() {
        state.set_codex_generation_session(req.run_id.clone(), session_id)?;
    }
    std::fs::write(&progress_stdout, "")?;
    std::fs::write(&progress_stderr, "")?;
    let _ = std::fs::remove_file(progress_dir.join("generation.cancel"));
    append_progress(&progress_stdout, "testharbor.status", "正在准备隔离工作区")?;
    let environment_config = environment
        .as_ref()
        .map(|input| input.config(state))
        .transpose()?;
    state.set_intent_meta(
        req.run_id.clone(),
        IntentMeta {
            objective: req.objective.clone(),
            acceptance_criteria: req.acceptance_criteria.clone(),
            test_intent_hash: ih.clone(),
            test_strategy: test_strategy.clone(),
            cleanup_test_data,
            agent_mode: Some(mode_key),
            agent_model: req.agent_model.clone(),
            reuse_agent_session: req.reuse_agent_session,
            environment: environment_config,
        },
    )?;
    let language_hint = project_language_hint(Path::new(&project.root_path));

    state.with_store(|s| {
        s.update_run_objective(run.id, &req.objective)?;
        s.update_run_worktree(run.id, Some(&worktree_path), WorkflowState::GeneratingTests)
    })?;

    append_progress(&progress_stdout, "testharbor.status", "正在调用生成服务")?;
    if let Some(environment) = environment.as_ref() {
        ensure_test_environment(environment, &progress_stdout)?;
    }
    if mode == AgentMode::Grok {
        append_progress(
            &progress_stdout,
            "testharbor.status",
            "正在检查 Grok 模型服务连接",
        )?;
    } else if mode == AgentMode::Claude {
        append_progress(
            &progress_stdout,
            "testharbor.status",
            "正在检查 Claude Code 登录状态",
        )?;
    }
    let pending_change = state.get_pending_change(&req.run_id);
    if generation_session_id.is_some() {
        append_progress(
            &progress_stdout,
            "testharbor.status",
            &format!("正在恢复 {} 生成会话", mode_label(mode)),
        )?;
    }
    let (generation_scope_hash, generation_evidence_refs) = generation_agent_contract(&snapshot);
    let generate_request = GenerateRequest {
        run_id: Some(req.run_id.clone()),
        objective: req.objective.clone(),
        acceptance_criteria: req.acceptance_criteria.clone(),
        test_strategy: Some(test_strategy),
        cleanup_test_data: Some(cleanup_test_data),
        change_request: pending_change
            .as_ref()
            .map(|request| request.message.clone()),
        test_intent_hash: ih.clone(),
        scope_hash: generation_scope_hash,
        evidence_refs: generation_evidence_refs,
        worktree_path: worktree_path.clone(),
        base_commit: base_commit.clone(),
        language_hint: language_hint.into(),
        progress_dir: Some(progress_dir.display().to_string()),
        session_id: generation_session_id,
        agent_model: req.agent_model.clone(),
        environment: environment
            .as_ref()
            .map(TestEnvironmentInput::agent_environment),
    };
    let gen = match generate(mode, &generate_request) {
        Ok(result) => result,
        Err(error)
            if generate_request.session_id.is_some()
                && is_session_resume_error(&error.to_string()) =>
        {
            append_progress(
                &progress_stdout,
                "testharbor.status",
                "生成会话已失效，正在创建新的生成会话",
            )?;
            let mut fresh_request = generate_request.clone();
            fresh_request.session_id = None;
            generate(mode, &fresh_request)?
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(session_id) = gen.session_id.clone() {
        state.set_codex_generation_session(req.run_id.clone(), session_id)?;
    }

    freeze_and_review_worktree(
        state,
        &run,
        &project,
        &worktree_path,
        &req.objective,
        &req.acceptance_criteria,
        &ih,
        pending_change,
        mode,
        gen.summary,
        gen.written_files,
        gen.proposed_intended_names,
        gen.test_runner,
        &progress_stdout,
        &progress_dir,
        0,
    )
}

fn mode_label(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Mock => "Mock",
        AgentMode::Codex => "Codex",
        AgentMode::Claude => "Claude Code",
        AgentMode::Grok => "Grok",
    }
}

pub fn retry_generation(state: &AppState, run_id: &str) -> Result<GenerateTestsResult> {
    retry_generation_with_options(state, run_id, false)
}

pub fn retry_generation_with_options(
    state: &AppState,
    run_id: &str,
    force_regenerate: bool,
) -> Result<GenerateTestsResult> {
    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|store| store.get_run(rid))?;
    let retry_after_failure = state
        .get_generation_recovery(run_id)
        .map(|recovery| {
            run.workflow_state == WorkflowState::NeedsHuman
                && recovery.target == WorkflowState::GeneratingTests
        })
        .unwrap_or(false);
    let retry_after_changes = run.workflow_state == WorkflowState::GeneratingTests
        && state.get_pending_change(run_id).is_some();
    let retry_after_review = run.workflow_state == WorkflowState::PendingTestReview
        && state.get_case_approval(run_id).is_none()
        && state.get_pending_change(run_id).is_none();
    if !retry_after_failure && !retry_after_changes && !retry_after_review {
        return Err(GenerateError::Other(
            "run is not at a retryable generation checkpoint".into(),
        ));
    }
    let intent = state
        .get_intent_meta(run_id)
        .ok_or_else(|| GenerateError::Other("persisted TestIntent is unavailable".into()))?;
    if force_regenerate {
        if run
            .worktree_path
            .as_deref()
            .is_some_and(|path| Path::new(path).is_dir())
        {
            crate::workspace::ensure_run_worktree_clean(state, run_id)
                .map_err(|error| GenerateError::Other(error.to_string()))?;
            crate::discard_run_worktree(state, run_id)
                .map_err(|error| GenerateError::Other(error.to_string()))?;
        }
        return generate_and_review(
            state,
            GenerateTestsRequest {
                run_id: run_id.to_string(),
                objective: intent.objective,
                acceptance_criteria: intent.acceptance_criteria,
                source_anchors: vec![],
                generation_scope: vec![],
                spec_attachments: vec![],
                test_strategy: Some(intent.test_strategy),
                cleanup_test_data: Some(intent.cleanup_test_data),
                force_regenerate: true,
                agent_mode: intent.agent_mode,
                reuse_agent_session: intent.reuse_agent_session,
                agent_model: intent.agent_model,
                environment: None,
                ..Default::default()
            },
        );
    }
    if retry_after_failure {
        if let Some(worktree_path) = run.worktree_path.clone() {
            let worktree = PathBuf::from(&worktree_path);
            if worktree.is_dir() {
                let project = state.with_store(|store| store.get_project(run.project_id))?;
                let handle = ws::WorkspaceHandle {
                    repo_root: PathBuf::from(&project.root_path),
                    worktree_path: worktree,
                    base_commit: run.base_commit.clone(),
                    run_id: run.id.to_string(),
                };
                let snapshot_json = state
                    .with_store(|store| store.get_run_snapshot_json(run.id))?
                    .ok_or_else(|| {
                        GenerateError::Other(
                            "RunSnapshot is required before recovering generated files".into(),
                        )
                    })?;
                let patterns = serde_json::from_str::<RunSnapshot>(&snapshot_json)
                    .map_err(|error| GenerateError::Other(error.to_string()))?
                    .test_intent
                    .generation_scope;
                let has_changes = ws::list_changes(&handle, &patterns)
                    .map(|changes| !changes.is_empty())
                    .unwrap_or(false);
                if has_changes {
                    let progress_dir = generation_progress_dir(state, run_id);
                    std::fs::create_dir_all(&progress_dir)?;
                    let progress_stdout = progress_dir.join("generation.stdout.jsonl");
                    let progress_stderr = progress_dir.join("generation.stderr.log");
                    std::fs::write(&progress_stdout, "")?;
                    std::fs::write(&progress_stderr, "")?;
                    append_progress(
                        &progress_stdout,
                        "testharbor.status",
                        "正在从已生成文件继续",
                    )?;
                    state.with_store(|s| {
                        s.update_run_worktree(
                            run.id,
                            Some(&worktree_path),
                            WorkflowState::GeneratingTests,
                        )
                    })?;
                    return freeze_and_review_worktree(
                        state,
                        &run,
                        &project,
                        &worktree_path,
                        &intent.objective,
                        &intent.acceptance_criteria,
                        &intent.test_intent_hash,
                        state.get_pending_change(run_id),
                        agent_mode(intent.agent_mode.as_deref()),
                        "Recovered generated files from existing worktree; frozen without rerunning Codex."
                            .into(),
                        vec![],
                        vec![],
                        None,
                        &progress_stdout,
                        &progress_dir,
                        0,
                    );
                }
            }
        }
    }
    generate_and_review(
        state,
        GenerateTestsRequest {
            run_id: run_id.to_string(),
            objective: intent.objective,
            acceptance_criteria: intent.acceptance_criteria,
            source_anchors: vec![],
            generation_scope: vec![],
            spec_attachments: vec![],
            test_strategy: Some(intent.test_strategy),
            cleanup_test_data: Some(intent.cleanup_test_data),
            force_regenerate: false,
            agent_mode: intent.agent_mode,
            reuse_agent_session: intent.reuse_agent_session,
            agent_model: intent.agent_model,
            environment: None,
            ..Default::default()
        },
    )
}

pub fn get_case_review(state: &AppState, run_id: &str) -> Result<CaseReviewState> {
    state.get_case_review(run_id).ok_or(GenerateError::NoReview)
}

/// Full case-review view for UI: findings + candidate meta + unified diff + approve gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseReviewDetail {
    pub run_id: String,
    pub workflow_state: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub review: CaseReviewState,
    pub candidate: Option<CandidateRevision>,
    /// Unified diff (base..candidate). Empty if unavailable.
    pub patch: String,
    /// Per-file slices of the patch for tabbed UI.
    pub file_diffs: Vec<FileDiffSlice>,
    pub approved: bool,
    pub can_approve: bool,
    pub approve_blockers: Vec<String>,
    pub intended_count: u32,
    pub pending_change_request: Option<String>,
    pub can_retry_generation: bool,
    pub agent_mode: Option<String>,
    pub agent_model: Option<String>,
    pub reuse_agent_session: bool,
    pub environment: Option<TestEnvironmentConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiffSlice {
    pub path: String,
    pub patch: String,
    pub additions: u32,
    pub deletions: u32,
}

fn split_file_diffs(patch: &str) -> Vec<FileDiffSlice> {
    if patch.trim().is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut current_path = String::new();
    let mut current_body = String::new();
    let mut adds = 0u32;
    let mut dels = 0u32;

    let flush = |path: &str, body: &str, adds: u32, dels: u32, out: &mut Vec<FileDiffSlice>| {
        if path.is_empty() && body.is_empty() {
            return;
        }
        out.push(FileDiffSlice {
            path: if path.is_empty() {
                "(patch)".into()
            } else {
                path.to_string()
            },
            patch: body.to_string(),
            additions: adds,
            deletions: dels,
        });
    };

    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            if !current_body.is_empty() || !current_path.is_empty() {
                flush(&current_path, &current_body, adds, dels, &mut out);
            }
            current_body.clear();
            adds = 0;
            dels = 0;
            // diff --git a/foo b/foo
            current_path = line
                .split_whitespace()
                .nth(3)
                .map(|s| s.trim_start_matches("b/").to_string())
                .or_else(|| {
                    line.split_whitespace()
                        .nth(2)
                        .map(|s| s.trim_start_matches("a/").to_string())
                })
                .unwrap_or_else(|| "unknown".into());
            current_body.push_str(line);
            current_body.push('\n');
        } else {
            if line.starts_with('+') && !line.starts_with("+++") {
                adds += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                dels += 1;
            }
            current_body.push_str(line);
            current_body.push('\n');
            // Also pick path from +++ b/file
            if line.starts_with("+++ b/") {
                current_path = line.trim_start_matches("+++ b/").to_string();
            } else if line.starts_with("+++ ") && !line.contains("/dev/null") {
                let p = line.trim_start_matches("+++ ").trim();
                if let Some(rest) = p.strip_prefix("b/") {
                    current_path = rest.to_string();
                }
            }
        }
    }
    flush(&current_path, &current_body, adds, dels, &mut out);

    if out.is_empty() && !patch.is_empty() {
        let mut a = 0u32;
        let mut d = 0u32;
        for line in patch.lines() {
            if line.starts_with('+') && !line.starts_with("+++") {
                a += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                d += 1;
            }
        }
        out.push(FileDiffSlice {
            path: "(full patch)".into(),
            patch: patch.to_string(),
            additions: a,
            deletions: d,
        });
    }
    out
}

pub fn get_case_review_detail(state: &AppState, run_id: &str) -> Result<CaseReviewDetail> {
    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(rid))?;
    let project = state.with_store(|s| s.get_project(run.project_id))?;
    let review = state
        .get_case_review(run_id)
        .ok_or(GenerateError::NoReview)?;
    let candidate = state.get_candidate(run_id);
    let approved = state.get_case_approval(run_id).is_some();
    let intent = state.get_intent_meta(run_id);
    let intended_count = state
        .get_intended(run_id)
        .map(|m| m.identities.len() as u32)
        .unwrap_or(0);
    let pending_change_request = state
        .get_pending_change(run_id)
        .map(|request| request.message);
    let can_retry_generation = (state.get_generation_recovery(run_id).is_some()
        && run.workflow_state == WorkflowState::NeedsHuman)
        || (run.workflow_state == WorkflowState::PendingTestReview
            && !approved
            && pending_change_request.is_none());

    let mut patch = String::new();
    if let Some(ref c) = candidate {
        let repo = PathBuf::from(&project.root_path);
        // Prefer worktree for readability; fall back to main repo object store.
        let locations = [run.worktree_path.as_ref().map(PathBuf::from), Some(repo)];
        for loc in locations.into_iter().flatten() {
            if loc.is_dir() {
                if let Ok(p) =
                    testharbor_git_adapter::display_patch(&loc, &c.base_commit, &c.candidate_commit)
                {
                    if !p.is_empty() {
                        patch = p;
                        break;
                    }
                }
            }
        }
    }

    let file_diffs = split_file_diffs(&patch);

    let (can_approve, approve_blockers) = if approved {
        (false, vec!["用例已批准".into()])
    } else if let (Some(c), Some(scope)) = (candidate.as_ref(), state.get_case_scope(run_id)) {
        let guard = evaluate_can_approve(
            run.workflow_state,
            RevisionStatus::Validated,
            &format!("r{}", c.sequence),
            &format!("r{}", c.sequence),
            &c.candidate_tree,
            &c.candidate_tree,
            &scope,
            &scope,
            &review,
            true,
        );
        (
            guard.ok,
            guard.reasons.iter().map(|r| format!("{r:?}")).collect(),
        )
    } else {
        let mut b = Vec::new();
        if candidate.is_none() {
            b.push("无 candidate revision".into());
        }
        if state.get_case_scope(run_id).is_none() {
            b.push("缺少 case scope".into());
        }
        (false, b)
    };

    Ok(CaseReviewDetail {
        run_id: run_id.to_string(),
        workflow_state: format!("{:?}", run.workflow_state),
        objective: intent
            .as_ref()
            .map(|i| i.objective.clone())
            .unwrap_or_else(|| run.objective.clone()),
        acceptance_criteria: intent
            .as_ref()
            .map(|i| i.acceptance_criteria.clone())
            .unwrap_or_default(),
        review,
        candidate,
        patch,
        file_diffs,
        approved,
        can_approve,
        approve_blockers,
        intended_count,
        pending_change_request,
        can_retry_generation,
        agent_mode: intent.as_ref().and_then(|meta| meta.agent_mode.clone()),
        agent_model: intent.as_ref().and_then(|meta| meta.agent_model.clone()),
        reuse_agent_session: intent
            .as_ref()
            .map(|meta| meta.reuse_agent_session)
            .unwrap_or(false),
        environment: intent.as_ref().and_then(|meta| meta.environment.clone()),
    })
}

pub fn set_agent_model(
    state: &AppState,
    run_id: &str,
    agent_model: Option<String>,
) -> Result<IntentMeta> {
    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|store| store.get_run(rid))?;
    if run.snapshot_version.is_some() {
        return Err(GenerateError::Other(
            "RunSnapshot 已冻结；Provider/model 变化必须创建 successor Run".into(),
        ));
    }
    if matches!(
        run.workflow_state,
        WorkflowState::GeneratingTests
            | WorkflowState::ExecutingTests
            | WorkflowState::AnalyzingResult
            | WorkflowState::AgentReviewingResult
    ) {
        return Err(GenerateError::Other(
            "当前 Agent 操作正在运行，完成或终止后才能切换模型".into(),
        ));
    }
    let mut intent = state
        .get_intent_meta(run_id)
        .ok_or_else(|| GenerateError::Other("任务尚未保存测试配置".into()))?;
    let next_model = agent_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string);
    if intent.agent_model != next_model {
        state.clear_codex_generation_session(run_id)?;
        state.with_store(|store| {
            store.append_event(
                Some(run.id),
                Some(run.project_id),
                "agent_model_changed",
                &serde_json::json!({
                    "agent_mode": intent.agent_mode,
                    "previous_model": intent.agent_model,
                    "model": next_model,
                }),
            )
        })?;
    }
    intent.agent_model = next_model;
    state.set_intent_meta(run_id.to_string(), intent.clone())?;
    Ok(intent)
}

pub fn set_finding_disposition(
    state: &AppState,
    run_id: &str,
    finding_id: &str,
    disposition: FindingDisposition,
    risk_reason: Option<String>,
) -> Result<CaseReviewState> {
    let mut review = state
        .get_case_review(run_id)
        .ok_or(GenerateError::NoReview)?;
    if !review.set_disposition(finding_id, disposition, risk_reason) {
        return Err(GenerateError::Other(
            "failed to set disposition (AcceptedRisk requires reason)".into(),
        ));
    }
    state.set_case_review(run_id.to_string(), review.clone())?;
    if state.get_case_approval(run_id).is_some() {
        state.clear_case_approval(run_id)?;
        state.clear_result_decision(run_id)?;
        state.clear_last_execution(run_id)?;
        let rid = parse_run_id(run_id)?;
        let run = state.with_store(|store| store.get_run(rid))?;
        state.with_store(|store| {
            store.update_run_worktree(
                run.id,
                run.worktree_path.as_deref(),
                WorkflowState::PendingTestReview,
            )?;
            store.append_event(
                Some(run.id),
                Some(run.project_id),
                "case_approval_invalidated",
                &serde_json::json!({"reason": "finding disposition changed"}),
            )?;
            Ok::<(), StorageError>(())
        })?;
    }
    Ok(review)
}

pub fn approve_test_case(state: &AppState, run_id: &str) -> Result<ApproveCaseResult> {
    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(rid))?;
    let candidate = state
        .get_candidate(run_id)
        .ok_or(GenerateError::NoCandidate)?;
    let review = state
        .get_case_review(run_id)
        .ok_or(GenerateError::NoReview)?;
    let scope = state
        .get_case_scope(run_id)
        .ok_or(GenerateError::Other("missing case scope".into()))?;

    let guard = evaluate_can_approve(
        run.workflow_state,
        RevisionStatus::Validated,
        &format!("r{}", candidate.sequence),
        &format!("r{}", candidate.sequence),
        &candidate.candidate_tree,
        &candidate.candidate_tree,
        &scope,
        &scope,
        &review,
        true, // case approve does not require TrustGrant; execution still does
    );

    if !guard.ok {
        return Err(GenerateError::ApproveBlocked(
            guard
                .reasons
                .iter()
                .map(|r| format!("{r:?}"))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }

    let test_intent_hash = state
        .get_intent_meta(run_id)
        .map(|intent| intent.test_intent_hash)
        .unwrap_or_else(|| "no-intent".into());
    let intended_hash = state.get_intended(run_id).map(|manifest| manifest.hash);
    let intended_scope_hash = intended_hash.as_deref().unwrap_or("no-intended");
    let trust_scope_hash = state
        .get_trust_grant(run_id)
        .map(|grant| grant.trust_scope_hash)
        .unwrap_or_else(|| "no-trust".into());
    let scope = test_case_scope_hash(
        &candidate.candidate_tree,
        &candidate.base_commit,
        &test_intent_hash,
        intended_scope_hash,
        "cmd",
        "cfg",
        "path",
        &trust_scope_hash,
    );
    state.set_case_scope(run_id.to_string(), scope.clone())?;
    let ctx_hash = case_approval_context_hash(&scope, &review.disposition_snapshot_hash);

    let approval = CaseApprovalRecord {
        decision: HumanTestDecision::Approve,
        candidate_commit: candidate.candidate_commit.clone(),
        candidate_tree: candidate.candidate_tree.clone(),
        base_commit: candidate.base_commit.clone(),
        test_case_scope_hash: scope,
        case_approval_context_hash: ctx_hash.clone(),
        intended_manifest_hash: intended_hash,
        disposition_snapshot_hash: review.disposition_snapshot_hash.clone(),
    };

    state.set_case_approval(run_id.to_string(), approval.clone())?;

    state.with_store(|s| {
        s.update_run_worktree(
            run.id,
            run.worktree_path.as_deref(),
            WorkflowState::ExecutionReady,
        )?;
        s.append_event(Some(run.id), None, "case_approved", &approval)?;
        Ok::<(), StorageError>(())
    })?;

    Ok(ApproveCaseResult {
        approval,
        workflow_state: format!("{:?}", WorkflowState::ExecutionReady),
    })
}

pub fn comment_case_review(
    state: &AppState,
    run_id: &str,
    message: &str,
) -> Result<CommentCaseResult> {
    let message = message.trim();
    if message.is_empty() {
        return Err(GenerateError::Other(
            "case review comment requires a non-empty message".into(),
        ));
    }

    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(rid))?;
    state
        .get_case_review(run_id)
        .ok_or(GenerateError::NoReview)?;
    let transition =
        try_transition(run.workflow_state, WorkflowEvent::AddComment).map_err(|_| {
            GenerateError::Other(format!(
                "case review comment blocked in state {:?}",
                run.workflow_state
            ))
        })?;

    state.with_store(|s| {
        s.append_event(
            Some(run.id),
            Some(run.project_id),
            "case_review_commented",
            &serde_json::json!({
                "message": message,
                "state": format!("{:?}", transition.to),
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;

    Ok(CommentCaseResult {
        workflow_state: format!("{:?}", transition.to),
        message: message.to_string(),
    })
}

pub fn reject_test_case(state: &AppState, run_id: &str, message: &str) -> Result<RejectCaseResult> {
    let message = message.trim();
    if message.is_empty() {
        return Err(GenerateError::Other(
            "reject case requires a non-empty message".into(),
        ));
    }

    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(rid))?;
    let review = state
        .get_case_review(run_id)
        .ok_or(GenerateError::NoReview)?;
    let transition =
        try_transition(run.workflow_state, WorkflowEvent::HumanReject).map_err(|_| {
            GenerateError::Other(format!(
                "reject case blocked in state {:?}",
                run.workflow_state
            ))
        })?;

    state.clear_case_approval(run_id)?;
    state.clear_result_decision(run_id)?;
    state.clear_last_execution(run_id)?;

    state.with_store(|s| {
        s.update_run_worktree(run.id, run.worktree_path.as_deref(), transition.to)?;
        s.append_event(
            Some(run.id),
            Some(run.project_id),
            "case_rejected",
            &serde_json::json!({
                "message": message,
                "from": format!("{:?}", run.workflow_state),
                "to": format!("{:?}", transition.to),
                "disposition_snapshot_hash": review.disposition_snapshot_hash,
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;

    Ok(RejectCaseResult {
        workflow_state: format!("{:?}", transition.to),
        message: message.to_string(),
    })
}

pub fn request_case_changes(
    state: &AppState,
    run_id: &str,
    message: &str,
) -> Result<CaseReviewState> {
    if message.trim().is_empty() {
        return Err(GenerateError::Other(
            "request changes requires a non-empty message".into(),
        ));
    }
    let mut review = state
        .get_case_review(run_id)
        .ok_or(GenerateError::NoReview)?;
    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(rid))?;
    if run.workflow_state != WorkflowState::PendingTestReview {
        return Err(GenerateError::Other(format!(
            "request changes blocked in state {:?}",
            run.workflow_state
        )));
    }
    let targeted_findings = review
        .findings
        .iter()
        .filter(|finding| finding.disposition == FindingDisposition::MustFix)
        .map(|finding| {
            format!(
                "- [{}] {}{}：{}",
                finding.severity,
                finding.file.as_deref().unwrap_or(&finding.category),
                finding
                    .line
                    .map(|line| format!(":{line}"))
                    .unwrap_or_default(),
                finding.message
            )
        })
        .collect::<Vec<_>>();
    let revision_message = if targeted_findings.is_empty() {
        message.trim().to_string()
    } else {
        format!(
            "{}\n\n本次定向修订只处理以下已标记为‘必须修复’的评审问题，保留其他测试场景和断言：\n{}",
            message.trim(),
            targeted_findings.join("\n")
        )
    };
    review.summary = format!("{}\n\nHuman request: {}", review.summary, revision_message);
    // Add a MustFix finding for the request
    review.findings.push(ReviewFinding {
        id: Uuid::new_v4().to_string(),
        severity: "error".into(),
        category: "human_request".into(),
        file: None,
        line: None,
        message: revision_message.clone(),
        suggestion: None,
        disposition: FindingDisposition::MustFix,
        risk_reason: None,
    });
    review.sort_findings();
    review.recompute_disposition_hash();
    state.set_case_review(run_id.to_string(), review.clone())?;
    state.set_pending_change(
        run_id.to_string(),
        PendingChangeRequest {
            message: revision_message,
            previous_revision_sequence: state.get_candidate(run_id).map(|c| c.sequence),
        },
    )?;
    state.clear_case_approval(run_id)?;
    state.clear_result_decision(run_id)?;
    state.clear_last_execution(run_id)?;

    state.with_store(|s| {
        s.update_run_worktree(
            run.id,
            run.worktree_path.as_deref(),
            WorkflowState::GeneratingTests,
        )?;
        s.append_event(
            Some(run.id),
            None,
            "case_changes_requested",
            &serde_json::json!({"message": message}),
        )?;
        Ok::<(), StorageError>(())
    })?;
    Ok(review)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentMeta {
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub test_intent_hash: String,
    #[serde(default = "default_test_strategy")]
    pub test_strategy: String,
    #[serde(default = "default_cleanup_test_data")]
    pub cleanup_test_data: bool,
    /// Agent mode used by later Analyzer / Result Reviewer.
    #[serde(default)]
    pub agent_mode: Option<String>,
    /// Optional model used by generation and later Agent operations.
    #[serde(default)]
    pub agent_model: Option<String>,
    /// Whether this task may resume one AI session across compatible phases.
    #[serde(default)]
    pub reuse_agent_session: bool,
    /// Runtime target configuration supplied to the CLI. The context is user
    /// authored and may intentionally contain test credentials.
    #[serde(default)]
    pub environment: Option<TestEnvironmentConfig>,
}

fn default_test_strategy() -> String {
    "e2e_api_flow".into()
}

fn default_cleanup_test_data() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::open_project;
    use crate::AppState;
    use std::process::Command;

    fn intent_hash<T: AsRef<str>>(
        objective: &str,
        criteria: &[T],
        strategy: &str,
        cleanup_test_data: bool,
    ) -> String {
        let criteria = criteria.iter().map(AsRef::as_ref).collect::<Vec<_>>();
        sha256_hex(format!("{objective}\n{criteria:?}\n{strategy}\n{cleanup_test_data}").as_bytes())
    }

    #[test]
    fn service_endpoint_accepts_bare_host_and_port() {
        assert_eq!(
            service_endpoint("localhost:8300"),
            Some("localhost:8300".into())
        );
        assert_eq!(
            service_endpoint("http://localhost:8300/api/health"),
            Some("localhost:8300".into())
        );
    }

    #[test]
    fn split_file_diffs_parses_unified() {
        let patch = "\
diff --git a/foo_test.go b/foo_test.go
index 111..222 100644
--- a/foo_test.go
+++ b/foo_test.go
@@ -1,2 +1,3 @@
 package foo
+func TestX() {}
-func old() {}
";
        let slices = split_file_diffs(patch);
        assert_eq!(slices.len(), 1);
        assert!(slices[0].path.contains("foo_test.go"));
        assert_eq!(slices[0].additions, 1);
        assert_eq!(slices[0].deletions, 1);
    }

    #[test]
    fn intent_hash_binds_strategy_and_cleanup_policy() {
        let criteria = vec!["flow passes".to_string()];
        let e2e_cleanup = intent_hash("publish flow", &criteria, "e2e_api_flow", true);
        let unit_cleanup = intent_hash("publish flow", &criteria, "unit", true);
        let e2e_keep_data = intent_hash("publish flow", &criteria, "e2e_api_flow", false);

        assert_ne!(e2e_cleanup, unit_cleanup);
        assert_ne!(e2e_cleanup, e2e_keep_data);
        assert_eq!(
            normalize_test_strategy(Some("")),
            "e2e_api_flow",
            "blank strategy defaults to e2e flow"
        );
    }

    #[test]
    fn runtime_secret_config_persists_only_hmac_fingerprints() {
        let data = tempfile::tempdir().unwrap();
        let state = AppState::open_at(data.path()).unwrap();
        let input = TestEnvironmentInput {
            context: "服务运行在项目配置指定的环境，使用 qa 租户".into(),
            username: Some("qa-user".into()),
            password: Some("do-not-store-this".into()),
            variables: HashMap::from([("API_TOKEN".into(), "token-value".into())]),
            ..Default::default()
        };
        let first = input.config(&state).unwrap();
        let encoded = serde_json::to_string(&first).unwrap();
        assert!(!encoded.contains("do-not-store-this"));
        assert!(!encoded.contains("token-value"));
        assert!(encoded.contains("hmac-sha256:"));
        assert_eq!(first.context, input.context);
        assert_eq!(first, input.config(&state).unwrap());
    }

    #[test]
    fn ai_review_auto_revision_stops_only_for_major_findings() {
        assert!(needs_auto_revision(
            "CHANGES",
            &[AgentFinding {
                severity: "high".into(),
                category: "coverage".into(),
                file: None,
                line: None,
                message: "missing critical path".into(),
                suggestion: None,
            }]
        ));
        assert!(needs_auto_revision("BLOCK", &[]));
        assert!(!needs_auto_revision(
            "CHANGES",
            &[AgentFinding {
                severity: "medium".into(),
                category: "style".into(),
                file: None,
                line: None,
                message: "improve naming".into(),
                suggestion: None,
            }]
        ));
    }

    #[test]
    fn extracts_codex_session_from_progress_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("generation.stdout.jsonl");
        std::fs::write(
            &log,
            "{\"type\":\"testharbor.status\",\"message\":\"x\"}\n{\"type\":\"thread.started\",\"thread_id\":\"019f6a7e-b361-70a0-8c72-61b190313d60\"}\n",
        )
        .unwrap();
        assert_eq!(
            codex_session_from_progress(&log).as_deref(),
            Some("019f6a7e-b361-70a0-8c72-61b190313d60")
        );
    }

    fn init_go_repo(dir: &std::path::Path) {
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
        Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
            .current_dir(dir)
            .status()
            .unwrap();
        std::fs::write(dir.join("go.mod"), "module example.com/s5\n\ngo 1.21\n").unwrap();
        std::fs::write(dir.join("main.go"), "package main\nfunc main() {}\n").unwrap();
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

    fn init_plain_repo(dir: &std::path::Path) {
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
        std::fs::write(dir.join("README.md"), "plain repository\n").unwrap();
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
    fn fallback_runner_uses_a_plain_script_when_cli_does_not_return_one() {
        let repo = tempfile::tempdir().unwrap();
        init_plain_repo(repo.path());
        std::fs::write(repo.path().join("test.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        let data = tempfile::tempdir().unwrap();
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run = state
            .with_store(|store| {
                store.get_run(parse_run_id(&opened.created_run.unwrap().run_id).unwrap())
            })
            .unwrap();
        let project = state
            .with_store(|store| store.get_project(run.project_id))
            .unwrap();

        let runner = fallback_agent_runner(&project).expect("sh should be available");
        assert_eq!(runner.executable, "sh");
        assert_eq!(runner.args, vec!["test.sh"]);
        assert_eq!(runner.parser, "exit-code");
    }

    #[test]
    fn agent_runner_binds_without_mutating_deferred_snapshot() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_plain_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;
        let rid = parse_run_id(&run_id).unwrap();
        let run = state.with_store(|store| store.get_run(rid)).unwrap();
        let project = state
            .with_store(|store| store.get_project(run.project_id))
            .unwrap();
        let request = GenerateTestsRequest {
            run_id: run_id.clone(),
            objective: "run the repository test command".into(),
            acceptance_criteria: vec!["the command exits successfully".into()],
            agent_mode: Some("codex".into()),
            ..Default::default()
        };
        let snapshot =
            build_run_snapshot(&state, &run, &project, &request, AgentMode::Codex).unwrap();
        assert!(crate::is_deferred_runner_hash(
            &snapshot.required_trust_scope_hash
        ));
        let snapshot_hash = sha256_hex(&serde_json::to_vec(&snapshot).unwrap());
        state
            .with_store(|store| {
                store.set_run_snapshot(
                    run.id,
                    &snapshot.test_intent.hash,
                    &snapshot_hash,
                    &snapshot,
                )
            })
            .unwrap();

        bind_agent_runner(
            &state,
            &run,
            &project,
            Some(AgentTestRunner {
                executable: "/bin/echo".into(),
                args: vec!["testharbor".into()],
                cwd: Some(".".into()),
                parser: "exit-code".into(),
                timeout_ms: Some(10_000),
            }),
        )
        .unwrap();
        assert!(state.get_trust_grant(&run_id).is_some());
        let persisted = state
            .with_store(|store| store.get_run_snapshot_json(run.id))
            .unwrap()
            .unwrap();
        let persisted: RunSnapshot = serde_json::from_str(&persisted).unwrap();
        assert_eq!(persisted, snapshot);
    }

    #[test]
    fn case_review_detail_includes_patch() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_go_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;
        let gen = generate_and_review(
            &state,
            GenerateTestsRequest {
                run_id: run_id.clone(),
                objective: "cover".into(),
                acceptance_criteria: vec!["has test".into()],
                test_strategy: Some("unit".into()),
                cleanup_test_data: Some(true),
                force_regenerate: false,
                agent_mode: Some("mock".into()),
                reuse_agent_session: false,
                agent_model: None,
                environment: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!gen.candidate.changed_paths.is_empty());
        let detail = get_case_review_detail(&state, &run_id).unwrap();
        assert!(!detail.approved);
        assert!(!detail.patch.is_empty() || !detail.file_diffs.is_empty());
        assert!(
            detail
                .file_diffs
                .iter()
                .any(|f| f.path.contains("test") || f.additions > 0)
                || detail.patch.contains("Test")
                || detail.patch.contains("package"),
            "expected test-ish patch, got {:?}",
            detail
                .file_diffs
                .iter()
                .map(|f| &f.path)
                .collect::<Vec<_>>()
        );
        assert_eq!(detail.review.recommendation, gen.review.recommendation);
    }

    #[test]
    fn run_snapshot_uses_base_commit_markdown_and_truncates_attachments() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_go_repo(repo.path());
        std::fs::create_dir_all(repo.path().join("docs")).unwrap();
        std::fs::write(repo.path().join("docs/spec.md"), "committed acceptance\n").unwrap();
        Command::new("git")
            .args(["add", "docs/spec.md"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "spec"])
            .current_dir(repo.path())
            .status()
            .unwrap();

        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;
        // Working-tree edits must not affect the immutable repository
        // attachment snapshot.
        std::fs::write(
            repo.path().join("docs/spec.md"),
            "uncommitted replacement\n",
        )
        .unwrap();
        generate_and_review(
            &state,
            GenerateTestsRequest {
                run_id: run_id.clone(),
                objective: "cover committed spec".into(),
                acceptance_criteria: vec!["generated test exists".into()],
                spec_attachments: vec![
                    SpecAttachmentInput {
                        kind: SpecAttachmentKind::RepoMarkdown,
                        label_or_path: "docs/spec.md".into(),
                        body: None,
                    },
                    SpecAttachmentInput {
                        kind: SpecAttachmentKind::PastedText,
                        label_or_path: "large paste".into(),
                        body: Some("界".repeat(30_000)),
                    },
                ],
                test_strategy: Some("unit".into()),
                cleanup_test_data: Some(true),
                agent_mode: Some("mock".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let rid = parse_run_id(&run_id).unwrap();
        let snapshot_json = state
            .with_store(|store| store.get_run_snapshot_json(rid))
            .unwrap()
            .unwrap();
        let snapshot: RunSnapshot = serde_json::from_str(&snapshot_json).unwrap();
        assert_eq!(snapshot.test_intent.spec_attachments.len(), 2);
        let markdown = &snapshot.test_intent.spec_attachments[0];
        assert_eq!(
            markdown.content_sha256,
            content_body_hash("committed acceptance\n")
        );
        assert!(markdown.git_blob_oid.is_some());
        let pasted = &snapshot.test_intent.spec_attachments[1];
        assert!(pasted.truncated);
        assert!(pasted.byte_len <= SPEC_MAX_BYTES_PER_ATTACHMENT);
        assert_eq!(pasted.byte_len % 3, 0, "UTF-8 truncation split a codepoint");
        assert!(snapshot.test_intent.generation_scope.len() > 0);
        assert!(snapshot.test_intent.execution_scope_summary.contains("go"));
        assert!(snapshot.test_intent.pass_policy_summary.contains("strict"));
    }

    #[test]
    fn unapproved_pending_review_can_refresh_generation() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_go_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;

        let first = generate_and_review(
            &state,
            GenerateTestsRequest {
                run_id: run_id.clone(),
                objective: "publish flow".into(),
                acceptance_criteria: vec!["business flow is covered".into()],
                test_strategy: Some("e2e_api_flow".into()),
                cleanup_test_data: Some(true),
                force_regenerate: false,
                agent_mode: Some("mock".into()),
                reuse_agent_session: false,
                agent_model: None,
                environment: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(first.candidate.sequence, 1);

        let refreshed = retry_generation(&state, &run_id).unwrap();
        assert_eq!(refreshed.candidate.sequence, 2);
        assert_eq!(
            state
                .with_store(|store| store.get_run(parse_run_id(&run_id).unwrap()))
                .unwrap()
                .workflow_state,
            WorkflowState::PendingTestReview
        );
    }

    #[test]
    fn mock_generate_review_approve() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_go_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;

        let gen = generate_and_review(
            &state,
            GenerateTestsRequest {
                run_id: run_id.clone(),
                objective: "cover main package".into(),
                acceptance_criteria: vec!["basic test exists".into()],
                test_strategy: Some("unit".into()),
                cleanup_test_data: Some(true),
                force_regenerate: false,
                agent_mode: Some("mock".into()),
                reuse_agent_session: false,
                agent_model: None,
                environment: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!gen.candidate.candidate_tree.is_empty());
        assert!(!gen.review.findings.is_empty() || gen.review.recommendation == "APPROVE");

        // Resolve must-fix findings
        let mut review = gen.review;
        for f in review.findings.clone() {
            if f.disposition == FindingDisposition::MustFix {
                set_finding_disposition(&state, &run_id, &f.id, FindingDisposition::Resolved, None)
                    .unwrap();
            } else if f.disposition == FindingDisposition::Open {
                set_finding_disposition(
                    &state,
                    &run_id,
                    &f.id,
                    FindingDisposition::AcceptedRisk,
                    Some("acceptable for mock".into()),
                )
                .unwrap();
            }
        }
        // refresh
        review = get_case_review(&state, &run_id).unwrap();
        // resolve any remaining must fix
        for f in review.findings.clone() {
            if f.disposition == FindingDisposition::MustFix {
                set_finding_disposition(&state, &run_id, &f.id, FindingDisposition::Resolved, None)
                    .unwrap();
            }
        }

        let approved = approve_test_case(&state, &run_id).unwrap();
        assert_eq!(approved.approval.decision, HumanTestDecision::Approve);
        assert!(!approved.approval.case_approval_context_hash.is_empty());
    }

    #[test]
    fn request_changes_generates_next_revision_with_feedback() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_go_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;

        let first = generate_and_review(
            &state,
            GenerateTestsRequest {
                run_id: run_id.clone(),
                objective: "cover main package".into(),
                acceptance_criteria: vec!["basic test exists".into()],
                test_strategy: Some("unit".into()),
                cleanup_test_data: Some(true),
                force_regenerate: false,
                agent_mode: Some("mock".into()),
                reuse_agent_session: false,
                agent_model: None,
                environment: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(first.candidate.sequence, 1);

        request_case_changes(&state, &run_id, "增加边界条件断言").unwrap();
        let second = retry_generation(&state, &run_id).unwrap();
        assert_eq!(second.candidate.sequence, 2);
        assert!(second.review.findings.iter().any(|finding| {
            finding.category == "human_request_followup"
                && finding.message.contains("增加边界条件断言")
        }));
        assert!(state.get_pending_change(&run_id).is_none());

        let worktree = state
            .with_store(|store| store.get_run(parse_run_id(&run_id).unwrap()))
            .unwrap()
            .worktree_path
            .unwrap();
        let generated = std::fs::read_to_string(
            std::path::Path::new(&worktree).join("testharbor_generated_test.go"),
        )
        .unwrap();
        assert!(generated.contains("Requested change: 增加边界条件断言"));
    }

    #[test]
    fn comment_case_review_does_not_change_workflow_state() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_go_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;

        generate_and_review(
            &state,
            GenerateTestsRequest {
                run_id: run_id.clone(),
                objective: "cover main package".into(),
                acceptance_criteria: vec!["basic test exists".into()],
                test_strategy: Some("unit".into()),
                cleanup_test_data: Some(true),
                force_regenerate: false,
                agent_mode: Some("mock".into()),
                reuse_agent_session: false,
                agent_model: None,
                environment: None,
                ..Default::default()
            },
        )
        .unwrap();

        let commented = comment_case_review(&state, &run_id, "先保留这个断言").unwrap();
        assert_eq!(commented.workflow_state, "PendingTestReview");

        let run = state
            .with_store(|store| store.get_run(parse_run_id(&run_id).unwrap()))
            .unwrap();
        assert_eq!(run.workflow_state, WorkflowState::PendingTestReview);
        assert!(state.get_case_approval(&run_id).is_none());
    }

    #[test]
    fn reject_case_moves_pending_review_to_rejected() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_go_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;

        generate_and_review(
            &state,
            GenerateTestsRequest {
                run_id: run_id.clone(),
                objective: "cover main package".into(),
                acceptance_criteria: vec!["basic test exists".into()],
                test_strategy: Some("unit".into()),
                cleanup_test_data: Some(true),
                force_regenerate: false,
                agent_mode: Some("mock".into()),
                reuse_agent_session: false,
                agent_model: None,
                environment: None,
                ..Default::default()
            },
        )
        .unwrap();

        let blank = reject_test_case(&state, &run_id, "   ").unwrap_err();
        assert!(blank
            .to_string()
            .contains("reject case requires a non-empty message"));

        let rejected = reject_test_case(&state, &run_id, "生成的断言覆盖了错误行为").unwrap();
        assert_eq!(rejected.workflow_state, "Rejected");
        assert!(state.get_case_approval(&run_id).is_none());
        assert!(state.get_result_decision(&run_id).is_none());
        assert!(state.get_last_execution(&run_id).is_none());

        let run = state
            .with_store(|store| store.get_run(parse_run_id(&run_id).unwrap()))
            .unwrap();
        assert_eq!(run.workflow_state, WorkflowState::Rejected);

        let blocked = reject_test_case(&state, &run_id, "再次拒绝").unwrap_err();
        assert!(blocked
            .to_string()
            .contains("reject case blocked in state Rejected"));
    }

    #[test]
    fn retry_after_failure_without_frozen_snapshot_fails_closed() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_go_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;
        let prepared = crate::prepare_worktree(&state, &run_id).unwrap();
        std::fs::write(
            PathBuf::from(&prepared.worktree_path).join("recovered_test.go"),
            "package main\n\nimport \"testing\"\n\nfunc TestRecovered(t *testing.T) {}\n",
        )
        .unwrap();
        let run = state
            .with_store(|store| store.get_run(parse_run_id(&run_id).unwrap()))
            .unwrap();
        state
            .set_intent_meta(
                run_id.clone(),
                IntentMeta {
                    objective: "recover generated tests".into(),
                    acceptance_criteria: vec!["has recovered test".into()],
                    test_intent_hash: intent_hash(
                        "recover generated tests",
                        &["has recovered test"],
                        "e2e_api_flow",
                        true,
                    ),
                    test_strategy: "e2e_api_flow".into(),
                    cleanup_test_data: true,
                    agent_mode: Some("mock".into()),
                    reuse_agent_session: false,
                    agent_model: None,
                    environment: None,
                },
            )
            .unwrap();
        state
            .set_generation_recovery(
                run_id.clone(),
                GenerationRecovery {
                    target: WorkflowState::GeneratingTests,
                    reason: "freeze failed after generation".into(),
                },
            )
            .unwrap();
        state
            .with_store(|store| {
                store.update_run_worktree(
                    run.id,
                    Some(&prepared.worktree_path),
                    WorkflowState::NeedsHuman,
                )
            })
            .unwrap();

        let error = retry_generation(&state, &run_id).unwrap_err();
        assert!(error.to_string().contains("RunSnapshot is required"));
    }

    #[test]
    fn persisted_review_and_approval_survive_restart() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_go_repo(repo.path());
        let run_id = {
            let state = AppState::open_at(data.path()).unwrap();
            let opened = open_project(&state, repo.path(), true).unwrap();
            let run_id = opened.created_run.unwrap().run_id;
            let generated = generate_and_review(
                &state,
                GenerateTestsRequest {
                    run_id: run_id.clone(),
                    objective: "cover main package".into(),
                    acceptance_criteria: vec!["basic test exists".into()],
                    test_strategy: Some("unit".into()),
                    cleanup_test_data: Some(true),
                    force_regenerate: false,
                    agent_mode: Some("mock".into()),
                    reuse_agent_session: false,
                    agent_model: None,
                    environment: None,
                    ..Default::default()
                },
            )
            .unwrap();
            for finding in generated.review.findings {
                let disposition = if finding.disposition == FindingDisposition::MustFix {
                    FindingDisposition::Resolved
                } else {
                    FindingDisposition::AcceptedRisk
                };
                let reason = (disposition == FindingDisposition::AcceptedRisk)
                    .then(|| "accepted for persistence test".into());
                set_finding_disposition(&state, &run_id, &finding.id, disposition, reason).unwrap();
            }
            approve_test_case(&state, &run_id).unwrap();
            run_id
        };

        let reopened = AppState::open_at(data.path()).unwrap();
        let detail = get_case_review_detail(&reopened, &run_id).unwrap();
        assert!(detail.approved);
        assert_eq!(detail.candidate.unwrap().sequence, 1);
        assert!(reopened.get_case_approval(&run_id).is_some());
        assert!(reopened.get_intent_meta(&run_id).is_some());
    }
}
