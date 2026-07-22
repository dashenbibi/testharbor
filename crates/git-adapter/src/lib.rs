//! System `git` adapter — no libgit2 in MVP.

mod worktree;

pub use worktree::*;

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("not a git repository: {0}")]
    NotARepository(PathBuf),
    #[error("git command failed: {0}")]
    Command(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, GitError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepoInfo {
    pub root: PathBuf,
    pub head_commit: String,
    pub branch: Option<String>,
    pub remote_url: Option<String>,
    pub is_dirty: bool,
    pub has_submodules: bool,
}

/// Resolve git toplevel for a path (file or directory inside a repo).
pub fn discover_repo(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    let out = git_capture(path, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(out.trim());
    if !root.is_dir() {
        return Err(GitError::NotARepository(path.to_path_buf()));
    }
    Ok(root)
}

pub fn is_git_repo(path: impl AsRef<Path>) -> bool {
    discover_repo(path).is_ok()
}

pub fn inspect_repo(path: impl AsRef<Path>) -> Result<GitRepoInfo> {
    let root = discover_repo(path)?;
    let head_commit = rev_parse(&root, "HEAD")?;
    if head_commit.is_empty() {
        return Err(GitError::Other(
            "repository has no commits; create an initial commit first".into(),
        ));
    }

    let branch = {
        let b = git_capture(&root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let b = b.trim().to_string();
        if b == "HEAD" {
            None
        } else {
            Some(b)
        }
    };

    let remote_url = git_capture(&root, &["config", "--get", "remote.origin.url"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let is_dirty = !is_worktree_clean(&root)?;
    let has_submodules = root.join(".gitmodules").is_file();

    Ok(GitRepoInfo {
        root,
        head_commit,
        branch,
        remote_url,
        is_dirty,
        has_submodules,
    })
}

pub fn rev_parse(cwd: &Path, rev: &str) -> Result<String> {
    Ok(git_capture(cwd, &["rev-parse", rev])?.trim().to_string())
}

pub fn is_worktree_clean(cwd: &Path) -> Result<bool> {
    let status = git_capture(cwd, &["status", "--porcelain"])?;
    Ok(status.trim().is_empty())
}

/// List files from the current working tree that may be used as local-only
/// runtime context. This intentionally includes ignored and untracked files;
/// callers still apply a narrow allowlist before copying anything.
pub fn list_runtime_context_paths(cwd: &Path) -> Result<Vec<String>> {
    let mut paths = BTreeSet::new();
    for args in [
        ["ls-files", "--cached", "-z"].as_slice(),
        ["ls-files", "--others", "--exclude-standard", "-z"].as_slice(),
        [
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ]
        .as_slice(),
    ] {
        for path in git_capture(cwd, args)?.split('\0') {
            let path = path.trim();
            if path.is_empty() {
                continue;
            }
            let relative = Path::new(path);
            if relative.is_absolute()
                || relative.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                continue;
            }
            paths.insert(path.to_string());
        }
    }
    Ok(paths.into_iter().collect())
}

/// List tracked files changed from HEAD plus ordinary untracked files in the
/// current worktree. Ignored files are intentionally excluded here because
/// runtime configuration is handled by the narrower runtime-context mirror.
pub fn list_working_tree_overlay_paths(cwd: &Path) -> Result<Vec<String>> {
    let mut paths = BTreeSet::new();
    for path in git_capture(cwd, &["diff", "--name-only", "HEAD"])?.lines() {
        let path = path.trim();
        if !path.is_empty() {
            paths.insert(path.to_string());
        }
    }
    for path in git_capture(cwd, &["ls-files", "--others", "--exclude-standard", "-z"])?.split('\0')
    {
        let path = path.trim();
        if !path.is_empty() {
            paths.insert(path.to_string());
        }
    }
    Ok(paths.into_iter().collect())
}

/// Read a regular UTF-8 blob from an exact Git revision. This never consults
/// the working tree, so repository-backed intent attachments remain bound to
/// the Run's base commit.
pub fn read_text_blob_at(
    cwd: &Path,
    revision: &str,
    repo_relative_path: &str,
) -> Result<(String, String)> {
    let path = Path::new(repo_relative_path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(GitError::Other(format!(
            "invalid repository-relative path: {repo_relative_path}"
        )));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension != "md" && extension != "markdown" {
        return Err(GitError::Other(
            "repository intent attachment must be .md or .markdown".into(),
        ));
    }

    read_utf8_blob_at(cwd, revision, repo_relative_path)
}

/// Read any regular UTF-8 blob from an exact revision.
pub fn read_utf8_blob_at(
    cwd: &Path,
    revision: &str,
    repo_relative_path: &str,
) -> Result<(String, String)> {
    let path = Path::new(repo_relative_path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(GitError::Other(format!(
            "invalid repository-relative path: {repo_relative_path}"
        )));
    }
    let object = format!("{revision}:{repo_relative_path}");
    let kind = git_capture(cwd, &["cat-file", "-t", &object])?;
    if kind.trim() != "blob" {
        return Err(GitError::Other(format!(
            "repository intent attachment is not a regular blob: {repo_relative_path}"
        )));
    }
    let oid = git_capture(cwd, &["rev-parse", &object])?
        .trim()
        .to_string();
    let output = Command::new("git")
        .args(["cat-file", "blob", &object])
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        return Err(GitError::Command(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let body = String::from_utf8(output.stdout)
        .map_err(|_| GitError::Other("repository intent attachment is not UTF-8".into()))?;
    Ok((oid, body.replace("\r\n", "\n").replace('\r', "\n")))
}

/// Add a local-only ignore rule to the worktree's Git exclude file. This is
/// intentionally not a tracked `.gitignore` change, so runtime artifacts can
/// never become part of a candidate revision.
pub fn ensure_info_exclude(cwd: &Path, pattern: &str) -> Result<()> {
    let raw_path = git_capture(cwd, &["rev-parse", "--git-path", "info/exclude"])?;
    let raw_path = raw_path.trim();
    let exclude = if Path::new(raw_path).is_absolute() {
        PathBuf::from(raw_path)
    } else {
        cwd.join(raw_path)
    };
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == pattern.trim()) {
        return Ok(());
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str("# TestHarbor runtime artifacts\n");
    content.push_str(pattern.trim());
    content.push('\n');
    std::fs::write(exclude, content)?;
    Ok(())
}

/// Mark a tracked file as skip-worktree in this isolated worktree only. This
/// is used for read-only overlays sourced from the user's dirty main tree so
/// Git does not mistake the overlay for a candidate change.
pub fn mark_skip_worktree(cwd: &Path, repo_relative_path: &str) -> Result<()> {
    if git_capture(
        cwd,
        &["ls-files", "--error-unmatch", "--", repo_relative_path],
    )
    .is_err()
    {
        return Ok(());
    }
    let output = Command::new("git")
        .args(["update-index", "--skip-worktree", "--", repo_relative_path])
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        return Err(GitError::Command(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn git_capture(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let msg = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("git {} failed", args.join(" "))
        };
        if msg.contains("not a git repository") {
            return Err(GitError::NotARepository(cwd.to_path_buf()));
        }
        return Err(GitError::Command(msg));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(crate) fn git_run(cwd: &Path, args: &[&str]) -> Result<()> {
    git_capture(cwd, args)?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::process::Command;

    pub fn init_repo(dir: &Path) {
        assert!(Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        // Avoid commit-tree env issues
        assert!(Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        std::fs::write(dir.join("README.md"), "hello").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::init_repo;

    #[test]
    fn inspect_fresh_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let info = inspect_repo(dir.path()).unwrap();
        assert_eq!(
            info.root.canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
        assert_eq!(info.head_commit.len(), 40);
        assert!(!info.is_dirty);
    }

    #[test]
    fn not_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let err = inspect_repo(dir.path()).unwrap_err();
        assert!(matches!(
            err,
            GitError::NotARepository(_) | GitError::Command(_)
        ));
    }

    #[test]
    fn reads_markdown_from_commit_not_worktree() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let base = rev_parse(dir.path(), "HEAD").unwrap();
        std::fs::write(dir.path().join("README.md"), "working tree").unwrap();
        let (_, body) = read_text_blob_at(dir.path(), &base, "README.md").unwrap();
        assert_eq!(body, "hello");
        assert!(read_text_blob_at(dir.path(), &base, "../README.md").is_err());
        assert!(read_text_blob_at(dir.path(), &base, "README.txt").is_err());
    }
}
