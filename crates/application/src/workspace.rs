//! Worktree prepare / freeze / discard use cases.

use crate::AppState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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

const RUNTIME_CONTEXT_FILE_LIMIT: u64 = 2 * 1024 * 1024;
const RUNTIME_CONTEXT_TOTAL_LIMIT: u64 = 16 * 1024 * 1024;

fn is_runtime_context_path(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let config_extension = matches!(
        extension.as_str(),
        "env" | "ini" | "json" | "php" | "properties" | "toml" | "yaml" | "yml"
    );
    let under_config_dir = path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("config")
    });
    let under_runtime_config = path.starts_with(Path::new("rtp/config"));
    let under_runtime_db = path.starts_with(Path::new("rtp/db")) && extension == "sql";
    let root_level_markdown = path
        .parent()
        .is_some_and(|parent| parent.as_os_str().is_empty())
        && matches!(extension.as_str(), "md" | "markdown");
    let env_file = file_name == ".env" || file_name.starts_with(".env.");
    let named_config = file_name == "config"
        || file_name.starts_with("config.")
        || (file_name.contains("config") && config_extension);

    under_runtime_config
        || under_runtime_db
        || root_level_markdown
        || env_file
        || (config_extension && (under_config_dir || named_config))
}

/// Mirror safe, local-only runtime context from the main worktree into the
/// detached Run worktree. The copy is deliberately read-only to Git: every
/// mirrored path is added to that worktree's local exclude file, so it can be
/// read by a CLI/test runner without becoming candidate code or requiring a
/// commit in the user's project.
pub(crate) fn sync_runtime_context(repo_root: &Path, worktree: &Path) -> Result<Vec<String>> {
    let candidates = testharbor_git_adapter::list_runtime_context_paths(repo_root)
        .map_err(|error| WorkspaceAppError::Other(error.to_string()))?;
    let mut copied = Vec::new();
    let mut total_bytes = 0_u64;

    for relative in candidates {
        let path = Path::new(&relative);
        if !is_runtime_context_path(path) {
            continue;
        }
        let source = repo_root.join(path);
        let destination = worktree.join(path);
        let metadata = match std::fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.file_type().is_file() || metadata.len() > RUNTIME_CONTEXT_FILE_LIMIT {
            continue;
        }
        if destination.exists() {
            continue;
        }
        if total_bytes.saturating_add(metadata.len()) > RUNTIME_CONTEXT_TOTAL_LIMIT {
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| WorkspaceAppError::Other(error.to_string()))?;
        }
        std::fs::copy(&source, &destination)
            .map_err(|error| WorkspaceAppError::Other(error.to_string()))?;
        let mut permissions = std::fs::metadata(&destination)
            .map_err(|error| WorkspaceAppError::Other(error.to_string()))?
            .permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&destination, permissions)
            .map_err(|error| WorkspaceAppError::Other(error.to_string()))?;
        testharbor_git_adapter::ensure_info_exclude(
            worktree,
            &format!("/{}", relative.replace(std::path::MAIN_SEPARATOR, "/")),
        )
        .map_err(|error| WorkspaceAppError::Other(error.to_string()))?;
        total_bytes += metadata.len();
        copied.push(relative);
    }

    Ok(copied)
}

const WORKING_TREE_OVERLAY_FILE_LIMIT: u64 = 4 * 1024 * 1024;
const WORKING_TREE_OVERLAY_TOTAL_LIMIT: u64 = 32 * 1024 * 1024;

fn is_likely_generated_test_path(path: &Path) -> bool {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let in_test_directory = normalized == "test"
        || normalized.starts_with("test/")
        || normalized == "tests"
        || normalized.starts_with("tests/");
    let conventional_name = file_name == "test.php"
        || file_name == "test.py"
        || file_name.contains("_test.")
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
        || file_name.starts_with("test_")
        || file_name.ends_with("test.java")
        || file_name.ends_with("test.kt")
        || file_name.ends_with("test.php");
    in_test_directory || conventional_name
}

/// Overlay current uncommitted product/source files into the isolated Run so
/// the Agent can test the user's working-tree changes without requiring a
/// commit. Test-shaped files are left for normal candidate generation and
/// remain writable; source overlays are read-only and locally excluded.
pub(crate) fn sync_working_tree_overlay(
    repo_root: &Path,
    worktree: &Path,
    base_commit: &str,
) -> Result<Vec<String>> {
    let candidates = testharbor_git_adapter::list_working_tree_overlay_paths(repo_root)
        .map_err(|error| WorkspaceAppError::Other(error.to_string()))?;
    let existing_changes =
        testharbor_git_adapter::list_changed_paths_from_base(worktree, base_commit)
            .map_err(|error| WorkspaceAppError::Other(error.to_string()))?
            .into_iter()
            .map(|change| change.path)
            .collect::<std::collections::BTreeSet<_>>();
    let mut copied = Vec::new();
    let mut total_bytes = 0_u64;

    for relative in candidates {
        let path = Path::new(&relative);
        if is_runtime_context_path(path)
            || is_likely_generated_test_path(path)
            || existing_changes.contains(&relative)
        {
            continue;
        }
        let source = repo_root.join(path);
        let destination = worktree.join(path);
        let metadata = match std::fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.file_type().is_file()
            || metadata.len() > WORKING_TREE_OVERLAY_FILE_LIMIT
            || total_bytes.saturating_add(metadata.len()) > WORKING_TREE_OVERLAY_TOTAL_LIMIT
        {
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| WorkspaceAppError::Other(error.to_string()))?;
        }
        std::fs::copy(&source, &destination)
            .map_err(|error| WorkspaceAppError::Other(error.to_string()))?;
        let mut permissions = std::fs::metadata(&destination)
            .map_err(|error| WorkspaceAppError::Other(error.to_string()))?
            .permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&destination, permissions)
            .map_err(|error| WorkspaceAppError::Other(error.to_string()))?;
        testharbor_git_adapter::mark_skip_worktree(worktree, &relative)
            .map_err(|error| WorkspaceAppError::Other(error.to_string()))?;
        testharbor_git_adapter::ensure_info_exclude(
            worktree,
            &format!("/{}", relative.replace(std::path::MAIN_SEPARATOR, "/")),
        )
        .map_err(|error| WorkspaceAppError::Other(error.to_string()))?;
        total_bytes += metadata.len();
        copied.push(relative);
    }

    Ok(copied)
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
            let mirrored =
                sync_runtime_context(Path::new(&project.root_path), Path::new(existing))?;
            let overlay = sync_working_tree_overlay(
                Path::new(&project.root_path),
                Path::new(existing),
                &run.base_commit,
            )?;
            if !mirrored.is_empty() || !overlay.is_empty() {
                state.with_store(|s| {
                    s.append_event(
                        Some(run.id),
                        Some(project.id),
                        "runtime_context_mirrored",
                        &serde_json::json!({
                            "runtime_context_paths": mirrored,
                            "working_tree_overlay_paths": overlay,
                        }),
                    )
                })?;
            }
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
    let mirrored = sync_runtime_context(Path::new(&project.root_path), &handle.worktree_path)?;
    let overlay = sync_working_tree_overlay(
        Path::new(&project.root_path),
        &handle.worktree_path,
        &run.base_commit,
    )?;
    state.with_store(|s| {
        s.update_run_worktree(run.id, Some(&path_str), WorkflowState::GeneratingTests)?;
        s.append_event(
            Some(run.id),
            Some(project.id),
            "worktree_prepared",
            &serde_json::json!({
                "worktree_path": path_str,
                "base_commit": run.base_commit,
                "runtime_context_paths": mirrored,
                "working_tree_overlay_paths": overlay,
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

    #[test]
    fn prepare_mirrors_ignored_runtime_context_without_candidate_changes() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        std::fs::write(repo.path().join(".gitignore"), "rtp/config/\n").unwrap();
        std::fs::create_dir_all(repo.path().join("rtp/config")).unwrap();
        std::fs::write(
            repo.path().join("rtp/config/eo_config.php"),
            "<?php return ['db' => 'local'];\n",
        )
        .unwrap();
        std::fs::write(repo.path().join("技术实现方案.md"), "runtime notes\n").unwrap();

        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;
        let prep = prepare_worktree(&state, &run_id).unwrap();
        let worktree = PathBuf::from(&prep.worktree_path);

        assert_eq!(
            std::fs::read_to_string(worktree.join("rtp/config/eo_config.php")).unwrap(),
            "<?php return ['db' => 'local'];\n"
        );
        assert!(worktree.join("技术实现方案.md").is_file());
        assert!(std::fs::metadata(worktree.join("rtp/config/eo_config.php"))
            .unwrap()
            .permissions()
            .readonly());
        let status = get_worktree_status(&state, &run_id).unwrap();
        assert!(!status
            .changes
            .iter()
            .any(|change| change.path == "rtp/config/eo_config.php"));
        assert!(!status
            .changes
            .iter()
            .any(|change| change.path == "技术实现方案.md"));

        discard_run_worktree(&state, &run_id).unwrap();
    }

    #[test]
    fn prepare_overlays_uncommitted_product_sources_without_freezing_them() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        std::fs::create_dir_all(repo.path().join("server/admin")).unwrap();
        std::fs::write(
            repo.path().join("server/admin/Setting.php"),
            "<?php return 'updated';\n",
        )
        .unwrap();
        std::fs::write(
            repo.path().join("devops_sync_api.yaml"),
            "endpoint: /api/sync\n",
        )
        .unwrap();

        let state = AppState::open_at(data.path()).unwrap();
        let opened = open_project(&state, repo.path(), true).unwrap();
        let run_id = opened.created_run.unwrap().run_id;
        let prep = prepare_worktree(&state, &run_id).unwrap();
        let worktree = PathBuf::from(&prep.worktree_path);

        assert_eq!(
            std::fs::read_to_string(worktree.join("server/admin/Setting.php")).unwrap(),
            "<?php return 'updated';\n"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("devops_sync_api.yaml")).unwrap(),
            "endpoint: /api/sync\n"
        );
        let status = get_worktree_status(&state, &run_id).unwrap();
        assert!(!status
            .changes
            .iter()
            .any(|change| change.path == "server/admin/Setting.php"));
        assert!(!status
            .changes
            .iter()
            .any(|change| change.path == "devops_sync_api.yaml"));

        discard_run_worktree(&state, &run_id).unwrap();
    }
}
