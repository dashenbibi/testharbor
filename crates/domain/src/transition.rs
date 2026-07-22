//! Workflow transition table (tech design §10.2).

use crate::state::WorkflowState;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Events that may drive a Run workflow transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowEvent {
    StartRunTrustCovered,
    StartRunTrustMissing,
    ConfirmProject,
    RejectProject,
    WorktreeReady,
    WorktreeFailed,
    AgentFinishedWithChanges,
    AgentNoChanges,
    AgentFailed,
    PolicyPassed,
    PolicyBlocked,
    RevisionFrozen,
    FreezeFailed,
    ValidationPassed,
    ValidationFailedAutoRevise,
    ValidationFailedNeedsHuman,
    AgentReviewReady,
    RequiredAgentReviewFailed,
    AddComment,
    HumanApprove,
    HumanRequestChanges,
    HumanRequestChangesBudgetExhausted,
    HumanReject,
    StartExecution,
    ProtectedScopeInvalidated,
    ProcessTerminated,
    ManifestFinalized,
    EvidenceFailed,
    AnalysisReady,
    AnalyzerFailed,
    AgentResultReviewReady,
    RequiredResultReviewFailed,
    HumanAcceptPass,
    HumanAcceptFail,
    HumanMarkProductBug,
    HumanMarkFlaky,
    HumanMarkEnvironmentIssue,
    HumanRerun,
    HumanRequestTestChange,
    HumanRequestTestChangeBudgetExhausted,
    ExportKeepReviewing,
    ExportAndComplete,
    EndWithoutDecision,
    Apply,
    Applied,
    ApplyFailedRecovered,
    ApplyFailedUnsafe,
    Discard,
    RetryBlockedStage,
    CreateSuccessorRun,
    EndRun,
    StopCurrentActivity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: WorkflowState,
    pub event: WorkflowEvent,
    pub to: WorkflowState,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransitionError {
    #[error("transition rejected: {from:?} + {event:?}")]
    Rejected {
        from: WorkflowState,
        event: WorkflowEvent,
    },
}

/// Pure transition function. Unlisted edges are rejected (INV: no silent skips).
pub fn try_transition(
    from: WorkflowState,
    event: WorkflowEvent,
) -> Result<Transition, TransitionError> {
    use WorkflowEvent as E;
    use WorkflowState as S;

    // Comments never change state.
    if matches!(event, E::AddComment) {
        return Ok(Transition {
            from,
            event,
            to: from,
        });
    }

    // Stop current activity always goes to NeedsHuman from active process states.
    if matches!(event, E::StopCurrentActivity) {
        return match from {
            S::GeneratingTests
            | S::NormalizingCandidate
            | S::ValidatingRevision
            | S::AgentReviewingTests
            | S::ExecutingTests
            | S::CollectingEvidence
            | S::AnalyzingResult
            | S::AgentReviewingResult => Ok(Transition {
                from,
                event,
                to: S::NeedsHuman,
            }),
            _ => Err(TransitionError::Rejected { from, event }),
        };
    }

    // EndRun: any non-terminal except ApplyingRevision without recovery path.
    if matches!(event, E::EndRun) {
        return match from {
            S::ApplyingRevision => Err(TransitionError::Rejected { from, event }),
            s if s.is_terminal() => Err(TransitionError::Rejected { from, event }),
            _ => Ok(Transition {
                from,
                event,
                to: S::Cancelled,
            }),
        };
    }

    let to = match (from, &event) {
        (S::Created, E::StartRunTrustCovered) => S::PreparingWorktree,
        (S::Created, E::StartRunTrustMissing) => S::WaitingProjectConfirmation,
        (S::WaitingProjectConfirmation, E::ConfirmProject) => S::PreparingWorktree,
        (S::WaitingProjectConfirmation, E::RejectProject) => S::Rejected,
        (S::PreparingWorktree, E::WorktreeReady) => S::GeneratingTests,
        (S::PreparingWorktree, E::WorktreeFailed) => S::NeedsHuman,
        (S::GeneratingTests, E::AgentFinishedWithChanges) => S::NormalizingCandidate,
        (S::GeneratingTests, E::AgentNoChanges) => S::NeedsHuman,
        (S::GeneratingTests, E::AgentFailed) => S::NeedsHuman,
        (S::NormalizingCandidate, E::PolicyPassed) => S::FreezingRevision,
        (S::NormalizingCandidate, E::PolicyBlocked) => S::NeedsHuman,
        (S::FreezingRevision, E::RevisionFrozen) => S::ValidatingRevision,
        (S::FreezingRevision, E::FreezeFailed) => S::NeedsHuman,
        (S::ValidatingRevision, E::ValidationPassed) => S::AgentReviewingTests,
        (S::ValidatingRevision, E::ValidationFailedAutoRevise) => S::GeneratingTests,
        (S::ValidatingRevision, E::ValidationFailedNeedsHuman) => S::NeedsHuman,
        (S::AgentReviewingTests, E::AgentReviewReady) => S::PendingTestReview,
        (S::AgentReviewingTests, E::RequiredAgentReviewFailed | E::AgentFailed) => S::NeedsHuman,
        (S::PendingTestReview, E::HumanApprove) => S::ExecutionReady,
        (S::PendingTestReview, E::HumanRequestChanges) => S::GeneratingTests,
        (S::PendingTestReview, E::HumanRequestChangesBudgetExhausted) => S::NeedsHuman,
        (S::PendingTestReview, E::HumanReject) => S::Rejected,
        (S::ExecutionReady, E::StartExecution) => S::ExecutingTests,
        (S::ExecutionReady, E::ProtectedScopeInvalidated) => S::NeedsHuman,
        (S::ExecutingTests, E::ProcessTerminated) => S::CollectingEvidence,
        (S::CollectingEvidence, E::ManifestFinalized) => S::AnalyzingResult,
        (S::CollectingEvidence, E::EvidenceFailed) => S::NeedsHuman,
        (S::AnalyzingResult, E::AnalysisReady) => S::AgentReviewingResult,
        (S::AnalyzingResult, E::AnalyzerFailed | E::AgentFailed) => S::NeedsHuman,
        (S::AgentReviewingResult, E::AgentResultReviewReady) => S::PendingResultReview,
        (S::AgentReviewingResult, E::RequiredResultReviewFailed | E::AgentFailed) => S::NeedsHuman,
        (S::PendingResultReview, E::HumanAcceptPass) => S::ReadyToApply,
        (S::PendingResultReview, E::HumanAcceptFail) => S::Completed,
        (S::PendingResultReview, E::HumanMarkProductBug) => S::Completed,
        (S::PendingResultReview, E::HumanMarkFlaky) => S::Completed,
        (S::PendingResultReview, E::HumanMarkEnvironmentIssue) => S::NeedsHuman,
        (S::PendingResultReview, E::HumanRerun) => S::ExecutionReady,
        (S::PendingResultReview, E::HumanRequestTestChange) => S::GeneratingTests,
        (S::PendingResultReview, E::HumanRequestTestChangeBudgetExhausted) => S::NeedsHuman,
        (S::PendingResultReview, E::ExportKeepReviewing) => S::PendingResultReview,
        (S::PendingResultReview, E::EndWithoutDecision) => S::Cancelled,
        (S::PendingResultReview, E::ProtectedScopeInvalidated) => S::NeedsHuman,
        (S::ReadyToApply, E::Apply) => S::ApplyingRevision,
        (S::ReadyToApply, E::HumanRerun) => S::ExecutionReady,
        (S::ReadyToApply, E::ExportAndComplete) => S::Completed,
        (S::ReadyToApply, E::Discard) => S::Completed,
        (S::ReadyToApply, E::ProtectedScopeInvalidated) => S::NeedsHuman,
        (S::ApplyingRevision, E::Applied) => S::Completed,
        (S::ApplyingRevision, E::ApplyFailedRecovered) => S::ReadyToApply,
        (S::ApplyingRevision, E::ApplyFailedUnsafe) => S::NeedsHuman,
        (S::NeedsHuman, E::RetryBlockedStage) => {
            // Resume target is application-provided; domain allows staying NeedsHuman
            // only if caller remaps. For pure table, RetryBlockedStage requires external to_state.
            // Use a dedicated API for resume; reject bare event here.
            return Err(TransitionError::Rejected { from, event });
        }
        (S::NeedsHuman, E::ExportAndComplete) => S::Completed,
        (S::NeedsHuman, E::CreateSuccessorRun) => S::Superseded,
        _ => {
            return Err(TransitionError::Rejected { from, event });
        }
    };

    Ok(Transition { from, event, to })
}

/// Resume from NeedsHuman to a recorded target (application supplies target).
pub fn try_resume(
    from: WorkflowState,
    target: WorkflowState,
) -> Result<Transition, TransitionError> {
    if from != WorkflowState::NeedsHuman {
        return Err(TransitionError::Rejected {
            from,
            event: WorkflowEvent::RetryBlockedStage,
        });
    }
    if target.is_terminal() || target == WorkflowState::NeedsHuman {
        return Err(TransitionError::Rejected {
            from,
            event: WorkflowEvent::RetryBlockedStage,
        });
    }
    Ok(Transition {
        from,
        event: WorkflowEvent::RetryBlockedStage,
        to: target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_path_created_to_ready() {
        let steps = [
            (
                WorkflowState::Created,
                WorkflowEvent::StartRunTrustCovered,
                WorkflowState::PreparingWorktree,
            ),
            (
                WorkflowState::PreparingWorktree,
                WorkflowEvent::WorktreeReady,
                WorkflowState::GeneratingTests,
            ),
            (
                WorkflowState::GeneratingTests,
                WorkflowEvent::AgentFinishedWithChanges,
                WorkflowState::NormalizingCandidate,
            ),
            (
                WorkflowState::NormalizingCandidate,
                WorkflowEvent::PolicyPassed,
                WorkflowState::FreezingRevision,
            ),
            (
                WorkflowState::FreezingRevision,
                WorkflowEvent::RevisionFrozen,
                WorkflowState::ValidatingRevision,
            ),
            (
                WorkflowState::ValidatingRevision,
                WorkflowEvent::ValidationPassed,
                WorkflowState::AgentReviewingTests,
            ),
            (
                WorkflowState::AgentReviewingTests,
                WorkflowEvent::AgentReviewReady,
                WorkflowState::PendingTestReview,
            ),
            (
                WorkflowState::PendingTestReview,
                WorkflowEvent::HumanApprove,
                WorkflowState::ExecutionReady,
            ),
        ];
        for (from, event, expected) in steps {
            let t = try_transition(from, event).unwrap();
            assert_eq!(t.to, expected, "{from:?} + {event:?}");
        }
    }

    #[test]
    fn comment_does_not_change_state() {
        let t =
            try_transition(WorkflowState::PendingTestReview, WorkflowEvent::AddComment).unwrap();
        assert_eq!(t.to, WorkflowState::PendingTestReview);
    }

    #[test]
    fn reject_unlisted_edge() {
        let err =
            try_transition(WorkflowState::Created, WorkflowEvent::HumanAcceptPass).unwrap_err();
        assert!(matches!(err, TransitionError::Rejected { .. }));
    }

    #[test]
    fn cannot_end_during_applying() {
        assert!(try_transition(WorkflowState::ApplyingRevision, WorkflowEvent::EndRun).is_err());
    }

    #[test]
    fn end_without_decision_cancels() {
        let t = try_transition(
            WorkflowState::PendingResultReview,
            WorkflowEvent::EndWithoutDecision,
        )
        .unwrap();
        assert_eq!(t.to, WorkflowState::Cancelled);
    }

    #[test]
    fn accept_pass_to_ready_to_apply() {
        let t = try_transition(
            WorkflowState::PendingResultReview,
            WorkflowEvent::HumanAcceptPass,
        )
        .unwrap();
        assert_eq!(t.to, WorkflowState::ReadyToApply);
    }

    #[test]
    fn stop_from_generating() {
        let t = try_transition(
            WorkflowState::GeneratingTests,
            WorkflowEvent::StopCurrentActivity,
        )
        .unwrap();
        assert_eq!(t.to, WorkflowState::NeedsHuman);
    }

    #[test]
    fn resume_from_needs_human() {
        let t = try_resume(WorkflowState::NeedsHuman, WorkflowState::GeneratingTests).unwrap();
        assert_eq!(t.to, WorkflowState::GeneratingTests);
    }
}
