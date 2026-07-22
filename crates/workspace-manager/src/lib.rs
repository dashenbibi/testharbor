//! Worktree lifecycle + candidate freeze with path policy.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use testharbor_git_adapter::{self as git, ChangedPath, GitError};
use testharbor_path_policy::{
    normalize_repo_path, path_matches_any, validate_repo_path_on_disk, PathPolicyError,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Path(#[from] PathPolicyError),
    #[error("path policy violation: {0}")]
    PolicyViolation(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, WorkspaceError>;

/// Default writable globs for generated tests. Keep this test-oriented:
/// agents may create/modify tests, but product code remains protected unless a
/// future task explicitly broadens the scope.
pub fn default_writable_patterns() -> Vec<String> {
    vec![
        "**/*_test.go".into(),
        "**/*.test.go".into(),
        "tests/**".into(),
        "test/**".into(),
        "**/*_test.rs".into(),
        "**/*_test.ts".into(),
        "**/*_test.tsx".into(),
        "**/*_test.js".into(),
        "**/*_test.jsx".into(),
        "**/test_*.py".into(),
        "**/*_test.py".into(),
        "**/*.test.ts".into(),
        "**/*.test.tsx".into(),
        "**/*.test.js".into(),
        "**/*.test.jsx".into(),
        "**/*.spec.ts".into(),
        "**/*.spec.tsx".into(),
        "**/*.spec.js".into(),
        "**/*.spec.jsx".into(),
        "**/*Test.java".into(),
        "**/*IT.java".into(),
        "**/*Test.kt".into(),
        "**/*Spec.kt".into(),
        "**/*Test.php".into(),
        "**/*_test.php".into(),
        "**/test_*.php".into(),
        "test.php".into(),
        "Test.php".into(),
    ]
}

/// Always-denied paths even if they match a writable glob.
pub fn default_denied_patterns() -> Vec<String> {
    vec![
        "go.sum".into(),
        "Cargo.lock".into(),
        "package-lock.json".into(),
        "pnpm-lock.yaml".into(),
        "yarn.lock".into(),
        "poetry.lock".into(),
        ".github/**".into(),
        ".gitlab-ci.yml".into(),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceHandle {
    pub repo_root: PathBuf,
    pub worktree_path: PathBuf,
    pub base_commit: String,
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeEntry {
    pub path: String,
    pub status: String,
    pub writable: bool,
    pub denied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateRevision {
    pub sequence: u32,
    pub base_commit: String,
    pub candidate_commit: String,
    pub candidate_tree: String,
    pub changed_paths: Vec<String>,
    pub display_patch_sha256: String,
    pub ref_name: String,
}

pub fn create_run_worktree(
    repo_root: &Path,
    worktrees_root: &Path,
    run_id: &str,
    base_commit: &str,
) -> Result<WorkspaceHandle> {
    let worktree_path = git::worktree_path_for_run(worktrees_root, run_id);
    git::add_detached_worktree(repo_root, &worktree_path, base_commit)?;
    Ok(WorkspaceHandle {
        repo_root: repo_root.to_path_buf(),
        worktree_path,
        base_commit: base_commit.to_string(),
        run_id: run_id.to_string(),
    })
}

pub fn discard_worktree(handle: &WorkspaceHandle) -> Result<()> {
    git::remove_worktree(&handle.repo_root, &handle.worktree_path, true)?;
    Ok(())
}

pub fn list_changes(
    handle: &WorkspaceHandle,
    writable_patterns: &[String],
) -> Result<Vec<ChangeEntry>> {
    let denied = default_denied_patterns();
    let raw = git::list_changed_paths_from_base(&handle.worktree_path, &handle.base_commit)?;
    let mut out = Vec::new();
    for ChangedPath { path, status } in raw {
        let norm = match normalize_repo_path(&path) {
            Ok(p) => p,
            Err(_) => {
                out.push(ChangeEntry {
                    path,
                    status,
                    writable: false,
                    denied: true,
                });
                continue;
            }
        };
        let denied_hit = path_matches_any(&norm, &denied);
        let disk_safe = validate_repo_path_on_disk(
            &handle.worktree_path,
            &norm,
            status == "D" || status == "??",
        )
        .is_ok();
        let writable = disk_safe && !denied_hit && path_matches_any(&norm, writable_patterns);
        out.push(ChangeEntry {
            path: norm.as_str().to_string(),
            status,
            writable,
            denied: denied_hit || !disk_safe,
        });
    }
    Ok(out)
}

/// Validate changes then freeze allowed paths into a candidate commit.
pub fn freeze_candidate(
    handle: &WorkspaceHandle,
    sequence: u32,
    writable_patterns: &[String],
    message: &str,
) -> Result<CandidateRevision> {
    let changes = list_changes(handle, writable_patterns)?;
    if changes.is_empty() {
        return Err(WorkspaceError::Other(
            "worktree has no changes to freeze".into(),
        ));
    }

    let mut bad = Vec::new();
    let mut to_stage = Vec::new();
    for c in &changes {
        if c.denied || !c.writable {
            bad.push(format!("{} ({})", c.path, c.status));
        } else {
            to_stage.push(c.path.clone());
        }
    }
    if !bad.is_empty() {
        return Err(WorkspaceError::PolicyViolation(format!(
            "changes outside writable scope: {}",
            bad.join(", ")
        )));
    }

    let ref_name = git::candidate_ref(&handle.run_id, sequence);
    let (candidate_commit, candidate_tree) = git::freeze_candidate_commit(
        &handle.worktree_path,
        &handle.base_commit,
        &to_stage,
        message,
        &ref_name,
    )?;

    let patch = git::display_patch(
        &handle.worktree_path,
        &handle.base_commit,
        &candidate_commit,
    )?;
    let display_patch_sha256 = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(patch.as_bytes());
        hex::encode(h.finalize())
    };

    if !git::worktree_tree_matches(&handle.worktree_path, &candidate_tree)? {
        return Err(WorkspaceError::Other(
            "integrity check failed after freeze".into(),
        ));
    }

    Ok(CandidateRevision {
        sequence,
        base_commit: handle.base_commit.clone(),
        candidate_commit,
        candidate_tree,
        changed_paths: to_stage,
        display_patch_sha256,
        ref_name,
    })
}

pub fn verify_integrity(handle: &WorkspaceHandle, expected_tree: &str) -> Result<bool> {
    Ok(git::worktree_tree_matches(
        &handle.worktree_path,
        expected_tree,
    )?)
}

/// Write a file under worktree (for tests / demo seed). Path must normalize.
pub fn write_worktree_file(handle: &WorkspaceHandle, rel_path: &str, content: &str) -> Result<()> {
    let norm = normalize_repo_path(rel_path)?;
    let full = validate_repo_path_on_disk(&handle.worktree_path, &norm, true)?;
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).map_err(GitError::from)?;
    }
    std::fs::write(&full, content).map_err(GitError::from)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyResult {
    pub applied_commit: String,
    pub base_commit: String,
    pub candidate_commit: String,
    pub candidate_tree: String,
}

/// Apply candidate onto main repository worktree (strict).
pub fn apply_candidate_to_main(
    repo_root: &Path,
    base_commit: &str,
    candidate: &CandidateRevision,
) -> Result<ApplyResult> {
    // Integrity: candidate tree still matches worktree if present — optional.
    let applied = git::cherry_pick_onto_main(repo_root, base_commit, &candidate.candidate_commit)?;
    Ok(ApplyResult {
        applied_commit: applied,
        base_commit: base_commit.to_string(),
        candidate_commit: candidate.candidate_commit.clone(),
        candidate_tree: candidate.candidate_tree.clone(),
    })
}

/// Export patch text for user manual apply.
pub fn export_patch(handle: &WorkspaceHandle, candidate: &CandidateRevision) -> Result<String> {
    Ok(git::display_patch(
        &handle.worktree_path,
        &candidate.base_commit,
        &candidate.candidate_commit,
    )?)
}

/// Replay candidate file contents onto a worktree already checked out at a new base,
/// then freeze a new candidate revision (successor path when HEAD drifted).
pub fn replay_and_freeze_candidate(
    handle: &WorkspaceHandle,
    source: &CandidateRevision,
    sequence: u32,
    writable_patterns: &[String],
    message: &str,
) -> Result<CandidateRevision> {
    if source.changed_paths.is_empty() {
        return Err(WorkspaceError::Other(
            "source candidate has no changed paths to replay".into(),
        ));
    }
    git::checkout_paths_from_commit(
        &handle.worktree_path,
        &source.candidate_commit,
        &source.changed_paths,
    )?;
    freeze_candidate(handle, sequence, writable_patterns, message)
}

/// Replay a candidate's exact changed paths into a successor worktree without
/// freezing. The caller must run the normal policy/format/validation/review
/// pipeline before this can become a Revision.
pub fn replay_candidate_paths(handle: &WorkspaceHandle, source: &CandidateRevision) -> Result<()> {
    if source.changed_paths.is_empty() {
        return Err(WorkspaceError::Other(
            "source candidate has no changed paths to replay".into(),
        ));
    }
    git::checkout_paths_from_commit(
        &handle.worktree_path,
        &source.candidate_commit,
        &source.changed_paths,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use testharbor_git_adapter::inspect_repo;

    fn init_repo(dir: &Path) {
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
        std::fs::write(dir.join("main.go"), "package main\n").unwrap();
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
    fn freeze_allows_test_file_rejects_product() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let info = inspect_repo(repo.path()).unwrap();
        let wts = tempfile::tempdir().unwrap();
        let handle =
            create_run_worktree(repo.path(), wts.path(), "run-abc", &info.head_commit).unwrap();

        write_worktree_file(&handle, "main_test.go", "package main\n").unwrap();
        write_worktree_file(&handle, "hack.go", "package main\n").unwrap();

        let patterns = default_writable_patterns();
        let err = freeze_candidate(&handle, 1, &patterns, "bad").unwrap_err();
        assert!(matches!(err, WorkspaceError::PolicyViolation(_)));

        // Remove illegal file and freeze only test
        std::fs::remove_file(handle.worktree_path.join("hack.go")).unwrap();
        let cand = freeze_candidate(&handle, 1, &patterns, "ok").unwrap();
        assert!(cand.changed_paths.iter().any(|p| p == "main_test.go"));
        assert!(verify_integrity(&handle, &cand.candidate_tree).unwrap());

        discard_worktree(&handle).unwrap();
    }

    #[test]
    fn freeze_allows_common_cross_language_test_files() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let info = inspect_repo(repo.path()).unwrap();
        let wts = tempfile::tempdir().unwrap();
        let handle =
            create_run_worktree(repo.path(), wts.path(), "run-php", &info.head_commit).unwrap();

        write_worktree_file(&handle, "test.php", "<?php\n").unwrap();
        write_worktree_file(&handle, "tests/ServiceReleaseTest.php", "<?php\n").unwrap();
        write_worktree_file(&handle, "web/service.spec.ts", "test('ok', () => {})\n").unwrap();

        let patterns = default_writable_patterns();
        let cand = freeze_candidate(&handle, 1, &patterns, "ok").unwrap();
        assert!(cand.changed_paths.iter().any(|p| p == "test.php"));
        assert!(cand
            .changed_paths
            .iter()
            .any(|p| p == "tests/ServiceReleaseTest.php"));
        assert!(cand
            .changed_paths
            .iter()
            .any(|p| p == "web/service.spec.ts"));

        discard_worktree(&handle).unwrap();
    }
}
