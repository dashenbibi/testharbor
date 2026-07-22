//! Worktree prepare / freeze / discard use cases.

use crate::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use testharbor_domain::{RunId, WorkflowState};
use testharbor_storage::StorageError;
use testharbor_workspace_manager::{
    self as ws, CandidateRevision, ChangeEntry, WorkspaceError, WorkspaceHandle,
};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum WorkspaceAppError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error("invalid run id")]
    InvalidRunId,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, WorkspaceAppError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeStatus {
    pub run_id: String,
    pub worktree_path: String,
    pub base_commit: String,
    pub workflow_state: String,
    pub changes: Vec<ChangeEntry>,
    pub writable_patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareWorktreeResult {
    pub run_id: String,
    pub worktree_path: String,
    pub base_commit: String,
    pub workflow_state: String,
}

fn parse_run_id(s: &str) -> Result<RunId> {
    Uuid::parse_str(s)
        .map(RunId)
        .map_err(|_| WorkspaceAppError::InvalidRunId)
}

fn handle_from_run(
    state: &AppState,
    run_id: RunId,
) -> Result<(
    testharbor_storage::RunRecord,
    testharbor_storage::ProjectRecord,
    WorkspaceHandle,
)> {
    let run = state.with_store(|s| s.get_run(run_id))?;
    let project = state.with_store(|s| s.get_project(run.project_id))?;
    let worktree_path = run.worktree_path.clone().ok_or_else(|| {
        WorkspaceAppError::Other("run has no worktree; call prepare first".into())
    })?;
    let handle = WorkspaceHandle {
        repo_root: PathBuf::from(&project.root_path),
        worktree_path: PathBuf::from(worktree_path),
        base_commit: run.base_commit.clone(),
        run_id: run.id.to_string(),
    };
    Ok((run, project, handle))
}

/// Create detached worktree for a run at its base_commit.
pub fn prepare_worktree(state: &AppState, run_id: &str) -> Result<PrepareWorktreeResult> {
    let run_id = parse_run_id(run_id)?;
    let run = state.with_store(|s| s.get_run(run_id))?;
    let project = state.with_store(|s| s.get_project(run.project_id))?;

    if let Some(existing) = &run.worktree_path {
        if PathBuf::from(existing).is_dir() {
            testharbor_git_adapter::ensure_info_exclude(
                PathBuf::from(existing).as_path(),
                "/.testharbor/",
            )
            .map_err(|e| WorkspaceAppError::Other(e.to_string()))?;
            return Ok(PrepareWorktreeResult {
                run_id: run.id.to_string(),
                worktree_path: existing.clone(),
                base_commit: run.base_commit.clone(),
                workflow_state: format!("{:?}", run.workflow_state),
            });
        }
    }

    let worktrees_root = state.paths().worktrees_dir;
    std::fs::create_dir_all(&worktrees_root)
        .map_err(|e| WorkspaceAppError::Other(e.to_string()))?;

    // Transition intent: PreparingWorktree then GeneratingTests (ready for agent writes).
    state.with_store(|s| s.update_run_worktree(run.id, None, WorkflowState::PreparingWorktree))?;

    let handle = ws::create_run_worktree(
        PathBuf::from(&project.root_path).as_path(),
        &worktrees_root,
        &run.id.to_string(),
        &run.base_commit,
    )?;

    let path_str = handle.worktree_path.display().to_string();
    testharbor_git_adapter::ensure_info_exclude(&handle.worktree_path, "/.testharbor/")
        .map_err(|e| WorkspaceAppError::Other(e.to_string()))?;
    state.with_store(|s| {
        s.update_run_worktree(run.id, Some(&path_str), WorkflowState::GeneratingTests)?;
        s.append_event(
            Some(run.id),
            Some(project.id),
            "worktree_prepared",
            &serde_json::json!({
                "worktree_path": path_str,
                "base_commit": run.base_commit,
            }),
        )?;
        Ok::<(), StorageError>(())
    })?;

    Ok(PrepareWorktreeResult {
        run_id: run.id.to_string(),
        worktree_path: path_str,
        base_commit: run.base_commit,
        workflow_state: format!("{:?}", WorkflowState::GeneratingTests),
    })
}

pub fn get_worktree_status(state: &AppState, run_id: &str) -> Result<WorktreeStatus> {
    let run_id = parse_run_id(run_id)?;
    let (run, _project, handle) = handle_from_run(state, run_id)?;
    let patterns = ws::default_writable_patterns();
    let changes = ws::list_changes(&handle, &patterns)?;
    Ok(WorktreeStatus {
        run_id: run.id.to_string(),
        worktree_path: handle.worktree_path.display().to_string(),
        base_commit: handle.base_commit,
        workflow_state: format!("{:?}", run.workflow_state),
        changes,
        writable_patterns: patterns,
    })
}

/// Never remove a worktree that contains changes not owned by TestHarbor.
/// Another CLI may be using the same isolated directory, so destructive
/// actions must stop before calling git worktree remove or deleting files.
pub(crate) fn ensure_run_worktree_clean(state: &AppState, run_id: &str) -> Result<()> {
    let run_id = parse_run_id(run_id)?;
    let (_run, _project, handle) = handle_from_run(state, run_id)?;
    if !handle.worktree_path.is_dir() {
        return Ok(());
    }
    // A few legacy/imported Run records can point to a plain directory rather
    // than a Git worktree. There is no Git state for TestHarbor to inspect in
    // that case; retain the existing filesystem cleanup behavior.
    if !handle.worktree_path.join(".git").exists() {
        return Ok(());
    }
    let changes = ws::list_changes(&handle, &ws::default_writable_patterns())?;
    let protected_changes = changes
        .iter()
        .filter(|change| !change.writable || change.denied)
        .collect::<Vec<_>>();
    if protected_changes.is_empty() {
        return Ok(());
    }
    let paths = protected_changes
        .iter()
        .map(|change| change.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(WorkspaceAppError::Other(format!(
        "当前 Run 的隔离 worktree 存在未提交修改（可能来自其他 CLI）：{paths}。为保护这些修改，TestHarbor 未清理该目录。"
    )))
}

/// Demo/helper: write a file in worktree (must pass path policy when freezing).
pub fn write_in_worktree(
    state: &AppState,
    run_id: &str,
    rel_path: &str,
    content: &str,
) -> Result<()> {
    let run_id = parse_run_id(run_id)?;
    let (_run, _project, handle) = handle_from_run(state, run_id)?;
    ws::write_worktree_file(&handle, rel_path, content)?;
    state.with_store(|s| {
        s.append_event(
            Some(run_id),
            None,
            "worktree_file_written",
            &serde_json::json!({ "path": rel_path }),
        )
    })?;
    Ok(())
}

pub fn freeze_worktree_candidate(
    state: &AppState,
    run_id: &str,
    sequence: u32,
) -> Result<CandidateRevision> {
    let run_id = parse_run_id(run_id)?;
    let (run, project, handle) = handle_from_run(state, run_id)?;
    let patterns = ws::default_writable_patterns();

    state.with_store(|s| {
        s.update_run_worktree(
            run.id,
            run.worktree_path.as_deref(),
            WorkflowState::FreezingRevision,
        )
    })?;

    let candidate = ws::freeze_candidate(
        &handle,
        sequence,
        &patterns,
        &format!("testharbor candidate r{sequence}"),
    )?;

    state.with_store(|s| {
        // After freeze, treat as validated candidate ready for review pipeline.
        s.update_run_worktree(
            run.id,
            run.worktree_path.as_deref(),
            WorkflowState::PendingTestReview,
        )?;
        s.append_event(
            Some(run.id),
            Some(project.id),
            "candidate_frozen",
            &candidate,
        )?;
        Ok::<(), StorageError>(())
    })?;

    Ok(candidate)
}

pub fn discard_run_worktree(state: &AppState, run_id: &str) -> Result<()> {
    ensure_run_worktree_clean(state, run_id)?;
    let run_id = parse_run_id(run_id)?;
    let (run, project, handle) = handle_from_run(state, run_id)?;
    ws::discard_worktree(&handle)?;
    state.with_store(|s| {
        s.update_run_worktree(run.id, None, WorkflowState::Cancelled)?;
        s.append_event(
            Some(run.id),
            Some(project.id),
            "worktree_discarded",
            &serde_json::json!({}),
        )?;
        Ok::<(), StorageError>(())
    })?;
    Ok(())
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
        Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
            .current_dir(dir)
            .status()
            .unwrap();
        std::fs::write(dir.join("lib.go"), "package lib\n").unwrap();
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
    fn prepare_write_freeze_discard() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;

        let prep = prepare_worktree(&state, &run_id).unwrap();
        assert!(PathBuf::from(&prep.worktree_path).join("lib.go").exists());

        write_in_worktree(&state, &run_id, "lib_test.go", "package lib\n").unwrap();

        let status = get_worktree_status(&state, &run_id).unwrap();
        assert!(status
            .changes
            .iter()
            .any(|c| c.path == "lib_test.go" && c.writable));

        let cand = freeze_worktree_candidate(&state, &run_id, 1).unwrap();
        assert!(!cand.candidate_tree.is_empty());

        discard_run_worktree(&state, &run_id).unwrap();
        assert!(!PathBuf::from(prep.worktree_path).exists());
    }
}
