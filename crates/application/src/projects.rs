//! Project open / probe use cases.

use crate::AppState;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use testharbor_domain::{DeliveryStatus, ProjectId, RunId, RunPointers, WorkflowState};
use testharbor_git_adapter::{self as git, GitRepoInfo};
use testharbor_storage::{ProjectRecord, RunRecord, StorageError};
use testharbor_trust::{resolve_executable, CommandIdentity, TestProfileDraft};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error(transparent)]
    Git(#[from] git::GitError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("path does not exist: {0}")]
    PathMissing(String),
    #[error("submodules are not supported in MVP")]
    SubmodulesUnsupported,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ProjectError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub path: String,
    pub is_git_repo: bool,
    pub repo: Option<GitRepoInfoDto>,
    pub readiness: Option<ProjectReadiness>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepoInfoDto {
    pub root: String,
    pub head_commit: String,
    pub short_commit: String,
    pub branch: Option<String>,
    pub remote_url: Option<String>,
    pub is_dirty: bool,
    pub has_submodules: bool,
    pub apply_hint: String,
}

impl From<GitRepoInfo> for GitRepoInfoDto {
    fn from(info: GitRepoInfo) -> Self {
        let short = info.head_commit.chars().take(7).collect::<String>();
        let apply_hint = format!(
            "Apply 时要求主工作区 clean 且 HEAD={}（当前分支: {}）",
            short,
            info.branch.clone().unwrap_or_else(|| "detached".into())
        );
        Self {
            root: info.root.display().to_string(),
            head_commit: info.head_commit,
            short_commit: short,
            branch: info.branch,
            remote_url: info.remote_url,
            is_dirty: info.is_dirty,
            has_submodules: info.has_submodules,
            apply_hint,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: String,
    pub root_path: String,
    pub name: String,
    pub git_remote: Option<String>,
    pub last_opened_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenProjectResult {
    pub project: ProjectSummary,
    pub repo: GitRepoInfoDto,
    pub readiness: ProjectReadiness,
    pub created_run: Option<CreatedRunSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedRunSummary {
    pub run_id: String,
    pub workflow_state: String,
    pub base_commit: String,
    pub objective: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectReadiness {
    pub detected_stacks: Vec<String>,
    pub test_file_count: u32,
    pub capability: String,
    pub capability_label: String,
    pub summary: String,
    pub profiles: Vec<ProjectProfileSummary>,
    pub checks: Vec<ReadinessCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectProfileSummary {
    pub key: String,
    pub label: String,
    pub command_preview: String,
    pub parser: String,
    pub structured: bool,
    pub strict: bool,
    pub available: bool,
    pub recommended: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessCheck {
    pub key: String,
    pub label: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectInspection {
    pub readiness: ProjectReadiness,
    pub profile: Option<TestProfileDraft>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapabilityLevel {
    Strict,
    RunOnly,
    ConfigIncomplete,
}

#[derive(Debug, Clone)]
struct DetectedProfile {
    key: String,
    label: String,
    stack: String,
    marker: String,
    executable: String,
    args: Vec<String>,
    parser: String,
    structured: bool,
    strict: bool,
    available: bool,
    recommended: bool,
}

impl DetectedProfile {
    fn command_preview(&self) -> String {
        let mut parts = vec![self.executable.clone()];
        parts.extend(self.args.clone());
        parts.join(" ")
    }

    fn to_summary(&self) -> ProjectProfileSummary {
        ProjectProfileSummary {
            key: self.key.clone(),
            label: self.label.clone(),
            command_preview: self.command_preview(),
            parser: self.parser.clone(),
            structured: self.structured,
            strict: self.strict,
            available: self.available,
            recommended: self.recommended,
        }
    }

    fn to_draft(&self, repo_root: &Path) -> TestProfileDraft {
        let resolved = resolve_executable(&self.executable);
        let version = resolved
            .as_deref()
            .and_then(testharbor_trust::probe_version);
        let executable = resolved
            .as_deref()
            .unwrap_or_else(|| Path::new(&self.executable))
            .display()
            .to_string();
        let command = |args: Vec<String>, parser: &str| CommandIdentity {
            executable: executable.clone(),
            args,
            cwd: repo_root.display().to_string(),
            parser: parser.into(),
            executable_version: version.clone(),
            executable_identity: None,
        };
        TestProfileDraft {
            name: self.key.clone(),
            format: None,
            collect: (self.key == "go-test-json")
                .then(|| command(vec!["list".into(), "./...".into()], "go-list")),
            compile: (self.key == "go-test-json").then(|| {
                command(
                    vec!["test".into(), "-run=^$".into(), "./...".into()],
                    "exit-code",
                )
            }),
            command: command(self.args.clone(), &self.parser),
            timeout_ms: if self.strict { 600_000 } else { 300_000 },
            service: None,
            env_policy: vec![],
            network_unrestricted: false,
        }
    }
}

/// Probe a filesystem path without writing to DB.
pub fn probe_path(path: impl AsRef<Path>) -> ProbeResult {
    let path = path.as_ref();
    let path_str = path.display().to_string();
    if !path.exists() {
        return ProbeResult {
            path: path_str,
            is_git_repo: false,
            repo: None,
            readiness: None,
            error: Some("路径不存在".into()),
        };
    }
    match git::inspect_repo(path) {
        Ok(info) => {
            let readiness = inspect_project(&info.root).readiness;
            ProbeResult {
                path: path_str,
                is_git_repo: true,
                repo: Some(info.into()),
                readiness: Some(readiness),
                error: None,
            }
        }
        Err(e) => ProbeResult {
            path: path_str,
            is_git_repo: false,
            repo: None,
            readiness: None,
            error: Some(e.to_string()),
        },
    }
}

/// Open (or re-open) a project. Run creation is an explicit separate command.
pub fn open_project(
    state: &AppState,
    path: impl AsRef<Path>,
    create_placeholder_run: bool,
) -> Result<OpenProjectResult> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(ProjectError::PathMissing(path.display().to_string()));
    }

    let info = git::inspect_repo(path)?;
    if info.has_submodules {
        return Err(ProjectError::SubmodulesUnsupported);
    }

    let root = info
        .root
        .canonicalize()
        .unwrap_or_else(|_| info.root.clone());
    // Keep TestHarbor's optional repository-local runtime directory out of
    // the user's project status without modifying the tracked .gitignore.
    // Generated candidates still live in detached TestHarbor worktrees and
    // are only applied to the main repository through the guarded apply flow.
    git::ensure_info_exclude(&root, "/.testharbor/")?;
    let root_str = root.display().to_string();
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".into());

    let now = Utc::now();
    let project = state.with_store(|store| -> Result<ProjectRecord> {
        if let Some(mut existing) = store.find_project_by_root(&root_str)? {
            existing.last_opened_at = now;
            existing.name = name.clone();
            existing.git_remote = info.remote_url.clone();
            store.upsert_project(&existing)?;
            store.append_event(
                None,
                Some(existing.id),
                "project_opened",
                &serde_json::json!({
                    "root_path": root_str,
                    "base_commit": info.head_commit,
                }),
            )?;
            return Ok(existing);
        }

        let record = ProjectRecord {
            id: ProjectId::new(),
            root_path: root_str.clone(),
            name: name.clone(),
            git_remote: info.remote_url.clone(),
            created_at: now,
            last_opened_at: now,
        };
        store.upsert_project(&record)?;
        store.append_event(
            None,
            Some(record.id),
            "project_created",
            &serde_json::json!({
                "root_path": root_str,
                "base_commit": info.head_commit,
            }),
        )?;
        Ok(record)
    })?;

    // Kept for in-process migration/tests only. Public desktop/CLI entry points
    // reject the legacy flag and expose explicit Run creation.
    let created_run = if create_placeholder_run {
        Some(create_placeholder_run_record(state, &project, &info)?)
    } else {
        None
    };
    let inspection = inspect_project(&root);

    Ok(OpenProjectResult {
        project: ProjectSummary {
            id: project.id.to_string(),
            root_path: project.root_path,
            name: project.name,
            git_remote: project.git_remote,
            last_opened_at: project.last_opened_at.to_rfc3339(),
        },
        repo: info.into(),
        readiness: inspection.readiness,
        created_run,
    })
}

pub(crate) fn inspect_project(repo_root: &Path) -> ProjectInspection {
    let discovery = git::rev_parse(repo_root, "HEAD")
        .ok()
        .map(|base| testharbor_project_discovery::discover(repo_root, &base));
    let mut profiles = detect_profiles(repo_root);
    let test_file_count = count_test_files(repo_root);
    let detected_stacks = unique_stacks(&profiles);

    let recommended_index = profiles
        .iter()
        .position(|profile| profile.available && profile.strict)
        .or_else(|| profiles.iter().position(|profile| profile.available))
        .or_else(|| (!profiles.is_empty()).then_some(0));
    if let Some(index) = recommended_index {
        profiles[index].recommended = true;
    }

    let capability = if profiles
        .iter()
        .any(|profile| profile.available && profile.strict)
    {
        CapabilityLevel::Strict
    } else if profiles.iter().any(|profile| profile.available) {
        CapabilityLevel::RunOnly
    } else {
        CapabilityLevel::ConfigIncomplete
    };

    let mut checks = Vec::new();
    if profiles.is_empty() {
        checks.push(ReadinessCheck {
            key: "detect".into(),
            label: "项目类型".into(),
            status: "failed".into(),
            detail: "未识别到已支持的测试项目标记（如 go.mod、Cargo.toml、pyproject.toml）。"
                .into(),
        });
    } else {
        let markers = profiles
            .iter()
            .map(|profile| format!("{} ({})", profile.stack, profile.marker))
            .collect::<Vec<_>>()
            .join("、");
        checks.push(ReadinessCheck {
            key: "detect".into(),
            label: "项目类型".into(),
            status: "passed".into(),
            detail: format!("检测到 {markers}"),
        });
    }

    checks.push(ReadinessCheck {
        key: "tests".into(),
        label: "测试资产".into(),
        status: if test_file_count > 0 {
            "passed"
        } else {
            "inconclusive"
        }
        .into(),
        detail: if test_file_count > 0 {
            format!("已发现 {test_file_count} 个测试文件，可据此对齐仓库习惯。")
        } else {
            "暂未识别到现有测试文件；仍可创建任务，但需要更依赖人工确认范围。".into()
        },
    });

    if let Some(config) = discovery.and_then(|item| item.suggested_config) {
        checks.push(ReadinessCheck {
            key: "testharbor_config".into(),
            label: "仓库建议配置".into(),
            status: "inconclusive".into(),
            detail: format!(
                "从 base commit 读取 .testharbor.yaml（{}）；仅作为建议，仍需 Profile/Trust 确认。",
                &config.content_sha256[..12]
            ),
        });
    }

    if let Some(profile) = recommended_index.and_then(|index| profiles.get(index)) {
        checks.push(ReadinessCheck {
            key: "command".into(),
            label: "推荐命令".into(),
            status: if profile.available {
                "passed"
            } else {
                "failed"
            }
            .into(),
            detail: if profile.available {
                format!("可使用 `{}`", profile.command_preview())
            } else {
                format!(
                    "检测到 {}，但当前环境缺少 `{}`。",
                    profile.label, profile.executable
                )
            },
        });
        checks.push(ReadinessCheck {
            key: "result_mode".into(),
            label: "结果能力".into(),
            status: if profile.strict || profile.structured {
                "passed"
            } else {
                "inconclusive"
            }
            .into(),
            detail: if profile.strict {
                "结构化结果可用于 intended observation 与严格 AcceptPass/Apply。".into()
            } else if profile.structured && profile.available {
                "可读取结构化结果并做结果评审，但缺少稳定 intended/inventory 绑定，不能严格 AcceptPass。".into()
            } else if profile.available {
                "当前先走 exit-code 命令级结果；可执行，但不能严格 AcceptPass/Apply。".into()
            } else {
                "命令尚不可执行，结果能力待配置修复后确认。".into()
            },
        });
    }

    checks.push(ReadinessCheck {
        key: "baseline".into(),
        label: "Baseline".into(),
        status: "not_run".into(),
        detail:
            "尚未运行 baseline/collect/compile 探测；当前只展示准备状态，不替代正式 Execution。"
                .into(),
    });

    let (capability_key, capability_label, summary) = match capability {
        CapabilityLevel::Strict => (
            "strict_accept_pass_apply",
            "可严格 AcceptPass/Apply",
            "已识别结构化测试链路；在仓库全绿前提下，可进入严格门禁路径。",
        ),
        CapabilityLevel::RunOnly => (
            "run_only",
            "仅可运行",
            "已能建议可执行命令，但当前结果仍是命令级或非结构化；不能严格 AcceptPass/Apply。",
        ),
        CapabilityLevel::ConfigIncomplete => (
            "config_incomplete",
            "配置不完整",
            "已发现项目线索，但当前缺少可执行命令或推荐 Profile；需要先修复环境/配置。",
        ),
    };

    let notes = recommended_index
        .and_then(|index| profiles.get(index))
        .map(|profile| {
            let mut notes = vec![format!("建议 Profile：{}", profile.command_preview())];
            if profile.strict {
                notes.push("结构化结果可支持 intended tests 与严格 AcceptPass。".into());
            } else if profile.structured && profile.available {
                notes.push("将解析 JUnit XML 等结构化结果，但当前仍不能严格 AcceptPass。".into());
            } else if profile.available {
                notes.push("当前将以 exit-code 模式执行，结果只证明命令成功或失败。".into());
            } else {
                notes.push(format!(
                    "当前环境缺少 `{}`，需修复后再确认命令。",
                    profile.executable
                ));
            }
            notes
        })
        .unwrap_or_else(|| vec!["未识别到可直接建议的 Profile；可后续手动确认自定义命令。".into()]);

    ProjectInspection {
        readiness: ProjectReadiness {
            detected_stacks,
            test_file_count,
            capability: capability_key.into(),
            capability_label: capability_label.into(),
            summary: summary.into(),
            profiles: profiles.iter().map(DetectedProfile::to_summary).collect(),
            checks,
        },
        profile: recommended_index
            .and_then(|index| profiles.get(index))
            .filter(|profile| profile.available)
            .map(|profile| profile.to_draft(repo_root)),
        notes,
    }
}

fn detect_profiles(repo_root: &Path) -> Vec<DetectedProfile> {
    let mut profiles = Vec::new();

    if repo_root.join("go.mod").is_file() {
        profiles.push(DetectedProfile {
            key: "go-test-json".into(),
            label: "Go Test JSON".into(),
            stack: "Go".into(),
            marker: "go.mod".into(),
            executable: resolve_executable("go")
                .unwrap_or_else(|| PathBuf::from("go"))
                .display()
                .to_string(),
            args: vec![
                "test".into(),
                "-json".into(),
                "-count=1".into(),
                "./...".into(),
            ],
            parser: "go-test-json".into(),
            structured: true,
            strict: true,
            available: resolve_executable("go").is_some(),
            recommended: false,
        });
    }

    if repo_root.join("Cargo.toml").is_file() {
        profiles.push(DetectedProfile {
            key: "cargo-test".into(),
            label: "Cargo Test".into(),
            stack: "Rust".into(),
            marker: "Cargo.toml".into(),
            executable: resolve_executable("cargo")
                .unwrap_or_else(|| PathBuf::from("cargo"))
                .display()
                .to_string(),
            args: vec!["test".into()],
            parser: "exit-code".into(),
            structured: false,
            strict: false,
            available: resolve_executable("cargo").is_some(),
            recommended: false,
        });
    }

    if repo_root.join("pyproject.toml").is_file() || repo_root.join("pytest.ini").is_file() {
        profiles.push(DetectedProfile {
            key: "pytest".into(),
            label: "Pytest".into(),
            stack: "Python".into(),
            marker: if repo_root.join("pyproject.toml").is_file() {
                "pyproject.toml".into()
            } else {
                "pytest.ini".into()
            },
            executable: resolve_executable("pytest")
                .unwrap_or_else(|| PathBuf::from("pytest"))
                .display()
                .to_string(),
            args: vec!["-q".into(), "--junitxml=.testharbor/junit.xml".into()],
            parser: "junit-xml:.testharbor/junit.xml".into(),
            structured: true,
            strict: false,
            available: resolve_executable("pytest").is_some(),
            recommended: false,
        });
    }

    if repo_root.join("pom.xml").is_file() {
        profiles.push(DetectedProfile {
            key: "maven-test".into(),
            label: "Maven Test".into(),
            stack: "Java".into(),
            marker: "pom.xml".into(),
            executable: resolve_executable("mvn")
                .unwrap_or_else(|| PathBuf::from("mvn"))
                .display()
                .to_string(),
            args: vec!["test".into()],
            parser: "junit-xml:target/surefire-reports".into(),
            structured: true,
            strict: false,
            available: resolve_executable("mvn").is_some(),
            recommended: false,
        });
    }

    if repo_root.join("build.gradle").is_file() || repo_root.join("build.gradle.kts").is_file() {
        let gradlew = repo_root.join("gradlew");
        let (executable, available) = if gradlew.is_file() {
            (gradlew.display().to_string(), true)
        } else {
            let resolved = resolve_executable("gradle");
            (
                resolved
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("gradle"))
                    .display()
                    .to_string(),
                resolved.is_some(),
            )
        };
        profiles.push(DetectedProfile {
            key: "gradle-test".into(),
            label: "Gradle Test".into(),
            stack: "Gradle".into(),
            marker: if repo_root.join("build.gradle.kts").is_file() {
                "build.gradle.kts".into()
            } else {
                "build.gradle".into()
            },
            executable,
            args: vec!["test".into()],
            parser: "junit-xml:build/test-results/test".into(),
            structured: true,
            strict: false,
            available,
            recommended: false,
        });
    }

    if repo_root.join("package.json").is_file() {
        profiles.push(DetectedProfile {
            key: "npm-test".into(),
            label: "npm test".into(),
            stack: "Node.js".into(),
            marker: "package.json".into(),
            executable: resolve_executable("npm")
                .unwrap_or_else(|| PathBuf::from("npm"))
                .display()
                .to_string(),
            args: vec!["test".into(), "--".into(), "--runInBand".into()],
            parser: "exit-code".into(),
            structured: false,
            strict: false,
            available: resolve_executable("npm").is_some(),
            recommended: false,
        });
    }

    if repo_root.join("Makefile").is_file() {
        profiles.push(DetectedProfile {
            key: "make-test".into(),
            label: "make test".into(),
            stack: "Make".into(),
            marker: "Makefile".into(),
            executable: resolve_executable("make")
                .unwrap_or_else(|| PathBuf::from("make"))
                .display()
                .to_string(),
            args: vec!["test".into()],
            parser: "exit-code".into(),
            structured: false,
            strict: false,
            available: resolve_executable("make").is_some(),
            recommended: false,
        });
    }

    profiles
}

fn unique_stacks(profiles: &[DetectedProfile]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    for profile in profiles {
        seen.insert(profile.stack.clone());
    }
    seen.into_iter().collect()
}

fn count_test_files(repo_root: &Path) -> u32 {
    walk_repo_files(repo_root)
        .into_iter()
        .filter(|path| is_test_file(repo_root, path))
        .count() as u32
}

fn walk_repo_files(repo_root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![repo_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if matches!(
                    name.as_ref(),
                    ".git" | "node_modules" | "target" | "dist" | "build" | ".venv" | "venv"
                ) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }
    files
}

fn is_test_file(repo_root: &Path, file: &Path) -> bool {
    let rel = file.strip_prefix(repo_root).unwrap_or(file);
    let rel_str = rel.to_string_lossy();
    let name = file
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    name.ends_with("_test.go")
        || name.starts_with("test_") && name.ends_with(".py")
        || name.ends_with("_test.py")
        || name.ends_with(".test.js")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".test.jsx")
        || name.ends_with(".spec.js")
        || name.ends_with(".spec.ts")
        || name.ends_with(".spec.tsx")
        || name.ends_with(".spec.jsx")
        || name.ends_with("Test.java")
        || name.ends_with("Tests.java")
        || name.ends_with("_test.rs")
        || rel_str.starts_with("tests/")
        || rel_str.contains("/tests/")
        || rel_str.starts_with("src/test/")
}

fn create_placeholder_run_record(
    state: &AppState,
    project: &ProjectRecord,
    info: &GitRepoInfo,
) -> Result<CreatedRunSummary> {
    let now = Utc::now();
    let run_id = RunId::new();
    let pointers = RunPointers::default();
    let run = RunRecord {
        id: run_id,
        project_id: project.id,
        objective: String::new(),
        workflow_state: WorkflowState::Created,
        delivery_status: DeliveryStatus::NotDelivered,
        result_verdict: None,
        base_commit: info.head_commit.clone(),
        worktree_path: None,
        test_intent_hash: None,
        config_snapshot_hash: None,
        pointers_json: serde_json::to_string(&pointers).unwrap_or_else(|_| "{}".into()),
        snapshot_version: None,
        legacy_read_only: false,
        created_at: now,
        updated_at: now,
    };

    state.with_store(|store| {
        store.insert_run(&run)?;
        store.append_event(
            Some(run.id),
            Some(project.id),
            "run_created",
            &serde_json::json!({
                "base_commit": info.head_commit,
                "workflow_state": "Created",
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;

    Ok(CreatedRunSummary {
        run_id: run_id.to_string(),
        workflow_state: format!("{:?}", WorkflowState::Created),
        base_commit: info.head_commit.clone(),
        objective: String::new(),
    })
}

pub fn create_run(state: &AppState, project_id: &str) -> Result<CreatedRunSummary> {
    let project_id = uuid::Uuid::parse_str(project_id)
        .map(ProjectId)
        .map_err(|_| ProjectError::Other("invalid project id".into()))?;
    let project = state.with_store(|store| store.get_project(project_id))?;
    let info = git::inspect_repo(&project.root_path)?;
    if info.has_submodules {
        return Err(ProjectError::SubmodulesUnsupported);
    }
    create_placeholder_run_record(state, &project, &info)
}

pub fn list_recent_projects(state: &AppState, limit: usize) -> Result<Vec<ProjectSummary>> {
    let rows = state.with_store(|s| s.list_recent_projects(limit))?;
    Ok(rows
        .into_iter()
        .map(|p| ProjectSummary {
            id: p.id.to_string(),
            root_path: p.root_path,
            name: p.name,
            git_remote: p.git_remote,
            last_opened_at: p.last_opened_at.to_rfc3339(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use std::process::Command;

    fn init_repo(dir: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@example.com"])
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
    fn open_project_persists() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());

        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        assert!(!opened.repo.head_commit.is_empty());
        assert!(opened.created_run.is_some());

        std::fs::create_dir_all(repo.path().join(".testharbor/runs")).unwrap();
        std::fs::write(repo.path().join(".testharbor/runs/output.json"), "{}").unwrap();
        let status = Command::new("git")
            .args(["status", "--porcelain", "--", ".testharbor"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(status.status.success());
        assert!(status.stdout.is_empty());

        let recent = list_recent_projects(&state, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(
            recent[0].name,
            repo.path().file_name().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn inspect_project_reports_go_readiness() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        std::fs::write(repo.path().join("go.mod"), "module example.com/demo\n").unwrap();
        std::fs::write(
            repo.path().join("main_test.go"),
            "package main\n\nfunc TestDemo(t *testing.T) {}\n",
        )
        .unwrap();

        let inspection = inspect_project(repo.path());
        assert!(inspection
            .readiness
            .detected_stacks
            .iter()
            .any(|stack| stack == "Go"));
        assert!(inspection.readiness.test_file_count >= 1);
        assert!(inspection
            .readiness
            .profiles
            .iter()
            .any(|profile| profile.key == "go-test-json"));
    }

    #[test]
    fn inspect_project_reports_rust_run_only_profile() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        std::fs::write(
            repo.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.path().join("tests")).unwrap();
        std::fs::write(
            repo.path().join("tests").join("smoke.rs"),
            "#[test]\nfn smoke() {}\n",
        )
        .unwrap();

        let inspection = inspect_project(repo.path());
        assert!(inspection
            .readiness
            .detected_stacks
            .iter()
            .any(|stack| stack == "Rust"));
        assert!(inspection.readiness.test_file_count >= 1);
        let cargo = inspection
            .readiness
            .profiles
            .iter()
            .find(|profile| profile.key == "cargo-test")
            .expect("cargo profile should be present");
        assert_eq!(cargo.parser, "exit-code");
        assert!(!cargo.strict);
    }

    #[test]
    fn inspect_project_reports_pytest_junit_profile() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        std::fs::write(
            repo.path().join("pyproject.toml"),
            "[tool.pytest.ini_options]\n",
        )
        .unwrap();
        std::fs::write(
            repo.path().join("test_auth.py"),
            "def test_auth():\n    assert True\n",
        )
        .unwrap();

        let inspection = inspect_project(repo.path());
        let pytest = inspection
            .readiness
            .profiles
            .iter()
            .find(|profile| profile.key == "pytest")
            .expect("pytest profile should be present");
        assert_eq!(pytest.parser, "junit-xml:.testharbor/junit.xml");
        assert!(pytest.structured);
        assert!(!pytest.strict);
    }
}
