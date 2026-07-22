//! Parse `go test -json` NDJSON events.

use crate::model::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use testharbor_domain::{ResultCompleteness, ResultConsistency};

#[derive(Debug, Deserialize)]
struct GoEvent {
    #[serde(default, rename = "Action")]
    action: String,
    #[serde(default, rename = "Package")]
    package: String,
    #[serde(default, rename = "Test")]
    test: Option<String>,
    #[serde(default, rename = "Output")]
    output: Option<String>,
    #[serde(default, rename = "Elapsed")]
    elapsed: Option<f64>,
    #[serde(default, rename = "Failed")]
    failed: Option<bool>,
}

#[derive(Default)]
struct CaseAcc {
    status: Option<CaseStatus>,
    elapsed: Option<f64>,
    output: String,
    started: bool,
}

#[derive(Default)]
struct PkgAcc {
    cases: HashMap<String, CaseAcc>,
    pkg_status: Option<CaseStatus>,
    failure_phase: Option<FailurePhase>,
    build_failed: bool,
}

/// Parse go test JSON stream; exit_code used for consistency check.
pub fn parse_go_test_json_with_exit(
    stdout: &str,
    exit_code: Option<i32>,
) -> NormalizedStructuredResult {
    let mut warnings = Vec::new();
    let mut packages: HashMap<String, PkgAcc> = HashMap::new();
    let mut unparsed = 0u32;

    for (lineno, line) in stdout.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let ev: GoEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => {
                unparsed += 1;
                if unparsed <= 5 {
                    warnings.push(format!("line {}: not json event", lineno + 1));
                }
                continue;
            }
        };
        if ev.package.is_empty() {
            continue;
        }
        let pkg = packages.entry(ev.package.clone()).or_default();
        let action = ev.action.to_lowercase();

        if let Some(test) = ev.test.filter(|t| !t.is_empty()) {
            let case = pkg.cases.entry(test).or_default();
            match action.as_str() {
                "run" | "start" => case.started = true,
                "pass" => {
                    case.started = true;
                    case.status = Some(CaseStatus::Pass);
                    case.elapsed = ev.elapsed;
                }
                "fail" => {
                    case.started = true;
                    case.status = Some(CaseStatus::Fail);
                    case.elapsed = ev.elapsed;
                }
                "skip" => {
                    case.started = true;
                    case.status = Some(CaseStatus::Skip);
                    case.elapsed = ev.elapsed;
                }
                "output" => {
                    if let Some(o) = ev.output {
                        case.output.push_str(&o);
                    }
                }
                _ => {}
            }
        } else {
            // Package-level event
            match action.as_str() {
                "fail" => {
                    // build fail often has no Test field
                    if pkg.cases.is_empty() || pkg.cases.values().all(|c| c.status.is_none()) {
                        pkg.build_failed = true;
                        pkg.failure_phase = Some(FailurePhase::Build);
                        pkg.pkg_status = Some(CaseStatus::Fail);
                    } else {
                        pkg.pkg_status = Some(CaseStatus::Fail);
                        pkg.failure_phase = Some(FailurePhase::Assertion);
                    }
                }
                "pass" if pkg.pkg_status.is_none() => pkg.pkg_status = Some(CaseStatus::Pass),
                "pass" => {}
                "skip" => pkg.pkg_status = Some(CaseStatus::Skip),
                "output" => {
                    if let Some(o) = &ev.output {
                        if o.contains("build failed")
                            || (o.contains("# ") && o.contains("undefined"))
                        {
                            pkg.build_failed = true;
                            pkg.failure_phase = Some(FailurePhase::Build);
                        }
                    }
                }
                _ => {}
            }
            let _ = ev.failed;
        }
    }

    if unparsed > 5 {
        warnings.push(format!("{unparsed} unparsed lines total"));
    }

    let mut out_packages = Vec::new();
    let mut total = 0u32;
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut incomplete = 0u32;
    let mut every_started_has_terminal = true;
    let mut any_build = false;

    for (pkg_name, mut pkg) in packages {
        if pkg.build_failed {
            any_build = true;
            // Mark incomplete cases as NotRunDueToBuildFailure
            for (_name, case) in pkg.cases.iter_mut() {
                if case.status.is_none() && case.started {
                    case.status = Some(CaseStatus::NotRunDueToBuildFailure);
                }
            }
        }

        let mut cases = Vec::new();
        for (name, case) in pkg.cases {
            let status = match case.status {
                Some(s) => s,
                None if case.started => {
                    every_started_has_terminal = false;
                    incomplete += 1;
                    CaseStatus::Incomplete
                }
                None => continue, // never started — ignore noise
            };
            total += 1;
            match status {
                CaseStatus::Pass => passed += 1,
                CaseStatus::Fail => failed += 1,
                CaseStatus::Skip => skipped += 1,
                CaseStatus::Incomplete | CaseStatus::NotRunDueToBuildFailure => {
                    if status == CaseStatus::NotRunDueToBuildFailure {
                        // count as error-ish for regression
                        failed += 1;
                    }
                }
            }
            cases.push(TestCaseResult {
                package: pkg_name.clone(),
                name,
                status,
                elapsed_sec: case.elapsed,
                output: case.output,
            });
        }
        cases.sort_by(|a, b| a.name.cmp(&b.name));

        let pkg_status = pkg.pkg_status.unwrap_or_else(|| {
            if cases.iter().any(|c| c.status == CaseStatus::Fail) {
                CaseStatus::Fail
            } else if cases.iter().any(|c| c.status == CaseStatus::Incomplete) {
                CaseStatus::Incomplete
            } else if cases.is_empty() && pkg.build_failed {
                CaseStatus::Fail
            } else {
                CaseStatus::Pass
            }
        });

        out_packages.push(PackageResult {
            package: pkg_name,
            status: pkg_status,
            failure_phase: pkg.failure_phase,
            cases,
        });
    }
    out_packages.sort_by(|a, b| a.package.cmp(&b.package));

    let empty = total == 0;
    let mut completeness = if !every_started_has_terminal || unparsed > 0 && total == 0 {
        ResultCompleteness::Partial
    } else if empty && exit_code == Some(0) {
        // empty run
        ResultCompleteness::Complete
    } else if total > 0 || any_build {
        ResultCompleteness::Complete
    } else {
        ResultCompleteness::Partial
    };

    // If we have no parseable events at all
    if stdout.trim().is_empty() {
        completeness = ResultCompleteness::Missing;
        warnings.push("empty stdout".into());
    }

    let has_fail_events = failed > 0 || any_build;
    let consistency = match exit_code {
        Some(0) if has_fail_events => ResultConsistency::Contradictory,
        Some(c) if c != 0 && !has_fail_events && !empty && every_started_has_terminal => {
            // non-zero but no fail cases — could still be ok if only package fail without tests
            if any_build {
                ResultConsistency::Consistent
            } else {
                ResultConsistency::Contradictory
            }
        }
        _ => ResultConsistency::Consistent,
    };

    if empty && exit_code == Some(0) {
        warnings.push("exit 0 but zero tests executed (Empty)".into());
    }

    let result_manifest_hash =
        hash_result(&out_packages, total, passed, failed, skipped, exit_code);

    NormalizedStructuredResult {
        parser: "go-test-json".into(),
        completeness,
        consistency,
        failure_phase: if any_build {
            Some(FailurePhase::Build)
        } else if failed > 0 {
            Some(FailurePhase::Assertion)
        } else {
            None
        },
        packages: out_packages,
        total,
        passed,
        failed,
        skipped,
        incomplete,
        empty,
        warnings,
        every_started_has_terminal,
        result_manifest_hash,
    }
}

pub fn parse_go_test_json(stdout: &str) -> NormalizedStructuredResult {
    parse_go_test_json_with_exit(stdout, None)
}

fn hash_result(
    packages: &[PackageResult],
    total: u32,
    passed: u32,
    failed: u32,
    skipped: u32,
    exit_code: Option<i32>,
) -> String {
    let mut h = Sha256::new();
    h.update(b"go-test-json-v1");
    h.update(total.to_le_bytes());
    h.update(passed.to_le_bytes());
    h.update(failed.to_le_bytes());
    h.update(skipped.to_le_bytes());
    h.update(exit_code.unwrap_or(0).to_le_bytes());
    for p in packages {
        h.update(p.package.as_bytes());
        for c in &p.cases {
            h.update(c.name.as_bytes());
            h.update(format!("{:?}", c.status).as_bytes());
        }
    }
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pass_fail() {
        let stdout = r#"
{"Action":"run","Package":"example.com/p","Test":"TestOk"}
{"Action":"pass","Package":"example.com/p","Test":"TestOk","Elapsed":0.01}
{"Action":"run","Package":"example.com/p","Test":"TestBad"}
{"Action":"fail","Package":"example.com/p","Test":"TestBad","Elapsed":0.02}
{"Action":"fail","Package":"example.com/p"}
"#;
        let r = parse_go_test_json_with_exit(stdout, Some(1));
        assert_eq!(r.total, 2);
        assert_eq!(r.passed, 1);
        assert_eq!(r.failed, 1);
        assert_eq!(r.completeness, ResultCompleteness::Complete);
        assert_eq!(r.consistency, ResultConsistency::Consistent);
    }

    #[test]
    fn empty_exit_zero() {
        let r = parse_go_test_json_with_exit("", Some(0));
        assert!(r.empty || r.total == 0);
        assert!(r.warnings.iter().any(|w| w.contains("empty")
            || w.contains("Empty")
            || r.completeness == ResultCompleteness::Missing));
    }

    #[test]
    fn contradictory_exit_zero_with_fail() {
        let stdout = r#"
{"Action":"run","Package":"p","Test":"TestX"}
{"Action":"fail","Package":"p","Test":"TestX"}
{"Action":"fail","Package":"p"}
"#;
        let r = parse_go_test_json_with_exit(stdout, Some(0));
        assert_eq!(r.consistency, ResultConsistency::Contradictory);
    }

    #[test]
    fn incomplete_missing_terminal() {
        let stdout = r#"
{"Action":"run","Package":"p","Test":"TestHang"}
"#;
        let r = parse_go_test_json_with_exit(stdout, Some(1));
        assert!(!r.every_started_has_terminal);
        assert_eq!(r.completeness, ResultCompleteness::Partial);
    }
}
