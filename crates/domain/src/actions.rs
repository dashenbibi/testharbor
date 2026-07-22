//! User action descriptors for UI (tech design §11.5).

use crate::guards::GuardReason;
use crate::state::WorkflowState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserAction {
    Approve,
    ApproveAndRun,
    RequestChanges,
    Reject,
    AcceptPass,
    AcceptFail,
    Rerun,
    RequestTestChange,
    MarkFlaky,
    MarkProductBug,
    MarkEnvironmentIssue,
    Apply,
    Export,
    Discard,
    StopCurrentActivity,
    EndRun,
    EndWithoutDecision,
    Retry,
    CreateSuccessorRun,
    StartExecution,
    ConfirmProject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserActionDescriptor {
    pub action: UserAction,
    pub label: String,
    pub enabled: bool,
    pub disabled_reasons: Vec<GuardReason>,
    pub confirmation_summary: String,
    pub next_state: Option<WorkflowState>,
    #[serde(default)]
    pub invalidates: Vec<String>,
    #[serde(default)]
    pub preserves: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub destructive: bool,
}

impl UserActionDescriptor {
    pub fn enabled(
        action: UserAction,
        label: impl Into<String>,
        next: Option<WorkflowState>,
    ) -> Self {
        Self {
            action,
            label: label.into(),
            enabled: true,
            disabled_reasons: vec![],
            confirmation_summary: String::new(),
            next_state: next,
            invalidates: vec![],
            preserves: vec![],
            evidence_refs: vec![],
            destructive: false,
        }
    }

    pub fn disabled(
        action: UserAction,
        label: impl Into<String>,
        reasons: Vec<GuardReason>,
    ) -> Self {
        Self {
            action,
            label: label.into(),
            enabled: false,
            disabled_reasons: reasons,
            confirmation_summary: String::new(),
            next_state: None,
            invalidates: vec![],
            preserves: vec![],
            evidence_refs: vec![],
            destructive: false,
        }
    }
}
