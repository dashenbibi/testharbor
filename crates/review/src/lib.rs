//! Human review state: findings, dispositions, approval context.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use testharbor_agent_drivers::AgentFinding;
use testharbor_domain::{
    can_approve_test_revision, finding_disposition_snapshot_hash, ApproveTestContext,
    FindingDisposition, GuardResult, HumanTestDecision, RevisionStatus, WorkflowState,
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub id: String,
    pub severity: String,
    pub category: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
    pub suggestion: Option<String>,
    pub disposition: FindingDisposition,
    pub risk_reason: Option<String>,
}

pub fn is_major_severity(severity: &str) -> bool {
    matches!(
        severity.trim().to_ascii_lowercase().as_str(),
        "critical" | "blocker" | "fatal" | "high" | "error" | "must_fix" | "must-fix"
    )
}

/// Stable display order for findings: highest risk first, while preserving
/// the agent's order within the same risk level.
pub fn severity_rank(severity: &str) -> u8 {
    match severity.trim().to_ascii_lowercase().as_str() {
        "critical" | "blocker" | "fatal" => 0,
        "high" | "error" | "must_fix" | "must-fix" => 1,
        "medium" | "warning" | "warn" => 2,
        "low" => 3,
        "info" | "informational" => 4,
        _ => 5,
    }
}

impl ReviewFinding {
    pub fn from_agent(f: AgentFinding) -> Self {
        let disposition = if is_major_severity(&f.severity) {
            FindingDisposition::MustFix
        } else {
            FindingDisposition::Open
        };
        Self {
            id: Uuid::new_v4().to_string(),
            severity: f.severity,
            category: f.category,
            file: f.file,
            line: f.line,
            message: f.message,
            suggestion: f.suggestion,
            disposition,
            risk_reason: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseReviewState {
    pub recommendation: String,
    pub summary: String,
    pub findings: Vec<ReviewFinding>,
    pub disposition_snapshot_hash: String,
}

impl CaseReviewState {
    pub fn from_agent_review(
        recommendation: String,
        summary: String,
        agent_findings: Vec<AgentFinding>,
    ) -> Self {
        let findings: Vec<_> = agent_findings
            .into_iter()
            .map(ReviewFinding::from_agent)
            .collect();
        let mut s = Self {
            recommendation,
            summary,
            findings,
            disposition_snapshot_hash: String::new(),
        };
        s.sort_findings();
        s.recompute_disposition_hash();
        s
    }

    pub fn sort_findings(&mut self) {
        self.findings
            .sort_by_key(|finding| severity_rank(&finding.severity));
    }

    pub fn recompute_disposition_hash(&mut self) {
        let mut parts = Vec::new();
        for f in &self.findings {
            parts.push(format!(
                "{}:{:?}:{}",
                f.id,
                f.disposition,
                f.risk_reason.clone().unwrap_or_default()
            ));
        }
        parts.sort();
        self.disposition_snapshot_hash = finding_disposition_snapshot_hash(&parts.join("|"));
    }

    pub fn set_disposition(
        &mut self,
        finding_id: &str,
        disposition: FindingDisposition,
        risk_reason: Option<String>,
    ) -> bool {
        if let Some(f) = self.findings.iter_mut().find(|x| x.id == finding_id) {
            if disposition == FindingDisposition::AcceptedRisk
                && risk_reason
                    .as_ref()
                    .map(|s| s.trim().is_empty())
                    .unwrap_or(true)
            {
                return false;
            }
            f.disposition = disposition;
            f.risk_reason = risk_reason;
            self.recompute_disposition_hash();
            return true;
        }
        false
    }

    pub fn unresolved_must_fix(&self) -> bool {
        self.findings
            .iter()
            .any(|f| f.disposition == FindingDisposition::MustFix)
    }

    pub fn accepted_risk_missing_reason(&self) -> bool {
        self.findings.iter().any(|f| {
            f.disposition == FindingDisposition::AcceptedRisk
                && f.risk_reason
                    .as_ref()
                    .map(|s| s.trim().is_empty())
                    .unwrap_or(true)
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseApprovalRecord {
    pub decision: HumanTestDecision,
    pub candidate_commit: String,
    pub candidate_tree: String,
    pub base_commit: String,
    pub test_case_scope_hash: String,
    pub case_approval_context_hash: String,
    pub intended_manifest_hash: Option<String>,
    pub disposition_snapshot_hash: String,
}

#[allow(clippy::too_many_arguments)]
pub fn evaluate_can_approve(
    workflow_state: WorkflowState,
    revision_status: RevisionStatus,
    current_revision_id: &str,
    revision_id: &str,
    candidate_tree: &str,
    recomputed_tree: &str,
    scope_hash: &str,
    recomputed_scope: &str,
    review: &CaseReviewState,
    trust_valid: bool,
) -> GuardResult {
    can_approve_test_revision(&ApproveTestContext {
        workflow_state,
        current_revision_id: current_revision_id.into(),
        revision_id: revision_id.into(),
        revision_status,
        candidate_tree: candidate_tree.into(),
        recomputed_candidate_tree: recomputed_tree.into(),
        test_case_scope_hash: scope_hash.into(),
        recomputed_test_case_scope_hash: recomputed_scope.into(),
        has_blocking_policy_finding: false,
        unresolved_must_fix: review.unresolved_must_fix(),
        accepted_risk_missing_reason: review.accepted_risk_missing_reason(),
        trust_grant_valid: trust_valid,
    })
}

pub fn case_approval_context_hash(scope: &str, disposition_hash: &str) -> String {
    testharbor_domain::case_approval_context_hash(scope, disposition_hash)
}

pub fn test_case_scope_hash_simple(
    candidate_tree: &str,
    base_commit: &str,
    test_intent_hash: &str,
    intended_hash: &str,
) -> String {
    testharbor_domain::test_case_scope_hash(
        candidate_tree,
        base_commit,
        test_intent_hash,
        intended_hash,
        "cmd",
        "cfg",
        "path",
        "trust",
    )
}

pub fn write_review_bundle(
    bundle_dir: &std::path::Path,
    objective: &str,
    criteria: &[String],
    intent_hash: &str,
    changed_paths: &[String],
    patch: &str,
    validation_summary: &str,
) -> std::io::Result<()> {
    const AGENT_REVIEW_PATCH_LIMIT: usize = 24_000;
    std::fs::create_dir_all(bundle_dir)?;
    std::fs::write(
        bundle_dir.join("test-intent.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "objective": objective,
            "acceptance_criteria": criteria,
            "hash": intent_hash,
        }))
        .unwrap(),
    )?;
    std::fs::write(bundle_dir.join("diff.patch"), patch)?;
    let agent_patch = patch
        .chars()
        .take(AGENT_REVIEW_PATCH_LIMIT)
        .collect::<String>();
    let agent_patch = if agent_patch.chars().count() < patch.chars().count() {
        format!("{agent_patch}\n[agent-review patch truncated; use changed-files.json for scope]")
    } else {
        agent_patch
    };
    std::fs::write(bundle_dir.join("agent-review.patch"), agent_patch)?;
    std::fs::write(
        bundle_dir.join("changed-files.json"),
        serde_json::to_vec_pretty(changed_paths).unwrap(),
    )?;
    std::fs::write(
        bundle_dir.join("validation-summary.json"),
        validation_summary,
    )?;
    let mut man = Sha256::new();
    man.update(intent_hash.as_bytes());
    man.update(patch.as_bytes());
    let manifest = hex::encode(man.finalize());
    std::fs::write(
        bundle_dir.join("manifest.json"),
        format!("{{\"hash\":\"{manifest}\"}}"),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use testharbor_domain::FindingDisposition;

    #[test]
    fn must_fix_blocks_until_resolved() {
        let mut state = CaseReviewState::from_agent_review(
            "CHANGES".into(),
            "x".into(),
            vec![testharbor_agent_drivers::AgentFinding {
                severity: "error".into(),
                category: "c".into(),
                file: None,
                line: None,
                message: "bad".into(),
                suggestion: None,
            }],
        );
        assert!(state.unresolved_must_fix());
        let id = state.findings[0].id.clone();
        assert!(state.set_disposition(&id, FindingDisposition::Resolved, None));
        assert!(!state.unresolved_must_fix());
    }

    #[test]
    fn major_severity_includes_high_and_critical() {
        assert!(is_major_severity("high"));
        assert!(is_major_severity("CRITICAL"));
        assert!(!is_major_severity("medium"));
    }

    #[test]
    fn findings_are_sorted_highest_risk_first() {
        let state = CaseReviewState::from_agent_review(
            "CHANGES".into(),
            "x".into(),
            vec![
                testharbor_agent_drivers::AgentFinding {
                    severity: "low".into(),
                    category: "low".into(),
                    file: None,
                    line: None,
                    message: "low".into(),
                    suggestion: None,
                },
                testharbor_agent_drivers::AgentFinding {
                    severity: "medium".into(),
                    category: "medium".into(),
                    file: None,
                    line: None,
                    message: "medium".into(),
                    suggestion: None,
                },
                testharbor_agent_drivers::AgentFinding {
                    severity: "high".into(),
                    category: "high".into(),
                    file: None,
                    line: None,
                    message: "high".into(),
                    suggestion: None,
                },
            ],
        );
        assert_eq!(
            state
                .findings
                .iter()
                .map(|finding| finding.severity.as_str())
                .collect::<Vec<_>>(),
            vec!["high", "medium", "low"]
        );
    }
}
