//! Path policy: normalize repo-relative paths, reject escapes.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathPolicyError {
    #[error("empty path")]
    Empty,
    #[error("absolute path not allowed")]
    Absolute,
    #[error("path contains parent segment '..'")]
    ParentSegment,
    #[error("path contains NUL")]
    Nul,
    #[error("path escapes repository root")]
    EscapesRoot,
    #[error("symlink path component is not allowed: {0}")]
    Symlink(String),
    #[error("non-regular file is not allowed: {0}")]
    NonRegular(String),
    #[error("hard-linked file is not allowed: {0}")]
    HardLink(String),
    #[error("executable file mode is not allowed for generated tests: {0}")]
    ExecutableMode(String),
    #[error("case-insensitive path collision: {0}")]
    CaseCollision(String),
}

/// Validated repository-relative path using `/` separators.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoPath(String);

impl RepoPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Normalize and validate a user/agent-supplied path relative to a repo.
pub fn normalize_repo_path(raw: &str) -> Result<RepoPath, PathPolicyError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(PathPolicyError::Empty);
    }
    if raw.contains('\0') {
        return Err(PathPolicyError::Nul);
    }
    if raw.starts_with('/') || raw.starts_with('\\') {
        return Err(PathPolicyError::Absolute);
    }
    // Windows drive letter
    if raw.len() >= 2 && raw.as_bytes()[1] == b':' {
        return Err(PathPolicyError::Absolute);
    }

    let mut parts: Vec<&str> = Vec::new();
    for seg in raw.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err(PathPolicyError::ParentSegment);
        }
        parts.push(seg);
    }
    if parts.is_empty() {
        return Err(PathPolicyError::Empty);
    }
    Ok(RepoPath(parts.join("/")))
}

/// Validate every existing path segment with lstat semantics. The final path
/// may be absent when the caller is about to create a file.
pub fn validate_repo_path_on_disk(
    repository_root: &Path,
    relative: &RepoPath,
    allow_missing_leaf: bool,
) -> Result<PathBuf, PathPolicyError> {
    let root = repository_root
        .canonicalize()
        .map_err(|_| PathPolicyError::EscapesRoot)?;
    let mut current = root.clone();
    let components = relative.as_str().split('/').collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let case_folded = component.to_lowercase();
        if let Ok(entries) = std::fs::read_dir(&current) {
            for entry in entries.flatten() {
                let existing = entry.file_name().to_string_lossy().to_string();
                if existing.to_lowercase() == case_folded && existing != *component {
                    return Err(PathPolicyError::CaseCollision(relative.as_str().into()));
                }
            }
        }
        current.push(component);
        let is_leaf = index + 1 == components.len();
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if allow_missing_leaf {
                    for remaining in components.iter().skip(index + 1) {
                        current.push(remaining);
                    }
                    return Ok(current);
                }
                return Err(PathPolicyError::EscapesRoot);
            }
            Err(_) => return Err(PathPolicyError::EscapesRoot),
        };
        if metadata.file_type().is_symlink() {
            return Err(PathPolicyError::Symlink(relative.as_str().into()));
        }
        if !is_leaf && !metadata.is_dir() {
            return Err(PathPolicyError::NonRegular(relative.as_str().into()));
        }
        if is_leaf {
            if !metadata.is_file() {
                return Err(PathPolicyError::NonRegular(relative.as_str().into()));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() > 1 {
                    return Err(PathPolicyError::HardLink(relative.as_str().into()));
                }
                if metadata.mode() & 0o111 != 0 {
                    return Err(PathPolicyError::ExecutableMode(relative.as_str().into()));
                }
            }
        }
    }
    let parent = current.parent().ok_or(PathPolicyError::EscapesRoot)?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|_| PathPolicyError::EscapesRoot)?;
    if !canonical_parent.starts_with(&root) {
        return Err(PathPolicyError::EscapesRoot);
    }
    Ok(current)
}

/// Glob-style match against writable patterns (simple suffix/prefix helpers for MVP).
pub fn path_matches_any(path: &RepoPath, patterns: &[String]) -> bool {
    let p = path.as_str();
    patterns.iter().any(|pat| match_glob_simple(p, pat))
}

fn match_glob_simple(path: &str, pattern: &str) -> bool {
    if pattern == "**" || pattern == "**/*" {
        return true;
    }
    if let Some(rest) = pattern.strip_prefix("**/") {
        // **/suffix or **/*suffix patterns — match against full path or basename.
        let base = path.rsplit('/').next().unwrap_or(path);
        return star_match(path, rest)
            || star_match(base, rest)
            || path.ends_with(&format!("/{rest}"))
            || path == rest;
    }
    if let Some(pre) = pattern.strip_suffix("/**") {
        return path == pre || path.starts_with(&format!("{pre}/"));
    }
    star_match(path, pattern)
}

/// Single-segment-friendly `*` matcher (no nested `**`).
fn star_match(text: &str, pattern: &str) -> bool {
    if !pattern.contains('*') {
        return text == pattern;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.is_empty() {
        return true;
    }
    if !text.starts_with(parts[0]) {
        return false;
    }
    let mut rest = &text[parts[0].len()..];
    for (i, part) in parts.iter().enumerate().skip(1) {
        if part.is_empty() {
            if i == parts.len() - 1 {
                return true;
            }
            continue;
        }
        if i == parts.len() - 1 {
            return rest.ends_with(part);
        }
        if let Some(idx) = rest.find(part) {
            rest = &rest[idx + part.len()..];
        } else {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent() {
        assert_eq!(
            normalize_repo_path("a/../b"),
            Err(PathPolicyError::ParentSegment)
        );
    }

    #[test]
    fn normalizes_slashes() {
        let p = normalize_repo_path(r"internal\auth\foo_test.go").unwrap();
        assert_eq!(p.as_str(), "internal/auth/foo_test.go");
    }

    #[test]
    fn writable_go_test() {
        let p = normalize_repo_path("pkg/foo_test.go").unwrap();
        assert!(path_matches_any(
            &p,
            &["**/*_test.go".into(), "tests/**".into()]
        ));
    }

    #[test]
    fn product_code_not_writable_by_default() {
        let p = normalize_repo_path("pkg/foo.go").unwrap();
        assert!(!path_matches_any(
            &p,
            &["**/*_test.go".into(), "tests/**".into()]
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_and_hardlink_targets() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        std::fs::write(root.path().join("outside"), "x").unwrap();
        symlink("../outside", root.path().join("tests/link_test.go")).unwrap();
        let link = normalize_repo_path("tests/link_test.go").unwrap();
        assert!(matches!(
            validate_repo_path_on_disk(root.path(), &link, false),
            Err(PathPolicyError::Symlink(_))
        ));

        std::fs::write(root.path().join("tests/a_test.go"), "x").unwrap();
        std::fs::hard_link(
            root.path().join("tests/a_test.go"),
            root.path().join("tests/b_test.go"),
        )
        .unwrap();
        let hardlink = normalize_repo_path("tests/a_test.go").unwrap();
        assert!(matches!(
            validate_repo_path_on_disk(root.path(), &hardlink, false),
            Err(PathPolicyError::HardLink(_))
        ));
    }
}
