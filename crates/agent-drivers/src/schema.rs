//! Minimal JSON shapes for agent final messages.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFinalEnvelope<T> {
    pub schema_version: u32,
    pub task_kind: String,
    pub test_intent_hash: String,
    pub scope_hash: String,
    pub evidence_refs: Vec<String>,
    pub status: String,
    pub summary: String,
    pub payload: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratePayload {
    pub written_files: Vec<String>,
    pub proposed_intended_names: Vec<String>,
    pub notes: Vec<String>,
    /// The repository-native test runner selected after bounded inspection.
    #[serde(default)]
    pub test_runner: Option<crate::AgentTestRunner>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPayload {
    pub recommendation: String,
    pub findings: Vec<crate::AgentFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisPayload {
    /// TEST_BUG | PRODUCT_BUG | ENVIRONMENT_ERROR | FLAKY | UNKNOWN | PASS
    pub suggested_verdict: String,
    pub confidence: String,
    pub evidence_notes: Vec<String>,
    pub findings: Vec<crate::AgentFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultReviewPayload {
    /// ACCEPT_PASS | ACCEPT_FAIL | RERUN | CHANGE_TEST | NEEDS_HUMAN
    pub recommendation: String,
    /// Whether Analyzer appears over-inferred
    pub analyzer_overreach: bool,
    pub findings: Vec<crate::AgentFinding>,
}

pub const SCHEMA_VERSION: u32 = 1;
