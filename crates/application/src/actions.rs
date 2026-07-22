//! Single action evaluator shared by desktop, CLI, and typed commands.

use crate::{apply_guard_status, cancel_execution, cancel_generation, AppState};
use serde::{Deserialize, Serialize};
use testharbor_domain::{
    try_transition, DeliveryStatus, GuardReason, RunId, UserAction, UserActionDescriptor,
    WorkflowEvent, WorkflowState,
};
use testharbor_storage::StorageError;
use uuid::Uuid;

fn parse_run_id(value: &str) -> Result<RunId, String> {
    Uuid::parse_str(value)
        .map(RunId)
        .map_err(|_| "invalid run id".to_string())
}

fn descriptor(
    action: UserAction,
    label: &str,
    enabled: bool,
    reason: GuardReason,
    next_state: Option<WorkflowState>,
) -> UserActionDescriptor {
    let mut descriptor = if enabled {
        UserActionDescriptor::enabled(action, label, next_state)
    } else {
        UserActionDescriptor::disabled(action, label, vec![reason])
    };
    descriptor.next_state = next_state;
    descriptor
}

fn has_flaky_evidence(state: &AppState, run_id: RunId) -> bool {
    let Ok(history) = state.with_store(|store| store.list_execution_history(run_id)) else {
        return false;
    };
    let Some(scope) = history
        .iter()
        .rev()
        .find(|item| item.purpose == "formal" && item.status == "complete")
        .map(|item| item.scope_hash.as_str())
    else {
        return false;
    };
    let mut count = 0;
    let mut pass = false;
    let mut fail = false;
    for item in history.iter().filter(|item| {
        item.purpose == "formal" && item.status == "complete" && item.scope_hash == scope
    }) {
        count += 1;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&item.payload_json) else {
            continue;
        };
        let exit_code = value
            .pointer("/generic/outcome/exit_code")
            .or_else(|| value.pointer("/outcome/exit_code"))
            .and_then(serde_json::Value::as_i64);
        let failures = value
            .pointer("/structured/failed")
            .and_then(serde_json::Value::as_u64);
        let empty = value
            .pointer("/structured/empty")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let succeeded = exit_code == Some(0) && failures == Some(0) && !empty;
        pass |= succeeded;
        fail |= !succeeded;
    }
    count >= 2 && pass && fail
}

pub fn get_run_actions(
    state: &AppState,
    run_id: &str,
) -> Result<Vec<UserActionDescriptor>, String> {
    let rid = parse_run_id(run_id)?;
    let run = state
        .with_store(|store| store.get_run(rid))
        .map_err(|e| e.to_string())?;
    let wrong = |expected: &str| GuardReason::WrongWorkflowState {
        expected: expected.into(),
        actual: format!("{:?}", run.workflow_state),
    };
    let legacy = run.legacy_read_only;
    let candidate = state.get_candidate(run_id);
    let approval = state.get_case_approval(run_id);
    let last_execution = state.get_last_execution(run_id);
    let active = state.get_active_execution(run_id);
    let approve_ready = crate::get_case_review_detail(state, run_id)
        .map(|detail| detail.can_approve)
        .unwrap_or(false);
    let trust_valid = state
        .get_trust_grant(run_id)
        .and_then(|grant| {
            state
                .with_store(|store| store.get_run_snapshot_json(rid))
                .ok()
                .flatten()
                .and_then(|json| serde_json::from_str::<testharbor_domain::RunSnapshot>(&json).ok())
                .map(|snapshot| {
                    snapshot.required_trust_scope_hash == grant.trust_scope_hash
                        || crate::is_deferred_runner_hash(&snapshot.required_trust_scope_hash)
                })
        })
        .unwrap_or(false);
    let accept_ready = crate::reevaluate_accept_pass(state, run_id)
        .map(|view| view.enabled)
        .unwrap_or(false)
        && last_execution.as_ref().is_some_and(|execution| {
            execution.agent_error.is_none()
                && execution.analysis.is_some()
                && execution.result_review.is_some()
        });
    let has_result = last_execution.is_some();
    let pending_result = run.workflow_state == WorkflowState::PendingResultReview;

    let mut actions = vec![
        descriptor(
            UserAction::Approve,
            "批准测试",
            !legacy
                && run.workflow_state == WorkflowState::PendingTestReview
                && approve_ready
                && trust_valid,
            if legacy {
                GuardReason::PolicyBlocked
            } else if !trust_valid {
                GuardReason::TrustGrantInvalid
            } else if !approve_ready {
                GuardReason::UnresolvedMustFix
            } else {
                wrong("pending_test_review")
            },
            Some(WorkflowState::ExecutionReady),
        ),
        descriptor(
            UserAction::RequestChanges,
            "要求修改",
            !legacy && run.workflow_state == WorkflowState::PendingTestReview,
            if legacy {
                GuardReason::PolicyBlocked
            } else {
                wrong("pending_test_review")
            },
            Some(WorkflowState::GeneratingTests),
        ),
        descriptor(
            UserAction::StartExecution,
            "开始正式执行",
            !legacy
                && run.workflow_state == WorkflowState::ExecutionReady
                && approval.is_some()
                && trust_valid
                && active.is_none(),
            if active.is_some() {
                GuardReason::ActiveExecutionExists
            } else if approval.is_none() {
                GuardReason::NotApprovedRevision
            } else if !trust_valid {
                GuardReason::TrustGrantInvalid
            } else if legacy {
                GuardReason::PolicyBlocked
            } else {
                wrong("execution_ready")
            },
            Some(WorkflowState::ExecutingTests),
        ),
        descriptor(
            UserAction::AcceptPass,
            "接受通过",
            !legacy && run.workflow_state == WorkflowState::PendingResultReview && accept_ready,
            if legacy {
                GuardReason::PolicyBlocked
            } else {
                GuardReason::ResultNotComplete
            },
            Some(WorkflowState::ReadyToApply),
        ),
        descriptor(
            UserAction::RequestTestChange,
            "修改测试",
            !legacy && run.workflow_state == WorkflowState::PendingResultReview,
            if legacy {
                GuardReason::PolicyBlocked
            } else {
                wrong("pending_result_review")
            },
            Some(WorkflowState::GeneratingTests),
        ),
        descriptor(
            UserAction::AcceptFail,
            "接受失败",
            !legacy && pending_result && has_result,
            if legacy {
                GuardReason::PolicyBlocked
            } else if !has_result {
                GuardReason::ExecutionNotCurrent
            } else {
                wrong("pending_result_review")
            },
            Some(WorkflowState::Completed),
        ),
        descriptor(
            UserAction::MarkProductBug,
            "标记产品缺陷",
            !legacy && pending_result && has_result,
            if legacy {
                GuardReason::PolicyBlocked
            } else if !has_result {
                GuardReason::ExecutionNotCurrent
            } else {
                wrong("pending_result_review")
            },
            Some(WorkflowState::Completed),
        ),
        descriptor(
            UserAction::MarkEnvironmentIssue,
            "标记环境问题",
            !legacy && pending_result && has_result,
            if legacy {
                GuardReason::PolicyBlocked
            } else if !has_result {
                GuardReason::ExecutionNotCurrent
            } else {
                wrong("pending_result_review")
            },
            Some(WorkflowState::NeedsHuman),
        ),
        descriptor(
            UserAction::MarkFlaky,
            "标记 Flaky",
            !legacy && pending_result && has_flaky_evidence(state, rid),
            if legacy {
                GuardReason::PolicyBlocked
            } else {
                GuardReason::FlakyEvidenceInsufficient
            },
            Some(WorkflowState::Completed),
        ),
        descriptor(
            UserAction::Rerun,
            "重新执行",
            !legacy
                && matches!(
                    run.workflow_state,
                    WorkflowState::PendingResultReview
                        | WorkflowState::NeedsHuman
                        | WorkflowState::ReadyToApply
                )
                && approval.is_some()
                && candidate.is_some()
                && active.is_none(),
            if legacy {
                GuardReason::PolicyBlocked
            } else if active.is_some() {
                GuardReason::ActiveExecutionExists
            } else {
                GuardReason::NotApprovedRevision
            },
            Some(WorkflowState::ExecutionReady),
        ),
        descriptor(
            UserAction::EndWithoutDecision,
            "不作结论并结束",
            !legacy && pending_result,
            if legacy {
                GuardReason::PolicyBlocked
            } else {
                wrong("pending_result_review")
            },
            Some(WorkflowState::Cancelled),
        ),
        descriptor(
            UserAction::Reject,
            "拒绝测试候选",
            !legacy && run.workflow_state == WorkflowState::PendingTestReview,
            if legacy {
                GuardReason::PolicyBlocked
            } else {
                wrong("pending_test_review")
            },
            Some(WorkflowState::Rejected),
        ),
        descriptor(
            UserAction::ConfirmProject,
            "确认项目与 Trust",
            !legacy && run.workflow_state == WorkflowState::WaitingProjectConfirmation,
            if legacy {
                GuardReason::PolicyBlocked
            } else {
                wrong("waiting_project_confirmation")
            },
            Some(WorkflowState::PreparingWorktree),
        ),
    ];

    let apply = apply_guard_status(state, run_id).ok();
    let mut apply_action = descriptor(
        UserAction::Apply,
        "Apply 到主仓",
        !legacy && apply.as_ref().is_some_and(|guard| guard.enabled),
        if legacy {
            GuardReason::PolicyBlocked
        } else if apply
            .as_ref()
            .is_some_and(|guard| guard.main_clean == Some(false))
        {
            GuardReason::MainWorktreeDirty
        } else if apply.as_ref().is_some_and(|guard| guard.head_drift) {
            GuardReason::HeadNotBaseCommit
        } else {
            GuardReason::LatestDecisionNotAcceptPass
        },
        Some(WorkflowState::ApplyingRevision),
    );
    apply_action.confirmation_summary =
        "将重新检查主仓 clean、HEAD、candidate tree、批准和正式执行 scope".into();
    apply_action.destructive = true;
    actions.push(apply_action);

    let mut export = descriptor(
        UserAction::Export,
        "导出补丁",
        candidate.is_some(),
        GuardReason::RevisionNotCurrent,
        None,
    );
    export.preserves = vec!["run_snapshot".into(), "revision_history".into()];
    actions.push(export);

    let discard_allowed = run.workflow_state == WorkflowState::ReadyToApply;
    let mut discard = descriptor(
        UserAction::Discard,
        "丢弃交付",
        !legacy && discard_allowed,
        if legacy {
            GuardReason::PolicyBlocked
        } else {
            wrong("ready_to_apply")
        },
        Some(WorkflowState::Completed),
    );
    discard.destructive = true;
    discard.preserves = vec!["audit_history".into(), "finalized_artifacts".into()];
    discard.invalidates = vec!["run_worktree".into()];
    actions.push(discard);

    let retryable = !legacy
        && run.workflow_state == WorkflowState::NeedsHuman
        && state.get_generation_recovery(run_id).is_some();
    let mut retry = descriptor(
        UserAction::Retry,
        "重试阻塞阶段",
        retryable,
        if legacy {
            GuardReason::PolicyBlocked
        } else {
            wrong("needs_human_with_checkpoint")
        },
        state
            .get_generation_recovery(run_id)
            .map(|recovery| recovery.target),
    );
    retry.preserves = vec!["run_snapshot".into(), "finalized_checkpoint".into()];
    actions.push(retry);

    let successor_allowed = apply
        .as_ref()
        .is_some_and(|guard| guard.can_create_successor);
    let mut successor = descriptor(
        UserAction::CreateSuccessorRun,
        "从当前 HEAD 创建 Successor",
        !legacy && successor_allowed,
        if legacy {
            GuardReason::PolicyBlocked
        } else {
            GuardReason::HeadNotBaseCommit
        },
        Some(WorkflowState::Superseded),
    );
    successor.invalidates = vec![
        "case_approval".into(),
        "formal_execution".into(),
        "result_decision".into(),
    ];
    successor.preserves = vec!["predecessor_history".into(), "intent_content".into()];
    actions.push(successor);

    let stop_allowed =
        try_transition(run.workflow_state, WorkflowEvent::StopCurrentActivity).is_ok();
    let mut stop = descriptor(
        UserAction::StopCurrentActivity,
        "停止当前活动",
        !legacy && stop_allowed,
        if legacy {
            GuardReason::PolicyBlocked
        } else {
            wrong("active_activity")
        },
        Some(WorkflowState::NeedsHuman),
    );
    stop.preserves = vec!["last_finalized_checkpoint".into(), "artifacts".into()];
    actions.push(stop);

    let end_transition = try_transition(run.workflow_state, WorkflowEvent::EndRun).ok();
    let mut end = descriptor(
        UserAction::EndRun,
        "结束 Run",
        !legacy && end_transition.is_some(),
        if legacy {
            GuardReason::PolicyBlocked
        } else {
            wrong("non_terminal_not_applying")
        },
        end_transition.map(|transition| transition.to),
    );
    end.destructive = true;
    end.preserves = vec!["audit_history".into(), "finalized_artifacts".into()];
    actions.push(end);

    for action in &mut actions {
        match action.action {
            UserAction::RequestChanges | UserAction::RequestTestChange => {
                action.invalidates = vec![
                    "case_approval".into(),
                    "intended_manifest".into(),
                    "formal_execution".into(),
                    "result_decision".into(),
                ];
                action.preserves = vec![
                    "run_snapshot".into(),
                    "revision_history".into(),
                    "execution_history".into(),
                ];
            }
            UserAction::Rerun => {
                action.invalidates =
                    vec!["current_formal_execution".into(), "result_decision".into()];
                action.preserves = vec!["approved_revision".into(), "execution_history".into()];
            }
            _ => {}
        }
    }

    for action in &mut actions {
        if let Some(candidate) = &candidate {
            action
                .evidence_refs
                .push(format!("candidate-tree:{}", candidate.candidate_tree));
        }
        if let Some(execution) = &last_execution {
            action
                .evidence_refs
                .push(format!("execution:{}", execution.execution_id));
        }
    }
    Ok(actions)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityActionResult {
    pub workflow_state: String,
    pub message: String,
}

pub fn stop_current_activity(
    state: &AppState,
    run_id: &str,
) -> Result<ActivityActionResult, String> {
    let descriptor = get_run_actions(state, run_id)?
        .into_iter()
        .find(|item| item.action == UserAction::StopCurrentActivity)
        .ok_or_else(|| "stop action unavailable".to_string())?;
    if !descriptor.enabled {
        return Err(format!("stop blocked: {:?}", descriptor.disabled_reasons));
    }
    let rid = parse_run_id(run_id)?;
    let run = state
        .with_store(|store| store.get_run(rid))
        .map_err(|e| e.to_string())?;
    if state.get_active_execution(run_id).is_some() {
        cancel_execution(state, run_id).map_err(|error| error.to_string())?;
    } else {
        cancel_generation(state, run_id).map_err(|error| error.to_string())?;
    }
    let transition = try_transition(run.workflow_state, WorkflowEvent::StopCurrentActivity)
        .map_err(|error| error.to_string())?;
    state
        .with_store_transaction(|store| {
            store.update_run_worktree(run.id, run.worktree_path.as_deref(), transition.to)?;
            store.append_event(
                Some(run.id),
                Some(run.project_id),
                "current_activity_stopped",
                &serde_json::json!({"preserved": "last_finalized_checkpoint"}),
            )?;
            Ok::<(), StorageError>(())
        })
        .map_err(|error| error.to_string())?;
    Ok(ActivityActionResult {
        workflow_state: format!("{:?}", transition.to),
        message: "已停止当前活动，并保留最后一个 finalized checkpoint".into(),
    })
}

pub fn end_run(state: &AppState, run_id: &str) -> Result<ActivityActionResult, String> {
    let rid = parse_run_id(run_id)?;
    let run = state
        .with_store(|store| store.get_run(rid))
        .map_err(|e| e.to_string())?;
    if run.legacy_read_only {
        return Err("legacy run is read-only".into());
    }
    let transition = try_transition(run.workflow_state, WorkflowEvent::EndRun)
        .map_err(|error| error.to_string())?;
    if state.get_active_execution(run_id).is_some() {
        cancel_execution(state, run_id).map_err(|error| error.to_string())?;
    }
    state
        .with_store_transaction(|store| {
            store.update_run_completion(
                run.id,
                transition.to,
                DeliveryStatus::NotDelivered,
                None,
                run.worktree_path.as_deref(),
            )?;
            store.append_event(
                Some(run.id),
                Some(run.project_id),
                "run_ended",
                &serde_json::json!({"verdict": null}),
            )?;
            Ok::<(), StorageError>(())
        })
        .map_err(|error| error.to_string())?;
    Ok(ActivityActionResult {
        workflow_state: format!("{:?}", transition.to),
        message: "Run 已结束；verdict 保持为空，审计历史已保留".into(),
    })
}
