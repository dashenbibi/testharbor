//! Run pointers and lightweight aggregates.

use crate::ids::{ExecutionId, ReviewId, TestRevisionId, TrustGrantId};
use crate::state::{DeliveryStatus, ResultVerdict, WorkflowState};
use serde::{Deserialize, Serialize};

/// Mutable workflow pointers for a Run (tech design §5.7).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunPointers {
    pub current_revision_id: Option<TestRevisionId>,
    pub approved_revision_id: Option<TestRevisionId>,
    pub current_execution_id: Option<ExecutionId>,
    pub accepted_execution_id: Option<ExecutionId>,
    pub latest_human_test_decision_id: Option<ReviewId>,
    pub latest_human_result_decision_id: Option<ReviewId>,
    pub latest_agent_test_review_id: Option<ReviewId>,
    pub latest_agent_result_review_id: Option<ReviewId>,
    pub active_trust_grant_id: Option<TrustGrantId>,
    /// When this Run was superseded, points at the successor Run id.
    #[serde(default)]
    pub successor_run_id: Option<String>,
    /// When this Run was created as a successor, points at the parent Run id.
    #[serde(default)]
    pub predecessor_run_id: Option<String>,
}

impl RunPointers {
    /// INV-17: creating a new revision clears approval and execution pointers.
    pub fn on_new_revision(&mut self, revision_id: TestRevisionId) {
        self.current_revision_id = Some(revision_id);
        self.approved_revision_id = None;
        self.current_execution_id = None;
        self.accepted_execution_id = None;
        self.latest_human_test_decision_id = None;
        self.latest_human_result_decision_id = None;
        // Agent reviews for previous revision are no longer authoritative.
        self.latest_agent_test_review_id = None;
        self.latest_agent_result_review_id = None;
    }

    /// Creating a new formal Execution clears acceptance.
    pub fn on_new_execution(&mut self, execution_id: ExecutionId) {
        self.current_execution_id = Some(execution_id);
        self.accepted_execution_id = None;
        self.latest_human_result_decision_id = None;
    }

    pub fn clear_approval_and_downstream(&mut self) {
        self.approved_revision_id = None;
        self.current_execution_id = None;
        self.accepted_execution_id = None;
        self.latest_human_test_decision_id = None;
        self.latest_human_result_decision_id = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    pub workflow_state: WorkflowState,
    pub delivery_status: DeliveryStatus,
    /// None means EndWithoutDecision or not yet decided.
    pub result_verdict: Option<ResultVerdict>,
    pub pointers: RunPointers,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TestRevisionId;

    #[test]
    fn new_revision_clears_downstream() {
        let mut p = RunPointers {
            current_revision_id: Some(TestRevisionId::new()),
            approved_revision_id: Some(TestRevisionId::new()),
            current_execution_id: Some(crate::ids::ExecutionId::new()),
            accepted_execution_id: Some(crate::ids::ExecutionId::new()),
            latest_human_test_decision_id: Some(crate::ids::ReviewId::new()),
            latest_human_result_decision_id: Some(crate::ids::ReviewId::new()),
            latest_agent_test_review_id: Some(crate::ids::ReviewId::new()),
            latest_agent_result_review_id: Some(crate::ids::ReviewId::new()),
            active_trust_grant_id: None,
            predecessor_run_id: None,
            successor_run_id: None,
        };
        let rid = TestRevisionId::new();
        p.on_new_revision(rid);
        assert_eq!(p.current_revision_id, Some(rid));
        assert!(p.approved_revision_id.is_none());
        assert!(p.current_execution_id.is_none());
        assert!(p.accepted_execution_id.is_none());
        assert!(p.latest_human_test_decision_id.is_none());
    }
}
