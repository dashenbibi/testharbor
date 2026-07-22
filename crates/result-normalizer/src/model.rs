use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use testharbor_domain::{ResultCompleteness, ResultConsistency};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePhase {
    Build,
    Setup,
    Assertion,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    Pass,
    Fail,
    Skip,
    NotRunDueToBuildFailure,
    Incomplete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCaseResult {
    pub package: String,
    pub name: String,
    pub status: CaseStatus,
    pub elapsed_sec: Option<f64>,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageResult {
    pub package: String,
    pub status: CaseStatus,
    pub failure_phase: Option<FailurePhase>,
    pub cases: Vec<TestCaseResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedStructuredResult {
    pub parser: String,
    pub completeness: ResultCompleteness,
    pub consistency: ResultConsistency,
    pub failure_phase: Option<FailurePhase>,
    pub packages: Vec<PackageResult>,
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub incomplete: u32,
    pub empty: bool,
    pub warnings: Vec<String>,
    pub every_started_has_terminal: bool,
    /// Canonical hash of structured summary (stable).
    pub result_manifest_hash: String,
}

impl NormalizedStructuredResult {
    pub fn case_map(&self) -> BTreeMap<(String, String), &TestCaseResult> {
        let mut m = BTreeMap::new();
        for p in &self.packages {
            for c in &p.cases {
                m.insert((c.package.clone(), c.name.clone()), c);
            }
        }
        m
    }

    pub fn observed_top_level(&self) -> Vec<(String, String)> {
        let mut v = Vec::new();
        for p in &self.packages {
            for c in &p.cases {
                // Skip subtests: name contains '/'
                if !c.name.contains('/') {
                    v.push((c.package.clone(), c.name.clone()));
                }
            }
        }
        v
    }
}
