//! Pure decision guards (tech design §11).

use crate::decisions::{FindingDisposition, HumanResultDecision, HumanTestDecision};
use crate::state::{
    ResultCompleteness, ResultConsistency, ResultVerdict, RevisionStatus, WorkflowState,
};
use serde::{Deserialize, Serialize};

/// Stable machine-readable guard failure reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GuardReason {
    WrongWorkflowState {
        expected: String,
        actual: String,
    },
    RevisionNotCurrent,
    RevisionNotValidated,
    CandidateTreeMismatch,
    ScopeHashMismatch,
    ApprovalContextMismatch,
    PolicyBlocked,
    UnresolvedMustFix,
    AcceptedRiskMissingReason,
    TrustGrantInvalid,
    ActiveExecutionExists,
    NotApprovedRevision,
    LatestDecisionNotApprove,
    ExecutionNotCurrent,
    ExecutionNotExited,
    ProcessNotSucceeded,
    ResultNotComplete,
    ResultOpaque,
    ResultContradictory,
    ManifestHashMismatch,
    IntendedCountZero,
    IntendedFailed,
    IntendedNotObserved,
    UnexplainedSkipped,
    RegressionFailed {
        total_failed: u32,
        total_errors: u32,
    },
    IncompleteSuites,
    MainWorktreeDirty,
    HeadNotBaseCommit,
    LatestDecisionNotAcceptPass,
    MissingAcceptedExecution,
    IntentNotCreatable,
    RequestChangesEmpty,
    FlakyEvidenceInsufficient,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardResult {
    pub ok: bool,
    pub reasons: Vec<GuardReason>,
}

impl GuardResult {
    pub fn pass() -> Self {
        Self {
            ok: true,
            reasons: vec![],
        }
    }

    pub fn fail(reason: GuardReason) -> Self {
        Self {
            ok: false,
            reasons: vec![reason],
        }
    }

    pub fn fail_many(reasons: Vec<GuardReason>) -> Self {
        Self { ok: false, reasons }
    }

    pub fn merge(mut self, other: GuardResult) -> Self {
        if !other.ok {
            self.ok = false;
            self.reasons.extend(other.reasons);
        }
        self
    }
}

/// Inputs for test-case approval (pure).
#[derive(Debug, Clone)]
pub struct ApproveTestContext {
    pub workflow_state: WorkflowState,
    pub current_revision_id: String,
    pub revision_id: String,
    pub revision_status: RevisionStatus,
    pub candidate_tree: String,
    pub recomputed_candidate_tree: String,
    pub test_case_scope_hash: String,
    pub recomputed_test_case_scope_hash: String,
    pub has_blocking_policy_finding: bool,
    pub unresolved_must_fix: bool,
    pub accepted_risk_missing_reason: bool,
    pub trust_grant_valid: bool,
}

pub fn can_approve_test_revision(ctx: &ApproveTestContext) -> GuardResult {
    let mut reasons = Vec::new();

    if ctx.workflow_state != WorkflowState::PendingTestReview {
        reasons.push(GuardReason::WrongWorkflowState {
            expected: "pending_test_review".into(),
            actual: format!("{:?}", ctx.workflow_state),
        });
    }
    if ctx.revision_id != ctx.current_revision_id {
        reasons.push(GuardReason::RevisionNotCurrent);
    }
    if ctx.revision_status != RevisionStatus::Validated {
        reasons.push(GuardReason::RevisionNotValidated);
    }
    if ctx.candidate_tree != ctx.recomputed_candidate_tree {
        reasons.push(GuardReason::CandidateTreeMismatch);
    }
    if ctx.test_case_scope_hash != ctx.recomputed_test_case_scope_hash {
        reasons.push(GuardReason::ScopeHashMismatch);
    }
    if ctx.has_blocking_policy_finding {
        reasons.push(GuardReason::PolicyBlocked);
    }
    if ctx.unresolved_must_fix {
        reasons.push(GuardReason::UnresolvedMustFix);
    }
    if ctx.accepted_risk_missing_reason {
        reasons.push(GuardReason::AcceptedRiskMissingReason);
    }
    if !ctx.trust_grant_valid {
        reasons.push(GuardReason::TrustGrantInvalid);
    }

    if reasons.is_empty() {
        GuardResult::pass()
    } else {
        GuardResult::fail_many(reasons)
    }
}

#[derive(Debug, Clone)]
pub struct StartExecutionContext {
    pub workflow_state: WorkflowState,
    pub approved_revision_id: Option<String>,
    pub current_revision_id: String,
    pub latest_human_test_decision: Option<HumanTestDecision>,
    pub approval_context_hash: String,
    pub recomputed_approval_context_hash: String,
    pub candidate_tree: String,
    pub recomputed_candidate_tree: String,
    pub trust_grant_valid: bool,
    pub has_active_execution: bool,
}

pub fn can_start_execution(ctx: &StartExecutionContext) -> GuardResult {
    let mut reasons = Vec::new();

    if ctx.workflow_state != WorkflowState::ExecutionReady {
        reasons.push(GuardReason::WrongWorkflowState {
            expected: "execution_ready".into(),
            actual: format!("{:?}", ctx.workflow_state),
        });
    }
    match &ctx.approved_revision_id {
        Some(id) if id == &ctx.current_revision_id => {}
        _ => reasons.push(GuardReason::NotApprovedRevision),
    }
    match ctx.latest_human_test_decision {
        Some(HumanTestDecision::Approve) => {}
        _ => reasons.push(GuardReason::LatestDecisionNotApprove),
    }
    if ctx.approval_context_hash != ctx.recomputed_approval_context_hash {
        reasons.push(GuardReason::ApprovalContextMismatch);
    }
    if ctx.candidate_tree != ctx.recomputed_candidate_tree {
        reasons.push(GuardReason::CandidateTreeMismatch);
    }
    if !ctx.trust_grant_valid {
        reasons.push(GuardReason::TrustGrantInvalid);
    }
    if ctx.has_active_execution {
        reasons.push(GuardReason::ActiveExecutionExists);
    }

    if reasons.is_empty() {
        GuardResult::pass()
    } else {
        GuardResult::fail_many(reasons)
    }
}

#[derive(Debug, Clone)]
pub struct AcceptPassContext {
    pub workflow_state: WorkflowState,
    pub execution_id: String,
    pub current_execution_id: String,
    pub execution_revision_id: String,
    pub approved_revision_id: String,
    pub execution_candidate_tree: String,
    pub current_candidate_tree: String,
    pub approval_context_hash: String,
    pub recomputed_approval_context_hash: String,
    pub execution_exited: bool,
    pub process_exit_code: Option<i32>,
    pub completeness: ResultCompleteness,
    pub consistency: ResultConsistency,
    pub result_manifest_hash: String,
    pub recomputed_result_manifest_hash: String,
    pub intended_manifest_hash: String,
    pub approved_intended_manifest_hash: String,
    pub intended_test_count: u32,
    pub intended_failed: u32,
    pub intended_observed: u32,
    pub unexplained_skipped: u32,
    pub total_failed: u32,
    pub total_errors: u32,
    pub every_started_suite_has_terminal: bool,
}

pub fn can_accept_pass(ctx: &AcceptPassContext) -> GuardResult {
    let mut reasons = Vec::new();

    if ctx.workflow_state != WorkflowState::PendingResultReview {
        reasons.push(GuardReason::WrongWorkflowState {
            expected: "pending_result_review".into(),
            actual: format!("{:?}", ctx.workflow_state),
        });
    }
    if ctx.execution_id != ctx.current_execution_id {
        reasons.push(GuardReason::ExecutionNotCurrent);
    }
    if ctx.execution_revision_id != ctx.approved_revision_id {
        reasons.push(GuardReason::NotApprovedRevision);
    }
    if ctx.execution_candidate_tree != ctx.current_candidate_tree {
        reasons.push(GuardReason::CandidateTreeMismatch);
    }
    if ctx.approval_context_hash != ctx.recomputed_approval_context_hash {
        reasons.push(GuardReason::ApprovalContextMismatch);
    }
    if !ctx.execution_exited {
        reasons.push(GuardReason::ExecutionNotExited);
    }
    if ctx.process_exit_code != Some(0) {
        reasons.push(GuardReason::ProcessNotSucceeded);
    }
    match ctx.completeness {
        ResultCompleteness::Complete => {}
        ResultCompleteness::Opaque => reasons.push(GuardReason::ResultOpaque),
        _ => reasons.push(GuardReason::ResultNotComplete),
    }
    if ctx.consistency == ResultConsistency::Contradictory {
        reasons.push(GuardReason::ResultContradictory);
    }
    if ctx.result_manifest_hash != ctx.recomputed_result_manifest_hash {
        reasons.push(GuardReason::ManifestHashMismatch);
    }
    if ctx.intended_manifest_hash != ctx.approved_intended_manifest_hash {
        reasons.push(GuardReason::ScopeHashMismatch);
    }
    if ctx.intended_test_count == 0 {
        reasons.push(GuardReason::IntendedCountZero);
    }
    if ctx.intended_failed > 0 {
        reasons.push(GuardReason::IntendedFailed);
    }
    // All green suite but intended identities missing from execution.
    if ctx.total_failed == 0
        && ctx.total_errors == 0
        && ctx.intended_test_count > 0
        && ctx.intended_observed < ctx.intended_test_count
    {
        reasons.push(GuardReason::IntendedNotObserved);
    }
    if ctx.unexplained_skipped > 0 {
        reasons.push(GuardReason::UnexplainedSkipped);
    }
    if ctx.total_failed > 0 || ctx.total_errors > 0 {
        reasons.push(GuardReason::RegressionFailed {
            total_failed: ctx.total_failed,
            total_errors: ctx.total_errors,
        });
    }
    if !ctx.every_started_suite_has_terminal {
        reasons.push(GuardReason::IncompleteSuites);
    }

    if reasons.is_empty() {
        GuardResult::pass()
    } else {
        GuardResult::fail_many(reasons)
    }
}

#[derive(Debug, Clone)]
pub struct ApplyContext {
    pub workflow_state: WorkflowState,
    pub accepted_execution_id: Option<String>,
    pub current_execution_id: String,
    pub accepted_revision_id: Option<String>,
    pub current_revision_id: String,
    pub latest_result_decision: Option<HumanResultDecision>,
    pub scopes_match: bool,
    pub main_worktree_clean: bool,
    pub head_equals_base: bool,
    pub candidate_tree_unchanged: bool,
}

pub fn can_apply(ctx: &ApplyContext) -> GuardResult {
    let mut reasons = Vec::new();

    if ctx.workflow_state != WorkflowState::ReadyToApply {
        reasons.push(GuardReason::WrongWorkflowState {
            expected: "ready_to_apply".into(),
            actual: format!("{:?}", ctx.workflow_state),
        });
    }
    match &ctx.accepted_execution_id {
        Some(id) if id == &ctx.current_execution_id => {}
        _ => reasons.push(GuardReason::MissingAcceptedExecution),
    }
    match &ctx.accepted_revision_id {
        Some(id) if id == &ctx.current_revision_id => {}
        _ => reasons.push(GuardReason::NotApprovedRevision),
    }
    match ctx.latest_result_decision {
        Some(HumanResultDecision::AcceptPass) => {}
        _ => reasons.push(GuardReason::LatestDecisionNotAcceptPass),
    }
    if !ctx.scopes_match {
        reasons.push(GuardReason::ScopeHashMismatch);
    }
    if !ctx.main_worktree_clean {
        reasons.push(GuardReason::MainWorktreeDirty);
    }
    if !ctx.head_equals_base {
        reasons.push(GuardReason::HeadNotBaseCommit);
    }
    if !ctx.candidate_tree_unchanged {
        reasons.push(GuardReason::CandidateTreeMismatch);
    }

    if reasons.is_empty() {
        GuardResult::pass()
    } else {
        GuardResult::fail_many(reasons)
    }
}

/// INV-19: unresolved MustFix blocks approve.
pub fn has_unresolved_must_fix(dispositions: impl IntoIterator<Item = FindingDisposition>) -> bool {
    dispositions
        .into_iter()
        .any(|d| d == FindingDisposition::MustFix)
}

/// Result decision → optional final verdict (None for non-terminal result events).
pub fn verdict_for_result_decision(d: HumanResultDecision) -> Option<ResultVerdict> {
    match d {
        HumanResultDecision::AcceptPass => Some(ResultVerdict::Pass),
        HumanResultDecision::AcceptFail => Some(ResultVerdict::Unknown),
        HumanResultDecision::MarkProductBug => Some(ResultVerdict::ProductBug),
        HumanResultDecision::MarkEnvironmentIssue => Some(ResultVerdict::Environment),
        HumanResultDecision::MarkFlaky => Some(ResultVerdict::Flaky),
        HumanResultDecision::Rerun | HumanResultDecision::RequestTestChange => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_approve() -> ApproveTestContext {
        ApproveTestContext {
            workflow_state: WorkflowState::PendingTestReview,
            current_revision_id: "r1".into(),
            revision_id: "r1".into(),
            revision_status: RevisionStatus::Validated,
            candidate_tree: "t1".into(),
            recomputed_candidate_tree: "t1".into(),
            test_case_scope_hash: "s1".into(),
            recomputed_test_case_scope_hash: "s1".into(),
            has_blocking_policy_finding: false,
            unresolved_must_fix: false,
            accepted_risk_missing_reason: false,
            trust_grant_valid: true,
        }
    }

    #[test]
    fn approve_passes_when_clean() {
        assert!(can_approve_test_revision(&base_approve()).ok);
    }

    #[test]
    fn must_fix_blocks_approve() {
        let mut ctx = base_approve();
        ctx.unresolved_must_fix = true;
        let r = can_approve_test_revision(&ctx);
        assert!(!r.ok);
        assert!(r.reasons.contains(&GuardReason::UnresolvedMustFix));
    }

    #[test]
    fn opaque_cannot_accept_pass() {
        let ctx = AcceptPassContext {
            workflow_state: WorkflowState::PendingResultReview,
            execution_id: "e1".into(),
            current_execution_id: "e1".into(),
            execution_revision_id: "r1".into(),
            approved_revision_id: "r1".into(),
            execution_candidate_tree: "t".into(),
            current_candidate_tree: "t".into(),
            approval_context_hash: "a".into(),
            recomputed_approval_context_hash: "a".into(),
            execution_exited: true,
            process_exit_code: Some(0),
            completeness: ResultCompleteness::Opaque,
            consistency: ResultConsistency::Consistent,
            result_manifest_hash: "m".into(),
            recomputed_result_manifest_hash: "m".into(),
            intended_manifest_hash: "i".into(),
            approved_intended_manifest_hash: "i".into(),
            intended_test_count: 1,
            intended_failed: 0,
            intended_observed: 1,
            unexplained_skipped: 0,
            total_failed: 0,
            total_errors: 0,
            every_started_suite_has_terminal: true,
        };
        let r = can_accept_pass(&ctx);
        assert!(!r.ok);
        assert!(r
            .reasons
            .iter()
            .any(|x| matches!(x, GuardReason::ResultOpaque)));
    }

    #[test]
    fn intended_not_observed() {
        let ctx = AcceptPassContext {
            workflow_state: WorkflowState::PendingResultReview,
            execution_id: "e1".into(),
            current_execution_id: "e1".into(),
            execution_revision_id: "r1".into(),
            approved_revision_id: "r1".into(),
            execution_candidate_tree: "t".into(),
            current_candidate_tree: "t".into(),
            approval_context_hash: "a".into(),
            recomputed_approval_context_hash: "a".into(),
            execution_exited: true,
            process_exit_code: Some(0),
            completeness: ResultCompleteness::Complete,
            consistency: ResultConsistency::Consistent,
            result_manifest_hash: "m".into(),
            recomputed_result_manifest_hash: "m".into(),
            intended_manifest_hash: "i".into(),
            approved_intended_manifest_hash: "i".into(),
            intended_test_count: 2,
            intended_failed: 0,
            intended_observed: 0,
            unexplained_skipped: 0,
            total_failed: 0,
            total_errors: 0,
            every_started_suite_has_terminal: true,
        };
        let r = can_accept_pass(&ctx);
        assert!(!r.ok);
        assert!(r
            .reasons
            .iter()
            .any(|x| matches!(x, GuardReason::IntendedNotObserved)));
    }

    #[test]
    fn accept_pass_happy_path() {
        let ctx = AcceptPassContext {
            workflow_state: WorkflowState::PendingResultReview,
            execution_id: "e1".into(),
            current_execution_id: "e1".into(),
            execution_revision_id: "r1".into(),
            approved_revision_id: "r1".into(),
            execution_candidate_tree: "t".into(),
            current_candidate_tree: "t".into(),
            approval_context_hash: "a".into(),
            recomputed_approval_context_hash: "a".into(),
            execution_exited: true,
            process_exit_code: Some(0),
            completeness: ResultCompleteness::Complete,
            consistency: ResultConsistency::Consistent,
            result_manifest_hash: "m".into(),
            recomputed_result_manifest_hash: "m".into(),
            intended_manifest_hash: "i".into(),
            approved_intended_manifest_hash: "i".into(),
            intended_test_count: 1,
            intended_failed: 0,
            intended_observed: 1,
            unexplained_skipped: 0,
            total_failed: 0,
            total_errors: 0,
            every_started_suite_has_terminal: true,
        };
        assert!(can_accept_pass(&ctx).ok);
    }
}
