//! TestIntent and spec attachments.

use crate::ids::SpecAttachmentId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecAttachmentKind {
    PastedText,
    RepoMarkdown,
}

/// Immutable snapshot of one spec attachment (paste or repo markdown).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecAttachmentSnapshot {
    pub id: SpecAttachmentId,
    pub kind: SpecAttachmentKind,
    /// Paste label or repo-relative path.
    pub label_or_path: String,
    /// SHA-256 hex of normalized UTF-8 body (after truncation).
    pub content_sha256: String,
    pub byte_len: u64,
    pub truncated: bool,
    /// Git blob oid for RepoMarkdown, if known.
    pub git_blob_oid: Option<String>,
    /// Content-addressed artifact containing the frozen body.
    pub artifact_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAnchor {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub test_intent: TestIntentSnapshot,
    pub base_commit: String,
    pub project_config_hash: String,
    pub test_profile_hash: String,
    pub all_command_snapshots_hash: String,
    pub agent_profile_hash: String,
    pub path_policy_hash: String,
    pub env_policy_hash: String,
    pub required_trust_scope_hash: String,
}

/// Immutable user-confirmed test intent for a Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestIntentSnapshot {
    pub objective: String,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub spec_attachments: Vec<SpecAttachmentSnapshot>,
    pub source_anchors: Vec<SourceAnchor>,
    pub git_diff_snapshot_hash: Option<String>,
    pub generation_scope: Vec<String>,
    pub execution_scope_summary: String,
    pub pass_policy_summary: String,
    /// Content hash covering all intent fields including spec bodies.
    pub hash: String,
}

impl TestIntentSnapshot {
    /// Product rule: at least one non-empty acceptance criterion.
    pub fn is_creatable(&self) -> bool {
        !self.objective.trim().is_empty()
            && self
                .acceptance_criteria
                .iter()
                .any(|c| !c.text.trim().is_empty())
    }
}

/// MVP size limits for spec attachments.
pub const SPEC_MAX_BYTES_PER_ATTACHMENT: u64 = 64 * 1024;
pub const SPEC_MAX_BYTES_TOTAL: u64 = 128 * 1024;
