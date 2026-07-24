//! Application services. Tauri commands call into this layer only.

mod actions;
mod app_state;
mod execution;
mod generate;
mod history;
mod projects;
mod result_review;
mod workspace;

pub(crate) fn deferred_runner_hash() -> String {
    testharbor_domain::canonical_hash(&["agent-selected-runner"])
}

pub(crate) fn is_deferred_runner_hash(value: &str) -> bool {
    value == deferred_runner_hash()
}

pub use actions::{end_run, get_run_actions, stop_current_activity, ActivityActionResult};
pub use app_state::{AppState, SharedAppState};
pub use execution::{
    cancel_execution, collect_inventory, confirm_custom_command, confirm_profile,
    get_last_execution_result, poll_execution, poll_execution_output, probe_profile_readiness,
    reevaluate_accept_pass, set_execution_timeout, set_intended_tests, start_test_execution,
    suggest_profile, AcceptPassView, ConfirmedTrust, ExecutionPollResult, InventoryView,
    LastExecutionRecord, ProfileProbeResult, ProfileProbeStage, ProfileSuggestion,
    StartExecutionResult,
};
pub use generate::{
    approve_test_case, cancel_generation, comment_case_review, generate_and_review,
    get_case_review, get_case_review_detail, get_generation_progress, reject_test_case,
    request_case_changes, retry_generation, retry_generation_with_options, set_agent_model,
    set_finding_disposition, ApproveCaseResult, CaseReviewDetail, CommentCaseResult, FileDiffSlice,
    GenerateTestsRequest, GenerateTestsResult, GenerationProgress, RejectCaseResult,
    SpecAttachmentInput, TestEnvironmentConfig, TestEnvironmentInput,
};
pub use history::{
    cleanup_storage, delete_run, disk_usage, get_run_audit_history, get_run_detail,
    list_run_events, list_run_history, recover_on_startup, CleanupResult, DeleteRunResult,
    DiskUsage, RecoveryReport, RunAuditHistory, RunEventItem, RunFilter, RunHistoryItem,
};
pub(crate) use projects::inspect_project;
pub use projects::{
    create_run, list_recent_projects, open_project, probe_path, CreatedRunSummary,
    OpenProjectResult, ProbeResult, ProjectSummary,
};
pub use result_review::{
    apply_guard_status, apply_patch, create_successor_run, decide_accept_fail, decide_accept_pass,
    decide_end_without_decision, decide_mark_environment, decide_mark_flaky,
    decide_mark_product_bug, discard_delivery, export_patch, prepare_execution_rerun,
    request_result_test_change, ApplyGuardView, ApplyPatchResult, ExportPatchResult,
    ResultDecisionResult, SuccessorRunResult,
};
pub use testharbor_agent_drivers::{CliModelCatalog, GrokModelCatalog};
pub use testharbor_domain::FindingDisposition;
pub use testharbor_result_normalizer::{
    IntendedTestManifest, NormalizedStructuredResult, TestIdentity, TestInventory,
};
pub use testharbor_review::{CaseApprovalRecord, CaseReviewState, ReviewFinding};
pub use testharbor_trust::{CommandIdentity, TestProfileDraft, TrustGrant};
pub use testharbor_workspace_manager::CandidateRevision;
pub use workspace::{
    discard_run_worktree, freeze_worktree_candidate, get_worktree_status, prepare_worktree,
    write_in_worktree, PrepareWorktreeResult, WorktreeStatus,
};

use serde::{Deserialize, Serialize};
use testharbor_domain::WorkflowState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub domain_ready: bool,
    pub sample_workflow_state: String,
    pub data_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfoExt {
    pub info: AppInfo,
    pub last_recovery: Option<RecoveryReport>,
    pub ui_preferences: UiPreferences,
    pub agent_mode_default: String,
    pub agent_preferences: AgentPreferences,
    pub codex: CodexEnvironment,
    pub claude: ClaudeEnvironment,
    pub grok: GrokEnvironment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiPreferences {
    pub language: String,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            language: "en".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPreferences {
    pub enabled_modes: Vec<String>,
}

impl Default for AgentPreferences {
    fn default() -> Self {
        Self {
            enabled_modes: vec!["codex".into(), "claude".into(), "grok".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexEnvironment {
    pub ready: bool,
    pub version: Option<String>,
    pub authenticated: bool,
    pub auth_status: Option<String>,
    pub has_output_schema: bool,
    pub has_ephemeral: bool,
    pub has_sandbox: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeEnvironment {
    pub ready: bool,
    pub version: Option<String>,
    pub authenticated: bool,
    pub auth_status: Option<String>,
    pub has_print: bool,
    pub has_stream_json: bool,
    pub has_json_schema: bool,
    pub has_resume: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrokEnvironment {
    pub ready: bool,
    pub version: Option<String>,
    pub authenticated: bool,
    pub auth_status: Option<String>,
    pub has_headless: bool,
    pub has_json: bool,
    pub has_resume: bool,
    pub error: Option<String>,
}

pub fn list_grok_models() -> Result<GrokModelCatalog, String> {
    testharbor_agent_drivers::list_grok_models().map_err(|error| error.to_string())
}

pub fn list_codex_models() -> Result<CliModelCatalog, String> {
    testharbor_agent_drivers::list_codex_models().map_err(|error| error.to_string())
}

pub fn list_claude_models() -> Result<CliModelCatalog, String> {
    testharbor_agent_drivers::list_claude_models().map_err(|error| error.to_string())
}

pub fn app_info(state: Option<&AppState>) -> AppInfo {
    AppInfo {
        name: "TestHarbor".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        domain_ready: true,
        sample_workflow_state: format!("{:?}", WorkflowState::Created),
        data_dir: state.map(|s| s.data_dir().display().to_string()),
    }
}

pub fn app_info_ext(state: &AppState) -> AppInfoExt {
    let codex = match testharbor_agent_drivers::probe_codex() {
        Ok(capabilities) => {
            let ready = capabilities.has_exec
                && capabilities.has_output_schema
                && capabilities.has_ephemeral
                && capabilities.has_sandbox
                && capabilities.authenticated;
            CodexEnvironment {
                ready,
                version: Some(capabilities.version),
                authenticated: capabilities.authenticated,
                auth_status: Some(capabilities.auth_status),
                has_output_schema: capabilities.has_output_schema,
                has_ephemeral: capabilities.has_ephemeral,
                has_sandbox: capabilities.has_sandbox,
                error: if ready {
                    None
                } else {
                    Some("当前 Codex CLI 缺少 TestHarbor 所需能力，请升级 Codex CLI 后重试".into())
                },
            }
        }
        Err(error) => CodexEnvironment {
            ready: false,
            version: None,
            authenticated: false,
            auth_status: None,
            has_output_schema: false,
            has_ephemeral: false,
            has_sandbox: false,
            error: Some(error.to_string()),
        },
    };
    let claude = match testharbor_agent_drivers::probe_claude() {
        Ok(capabilities) => {
            let ready = capabilities.has_print
                && capabilities.has_stream_json
                && capabilities.has_json_schema
                && capabilities.has_resume
                && capabilities.authenticated;
            ClaudeEnvironment {
                ready,
                version: Some(capabilities.version),
                authenticated: capabilities.authenticated,
                auth_status: Some(capabilities.auth_status),
                has_print: capabilities.has_print,
                has_stream_json: capabilities.has_stream_json,
                has_json_schema: capabilities.has_json_schema,
                has_resume: capabilities.has_resume,
                error: if ready {
                    None
                } else {
                    Some("当前 Claude Code CLI 尚未登录或缺少 TestHarbor 所需能力".into())
                },
            }
        }
        Err(error) => ClaudeEnvironment {
            ready: false,
            version: None,
            authenticated: false,
            auth_status: None,
            has_print: false,
            has_stream_json: false,
            has_json_schema: false,
            has_resume: false,
            error: Some(error.to_string()),
        },
    };
    let grok = match testharbor_agent_drivers::probe_grok() {
        Ok(capabilities) => {
            let ready = capabilities.has_headless
                && capabilities.has_json
                && capabilities.has_resume
                && capabilities.authenticated;
            GrokEnvironment {
                ready,
                version: Some(capabilities.version),
                authenticated: capabilities.authenticated,
                auth_status: Some(capabilities.auth_status),
                has_headless: capabilities.has_headless,
                has_json: capabilities.has_json,
                has_resume: capabilities.has_resume,
                error: if ready {
                    None
                } else {
                    Some("当前 Grok CLI 尚未登录或缺少 headless JSON/resume 能力".into())
                },
            }
        }
        Err(error) => GrokEnvironment {
            ready: false,
            version: None,
            authenticated: false,
            auth_status: None,
            has_headless: false,
            has_json: false,
            has_resume: false,
            error: Some(error.to_string()),
        },
    };
    let agent_mode_default = std::env::var("TESTHARBOR_AGENT_MODE")
        .ok()
        .filter(|mode| mode.eq_ignore_ascii_case("grok") || mode.eq_ignore_ascii_case("claude"))
        .unwrap_or_else(|| "codex".into());
    AppInfoExt {
        info: app_info(Some(state)),
        last_recovery: state.get_last_recovery(),
        ui_preferences: state.get_ui_preferences(),
        agent_mode_default,
        agent_preferences: state.get_agent_preferences(),
        codex,
        claude,
        grok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_info_reports_domain() {
        let info = app_info(None);
        assert!(info.domain_ready);
        assert_eq!(info.name, "TestHarbor");
    }
}
