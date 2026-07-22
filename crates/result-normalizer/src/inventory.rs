//! Go test inventory: go list + go test -list.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InventoryError {
    #[error("go command failed: {0}")]
    Go(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TestIdentity {
    /// Package import path
    pub package: String,
    /// Top-level Test/Example name (no subtest)
    pub name: String,
}

impl TestIdentity {
    pub fn key(&self) -> String {
        format!("{}/{}", self.package, self.name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestInventory {
    pub identities: Vec<TestIdentity>,
    pub hash: String,
    pub package_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntendedTestManifest {
    pub inventory_hash: String,
    pub identities: Vec<TestIdentity>,
    pub hash: String,
}

pub fn inventory_hash(identities: &[TestIdentity]) -> String {
    let mut sorted = identities.to_vec();
    sorted.sort();
    let mut h = Sha256::new();
    for id in &sorted {
        h.update(id.package.as_bytes());
        h.update([0xff]);
        h.update(id.name.as_bytes());
        h.update([0xff]);
    }
    hex::encode(h.finalize())
}

pub fn intended_manifest_hash(m: &IntendedTestManifest) -> String {
    let mut h = Sha256::new();
    h.update(m.inventory_hash.as_bytes());
    h.update(inventory_hash(&m.identities).as_bytes());
    hex::encode(h.finalize())
}

pub fn make_intended_manifest(
    inventory: &TestInventory,
    selected: Vec<TestIdentity>,
) -> Result<IntendedTestManifest, InventoryError> {
    let inv_set: std::collections::BTreeSet<_> = inventory.identities.iter().collect();
    for s in &selected {
        if !inv_set.contains(s) {
            return Err(InventoryError::Go(format!(
                "identity not in inventory: {}",
                s.key()
            )));
        }
    }
    let mut identities = selected;
    identities.sort();
    let mut m = IntendedTestManifest {
        inventory_hash: inventory.hash.clone(),
        identities,
        hash: String::new(),
    };
    m.hash = intended_manifest_hash(&m);
    Ok(m)
}

/// Collect Go test inventory under cwd for package patterns (e.g. ./...).
pub fn collect_go_inventory(
    cwd: &Path,
    package_patterns: &[&str],
) -> Result<TestInventory, InventoryError> {
    let patterns = if package_patterns.is_empty() {
        vec!["./..."]
    } else {
        package_patterns.to_vec()
    };

    // go list -f {{.ImportPath}} patterns
    let mut list_args = vec!["list", "-f", "{{.ImportPath}}"];
    list_args.extend(patterns.iter().copied());
    let list_out = run_go(cwd, &list_args)?;
    let mut packages: Vec<String> = list_out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    packages.sort();
    packages.dedup();

    let mut identities = Vec::new();
    for pkg in &packages {
        // go test -list '^(Test|Example)' <pkg>
        let out = run_go(cwd, &["test", "-list", "^(Test|Example)", pkg]);
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                // Package may fail to list; skip with empty
                let _ = e;
                continue;
            }
        };
        for line in out.lines() {
            let name = line.trim();
            if name.is_empty() || name.starts_with('?') || name.contains("ok ") {
                continue;
            }
            // go test -list prints names then sometimes "ok pkg"
            if name.starts_with("ok ") || name.starts_with("FAIL") {
                continue;
            }
            if name.starts_with("Test")
                || name.starts_with("Example")
                || name.starts_with("Benchmark")
            {
                // Only top-level (list doesn't include subtests typically)
                identities.push(TestIdentity {
                    package: pkg.clone(),
                    name: name.to_string(),
                });
            }
        }
    }

    identities.sort();
    identities.dedup();
    let hash = inventory_hash(&identities);
    Ok(TestInventory {
        package_count: packages.len() as u32,
        identities,
        hash,
    })
}

fn run_go(cwd: &Path, args: &[&str]) -> Result<String, InventoryError> {
    let output = Command::new("go").args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Some restricted desktop/sandbox environments let `go test -list`
        // complete successfully, then fail only while trimming the shared Go
        // build cache. The inventory output is still valid in that case.
        if err.contains("failed to trim cache") && !stdout.trim().is_empty() {
            return Ok(stdout.to_string());
        }
        return Err(InventoryError::Go(err.trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Match intended identities against structured run results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntendedObservation {
    pub intended_count: u32,
    pub observed_count: u32,
    pub failed_count: u32,
    pub missing: Vec<TestIdentity>,
    pub failed: Vec<TestIdentity>,
}

pub fn observe_intended(
    intended: &IntendedTestManifest,
    result: &crate::model::NormalizedStructuredResult,
) -> IntendedObservation {
    use crate::model::CaseStatus;
    let map = result.case_map();
    let mut missing = Vec::new();
    let mut failed = Vec::new();
    let mut observed = 0u32;
    for id in &intended.identities {
        match map.get(&(id.package.clone(), id.name.clone())) {
            None => missing.push(id.clone()),
            Some(c) => {
                observed += 1;
                if matches!(
                    c.status,
                    CaseStatus::Fail | CaseStatus::Incomplete | CaseStatus::NotRunDueToBuildFailure
                ) {
                    failed.push(id.clone());
                }
            }
        }
    }
    IntendedObservation {
        intended_count: intended.identities.len() as u32,
        observed_count: observed,
        failed_count: failed.len() as u32,
        missing,
        failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn inventory_on_temp_module() {
        if Command::new("go").arg("version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/s4demo\n\ngo 1.21\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("demo_test.go"),
            "package s4demo\n\nimport \"testing\"\n\nfunc TestDemo(t *testing.T) {}\n",
        )
        .unwrap();
        let inv = collect_go_inventory(dir.path(), &["./..."]).unwrap();
        assert!(
            inv.identities.iter().any(|i| i.name == "TestDemo"),
            "expected TestDemo in {:?}",
            inv.identities
        );
        let m = make_intended_manifest(&inv, inv.identities.clone()).unwrap();
        assert!(!m.hash.is_empty());
    }
}
