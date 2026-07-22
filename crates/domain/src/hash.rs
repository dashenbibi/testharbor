//! Deterministic hashing for intents, scopes, and approval contexts.

use sha2::{Digest, Sha256};

/// SHA-256 hex digest of raw bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Canonical field join used for golden-stable hashes.
/// Fields are length-prefixed to avoid ambiguity.
pub fn canonical_hash(parts: &[&str]) -> String {
    let mut buf = Vec::new();
    for p in parts {
        let b = p.as_bytes();
        buf.extend_from_slice(&(b.len() as u64).to_le_bytes());
        buf.extend_from_slice(b);
        buf.push(0xff);
    }
    sha256_hex(&buf)
}

/// Hash of normalized UTF-8 text body (for pasted / md content).
pub fn content_body_hash(normalized_utf8: &str) -> String {
    sha256_hex(normalized_utf8.as_bytes())
}

/// Truncate UTF-8 safely to max_bytes; returns (body, truncated).
pub fn truncate_utf8(input: &str, max_bytes: usize) -> (String, bool) {
    if input.len() <= max_bytes {
        return (input.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    (input[..end].to_string(), true)
}

/// Build test_intent_hash from ordered canonical fields.
/// Callers must pass already-normalized field strings.
pub fn test_intent_hash(fields: &TestIntentHashInput<'_>) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(fields.objective.to_string());
    for c in fields.acceptance_criteria {
        parts.push(c.to_string());
    }
    for a in fields.spec_attachments {
        parts.push(format!(
            "{}|{}|{}|{}|{}",
            a.kind,
            a.label_or_path,
            a.content_sha256,
            a.truncated,
            a.git_blob_oid.unwrap_or("")
        ));
    }
    for s in fields.source_anchors {
        parts.push(s.to_string());
    }
    parts.push(fields.git_diff_snapshot_hash.unwrap_or("").to_string());
    for g in fields.generation_scope {
        parts.push(g.to_string());
    }
    parts.push(fields.execution_scope_summary.to_string());
    parts.push(fields.pass_policy_summary.to_string());

    let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
    canonical_hash(&refs)
}

pub struct SpecHashPart<'a> {
    pub kind: &'a str,
    pub label_or_path: &'a str,
    pub content_sha256: &'a str,
    pub truncated: bool,
    pub git_blob_oid: Option<&'a str>,
}

pub struct TestIntentHashInput<'a> {
    pub objective: &'a str,
    pub acceptance_criteria: &'a [&'a str],
    pub spec_attachments: &'a [SpecHashPart<'a>],
    pub source_anchors: &'a [&'a str],
    pub git_diff_snapshot_hash: Option<&'a str>,
    pub generation_scope: &'a [&'a str],
    pub execution_scope_summary: &'a str,
    pub pass_policy_summary: &'a str,
}

#[allow(clippy::too_many_arguments)]
pub fn test_case_scope_hash(
    candidate_tree: &str,
    base_commit: &str,
    test_intent_hash: &str,
    intended_test_manifest_hash: &str,
    all_command_snapshots_hash: &str,
    config_snapshot_hash: &str,
    path_policy_hash: &str,
    trust_scope_hash: &str,
) -> String {
    canonical_hash(&[
        candidate_tree,
        base_commit,
        test_intent_hash,
        intended_test_manifest_hash,
        all_command_snapshots_hash,
        config_snapshot_hash,
        path_policy_hash,
        trust_scope_hash,
    ])
}

pub fn finding_disposition_snapshot_hash(canonical_resolutions: &str) -> String {
    sha256_hex(canonical_resolutions.as_bytes())
}

pub fn case_approval_context_hash(
    test_case_scope_hash: &str,
    finding_disposition_snapshot_hash: &str,
) -> String {
    canonical_hash(&[test_case_scope_hash, finding_disposition_snapshot_hash])
}

pub fn test_result_scope_hash(
    execution_id: &str,
    test_revision_id: &str,
    candidate_tree: &str,
    case_approval_context_hash: &str,
    intended_test_manifest_hash: &str,
    command_snapshot_hash: &str,
    result_manifest_hash: &str,
) -> String {
    canonical_hash(&[
        execution_id,
        test_revision_id,
        candidate_tree,
        case_approval_context_hash,
        intended_test_manifest_hash,
        command_snapshot_hash,
        result_manifest_hash,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_parts_same_hash() {
        let a = canonical_hash(&["x", "y"]);
        let b = canonical_hash(&["x", "y"]);
        assert_eq!(a, b);
    }

    #[test]
    fn different_parts_different_hash() {
        let a = canonical_hash(&["x", "y"]);
        let b = canonical_hash(&["xy"]);
        assert_ne!(a, b);
    }

    #[test]
    fn truncate_respects_utf8() {
        let s = "你好世界";
        let (out, trunc) = truncate_utf8(s, 4);
        assert!(trunc);
        assert!(out.is_char_boundary(out.len()));
    }

    #[test]
    fn intent_hash_includes_spec() {
        let without = test_intent_hash(&TestIntentHashInput {
            objective: "obj",
            acceptance_criteria: &["a1"],
            spec_attachments: &[],
            source_anchors: &[],
            git_diff_snapshot_hash: None,
            generation_scope: &[],
            execution_scope_summary: "go test",
            pass_policy_summary: "full green",
        });
        let with = test_intent_hash(&TestIntentHashInput {
            objective: "obj",
            acceptance_criteria: &["a1"],
            spec_attachments: &[SpecHashPart {
                kind: "pasted_text",
                label_or_path: "note",
                content_sha256: "abc",
                truncated: false,
                git_blob_oid: None,
            }],
            source_anchors: &[],
            git_diff_snapshot_hash: None,
            generation_scope: &[],
            execution_scope_summary: "go test",
            pass_policy_summary: "full green",
        });
        assert_ne!(without, with);
    }
}
