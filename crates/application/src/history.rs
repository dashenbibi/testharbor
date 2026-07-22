//! Run history, cleanup, and startup recovery.

use crate::AppState;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use testharbor_domain::{DeliveryStatus, RunId, WorkflowState};
use testharbor_git_adapter as git;
use testharbor_storage::StorageError;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid id")]
    InvalidId,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, HistoryError>;

/// Coarse filter buckets for UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFilter {
    All,
    InProgress,
    PendingReview,
    ReadyToApply,
    Completed,
    FailedOrCancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunHistoryItem {
    pub run_id: String,
    pub project_id: String,
    pub project_name: String,
    pub project_root: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub workflow_state: String,
    pub delivery_status: String,
    pub result_verdict: Option<String>,
    pub base_commit_short: String,
    pub has_worktree: bool,
    pub worktree_path: Option<String>,
    pub updated_at: String,
    pub created_at: String,
    pub status_bucket: String,
    pub resume_hint: String,
    pub can_retry_generation: bool,
    pub legacy_read_only: bool,
    #[serde(default)]
    pub agent_mode: Option<String>,
    #[serde(default)]
    pub agent_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskUsage {
    pub data_dir: String,
    pub total_bytes: u64,
    pub worktrees_bytes: u64,
    pub artifacts_bytes: u64,
    pub runs_bytes: u64,
    pub db_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupResult {
    pub removed_worktrees: Vec<String>,
    pub removed_cache_dirs: Vec<String>,
    pub removed_run_dirs: Vec<String>,
    pub deleted_runs: Vec<String>,
    pub freed_bytes_estimate: u64,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteRunResult {
    pub run_id: String,
    pub removed_worktree: Option<String>,
    pub removed_run_dir: Option<String>,
    pub freed_bytes_estimate: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub checked_runs: u32,
    pub marked_lost: Vec<String>,
    pub orphan_worktrees_removed: Vec<String>,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEventItem {
    pub id: i64,
    pub event_type: String,
    pub payload_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunAuditHistory {
    pub run: RunHistoryItem,
    pub snapshot: Option<serde_json::Value>,
    pub objects: Vec<serde_json::Value>,
    pub events: Vec<RunEventItem>,
}

fn parse_run_id(s: &str) -> Result<RunId> {
    Uuid::parse_str(s)
        .map(RunId)
        .map_err(|_| HistoryError::InvalidId)
}

fn bucket(state: WorkflowState, delivery: DeliveryStatus) -> &'static str {
    use WorkflowState::*;
    match state {
        ReadyToApply | ApplyingRevision => "ready_to_apply",
        PendingTestReview | PendingResultReview | ExecutionReady => "pending_review",
        Completed => match delivery {
            DeliveryStatus::Applied => "completed",
            DeliveryStatus::Exported => "completed",
            DeliveryStatus::Discarded => "completed",
            DeliveryStatus::NotDelivered => "completed",
        },
        Rejected | Failed | Cancelled | Superseded => "failed_or_cancelled",
        NeedsHuman => "in_progress",
        _ => "in_progress",
    }
}

fn resume_hint(state: WorkflowState) -> String {
    use WorkflowState::*;
    match state {
        PendingTestReview => "继续用例评审 / 批准；未批准候选可重新生成".into(),
        PendingResultReview => "继续结果评审".into(),
        ExecutionReady => "确认 Trust 后执行测试".into(),
        ReadyToApply => "Apply 或导出 Patch；HEAD 漂移则导出或创建 Successor".into(),
        NeedsHuman => "查看阻塞原因并恢复/重跑".into(),
        GeneratingTests | FreezingRevision | ValidatingRevision => {
            "可重新生成或丢弃 worktree".into()
        }
        ExecutingTests => "进程可能已丢失，建议重新执行".into(),
        Completed => "已完成，可查看历史".into(),
        Superseded => "已由 Successor 取代，打开 successor Run 继续".into(),
        Cancelled | Rejected | Failed => "已结束".into(),
        _ => "打开 Run 详情继续".into(),
    }
}

pub fn list_run_history(
    state: &AppState,
    project_id: Option<String>,
    filter: RunFilter,
    limit: usize,
) -> Result<Vec<RunHistoryItem>> {
    let runs = state.with_store(|s| s.list_runs(project_id.as_deref(), limit.max(1)))?;
    let mut out = Vec::new();
    for run in runs {
        let project = state.with_store(|s| s.get_project(run.project_id)).ok();
        let b = bucket(run.workflow_state, run.delivery_status);
        let matches = match filter {
            RunFilter::All => true,
            RunFilter::InProgress => b == "in_progress",
            RunFilter::PendingReview => b == "pending_review",
            RunFilter::ReadyToApply => b == "ready_to_apply",
            RunFilter::Completed => b == "completed",
            RunFilter::FailedOrCancelled => b == "failed_or_cancelled",
        };
        if !matches {
            continue;
        }
        let has_wt = run
            .worktree_path
            .as_ref()
            .map(|p| PathBuf::from(p).is_dir())
            .unwrap_or(false);
        let run_id = run.id.to_string();
        let intent = state.get_intent_meta(&run_id);
        out.push(RunHistoryItem {
            run_id,
            project_id: run.project_id.to_string(),
            project_name: project
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "unknown".into()),
            project_root: project
                .as_ref()
                .map(|p| p.root_path.clone())
                .unwrap_or_default(),
            objective: if run.objective.is_empty() {
                intent
                    .as_ref()
                    .map(|i| i.objective.clone())
                    .filter(|objective| !objective.is_empty())
                    .unwrap_or_else(|| "(无目标)".into())
            } else {
                run.objective
            },
            acceptance_criteria: intent
                .as_ref()
                .map(|i| i.acceptance_criteria.clone())
                .unwrap_or_default(),
            workflow_state: format!("{:?}", run.workflow_state),
            delivery_status: format!("{:?}", run.delivery_status),
            result_verdict: run.result_verdict.map(|v| format!("{v:?}")),
            base_commit_short: run.base_commit.chars().take(7).collect(),
            has_worktree: has_wt,
            worktree_path: run.worktree_path,
            updated_at: run.updated_at.to_rfc3339(),
            created_at: run.created_at.to_rfc3339(),
            status_bucket: b.into(),
            resume_hint: resume_hint(run.workflow_state),
            can_retry_generation: (state.get_generation_recovery(&run.id.to_string()).is_some()
                && run.workflow_state == WorkflowState::NeedsHuman)
                || (run.workflow_state == WorkflowState::PendingTestReview
                    && state.get_case_approval(&run.id.to_string()).is_none()
                    && state.get_pending_change(&run.id.to_string()).is_none()),
            legacy_read_only: run.legacy_read_only,
            agent_mode: intent.as_ref().and_then(|i| i.agent_mode.clone()),
            agent_model: intent.as_ref().and_then(|i| i.agent_model.clone()),
        });
    }
    Ok(out)
}

pub fn list_run_events(state: &AppState, run_id: &str, limit: usize) -> Result<Vec<RunEventItem>> {
    let id = parse_run_id(run_id)?;
    let events = state.with_store(|s| s.list_events_for_run(id))?;
    Ok(events
        .into_iter()
        .rev()
        .take(limit.max(1))
        .map(|e| RunEventItem {
            id: e.id,
            event_type: e.event_type,
            payload_json: e.payload_json,
            created_at: e.created_at.to_rfc3339(),
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect())
}

pub fn get_run_audit_history(state: &AppState, run_id: &str) -> Result<RunAuditHistory> {
    let id = parse_run_id(run_id)?;
    let run = get_run_detail(state, run_id)?;
    let snapshot = state
        .with_store(|store| store.get_run_snapshot_json(id))?
        .and_then(|value| serde_json::from_str(&value).ok());
    let objects = state.with_store(|store| store.list_audit_objects(id))?;
    let events = list_run_events(state, run_id, usize::MAX)?;
    Ok(RunAuditHistory {
        run,
        snapshot,
        objects,
        events,
    })
}

pub fn get_run_detail(state: &AppState, run_id: &str) -> Result<RunHistoryItem> {
    let id = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(id))?;
    let project = state.with_store(|s| s.get_project(run.project_id))?;
    let has_wt = run
        .worktree_path
        .as_ref()
        .map(|p| PathBuf::from(p).is_dir())
        .unwrap_or(false);
    let run_id_text = run.id.to_string();
    let intent = state.get_intent_meta(&run_id_text);
    Ok(RunHistoryItem {
        run_id: run_id_text,
        project_id: run.project_id.to_string(),
        project_name: project.name,
        project_root: project.root_path,
        objective: if run.objective.is_empty() {
            intent
                .as_ref()
                .map(|i| i.objective.clone())
                .filter(|objective| !objective.is_empty())
                .unwrap_or_else(|| "(无目标)".into())
        } else {
            run.objective
        },
        acceptance_criteria: intent
            .as_ref()
            .map(|i| i.acceptance_criteria.clone())
            .unwrap_or_default(),
        workflow_state: format!("{:?}", run.workflow_state),
        delivery_status: format!("{:?}", run.delivery_status),
        result_verdict: run.result_verdict.map(|v| format!("{v:?}")),
        base_commit_short: run.base_commit.chars().take(7).collect(),
        has_worktree: has_wt,
        worktree_path: run.worktree_path,
        updated_at: run.updated_at.to_rfc3339(),
        created_at: run.created_at.to_rfc3339(),
        status_bucket: bucket(run.workflow_state, run.delivery_status).into(),
        resume_hint: resume_hint(run.workflow_state),
        can_retry_generation: (state.get_generation_recovery(&run.id.to_string()).is_some()
            && run.workflow_state == WorkflowState::NeedsHuman)
            || (run.workflow_state == WorkflowState::PendingTestReview
                && state.get_case_approval(&run.id.to_string()).is_none()
                && state.get_pending_change(&run.id.to_string()).is_none()),
        legacy_read_only: run.legacy_read_only,
        agent_mode: intent.as_ref().and_then(|i| i.agent_mode.clone()),
        agent_model: intent.as_ref().and_then(|i| i.agent_model.clone()),
    })
}

pub fn delete_run(state: &AppState, run_id: &str) -> Result<DeleteRunResult> {
    if state.get_active_execution(run_id).is_some() {
        return Err(HistoryError::Other(
            "run has an active execution; cancel it before deleting".into(),
        ));
    }

    let id = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(id))?;
    let project = state.with_store(|s| s.get_project(run.project_id)).ok();
    let paths = state.paths();
    let mut result = DeleteRunResult {
        run_id: run.id.to_string(),
        removed_worktree: None,
        removed_run_dir: None,
        freed_bytes_estimate: 0,
    };

    if let Some(worktree) = &run.worktree_path {
        let path = PathBuf::from(worktree);
        if path.is_dir() {
            crate::workspace::ensure_run_worktree_clean(state, run_id)
                .map_err(|error| HistoryError::Other(error.to_string()))?;
            let size = dir_size(&path);
            if let Some(project) = &project {
                let _ =
                    git::remove_worktree(PathBuf::from(&project.root_path).as_path(), &path, true);
            }
            if path.exists() {
                fs::remove_dir_all(&path)?;
            }
            result.removed_worktree = Some(worktree.clone());
            result.freed_bytes_estimate = result.freed_bytes_estimate.saturating_add(size);
        }
    }

    let run_dir = paths.root.join("runs").join(run.id.to_string());
    if run_dir.is_dir() {
        let size = dir_size(&run_dir);
        fs::remove_dir_all(&run_dir)?;
        result.removed_run_dir = Some(run_dir.display().to_string());
        result.freed_bytes_estimate = result.freed_bytes_estimate.saturating_add(size);
    }

    state.with_store(|s| s.delete_run(run.id))?;
    state.clear_run_cache(run_id);
    Ok(result)
}

fn dir_size(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    let mut total = 0u64;
    let walk = walkdir_simple(path);
    for p in walk {
        if let Ok(meta) = fs::metadata(&p) {
            if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn walkdir_simple(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p.clone());
            }
            out.push(p);
        }
    }
    out
}

pub fn disk_usage(state: &AppState) -> Result<DiskUsage> {
    let paths = state.paths();
    let data_dir = paths.root.clone();
    Ok(DiskUsage {
        data_dir: data_dir.display().to_string(),
        total_bytes: dir_size(&data_dir),
        worktrees_bytes: dir_size(&paths.worktrees_dir),
        artifacts_bytes: dir_size(&paths.artifacts_dir),
        runs_bytes: dir_size(&data_dir.join("runs")),
        db_bytes: fs::metadata(&paths.db_path).map(|m| m.len()).unwrap_or(0),
    })
}

/// Remove worktrees for terminal runs; optionally delete completed run records older than keep_latest.
pub fn cleanup_storage(
    state: &AppState,
    remove_terminal_worktrees: bool,
    delete_completed_beyond: Option<usize>,
) -> Result<CleanupResult> {
    let mut result = CleanupResult {
        removed_worktrees: vec![],
        removed_cache_dirs: vec![],
        removed_run_dirs: vec![],
        deleted_runs: vec![],
        freed_bytes_estimate: 0,
        messages: vec![],
    };

    let runs = state.with_store(|s| s.list_runs(None, 500))?;
    let paths = state.paths();

    // Build caches are disposable and are not part of the test evidence. Older
    // versions placed one under every execution; remove those copies once the
    // execution is no longer active, while preserving all logs and reports.
    for run in &runs {
        if state.get_active_execution(&run.id.to_string()).is_some() {
            continue;
        }
        let run_dir = paths.root.join("runs").join(run.id.to_string());
        for path in walkdir_simple(&run_dir) {
            if !path.is_dir()
                || path
                    .file_name()
                    .map(|name| name != "go-build-cache")
                    .unwrap_or(true)
            {
                continue;
            }
            let size = dir_size(&path);
            if fs::remove_dir_all(&path).is_ok() {
                result.removed_cache_dirs.push(path.display().to_string());
                result.freed_bytes_estimate = result.freed_bytes_estimate.saturating_add(size);
            }
        }
    }
    if !result.removed_cache_dirs.is_empty() {
        result.messages.push(format!(
            "已清理已结束执行的构建缓存（{} 个）",
            result.removed_cache_dirs.len()
        ));
    }

    if remove_terminal_worktrees {
        for run in &runs {
            if !run.workflow_state.is_terminal() {
                continue;
            }
            if let Some(wt) = &run.worktree_path {
                let p = PathBuf::from(wt);
                if p.is_dir() {
                    let sz = dir_size(&p);
                    // Prefer git worktree remove when project still exists
                    if let Ok(project) = state.with_store(|s| s.get_project(run.project_id)) {
                        let _ = git::remove_worktree(
                            PathBuf::from(&project.root_path).as_path(),
                            &p,
                            true,
                        );
                    }
                    if p.exists() {
                        let _ = fs::remove_dir_all(&p);
                    }
                    result.removed_worktrees.push(wt.clone());
                    result.freed_bytes_estimate = result.freed_bytes_estimate.saturating_add(sz);
                    let _ = state
                        .with_store(|s| s.update_run_worktree(run.id, None, run.workflow_state));
                }
            }
        }
        result.messages.push("已清理终态 Run 的 worktree".into());
    }

    if let Some(keep) = delete_completed_beyond {
        let completed: Vec<_> = runs
            .iter()
            .filter(|r| r.workflow_state.is_terminal())
            .cloned()
            .collect();
        if completed.len() > keep {
            for run in completed.into_iter().skip(keep) {
                let run_dir = paths.root.join("runs").join(run.id.to_string());
                let sz = dir_size(&run_dir);
                if run_dir.is_dir() {
                    let _ = fs::remove_dir_all(&run_dir);
                    result.removed_run_dirs.push(run_dir.display().to_string());
                }
                if let Some(wt) = &run.worktree_path {
                    let p = PathBuf::from(wt);
                    if p.is_dir() {
                        let _ = fs::remove_dir_all(&p);
                    }
                }
                let _ = state.with_store(|s| s.delete_run(run.id));
                result.deleted_runs.push(run.id.to_string());
                result.freed_bytes_estimate = result.freed_bytes_estimate.saturating_add(sz);
            }
            result
                .messages
                .push(format!("已删除超出保留数的终态 Run（保留最新 {keep} 条）"));
        }
    }

    // Orphan worktree dirs not referenced
    if paths.worktrees_dir.is_dir() {
        let referenced: std::collections::HashSet<String> = runs
            .iter()
            .filter_map(|r| r.worktree_path.clone())
            .collect();
        if let Ok(rd) = fs::read_dir(&paths.worktrees_dir) {
            for ent in rd.flatten() {
                let p = ent.path();
                if !p.is_dir() {
                    continue;
                }
                let s = p.display().to_string();
                let known = referenced.iter().any(|r| r == &s)
                    || runs
                        .iter()
                        .any(|r| r.id.to_string() == ent.file_name().to_string_lossy());
                if !known {
                    let sz = dir_size(&p);
                    let _ = fs::remove_dir_all(&p);
                    result.removed_worktrees.push(s);
                    result.freed_bytes_estimate = result.freed_bytes_estimate.saturating_add(sz);
                }
            }
        }
    }

    if result.messages.is_empty() {
        result.messages.push("无需清理".into());
    }
    Ok(result)
}

/// On startup: mark executing runs without live process as needing human; prune orphan worktrees lightly.
pub fn recover_on_startup(state: &AppState) -> Result<RecoveryReport> {
    let mut report = RecoveryReport {
        checked_runs: 0,
        marked_lost: vec![],
        orphan_worktrees_removed: vec![],
        messages: vec![],
    };

    let runs = state.with_store(|s| s.list_runs(None, 200))?;
    report.checked_runs = runs.len() as u32;

    for run in &runs {
        use WorkflowState::*;
        let mut execution_result_recovered = false;
        if run.workflow_state == ExecutingTests {
            if let Some(execution) = state.get_last_execution(&run.id.to_string()) {
                if matches!(
                    execution.outcome.phase,
                    testharbor_process_supervisor::TaskPhase::Exited
                        | testharbor_process_supervisor::TaskPhase::TimedOut
                        | testharbor_process_supervisor::TaskPhase::Cancelled
                        | testharbor_process_supervisor::TaskPhase::Failed
                ) {
                    let recovered_state = if execution.agent_error.is_some() {
                        NeedsHuman
                    } else {
                        PendingResultReview
                    };
                    let _ = state.with_store(|store| {
                        store.update_run_worktree(
                            run.id,
                            run.worktree_path.as_deref(),
                            recovered_state,
                        )?;
                        store.append_event(
                            Some(run.id),
                            Some(run.project_id),
                            "execution_result_recovered",
                            &serde_json::json!({
                                "execution_id": execution.execution_id,
                                "workflow_state": format!("{recovered_state:?}"),
                            }),
                        )?;
                        Ok::<(), StorageError>(())
                    });
                    execution_result_recovered = true;
                }
            }
        }
        let live_generation_owner = run.workflow_state == GeneratingTests
            && state.has_live_run_activity(run.id, "generation");
        if !execution_result_recovered
            && !live_generation_owner
            && state.get_active_execution(&run.id.to_string()).is_none()
            && matches!(
                run.workflow_state,
                ExecutingTests | ApplyingRevision | GeneratingTests
            )
        {
            // No process handle after restart → NeedsHuman / lost semantics
            if !matches!(run.workflow_state, WorkflowState::NeedsHuman) {
                let _ = state.with_store(|s| {
                    s.mark_active_process_tasks_for_run(run.id, "lost")?;
                    s.update_run_worktree(
                        run.id,
                        run.worktree_path.as_deref(),
                        WorkflowState::NeedsHuman,
                    )?;
                    s.append_event(
                        Some(run.id),
                        Some(run.project_id),
                        "recovery_marked_needs_human",
                        &serde_json::json!({
                            "previous_state": format!("{:?}", run.workflow_state),
                            "reason": "process handle lost after restart",
                        }),
                    )?;
                    Ok::<(), StorageError>(())
                });
                report.marked_lost.push(run.id.to_string());
            }
            if run.workflow_state == GeneratingTests {
                let _ = state.set_generation_recovery(
                    run.id.to_string(),
                    crate::generate::GenerationRecovery {
                        target: WorkflowState::GeneratingTests,
                        reason: "generation process handle lost after restart".into(),
                    },
                );
            }
        }

        // Clear stale worktree_path if directory missing
        if let Some(wt) = &run.worktree_path {
            if !PathBuf::from(wt).exists() {
                let _ =
                    state.with_store(|s| s.update_run_worktree(run.id, None, run.workflow_state));
            }
        }
    }

    // Light orphan cleanup under worktrees/
    let paths = state.paths();
    if paths.worktrees_dir.is_dir() {
        let ids: std::collections::HashSet<String> =
            runs.iter().map(|r| r.id.to_string()).collect();
        if let Ok(rd) = fs::read_dir(&paths.worktrees_dir) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().to_string();
                if !ids.contains(&name) && ent.path().is_dir() {
                    // only remove if no DB row references this path
                    let referenced = runs.iter().any(|r| {
                        r.worktree_path
                            .as_ref()
                            .map(|p| p == &ent.path().display().to_string())
                            .unwrap_or(false)
                    });
                    if !referenced {
                        let _ = fs::remove_dir_all(ent.path());
                        report
                            .orphan_worktrees_removed
                            .push(ent.path().display().to_string());
                    }
                }
            }
        }
    }

    report.messages.push(format!(
        "恢复检查完成：检查 {} 条 Run，标记 {} 条为 NeedsHuman，清理 {} 个孤儿 worktree",
        report.checked_runs,
        report.marked_lost.len(),
        report.orphan_worktrees_removed.len()
    ));
    state.set_last_recovery(report.clone());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::open_project;
    use crate::AppState;
    use std::process::Command;

    fn init_repo(dir: &std::path::Path) {
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
        std::fs::write(dir.join("a.txt"), "x").unwrap();
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
    fn history_and_cleanup() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;

        let hist = list_run_history(&state, None, RunFilter::All, 50).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].run_id, run_id);

        let usage = disk_usage(&state).unwrap();
        assert!(usage.total_bytes > 0 || usage.db_bytes > 0);

        let rec = recover_on_startup(&state).unwrap();
        assert!(rec.checked_runs >= 1);

        // Mark completed and cleanup worktree path none
        let run = state
            .with_store(|s| s.get_run(uuid::Uuid::parse_str(&run_id).map(RunId).unwrap()))
            .unwrap();
        state
            .with_store(|s| {
                s.update_run_completion(
                    run.id,
                    WorkflowState::Completed,
                    DeliveryStatus::Discarded,
                    None,
                    None,
                )
            })
            .unwrap();

        let cleaned = cleanup_storage(&state, true, Some(0)).unwrap();
        assert!(cleaned.deleted_runs.contains(&run_id) || !cleaned.messages.is_empty());
    }

    #[test]
    fn delete_run_removes_history_and_files() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;
        let run_uuid = uuid::Uuid::parse_str(&run_id).map(RunId).unwrap();
        let worktree = data.path().join("worktrees").join("manual-delete-worktree");
        let run_dir = data.path().join("runs").join(&run_id);
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("artifact.txt"), "x").unwrap();
        state
            .with_store(|s| {
                s.update_run_worktree(
                    run_uuid,
                    Some(worktree.to_str().unwrap()),
                    WorkflowState::Created,
                )
            })
            .unwrap();
        state
            .set_intent_meta(
                run_id.clone(),
                crate::generate::IntentMeta {
                    objective: "delete me".into(),
                    acceptance_criteria: vec!["gone".into()],
                    test_intent_hash: "hash".into(),
                    test_strategy: "e2e_api_flow".into(),
                    cleanup_test_data: true,
                    agent_mode: Some("mock".into()),
                    reuse_agent_session: false,
                    agent_model: None,
                    environment: None,
                },
            )
            .unwrap();

        let deleted = delete_run(&state, &run_id).unwrap();
        assert_eq!(deleted.run_id, run_id);
        assert!(!worktree.exists());
        assert!(!run_dir.exists());
        assert!(list_run_history(&state, None, RunFilter::All, 50)
            .unwrap()
            .is_empty());
        assert!(state.get_intent_meta(&run_id).is_none());
    }

    #[test]
    fn history_restores_persisted_intent() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;
        state
            .set_intent_meta(
                run_id.clone(),
                crate::generate::IntentMeta {
                    objective: "cover checkout".into(),
                    acceptance_criteria: vec!["handles success".into(), "handles failure".into()],
                    test_intent_hash: "hash".into(),
                    test_strategy: "e2e_api_flow".into(),
                    cleanup_test_data: true,
                    agent_mode: Some("mock".into()),
                    reuse_agent_session: false,
                    agent_model: None,
                    environment: None,
                },
            )
            .unwrap();

        let item = get_run_detail(&state, &run_id).unwrap();
        assert_eq!(item.objective, "cover checkout");
        assert_eq!(
            item.acceptance_criteria,
            vec!["handles success".to_string(), "handles failure".to_string()]
        );
    }

    #[test]
    fn codex_generation_session_survives_restart() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;
        state
            .set_codex_generation_session(
                run_id.clone(),
                "019f6a7e-b361-70a0-8c72-61b190313d60".into(),
            )
            .unwrap();

        let reopened = AppState::open_at(data.path()).unwrap();
        assert_eq!(
            reopened.get_codex_generation_session(&run_id).as_deref(),
            Some("019f6a7e-b361-70a0-8c72-61b190313d60")
        );
    }
}
