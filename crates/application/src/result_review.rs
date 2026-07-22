//! Result-side decisions and Apply delivery.

use crate::generate::{
    GenerateTestsRequest, GenerateTestsResult, IntentMeta, PendingChangeRequest,
};
use crate::AppState;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use testharbor_domain::{
    can_apply, ApplyContext, DeliveryStatus, HumanResultDecision, ResultVerdict, RunId,
    RunPointers, WorkflowState,
};
use testharbor_git_adapter::{self as git};
use testharbor_review::CaseReviewState;
use testharbor_storage::{RunRecord, StorageError};
use testharbor_workspace_manager::{self as ws, ApplyResult, CandidateRevision};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ResultReviewError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Workspace(#[from] ws::WorkspaceError),
    #[error(transparent)]
    Git(#[from] git::GitError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid run id")]
    InvalidRunId,
    #[error("apply blocked: {0}")]
    ApplyBlocked(String),
    #[error("decision not allowed: {0}")]
    DecisionBlocked(String),
    #[error("no candidate")]
    NoCandidate,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ResultReviewError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultDecisionResult {
    pub decision: String,
    pub workflow_state: String,
    pub delivery_status: String,
    pub result_verdict: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyPatchResult {
    pub applied: ApplyResult,
    pub workflow_state: String,
    pub delivery_status: String,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPatchResult {
    pub patch: String,
    pub candidate_commit: String,
    pub candidate_tree: String,
    pub delivery_status: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyGuardView {
    pub enabled: bool,
    pub reasons: Vec<String>,
    pub head: Option<String>,
    pub base_commit: Option<String>,
    pub main_clean: Option<bool>,
    /// True when main HEAD ≠ Run base_commit (Apply forbidden; Export or Successor OK).
    pub head_drift: bool,
    /// Candidate exists and HEAD drifted → CreateSuccessorRun is a legal exit.
    pub can_create_successor: bool,
    pub successor_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessorRunResult {
    pub predecessor_run_id: String,
    pub successor_run_id: String,
    pub new_base_commit: String,
    pub old_base_commit: String,
    pub candidate: CandidateRevision,
    pub review: CaseReviewState,
    pub workflow_state: String,
    pub message: String,
    pub next_steps: Vec<String>,
}

fn parse_run_id(s: &str) -> Result<RunId> {
    Uuid::parse_str(s)
        .map(RunId)
        .map_err(|_| ResultReviewError::InvalidRunId)
}

fn require_pending_result_review(state: &AppState, run_id: &str) -> Result<RunRecord> {
    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|store| store.get_run(rid))?;
    if run.legacy_read_only {
        return Err(ResultReviewError::DecisionBlocked(
            "legacy run is read-only".into(),
        ));
    }
    if run.workflow_state != WorkflowState::PendingResultReview {
        return Err(ResultReviewError::DecisionBlocked(format!(
            "result decision requires PendingResultReview, current state is {:?}",
            run.workflow_state
        )));
    }
    Ok(run)
}

/// Human accepts pass → ReadyToApply (requires can_accept_pass-like preconditions already evaluated in UI).
pub fn decide_accept_pass(state: &AppState, run_id: &str) -> Result<ResultDecisionResult> {
    let run = require_pending_result_review(state, run_id)?;
    if state.get_case_approval(run_id).is_none() {
        return Err(ResultReviewError::DecisionBlocked(
            "case not approved".into(),
        ));
    }
    let execution = state
        .get_last_execution(run_id)
        .ok_or_else(|| ResultReviewError::DecisionBlocked("no completed execution".into()))?;
    if execution.structured.is_none() {
        return Err(ResultReviewError::DecisionBlocked(
            "structured result required; opaque command results cannot AcceptPass".into(),
        ));
    }
    let view = crate::reevaluate_accept_pass(state, run_id)
        .map_err(|e| ResultReviewError::DecisionBlocked(e.to_string()))?;
    if !view.enabled {
        return Err(ResultReviewError::DecisionBlocked(format!(
            "AcceptPass guard failed: {}",
            view.reasons.join("; ")
        )));
    }
    if execution.agent_error.is_some()
        || execution.analysis.is_none()
        || execution.result_review.is_none()
    {
        return Err(ResultReviewError::DecisionBlocked(
            "Analyzer or Result Reviewer failed; rerun result analysis or inspect the evidence before AcceptPass".into(),
        ));
    }

    state.set_result_decision(
        run_id.to_string(),
        ResultDecisionRecord {
            decision: HumanResultDecision::AcceptPass,
            execution_id: execution.execution_id.clone(),
            verdict: Some(ResultVerdict::Pass),
        },
    )?;

    state.with_store_transaction(|s| {
        s.insert_decision(
            &Uuid::new_v4().to_string(),
            run.id,
            "execution",
            &execution.execution_id,
            "accept_pass",
            &testharbor_domain::canonical_hash(&[
                &execution.execution_id,
                run.test_intent_hash.as_deref().unwrap_or(""),
            ]),
            &serde_json::json!({"verdict": "pass"}),
        )?;
        s.update_run_completion(
            run.id,
            WorkflowState::ReadyToApply,
            DeliveryStatus::NotDelivered,
            Some(ResultVerdict::Pass),
            run.worktree_path.as_deref(),
        )?;
        s.append_event(
            Some(run.id),
            None,
            "result_accept_pass",
            &serde_json::json!({}),
        )?;
        Ok::<(), StorageError>(())
    })?;

    Ok(ResultDecisionResult {
        decision: "accept_pass".into(),
        workflow_state: format!("{:?}", WorkflowState::ReadyToApply),
        delivery_status: format!("{:?}", DeliveryStatus::NotDelivered),
        result_verdict: Some("pass".into()),
        message: "结果已接受为通过；可 Apply 或 Export。".into(),
    })
}

pub fn decide_accept_fail(state: &AppState, run_id: &str) -> Result<ResultDecisionResult> {
    finish_with_verdict(
        state,
        run_id,
        HumanResultDecision::AcceptFail,
        Some(ResultVerdict::Unknown),
        WorkflowState::Completed,
        DeliveryStatus::NotDelivered,
        "已接受失败结论并归档；不 Apply。",
    )
}

pub fn decide_mark_product_bug(state: &AppState, run_id: &str) -> Result<ResultDecisionResult> {
    finish_with_verdict(
        state,
        run_id,
        HumanResultDecision::MarkProductBug,
        Some(ResultVerdict::ProductBug),
        WorkflowState::Completed,
        DeliveryStatus::NotDelivered,
        "已标记产品缺陷并归档；patch 可导出，不自动 Apply。",
    )
}

pub fn decide_mark_environment(state: &AppState, run_id: &str) -> Result<ResultDecisionResult> {
    let run = require_pending_result_review(state, run_id)?;
    let execution_id = state
        .get_last_execution(run_id)
        .ok_or_else(|| ResultReviewError::DecisionBlocked("no completed execution".into()))?
        .execution_id;
    state.set_result_decision(
        run_id.to_string(),
        ResultDecisionRecord {
            decision: HumanResultDecision::MarkEnvironmentIssue,
            execution_id: execution_id.clone(),
            verdict: Some(ResultVerdict::Environment),
        },
    )?;
    state.with_store_transaction(|s| {
        s.update_run_completion(
            run.id,
            WorkflowState::NeedsHuman,
            DeliveryStatus::NotDelivered,
            Some(ResultVerdict::Environment),
            run.worktree_path.as_deref(),
        )?;
        s.append_event(
            Some(run.id),
            None,
            "result_environment_issue",
            &serde_json::json!({}),
        )?;
        Ok::<(), StorageError>(())
    })?;
    Ok(ResultDecisionResult {
        decision: "mark_environment_issue".into(),
        workflow_state: format!("{:?}", WorkflowState::NeedsHuman),
        delivery_status: format!("{:?}", DeliveryStatus::NotDelivered),
        result_verdict: Some("environment".into()),
        message: "已标记环境问题；修复外部依赖后可重新执行。".into(),
    })
}

pub fn decide_mark_flaky(state: &AppState, run_id: &str) -> Result<ResultDecisionResult> {
    let rid = parse_run_id(run_id)?;
    let history = state.with_store(|store| store.list_execution_history(rid))?;
    let latest_scope = history
        .iter()
        .rev()
        .find(|execution| execution.purpose == "formal" && execution.status == "complete")
        .map(|execution| execution.scope_hash.clone())
        .ok_or_else(|| {
            ResultReviewError::DecisionBlocked("Flaky 至少需要两次同 scope 的完整正式执行".into())
        })?;
    let matching = history
        .iter()
        .filter(|execution| {
            execution.purpose == "formal"
                && execution.status == "complete"
                && execution.scope_hash == latest_scope
        })
        .collect::<Vec<_>>();
    let mut observed_pass = false;
    let mut observed_fail = false;
    for execution in &matching {
        let value: serde_json::Value = serde_json::from_str(&execution.payload_json)?;
        let exit_code = value
            .pointer("/generic/outcome/exit_code")
            .or_else(|| value.pointer("/outcome/exit_code"))
            .and_then(serde_json::Value::as_i64);
        let failed = value
            .pointer("/structured/failed")
            .and_then(serde_json::Value::as_u64);
        let empty = value
            .pointer("/structured/empty")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let pass = exit_code == Some(0) && failed == Some(0) && !empty;
        observed_pass |= pass;
        observed_fail |= !pass;
    }
    if matching.len() < 2 || !observed_pass || !observed_fail {
        return Err(ResultReviewError::DecisionBlocked(
            "Flaky 需要至少两个同 candidate/command/env/source scope 的 COMPLETE 正式执行，且至少一次通过、一次失败"
                .into(),
        ));
    }
    finish_with_verdict(
        state,
        run_id,
        HumanResultDecision::MarkFlaky,
        Some(ResultVerdict::Flaky),
        WorkflowState::Completed,
        DeliveryStatus::NotDelivered,
        "已标记 ObservedFlaky 并归档；不 Apply。",
    )
}

/// Result-stage test change request. The current revision's downstream
/// pointers are invalidated, while its persisted reviews/executions remain in
/// history. Generation then produces a new revision and re-enters validation
/// and review.
pub fn request_result_test_change(
    state: &AppState,
    run_id: &str,
    feedback: &str,
) -> Result<GenerateTestsResult> {
    let run = require_pending_result_review(state, run_id)?;
    if feedback.trim().is_empty() {
        return Err(ResultReviewError::DecisionBlocked(
            "RequestTestChange requires structured non-empty feedback".into(),
        ));
    }
    let intent = state
        .get_intent_meta(run_id)
        .ok_or_else(|| ResultReviewError::Other("persisted TestIntent unavailable".into()))?;
    state.set_pending_change(
        run_id.to_string(),
        PendingChangeRequest {
            message: feedback.trim().to_string(),
            previous_revision_sequence: state.get_candidate(run_id).map(|item| item.sequence),
        },
    )?;
    state.clear_case_approval(run_id)?;
    state.clear_result_decision(run_id)?;
    state.clear_last_execution(run_id)?;
    state.clear_structured(run_id)?;
    state.clear_analysis(run_id)?;
    state.clear_result_agent_review(run_id)?;
    state.clear_intended(run_id)?;
    state.with_store(|store| {
        store.update_run_worktree(
            run.id,
            run.worktree_path.as_deref(),
            WorkflowState::GeneratingTests,
        )?;
        store.append_event(
            Some(run.id),
            Some(run.project_id),
            "result_test_change_requested",
            &serde_json::json!({
                "feedback": feedback.trim(),
                "invalidates": ["case_approval", "intended", "formal_execution", "result_decision"],
                "preserves": ["run_snapshot", "revision_history", "execution_history"],
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;
    crate::generate_and_review(
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
        },
    )
    .map_err(|error| ResultReviewError::Other(error.to_string()))
}

pub fn prepare_execution_rerun(state: &AppState, run_id: &str) -> Result<ResultDecisionResult> {
    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|store| store.get_run(rid))?;
    if !matches!(
        run.workflow_state,
        WorkflowState::PendingResultReview
            | WorkflowState::NeedsHuman
            | WorkflowState::ReadyToApply
    ) {
        return Err(ResultReviewError::DecisionBlocked(format!(
            "rerun is not allowed in state {:?}",
            run.workflow_state
        )));
    }
    if state.get_case_approval(run_id).is_none() || state.get_candidate(run_id).is_none() {
        return Err(ResultReviewError::DecisionBlocked(
            "approved candidate required for rerun".into(),
        ));
    }
    let approval = state
        .get_case_approval(run_id)
        .ok_or_else(|| ResultReviewError::DecisionBlocked("case not approved".into()))?;
    let current_intended_hash = state.get_intended(run_id).map(|manifest| manifest.hash);
    if approval.intended_manifest_hash != current_intended_hash {
        state.clear_case_approval(run_id)?;
        state.clear_result_decision(run_id)?;
        state.clear_last_execution(run_id)?;
        state.with_store(|store| {
            store.update_run_worktree(
                run.id,
                run.worktree_path.as_deref(),
                WorkflowState::PendingTestReview,
            )?;
            store.append_event(
                Some(run.id),
                Some(run.project_id),
                "execution_rerun_blocked_intended_changed",
                &serde_json::json!({
                    "approved_intended_manifest_hash": approval.intended_manifest_hash,
                    "current_intended_manifest_hash": current_intended_hash,
                }),
            )?;
            Ok::<(), StorageError>(())
        })?;
        return Ok(ResultDecisionResult {
            decision: "reapprove".into(),
            workflow_state: format!("{:?}", WorkflowState::PendingTestReview),
            delivery_status: format!("{:?}", run.delivery_status),
            result_verdict: None,
            message: "批准后的测试清单发生变化，已回到用例评审；请重新批准后再执行。".into(),
        });
    }
    state.clear_result_decision(run_id)?;
    state.with_store(|store| {
        store.update_run_worktree(
            run.id,
            run.worktree_path.as_deref(),
            WorkflowState::ExecutionReady,
        )?;
        store.append_event(
            Some(run.id),
            Some(run.project_id),
            "execution_rerun_requested",
            &serde_json::json!({
                "previous_execution_id": state
                    .get_last_execution(run_id)
                    .map(|execution| execution.execution_id),
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;
    Ok(ResultDecisionResult {
        decision: "rerun".into(),
        workflow_state: format!("{:?}", WorkflowState::ExecutionReady),
        delivery_status: format!("{:?}", run.delivery_status),
        result_verdict: run
            .result_verdict
            .map(|verdict| format!("{verdict:?}").to_lowercase()),
        message: "已回到 ExecutionReady；将对同一批准 revision 重新执行。".into(),
    })
}

pub fn decide_end_without_decision(state: &AppState, run_id: &str) -> Result<ResultDecisionResult> {
    let run = require_pending_result_review(state, run_id)?;
    state.with_store_transaction(|s| {
        s.update_run_completion(
            run.id,
            WorkflowState::Cancelled,
            DeliveryStatus::NotDelivered,
            None, // no verdict
            run.worktree_path.as_deref(),
        )?;
        s.append_event(
            Some(run.id),
            None,
            "end_without_decision",
            &serde_json::json!({}),
        )?;
        Ok::<(), StorageError>(())
    })?;
    Ok(ResultDecisionResult {
        decision: "end_without_decision".into(),
        workflow_state: format!("{:?}", WorkflowState::Cancelled),
        delivery_status: format!("{:?}", DeliveryStatus::NotDelivered),
        result_verdict: None,
        message: "已结束且不形成结果结论；审计产物保留。".into(),
    })
}

fn finish_with_verdict(
    state: &AppState,
    run_id: &str,
    decision: HumanResultDecision,
    verdict: Option<ResultVerdict>,
    wf: WorkflowState,
    delivery: DeliveryStatus,
    message: &str,
) -> Result<ResultDecisionResult> {
    let run = require_pending_result_review(state, run_id)?;
    let execution_id = state
        .get_last_execution(run_id)
        .ok_or_else(|| ResultReviewError::DecisionBlocked("no completed execution".into()))?
        .execution_id;
    state.set_result_decision(
        run_id.to_string(),
        ResultDecisionRecord {
            decision,
            execution_id: execution_id.clone(),
            verdict,
        },
    )?;
    state.with_store_transaction(|s| {
        s.insert_decision(
            &Uuid::new_v4().to_string(),
            run.id,
            "execution",
            &execution_id,
            &format!("{decision:?}").to_lowercase(),
            &testharbor_domain::canonical_hash(&[
                &execution_id,
                run.test_intent_hash.as_deref().unwrap_or(""),
            ]),
            &serde_json::json!({"decision": decision, "verdict": verdict}),
        )?;
        s.update_run_completion(run.id, wf, delivery, verdict, run.worktree_path.as_deref())?;
        s.append_event(
            Some(run.id),
            None,
            "result_decision",
            &serde_json::json!({
                "decision": format!("{:?}", decision),
                "verdict": verdict,
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;
    Ok(ResultDecisionResult {
        decision: format!("{decision:?}").to_lowercase(),
        workflow_state: format!("{wf:?}"),
        delivery_status: format!("{delivery:?}"),
        result_verdict: verdict.map(|v| format!("{v:?}").to_lowercase()),
        message: message.into(),
    })
}

pub fn apply_guard_status(state: &AppState, run_id: &str) -> Result<ApplyGuardView> {
    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(rid))?;
    let project = state.with_store(|s| s.get_project(run.project_id))?;
    let candidate = state.get_candidate(run_id);
    let approval = state.get_case_approval(run_id);
    let result_dec = state.get_result_decision(run_id);
    let last_execution = state.get_last_execution(run_id);
    let review = state.get_case_review(run_id);
    let case_scope = state.get_case_scope(run_id);
    let intended = state.get_intended(run_id);

    let repo = PathBuf::from(&project.root_path);
    let head = git::rev_parse(&repo, "HEAD").ok();
    let clean = git::is_worktree_clean(&repo).ok();

    let approval_context_matches = match (&approval, &review, &case_scope) {
        (Some(approval), Some(review), Some(scope)) => {
            approval.case_approval_context_hash
                == testharbor_domain::case_approval_context_hash(
                    scope,
                    &review.disposition_snapshot_hash,
                )
        }
        _ => false,
    };
    let intended_matches = approval
        .as_ref()
        .map(|approval| approval.intended_manifest_hash.clone())
        == Some(intended.as_ref().map(|manifest| manifest.hash.clone()));
    let execution_matches = match (&result_dec, &last_execution) {
        (Some(decision), Some(execution)) => decision.execution_id == execution.execution_id,
        _ => false,
    };
    let candidate_matches = match (&candidate, &approval) {
        (Some(candidate), Some(approval)) => {
            candidate.candidate_tree == approval.candidate_tree
                && candidate.candidate_commit == approval.candidate_commit
                && candidate.base_commit == approval.base_commit
        }
        _ => false,
    };
    let candidate_tree_unchanged = match (&candidate, &run.worktree_path) {
        (Some(candidate), Some(worktree_path)) if PathBuf::from(worktree_path).is_dir() => {
            let handle = ws::WorkspaceHandle {
                repo_root: repo.clone(),
                worktree_path: PathBuf::from(worktree_path),
                base_commit: run.base_commit.clone(),
                run_id: run.id.to_string(),
            };
            ws::verify_integrity(&handle, &candidate.candidate_tree).unwrap_or(false)
        }
        _ => false,
    };
    let accepted_execution_id = result_dec.as_ref().map(|d| d.execution_id.clone());
    let current_execution_id = last_execution
        .as_ref()
        .map(|execution| execution.execution_id.clone())
        .unwrap_or_default();

    let ctx = ApplyContext {
        workflow_state: run.workflow_state,
        accepted_execution_id,
        current_execution_id,
        accepted_revision_id: approval.as_ref().map(|_| {
            candidate
                .as_ref()
                .map(|c| format!("r{}", c.sequence))
                .unwrap_or_else(|| "approved".into())
        }),
        current_revision_id: candidate
            .as_ref()
            .map(|c| format!("r{}", c.sequence))
            .unwrap_or_default(),
        latest_result_decision: result_dec.as_ref().map(|d| d.decision),
        scopes_match: approval_context_matches
            && intended_matches
            && execution_matches
            && candidate_matches,
        main_worktree_clean: clean.unwrap_or(false),
        head_equals_base: head
            .as_ref()
            .map(|h| h == &run.base_commit)
            .unwrap_or(false),
        candidate_tree_unchanged,
    };

    // Also require ReadyToApply and AcceptPass decision
    let mut guard = can_apply(&ctx);
    if result_dec
        .as_ref()
        .map(|d| d.decision != HumanResultDecision::AcceptPass)
        .unwrap_or(true)
    {
        guard.ok = false;
        if !guard.reasons.iter().any(|r| {
            matches!(
                r,
                testharbor_domain::GuardReason::LatestDecisionNotAcceptPass
            )
        }) {
            guard
                .reasons
                .push(testharbor_domain::GuardReason::LatestDecisionNotAcceptPass);
        }
    }

    let head_drift = head
        .as_ref()
        .map(|h| h != &run.base_commit)
        .unwrap_or(false);
    // Successor is legal whenever HEAD drifted and a candidate still exists,
    // except mid-apply or already superseded without re-entry.
    let can_create_successor = head_drift
        && candidate.is_some()
        && !matches!(
            run.workflow_state,
            WorkflowState::Superseded | WorkflowState::ApplyingRevision
        );

    let successor_hint = if can_create_successor {
        Some(
            "主工作区 HEAD 已漂移，禁止在原 base 上 Apply。可导出 Patch，或以新 HEAD 创建 Successor Run（完整重走批准与执行）。"
                .into(),
        )
    } else if head_drift {
        Some("HEAD 已漂移；无 candidate 时只能结束或从新 base 重新生成。".into())
    } else {
        None
    };

    Ok(ApplyGuardView {
        enabled: guard.ok,
        reasons: guard.reasons.iter().map(|r| format!("{r:?}")).collect(),
        head,
        base_commit: Some(run.base_commit),
        main_clean: clean,
        head_drift,
        can_create_successor,
        successor_hint,
    })
}

/// CreateSuccessorRun (tech design §16.4): new base = current HEAD, replay candidate files,
/// freeze new revision, re-run agent case review. Parent → Superseded. Approvals not reused.
pub fn create_successor_run(state: &AppState, run_id: &str) -> Result<SuccessorRunResult> {
    let parent_id = parse_run_id(run_id)?;
    let parent = state.with_store(|s| s.get_run(parent_id))?;
    let project = state.with_store(|s| s.get_project(parent.project_id))?;
    let repo = PathBuf::from(&project.root_path);

    if matches!(parent.workflow_state, WorkflowState::Superseded) {
        // Idempotent: return existing successor if linked
        let pointers: RunPointers = serde_json::from_str(&parent.pointers_json).unwrap_or_default();
        if let Some(sid) = pointers.successor_run_id {
            let succ_rid = parse_run_id(&sid)?;
            let succ_run = state.with_store(|s| s.get_run(succ_rid))?;
            let cand = state
                .get_candidate(&sid)
                .ok_or(ResultReviewError::NoCandidate)?;
            let review = state.get_case_review(&sid).unwrap_or_else(|| {
                CaseReviewState::from_agent_review(
                    "NEEDS_HUMAN".into(),
                    "existing successor".into(),
                    vec![],
                )
            });
            return Ok(SuccessorRunResult {
                predecessor_run_id: run_id.to_string(),
                successor_run_id: sid,
                new_base_commit: succ_run.base_commit,
                old_base_commit: parent.base_commit,
                candidate: cand,
                review,
                workflow_state: format!("{:?}", succ_run.workflow_state),
                message: "该 Run 已 Superseded，返回已有 Successor。".into(),
                next_steps: vec!["在 Successor Run 上完成用例批准 → 执行 → 结果评审".into()],
            });
        }
        return Err(ResultReviewError::Other(
            "Run 已 Superseded 且无 successor 链接".into(),
        ));
    }

    let source = state
        .get_candidate(run_id)
        .ok_or(ResultReviewError::NoCandidate)?;

    let new_head = git::rev_parse(&repo, "HEAD")?;
    if new_head == parent.base_commit {
        return Err(ResultReviewError::Other(
            "HEAD 仍等于 Run base_commit，无需 Successor（可直接 Apply 或先导出）".into(),
        ));
    }

    let parent_snapshot_json = state
        .with_store(|store| store.get_run_snapshot_json(parent_id))?
        .ok_or_else(|| ResultReviewError::Other("parent RunSnapshot is unavailable".into()))?;
    let mut successor_snapshot: testharbor_domain::RunSnapshot =
        serde_json::from_str(&parent_snapshot_json)?;
    let intent = state.get_intent_meta(run_id).unwrap_or(IntentMeta {
        objective: successor_snapshot.test_intent.objective.clone(),
        acceptance_criteria: successor_snapshot
            .test_intent
            .acceptance_criteria
            .iter()
            .map(|criterion| criterion.text.clone())
            .collect(),
        test_intent_hash: successor_snapshot.test_intent.hash.clone(),
        test_strategy: "e2e_api_flow".into(),
        cleanup_test_data: true,
        agent_mode: Some("codex".into()),
        agent_model: None,
        reuse_agent_session: false,
        environment: None,
    });
    let grant = state.get_trust_grant(run_id).ok_or_else(|| {
        ResultReviewError::Other("successor requires the predecessor TrustGrant".into())
    })?;
    let current_scope =
        testharbor_trust::profile_trust_scope_hash(&project.root_path, &grant.profile);
    if current_scope != successor_snapshot.required_trust_scope_hash {
        return Err(ResultReviewError::Other(
            "predecessor TrustGrant no longer matches its frozen RunSnapshot".into(),
        ));
    }
    successor_snapshot.base_commit = new_head.clone();
    successor_snapshot.project_config_hash =
        git::read_utf8_blob_at(&repo, &new_head, ".testharbor.yaml")
            .map(|(_, body)| testharbor_domain::content_body_hash(&body))
            .unwrap_or_else(|_| testharbor_domain::canonical_hash(&["no-project-config"]));

    let now = Utc::now();
    let new_run_id = RunId::new();
    let mut succ_pointers = RunPointers {
        predecessor_run_id: Some(run_id.to_string()),
        ..Default::default()
    };
    let new_run = RunRecord {
        id: new_run_id,
        project_id: project.id,
        objective: intent.objective.clone(),
        workflow_state: WorkflowState::PreparingWorktree,
        delivery_status: DeliveryStatus::NotDelivered,
        result_verdict: None,
        base_commit: new_head.clone(),
        worktree_path: None,
        test_intent_hash: Some(successor_snapshot.test_intent.hash.clone()),
        config_snapshot_hash: None,
        pointers_json: serde_json::to_string(&succ_pointers).unwrap_or_else(|_| "{}".into()),
        snapshot_version: parent.snapshot_version,
        legacy_read_only: false,
        created_at: now,
        updated_at: now,
    };

    let snapshot_bytes = serde_json::to_vec(&successor_snapshot)?;
    let snapshot_hash = testharbor_domain::sha256_hex(&snapshot_bytes);
    state.with_store_transaction(|s| {
        s.insert_run(&new_run)?;
        s.set_run_snapshot_in_current_transaction(
            new_run_id,
            &successor_snapshot.test_intent.hash,
            &snapshot_hash,
            &successor_snapshot,
        )?;
        s.append_event(
            Some(new_run_id),
            Some(project.id),
            "successor_created",
            &serde_json::json!({
                "predecessor_run_id": run_id,
                "new_base_commit": new_head,
                "old_base_commit": parent.base_commit,
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;

    let sid = new_run_id.to_string();
    state.set_trust_grant(sid.clone(), grant)?;
    state.set_intent_meta(sid.clone(), intent.clone())?;

    // Worktree at new HEAD + replay source candidate paths. The replay is not
    // itself a Revision; it must pass the same policy/format/freeze/
    // collect/compile/inventory/reviewer pipeline as generation.
    let worktrees_root = state.paths().worktrees_dir.clone();
    let handle = ws::create_run_worktree(&repo, &worktrees_root, &sid, &new_head)?;
    let generation = match ws::replay_candidate_paths(&handle, &source).and_then(|_| {
        let progress_dir = state.paths().root.join("runs").join(&sid).join("agent");
        std::fs::create_dir_all(&progress_dir)
            .map_err(|error| ws::WorkspaceError::Other(error.to_string()))?;
        let stdout = progress_dir.join("generation.stdout.jsonl");
        let stderr = progress_dir.join("generation.stderr.log");
        std::fs::write(&stdout, "")
            .and_then(|_| std::fs::write(&stderr, ""))
            .map_err(|error| ws::WorkspaceError::Other(error.to_string()))?;
        let intended_names = state
            .get_intended(run_id)
            .map(|manifest| {
                manifest
                    .identities
                    .into_iter()
                    .map(|identity| identity.name)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mode = match intent.agent_mode.as_deref() {
            Some("mock") => testharbor_agent_drivers::AgentMode::Mock,
            Some("claude") => testharbor_agent_drivers::AgentMode::Claude,
            Some("grok") => testharbor_agent_drivers::AgentMode::Grok,
            _ => testharbor_agent_drivers::AgentMode::Codex,
        };
        crate::generate::freeze_and_review_worktree(
            state,
            &new_run,
            &project,
            handle.worktree_path.to_str().unwrap_or_default(),
            &successor_snapshot.test_intent.objective,
            &successor_snapshot
                .test_intent
                .acceptance_criteria
                .iter()
                .map(|criterion| criterion.text.clone())
                .collect::<Vec<_>>(),
            &successor_snapshot.test_intent.hash,
            None,
            mode,
            "Replayed predecessor candidate onto successor base".into(),
            source.changed_paths.clone(),
            intended_names,
            None,
            &stdout,
            &progress_dir,
            0,
        )
        .map_err(|error| ws::WorkspaceError::Other(error.to_string()))
    }) {
        Ok(result) => result,
        Err(e) => {
            let _ = ws::discard_worktree(&handle);
            state.with_store_transaction(|s| {
                s.update_run_completion(
                    new_run_id,
                    WorkflowState::Failed,
                    DeliveryStatus::NotDelivered,
                    None,
                    None,
                )
            })?;
            return Err(ResultReviewError::Other(format!(
                "Successor 重放或完整 Validation/Review 失败: {e}"
            )));
        }
    };
    let candidate = generation.candidate;
    let review_state = generation.review;

    // Mark parent Superseded + link both ways
    let mut parent_pointers: RunPointers =
        serde_json::from_str(&parent.pointers_json).unwrap_or_default();
    parent_pointers.successor_run_id = Some(sid.clone());
    parent_pointers.clear_approval_and_downstream();
    succ_pointers.current_revision_id = None; // revision tracked via candidate in memory

    state.with_store_transaction(|s| {
        s.update_run_state(parent_id, WorkflowState::Superseded, &parent_pointers)?;
        s.append_event(
            Some(parent_id),
            Some(project.id),
            "run_superseded",
            &serde_json::json!({
                "successor_run_id": sid,
                "reason": "head_drift",
                "old_base": parent.base_commit,
                "new_base": new_head,
            }),
        )?;
        s.append_event(
            Some(new_run_id),
            Some(project.id),
            "successor_ready_for_case_review",
            &serde_json::json!({
                "candidate": candidate,
                "review": review_state,
                "predecessor_run_id": run_id,
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;

    Ok(SuccessorRunResult {
        predecessor_run_id: run_id.to_string(),
        successor_run_id: sid,
        new_base_commit: new_head,
        old_base_commit: parent.base_commit,
        candidate,
        review: review_state,
        workflow_state: format!("{:?}", WorkflowState::PendingTestReview),
        message: "已创建 Successor Run：候选文件已在新 HEAD 上重放并重新冻结；旧批准作废，请重新批准用例后执行。".into(),
        next_steps: vec![
            "在新 Run 上处理 findings 并批准用例".into(),
            "重新 Trust + 执行（旧 Trust 不自动继承）".into(),
            "结果评审通过后 Apply（此时 HEAD 应等于新 base）".into(),
            "或导出旧 Run 的 patch 自行处理".into(),
        ],
    })
}

pub fn apply_patch(state: &AppState, run_id: &str) -> Result<ApplyPatchResult> {
    let status = apply_guard_status(state, run_id)?;
    if !status.enabled {
        return Err(ResultReviewError::ApplyBlocked(status.reasons.join("; ")));
    }

    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(rid))?;
    let project = state.with_store(|s| s.get_project(run.project_id))?;
    let candidate = state
        .get_candidate(run_id)
        .ok_or(ResultReviewError::NoCandidate)?;
    let apply_intent_id = Uuid::new_v4().to_string();

    state.with_store_transaction(|s| {
        s.insert_apply_intent(
            &apply_intent_id,
            run.id,
            &serde_json::json!({
                "base_commit": run.base_commit,
                "candidate_commit": candidate.candidate_commit,
                "candidate_tree": candidate.candidate_tree,
                "guard": status,
            }),
        )?;
        s.update_run_worktree(
            run.id,
            run.worktree_path.as_deref(),
            WorkflowState::ApplyingRevision,
        )
    })?;

    let applied = match ws::apply_candidate_to_main(
        PathBuf::from(&project.root_path).as_path(),
        &run.base_commit,
        &candidate,
    ) {
        Ok(a) => a,
        Err(e) => {
            let _ = git::abort_cherry_pick(PathBuf::from(&project.root_path).as_path());
            state.with_store_transaction(|s| {
                s.finalize_apply_intent(
                    &apply_intent_id,
                    "failed_recovered",
                    &serde_json::json!({"error": e.to_string()}),
                )?;
                s.update_run_worktree(
                    run.id,
                    run.worktree_path.as_deref(),
                    WorkflowState::ReadyToApply,
                )
            })?;
            return Err(ResultReviewError::ApplyBlocked(e.to_string()));
        }
    };

    state.with_store_transaction(|s| {
        s.finalize_apply_intent(&apply_intent_id, "applied", &applied)?;
        s.update_run_completion(
            run.id,
            WorkflowState::Completed,
            DeliveryStatus::Applied,
            Some(ResultVerdict::Pass),
            run.worktree_path.as_deref(),
        )?;
        s.append_event(Some(run.id), Some(project.id), "patch_applied", &applied)?;
        Ok::<(), StorageError>(())
    })?;

    Ok(ApplyPatchResult {
        applied,
        workflow_state: format!("{:?}", WorkflowState::Completed),
        delivery_status: format!("{:?}", DeliveryStatus::Applied),
        next_steps: vec![
            "检查 git log / git show HEAD".into(),
            "按需 git push 或开 PR（TestHarbor 不自动推送）".into(),
        ],
    })
}

pub fn export_patch(
    state: &AppState,
    run_id: &str,
    complete_run: bool,
) -> Result<ExportPatchResult> {
    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(rid))?;
    let project = state.with_store(|s| s.get_project(run.project_id))?;
    let candidate = state
        .get_candidate(run_id)
        .ok_or(ResultReviewError::NoCandidate)?;

    let handle = ws::WorkspaceHandle {
        repo_root: PathBuf::from(&project.root_path),
        worktree_path: run
            .worktree_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from(&project.root_path)),
        base_commit: candidate.base_commit.clone(),
        run_id: run.id.to_string(),
    };

    let patch = if handle.worktree_path.exists() {
        ws::export_patch(&handle, &candidate)?
    } else {
        // Fallback: show empty with commit ids
        format!(
            "# candidate {} on base {}\n# worktree missing; use: git cherry-pick {}\n",
            candidate.candidate_commit, candidate.base_commit, candidate.candidate_commit
        )
    };

    let export_path = state
        .paths()
        .root
        .join("runs")
        .join(run.id.to_string())
        .join("exports");
    std::fs::create_dir_all(&export_path).ok();
    let file = export_path.join(format!(
        "candidate-{}.patch",
        &candidate.candidate_commit[..12]
    ));
    let _ = std::fs::write(&file, &patch);

    let delivery = if complete_run {
        state.with_store(|s| {
            s.update_run_completion(
                run.id,
                WorkflowState::Completed,
                DeliveryStatus::Exported,
                run.result_verdict,
                run.worktree_path.as_deref(),
            )?;
            s.append_event(
                Some(run.id),
                None,
                "patch_exported",
                &serde_json::json!({"path": file.display().to_string()}),
            )?;
            Ok::<(), StorageError>(())
        })?;
        DeliveryStatus::Exported
    } else {
        DeliveryStatus::NotDelivered
    };

    Ok(ExportPatchResult {
        patch,
        candidate_commit: candidate.candidate_commit,
        candidate_tree: candidate.candidate_tree,
        delivery_status: format!("{delivery:?}"),
        path: Some(file.display().to_string()),
    })
}

pub fn discard_delivery(state: &AppState, run_id: &str) -> Result<ResultDecisionResult> {
    let rid = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(rid))?;
    // Discard worktree if present
    if run.worktree_path.is_some() {
        let _ = crate::discard_run_worktree(state, run_id);
    }
    state.with_store(|s| {
        s.update_run_completion(
            run.id,
            WorkflowState::Completed,
            DeliveryStatus::Discarded,
            run.result_verdict,
            None,
        )?;
        s.append_event(
            Some(run.id),
            None,
            "delivery_discarded",
            &serde_json::json!({}),
        )?;
        Ok::<(), StorageError>(())
    })?;
    Ok(ResultDecisionResult {
        decision: "discard".into(),
        workflow_state: format!("{:?}", WorkflowState::Completed),
        delivery_status: format!("{:?}", DeliveryStatus::Discarded),
        result_verdict: run.result_verdict.map(|v| format!("{v:?}").to_lowercase()),
        message: "已丢弃交付；审计记录保留。".into(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultDecisionRecord {
    pub decision: HumanResultDecision,
    pub execution_id: String,
    pub verdict: Option<ResultVerdict>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{
        approve_test_case, generate_and_review, set_finding_disposition, GenerateTestsRequest,
    };
    use crate::projects::open_project;
    use crate::AppState;
    use std::process::Command;
    use testharbor_domain::FindingDisposition;

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
        std::fs::write(dir.join("go.mod"), "module example.com/s6\n\ngo 1.21\n").unwrap();
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

    #[test]
    fn forged_accept_pass_without_execution_is_blocked() {
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
                objective: "basic".into(),
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

        for f in gen.review.findings {
            let d = if f.disposition == FindingDisposition::MustFix {
                FindingDisposition::Resolved
            } else {
                FindingDisposition::AcceptedRisk
            };
            let reason = if d == FindingDisposition::AcceptedRisk {
                Some("ok".into())
            } else {
                None
            };
            let _ = set_finding_disposition(&state, &run_id, &f.id, d, reason);
        }
        approve_test_case(&state, &run_id).unwrap();

        state
            .set_result_decision(
                run_id.clone(),
                ResultDecisionRecord {
                    decision: HumanResultDecision::AcceptPass,
                    execution_id: "last".into(),
                    verdict: Some(ResultVerdict::Pass),
                },
            )
            .unwrap();
        let run = state
            .with_store(|s| s.get_run(parse_run_id(&run_id).unwrap()))
            .unwrap();
        state
            .with_store(|s| {
                s.update_run_completion(
                    run.id,
                    WorkflowState::ReadyToApply,
                    DeliveryStatus::NotDelivered,
                    Some(ResultVerdict::Pass),
                    run.worktree_path.as_deref(),
                )
            })
            .unwrap();

        let guard = apply_guard_status(&state, &run_id).unwrap();
        assert!(!guard.enabled);
        assert!(apply_patch(&state, &run_id).is_err());
    }

    #[test]
    fn head_drift_blocks_apply_and_successor_replays() {
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
                objective: "basic".into(),
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
        for f in gen.review.findings {
            let d = if f.disposition == FindingDisposition::MustFix {
                FindingDisposition::Resolved
            } else {
                FindingDisposition::AcceptedRisk
            };
            let reason = if d == FindingDisposition::AcceptedRisk {
                Some("ok".into())
            } else {
                None
            };
            let _ = set_finding_disposition(&state, &run_id, &f.id, d, reason);
        }
        approve_test_case(&state, &run_id).unwrap();

        let run = state
            .with_store(|s| s.get_run(parse_run_id(&run_id).unwrap()))
            .unwrap();
        let old_base = run.base_commit.clone();

        // Drift main HEAD with an unrelated commit
        std::fs::write(repo.path().join("drift.txt"), "drift\n").unwrap();
        Command::new("git")
            .args(["add", "drift.txt"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "drift"])
            .current_dir(repo.path())
            .status()
            .unwrap();

        let guard = apply_guard_status(&state, &run_id).unwrap();
        assert!(!guard.enabled);
        assert!(guard.head_drift);
        assert!(guard.can_create_successor);
        assert!(apply_patch(&state, &run_id).is_err());

        let succ = create_successor_run(&state, &run_id).unwrap();
        assert_ne!(succ.new_base_commit, old_base);
        assert_eq!(succ.old_base_commit, old_base);
        assert!(!succ.candidate.candidate_commit.is_empty());
        assert!(
            succ.candidate
                .changed_paths
                .iter()
                .any(|p| p.contains("testharbor_generated") || p.ends_with("_test.go")),
            "replayed test files: {:?}",
            succ.candidate.changed_paths
        );

        let parent = state
            .with_store(|s| s.get_run(parse_run_id(&run_id).unwrap()))
            .unwrap();
        assert_eq!(parent.workflow_state, WorkflowState::Superseded);
        let pointers: testharbor_domain::RunPointers =
            serde_json::from_str(&parent.pointers_json).unwrap();
        assert_eq!(
            pointers.successor_run_id.as_deref(),
            Some(succ.successor_run_id.as_str())
        );

        // New run has no case approval
        assert!(state.get_case_approval(&succ.successor_run_id).is_none());
        assert_eq!(succ.workflow_state, "PendingTestReview");

        // Idempotent re-call returns same successor
        let again = create_successor_run(&state, &run_id).unwrap();
        assert_eq!(again.successor_run_id, succ.successor_run_id);

        // After approving successor, Apply should work on new base
        for f in again.review.findings {
            let d = if f.disposition == FindingDisposition::MustFix {
                FindingDisposition::Resolved
            } else {
                FindingDisposition::AcceptedRisk
            };
            let reason = if d == FindingDisposition::AcceptedRisk {
                Some("ok".into())
            } else {
                None
            };
            let _ = set_finding_disposition(&state, &succ.successor_run_id, &f.id, d, reason);
        }
        approve_test_case(&state, &succ.successor_run_id).unwrap();
        let successor_guard = apply_guard_status(&state, &succ.successor_run_id).unwrap();
        assert!(!successor_guard.enabled);
        assert!(apply_patch(&state, &succ.successor_run_id).is_err());
    }
}
