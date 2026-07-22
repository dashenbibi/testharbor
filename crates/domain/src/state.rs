//! Workflow and result state dimensions (tech design §10.1).

use serde::{Deserialize, Serialize};

/// Primary workflow phase for a Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    Created,
    WaitingProjectConfirmation,
    PreparingWorktree,
    GeneratingTests,
    NormalizingCandidate,
    FreezingRevision,
    ValidatingRevision,
    AgentReviewingTests,
    PendingTestReview,
    NeedsHuman,
    ExecutionReady,
    ExecutingTests,
    CollectingEvidence,
    AnalyzingResult,
    AgentReviewingResult,
    PendingResultReview,
    ReadyToApply,
    ApplyingRevision,
    Completed,
    Superseded,
    Rejected,
    Failed,
    Cancelled,
}

impl WorkflowState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Superseded | Self::Rejected | Self::Failed | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Starting,
    Running,
    Exited,
    TimedOut,
    Cancelled,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultCompleteness {
    Complete,
    Partial,
    Missing,
    Opaque,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultConsistency {
    Consistent,
    Contradictory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultVerdict {
    Pass,
    TestBug,
    ProductBug,
    Flaky,
    Environment,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    NotDelivered,
    Exported,
    Applied,
    Discarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionStatus {
    /// Frozen candidate, validation not yet passed.
    Frozen,
    /// Validation passed; may enter review.
    Validated,
    /// Policy/validation failed; cannot approve.
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessOutcomeKind {
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    SpawnFailed,
    Lost,
}
