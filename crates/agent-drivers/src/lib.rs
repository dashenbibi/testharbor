//! Agent drivers: Codex/Claude/Grok CLI for production and deterministic mock helpers for tests.

mod codex;
mod mock;
mod schema;

pub use codex::cancel_generation;
pub use codex::{
    claude_analyze, claude_generate, claude_review, claude_review_result, list_claude_models,
    list_codex_models, list_grok_models, probe_claude, probe_codex, probe_grok, ClaudeCapabilities,
    CliModelCatalog, CodexCapabilities, CodexError, GrokCapabilities, GrokModelCatalog,
};
pub use mock::{mock_analyze_result, mock_generate, mock_review_result, mock_review_tests};
pub use schema::*;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error(transparent)]
    Codex(#[from] CodexError),
    #[error("agent output invalid: {0}")]
    OutputInvalid(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AgentError>;

/// Runtime target metadata supplied to a generator. Secret values are never
/// interpolated into prompts; only variable names are described to the Agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestEnvironment {
    /// Deprecated compatibility field. It is not included in Agent prompts.
    #[serde(default)]
    pub context: String,
    pub base_url: String,
    #[serde(default)]
    pub health_url: Option<String>,
    #[serde(default)]
    pub login_url: Option<String>,
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

/// Returns true for common provider errors that mean a resumed session is
/// unavailable and a fresh session may be attempted.
pub fn is_session_resume_error(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("session")
        && (message.contains("not found")
            || message.contains("no conversation found")
            || message.contains("unknown")
            || message.contains("invalid")
            || message.contains("expired")
            || message.contains("resume"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    /// Deterministic offline generator/reviewer. Internal tests must opt in explicitly.
    Mock,
    /// Real Codex CLI. This is the production default.
    Codex,
    /// Real Claude Code CLI.
    Claude,
    /// Real Grok CLI.
    Grok,
}

impl AgentMode {
    pub fn from_env() -> Self {
        match std::env::var("TESTHARBOR_AGENT_MODE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "mock" => AgentMode::Mock,
            "claude" => AgentMode::Claude,
            "grok" => AgentMode::Grok,
            _ => AgentMode::Codex,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateRequest {
    /// TestHarbor Run id used to cancel the active Codex process.
    #[serde(default)]
    pub run_id: Option<String>,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    /// Generation strategy selected by the user, e.g. unit, integration, e2e_api_flow.
    #[serde(default)]
    pub test_strategy: Option<String>,
    /// Whether generated e2e/integration tests should clean their own test data.
    #[serde(default)]
    pub cleanup_test_data: Option<bool>,
    /// Human feedback from the previous reviewed revision, when regenerating.
    #[serde(default)]
    pub change_request: Option<String>,
    pub test_intent_hash: String,
    pub scope_hash: String,
    pub evidence_refs: Vec<String>,
    pub worktree_path: String,
    pub base_commit: String,
    pub language_hint: String,
    /// Directory for live stdout/stderr logs. Production supplies this per Run.
    #[serde(default)]
    pub progress_dir: Option<String>,
    /// Codex CLI session/thread id to resume for the generation conversation.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional provider model. The selected CLI default is used when omitted.
    #[serde(default)]
    pub agent_model: Option<String>,
    /// Runtime-only target context. Secret values are passed as process env,
    /// never interpolated into the prompt.
    #[serde(default)]
    pub environment: Option<TestEnvironment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateResult {
    pub mode: AgentMode,
    pub summary: String,
    pub written_files: Vec<String>,
    pub proposed_intended_names: Vec<String>,
    /// Runner selected by the Agent after inspecting the repository. The
    /// application validates and binds it to Trust before execution.
    #[serde(default)]
    pub test_runner: Option<AgentTestRunner>,
    pub raw_path: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTestRunner {
    /// Executable name or repository-relative executable path.
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Repository-relative working directory; defaults to the repository root.
    #[serde(default)]
    pub cwd: Option<String>,
    /// exit-code, go-test-json, or junit-xml:<path>.
    pub parser: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    /// TestHarbor Run id used to cancel the active Codex process.
    #[serde(default)]
    pub run_id: Option<String>,
    pub test_intent_hash: String,
    pub scope_hash: String,
    pub evidence_refs: Vec<String>,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub changed_paths: Vec<String>,
    pub patch_excerpt: String,
    #[serde(default)]
    pub change_request: Option<String>,
    pub bundle_dir: String,
    /// Directory for live stdout/stderr logs. Review appends to generation logs.
    #[serde(default)]
    pub progress_dir: Option<String>,
    /// Optional session to resume when the caller explicitly enables session reuse.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional provider model. The selected CLI default is used when omitted.
    #[serde(default)]
    pub agent_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFinding {
    pub severity: String,
    pub category: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewResult {
    pub mode: AgentMode,
    pub recommendation: String,
    pub summary: String,
    pub findings: Vec<AgentFinding>,
    pub test_intent_hash: String,
}

/// Input for Analyzer Agent (failure classification).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeRequest {
    /// Optional session to resume when the caller explicitly enables session reuse.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional provider model. The selected CLI default is used when omitted.
    #[serde(default)]
    pub agent_model: Option<String>,
    pub test_intent_hash: String,
    pub scope_hash: String,
    pub evidence_refs: Vec<String>,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub exit_code: Option<i32>,
    pub parser: String,
    pub completeness: String,
    pub consistency: String,
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub empty: bool,
    pub intended_count: u32,
    pub intended_observed: u32,
    pub intended_failed: u32,
    pub intended_missing: u32,
    pub stdout_excerpt: String,
    pub stderr_excerpt: String,
    pub failure_phase: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeResult {
    pub mode: AgentMode,
    pub suggested_verdict: String,
    pub confidence: String,
    pub summary: String,
    pub evidence_notes: Vec<String>,
    pub findings: Vec<AgentFinding>,
    pub test_intent_hash: String,
}

/// Input for Result Reviewer (independent check of analyzer + evidence).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultReviewRequest {
    /// Optional provider model. The selected CLI default is used when omitted.
    #[serde(default)]
    pub agent_model: Option<String>,
    pub test_intent_hash: String,
    pub scope_hash: String,
    pub evidence_refs: Vec<String>,
    pub objective: String,
    pub analyzer_verdict: String,
    pub analyzer_summary: String,
    pub analyzer_confidence: String,
    pub exit_code: Option<i32>,
    pub completeness: String,
    pub consistency: String,
    pub total: u32,
    pub failed: u32,
    pub empty: bool,
    pub intended_observed: u32,
    pub intended_count: u32,
    pub accept_pass_enabled: bool,
    pub accept_pass_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultReviewAgentResult {
    pub mode: AgentMode,
    pub recommendation: String,
    pub analyzer_overreach: bool,
    pub summary: String,
    pub findings: Vec<AgentFinding>,
    pub test_intent_hash: String,
}

pub fn generate(mode: AgentMode, req: &GenerateRequest) -> Result<GenerateResult> {
    match mode {
        AgentMode::Mock => mock_generate(req),
        AgentMode::Codex => codex::codex_generate(req),
        AgentMode::Claude => codex::claude_generate(req),
        AgentMode::Grok => codex::grok_generate(req),
    }
}

pub fn review_tests(mode: AgentMode, req: &ReviewRequest) -> Result<ReviewResult> {
    match mode {
        AgentMode::Mock => mock_review_tests(req),
        AgentMode::Codex => codex::codex_review(req),
        AgentMode::Claude => codex::claude_review(req),
        AgentMode::Grok => codex::grok_review(req),
    }
}

pub fn analyze_result(mode: AgentMode, req: &AnalyzeRequest) -> Result<AnalyzeResult> {
    match mode {
        AgentMode::Mock => mock_analyze_result(req),
        AgentMode::Codex => codex::codex_analyze(req),
        AgentMode::Claude => codex::claude_analyze(req),
        AgentMode::Grok => codex::grok_analyze(req),
    }
}

pub fn review_result(
    mode: AgentMode,
    req: &ResultReviewRequest,
) -> Result<ResultReviewAgentResult> {
    match mode {
        AgentMode::Mock => mock_review_result(req),
        AgentMode::Codex => codex::codex_review_result(req),
        AgentMode::Claude => codex::claude_review_result(req),
        AgentMode::Grok => codex::grok_review_result(req),
    }
}
