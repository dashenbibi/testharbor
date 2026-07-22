//! Read-only project discovery. Repository configuration is suggestion input;
//! it never creates execution authority or a TrustGrant.

use serde::{Deserialize, Serialize};
use std::path::Path;
use testharbor_domain::sha256_hex;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectDiscovery {
    pub base_commit: String,
    pub manifests: Vec<String>,
    pub suggested_config: Option<SuggestedProjectConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuggestedProjectConfig {
    pub source_path: String,
    pub git_blob_oid: String,
    pub content_sha256: String,
    pub body: String,
    /// Always false: a repository file cannot grant permission to execute.
    pub trusted: bool,
}

pub fn discover(repository_root: &Path, base_commit: &str) -> ProjectDiscovery {
    let manifest_names = [
        "go.mod",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "pom.xml",
        "build.gradle",
    ];
    let manifests = manifest_names
        .iter()
        .filter(|name| repository_root.join(name).is_file())
        .map(|name| (*name).to_string())
        .collect();
    let suggested_config =
        testharbor_git_adapter::read_utf8_blob_at(repository_root, base_commit, ".testharbor.yaml")
            .ok()
            .map(|(git_blob_oid, body)| SuggestedProjectConfig {
                source_path: ".testharbor.yaml".into(),
                git_blob_oid,
                content_sha256: sha256_hex(body.as_bytes()),
                body,
                trusted: false,
            });
    ProjectDiscovery {
        base_commit: base_commit.to_string(),
        manifests,
        suggested_config,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn config_is_read_from_base_and_never_trusted() {
        let root = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(root.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@example.com"])
            .current_dir(root.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(root.path())
            .status()
            .unwrap();
        std::fs::write(root.path().join(".testharbor.yaml"), "test: suggested").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(root.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(root.path())
            .status()
            .unwrap();
        let base = testharbor_git_adapter::rev_parse(root.path(), "HEAD").unwrap();
        std::fs::write(root.path().join(".testharbor.yaml"), "test: changed").unwrap();
        let result = discover(root.path(), &base);
        let config = result.suggested_config.unwrap();
        assert_eq!(config.body, "test: suggested");
        assert!(!config.trusted);
    }
}
