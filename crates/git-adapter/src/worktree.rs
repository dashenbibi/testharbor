//! Worktree, status, and candidate commit helpers.

use crate::{git_capture, git_run, rev_parse, GitError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedPath {
    pub path: String,
    pub status: String,
}

/// Create a detached worktree at `path` checked out at `commit`.
pub fn add_detached_worktree(repo_root: &Path, worktree_path: &Path, commit: &str) -> Result<()> {
    if worktree_path.exists() {
        return Err(GitError::Other(format!(
            "worktree path already exists: {}",
            worktree_path.display()
        )));
    }
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // git worktree add --detach <path> <commit>
    git_run(
        repo_root,
        &[
            "worktree",
            "add",
            "--detach",
            worktree_path
                .to_str()
                .ok_or_else(|| GitError::Other("worktree path not utf-8".into()))?,
            commit,
        ],
    )?;
    Ok(())
}

/// Remove a worktree and prune metadata.
pub fn remove_worktree(repo_root: &Path, worktree_path: &Path, force: bool) -> Result<()> {
    let path_str = worktree_path
        .to_str()
        .ok_or_else(|| GitError::Other("worktree path not utf-8".into()))?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(path_str);
    // If path already gone, still prune.
    match git_run(repo_root, &args) {
        Ok(()) => {}
        Err(e) => {
            if worktree_path.exists() {
                return Err(e);
            }
        }
    }
    let _ = git_run(repo_root, &["worktree", "prune"]);
    Ok(())
}

/// Porcelain status lines → changed paths (including untracked).
pub fn list_changed_paths(worktree: &Path) -> Result<Vec<ChangedPath>> {
    let out = git_capture(worktree, &["status", "--porcelain", "-uall"])?;
    let mut items = Vec::new();
    for line in out.lines() {
        if line.len() < 4 {
            continue;
        }
        let status = line[..2].trim().to_string();
        let rest = line[3..].trim();
        // Handle renames: "R  old -> new"
        let path = if let Some((_, new)) = rest.split_once(" -> ") {
            new.to_string()
        } else {
            rest.trim_matches('"').to_string()
        };
        if path.is_empty() {
            continue;
        }
        items.push(ChangedPath { path, status });
    }
    Ok(items)
}

/// List the complete candidate delta relative to an immutable base commit,
/// including paths already committed in a previous candidate revision.
pub fn list_changed_paths_from_base(
    worktree: &Path,
    base_commit: &str,
) -> Result<Vec<ChangedPath>> {
    let output = git_capture(
        worktree,
        &["diff", "--name-status", "--find-renames", base_commit],
    )?;
    let mut items = BTreeMap::new();
    for line in output.lines() {
        let mut parts = line.split('\t');
        let status = parts.next().unwrap_or_default().to_string();
        let first_path = parts.next().unwrap_or_default();
        let path = if status.starts_with('R') || status.starts_with('C') {
            parts.next().unwrap_or(first_path)
        } else {
            first_path
        };
        if path.is_empty() {
            continue;
        }
        items.insert(path.to_string(), status);
    }
    // `git diff <base>` deliberately omits untracked files. Merge porcelain
    // status so first-generation files are included while previously frozen
    // revision files still come from the immutable-base delta above.
    for changed in list_changed_paths(worktree)? {
        items.entry(changed.path).or_insert(changed.status);
    }
    Ok(items
        .into_iter()
        .map(|(path, status)| ChangedPath { path, status })
        .collect())
}

/// Stage specific paths then create a candidate commit with parent `base_commit`.
/// Returns (commit_oid, tree_oid).
pub fn freeze_candidate_commit(
    worktree: &Path,
    base_commit: &str,
    paths_to_stage: &[String],
    message: &str,
    ref_name: &str,
) -> Result<(String, String)> {
    if paths_to_stage.is_empty() {
        return Err(GitError::Other("no paths to freeze".into()));
    }

    // Reset index to base so we only stage intended files.
    git_run(worktree, &["reset", "--mixed", base_commit])?;

    let mut add_args = vec!["add", "-f", "--"];
    let path_refs: Vec<&str> = paths_to_stage.iter().map(|s| s.as_str()).collect();
    add_args.extend(path_refs.iter().copied());
    git_run(worktree, &add_args)?;

    let mut mode_args = vec!["ls-files", "-s", "--"];
    mode_args.extend(path_refs.iter().copied());
    let staged = git_capture(worktree, &mode_args)?;
    for line in staged.lines() {
        let mode = line.split_whitespace().next().unwrap_or_default();
        if mode == "120000" || mode == "160000" {
            return Err(GitError::Other(format!(
                "generated symlink or gitlink is forbidden: {line}"
            )));
        }
        if mode != "100644" {
            return Err(GitError::Other(format!(
                "generated test file mode must be 100644: {line}"
            )));
        }
    }

    let tree = git_capture(worktree, &["write-tree"])?.trim().to_string();
    let commit = git_capture(
        worktree,
        &["commit-tree", &tree, "-p", base_commit, "-m", message],
    )?
    .trim()
    .to_string();

    // Point TestHarbor ref at candidate (keeps object reachable).
    git_run(worktree, &["update-ref", ref_name, &commit])?;

    // Materialize candidate in worktree.
    git_run(worktree, &["reset", "--hard", &commit])?;

    Ok((commit, tree))
}

pub fn candidate_ref(run_id: &str, sequence: u32) -> String {
    format!("refs/testharbor/runs/{run_id}/revisions/{sequence}")
}

/// True if HEAD commit tree matches expected tree oid.
pub fn worktree_tree_matches(worktree: &Path, expected_tree: &str) -> Result<bool> {
    let head = rev_parse(worktree, "HEAD")?;
    let tree = rev_parse(worktree, &format!("{head}^{{tree}}"))?;
    let clean = crate::is_worktree_clean(worktree)?;
    Ok(tree == expected_tree && clean)
}

pub fn display_patch(worktree: &Path, base_commit: &str, candidate_commit: &str) -> Result<String> {
    git_capture(
        worktree,
        &["diff", "--binary", base_commit, candidate_commit],
    )
}

/// Materialize paths from `source_commit` into the worktree (for successor replay).
/// Overwrites those paths; does not create a commit.
pub fn checkout_paths_from_commit(
    worktree: &Path,
    source_commit: &str,
    paths: &[String],
) -> Result<()> {
    if paths.is_empty() {
        return Err(GitError::Other("no paths to checkout from commit".into()));
    }
    let mut args = vec!["checkout", source_commit, "--"];
    let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    args.extend(path_refs.iter().copied());
    git_run(worktree, &args)?;
    Ok(())
}

/// Ensure path is inside a worktree directory that exists.
pub fn assert_dir(path: &Path) -> Result<()> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(GitError::Other(format!(
            "worktree missing: {}",
            path.display()
        )))
    }
}

pub fn worktree_path_for_run(worktrees_root: &Path, run_id: &str) -> PathBuf {
    worktrees_root.join(run_id)
}

/// Strict apply: cherry-pick candidate onto main worktree when clean and HEAD==base.
/// Returns new HEAD commit oid after successful cherry-pick.
pub fn cherry_pick_onto_main(
    repo_root: &Path,
    base_commit: &str,
    candidate_commit: &str,
) -> Result<String> {
    let head = rev_parse(repo_root, "HEAD")?;
    if head != base_commit {
        return Err(GitError::Other(format!(
            "HEAD drift: expected base {base_commit}, got {head}"
        )));
    }
    if !crate::is_worktree_clean(repo_root)? {
        return Err(GitError::Other("main worktree is dirty".into()));
    }

    // Prefer -x for traceability; allow empty if already applied
    match git_run(
        repo_root,
        &["cherry-pick", "--allow-empty", candidate_commit],
    ) {
        Ok(()) => {}
        Err(e) => {
            let _ = git_run(repo_root, &["cherry-pick", "--abort"]);
            return Err(e);
        }
    }

    let new_head = rev_parse(repo_root, "HEAD")?;
    Ok(new_head)
}

pub fn abort_cherry_pick(repo_root: &Path) -> Result<()> {
    let _ = git_run(repo_root, &["cherry-pick", "--abort"]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_repo;
    use crate::{inspect_repo, is_worktree_clean};

    #[test]
    fn worktree_add_change_freeze_remove() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let info = inspect_repo(repo.path()).unwrap();
        let wt_root = tempfile::tempdir().unwrap();
        let wt = wt_root.path().join("run-1");

        add_detached_worktree(repo.path(), &wt, &info.head_commit).unwrap();
        assert!(wt.join("README.md").exists());
        assert!(is_worktree_clean(&wt).unwrap());

        // Write an allowed test-like file
        std::fs::write(wt.join("demo_test.go"), "package demo\n").unwrap();
        let changes = list_changed_paths(&wt).unwrap();
        assert!(changes.iter().any(|c| c.path == "demo_test.go"));

        let ref_name = candidate_ref("run-1", 1);
        let (commit, tree) = freeze_candidate_commit(
            &wt,
            &info.head_commit,
            &["demo_test.go".into()],
            "cand",
            &ref_name,
        )
        .unwrap();
        assert_eq!(commit.len(), 40);
        assert_eq!(tree.len(), 40);
        assert!(worktree_tree_matches(&wt, &tree).unwrap());
        assert!(is_worktree_clean(&wt).unwrap());

        remove_worktree(repo.path(), &wt, true).unwrap();
        assert!(!wt.exists());
    }
}
