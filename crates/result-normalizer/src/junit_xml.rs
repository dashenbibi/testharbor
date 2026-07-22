//! Parse JUnit XML reports into structured test results.

use crate::model::*;
use roxmltree::{Document, Node};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use testharbor_domain::{ResultCompleteness, ResultConsistency};

#[derive(Default)]
struct PackageAcc {
    cases: Vec<TestCaseResult>,
    failure_phase: Option<FailurePhase>,
}

pub fn parse_junit_xml_reports_with_exit(
    reports: &[(String, String)],
    exit_code: Option<i32>,
) -> NormalizedStructuredResult {
    let mut warnings = Vec::new();
    let mut packages: BTreeMap<String, PackageAcc> = BTreeMap::new();
    let mut total = 0u32;
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let incomplete = 0u32;

    for (source, xml) in reports {
        match Document::parse(xml) {
            Ok(doc) => {
                for suite in doc
                    .descendants()
                    .filter(|node| node.is_element() && node.tag_name().name() == "testsuite")
                {
                    let suite_name = suite.attribute("name").unwrap_or(source.as_str());
                    let suite_package = suite
                        .attribute("package")
                        .filter(|value| !value.is_empty())
                        .unwrap_or(suite_name);
                    for case in suite
                        .children()
                        .filter(|node| node.is_element() && node.tag_name().name() == "testcase")
                    {
                        let case_name = case.attribute("name").unwrap_or("(unnamed)");
                        let package = package_name_for_case(case, suite_package);
                        let (status, output, phase) = case_status_and_output(case);
                        let elapsed_sec = case
                            .attribute("time")
                            .and_then(|value| value.parse::<f64>().ok());

                        total += 1;
                        match status {
                            CaseStatus::Pass => passed += 1,
                            CaseStatus::Fail => failed += 1,
                            CaseStatus::Skip => skipped += 1,
                            CaseStatus::Incomplete | CaseStatus::NotRunDueToBuildFailure => {}
                        }

                        let acc = packages.entry(package.clone()).or_default();
                        if acc.failure_phase.is_none() && phase.is_some() {
                            acc.failure_phase = phase;
                        }
                        acc.cases.push(TestCaseResult {
                            package,
                            name: case_name.to_string(),
                            status,
                            elapsed_sec,
                            output,
                        });
                    }
                }
            }
            Err(error) => {
                warnings.push(format!("failed to parse {source}: {error}"));
            }
        }
    }

    if reports.is_empty() {
        warnings.push("no junit xml reports found".into());
    }

    let mut out_packages = Vec::new();
    let mut any_failures = false;
    for (package, mut acc) in packages {
        acc.cases.sort_by(|left, right| left.name.cmp(&right.name));
        let status = if acc.cases.iter().any(|case| case.status == CaseStatus::Fail) {
            any_failures = true;
            CaseStatus::Fail
        } else if acc
            .cases
            .iter()
            .any(|case| case.status == CaseStatus::Incomplete)
        {
            CaseStatus::Incomplete
        } else if acc.cases.iter().all(|case| case.status == CaseStatus::Skip)
            && !acc.cases.is_empty()
        {
            CaseStatus::Skip
        } else {
            CaseStatus::Pass
        };

        out_packages.push(PackageResult {
            package,
            status,
            failure_phase: acc.failure_phase,
            cases: acc.cases,
        });
    }

    let completeness = if reports.is_empty() {
        ResultCompleteness::Missing
    } else if warnings
        .iter()
        .any(|warning| warning.starts_with("failed to parse"))
        && total == 0
    {
        ResultCompleteness::Partial
    } else {
        ResultCompleteness::Complete
    };
    let consistency = match exit_code {
        Some(0) if failed > 0 => ResultConsistency::Contradictory,
        Some(code) if code != 0 && failed == 0 && total > 0 => ResultConsistency::Contradictory,
        _ => ResultConsistency::Consistent,
    };
    let failure_phase = if any_failures {
        Some(FailurePhase::Assertion)
    } else {
        None
    };
    let empty = total == 0;
    let result_manifest_hash =
        hash_result(&out_packages, total, passed, failed, skipped, exit_code);

    NormalizedStructuredResult {
        parser: "junit-xml".into(),
        completeness,
        consistency,
        failure_phase,
        packages: out_packages,
        total,
        passed,
        failed,
        skipped,
        incomplete,
        empty,
        warnings,
        every_started_has_terminal: true,
        result_manifest_hash,
    }
}

fn package_name_for_case(case: Node<'_, '_>, suite_package: &str) -> String {
    case.attribute("classname")
        .filter(|value| !value.is_empty())
        .unwrap_or(suite_package)
        .to_string()
}

fn case_status_and_output(case: Node<'_, '_>) -> (CaseStatus, String, Option<FailurePhase>) {
    let mut output = String::new();
    let mut status = CaseStatus::Pass;
    let mut phase = None;

    for child in case.children().filter(|node| node.is_element()) {
        match child.tag_name().name() {
            "failure" => {
                status = CaseStatus::Fail;
                phase = Some(FailurePhase::Assertion);
                append_child_text(&mut output, child);
            }
            "error" => {
                status = CaseStatus::Fail;
                phase = Some(FailurePhase::Setup);
                append_child_text(&mut output, child);
            }
            "skipped" => {
                if status != CaseStatus::Fail {
                    status = CaseStatus::Skip;
                }
                append_child_text(&mut output, child);
            }
            "system-out" | "system-err" => {
                append_child_text(&mut output, child);
            }
            _ => {}
        }
    }

    (status, output.trim().to_string(), phase)
}

fn append_child_text(output: &mut String, node: Node<'_, '_>) {
    if let Some(message) = node.attribute("message").filter(|value| !value.is_empty()) {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(message);
    }
    if let Some(text) = node.text().filter(|value| !value.trim().is_empty()) {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(text.trim());
    }
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
    h.update(b"junit-xml-v1");
    h.update(total.to_le_bytes());
    h.update(passed.to_le_bytes());
    h.update(failed.to_le_bytes());
    h.update(skipped.to_le_bytes());
    h.update(exit_code.unwrap_or(-999).to_le_bytes());
    for package in packages {
        h.update(package.package.as_bytes());
        h.update([0xff]);
        for case in &package.cases {
            h.update(case.package.as_bytes());
            h.update([0xfe]);
            h.update(case.name.as_bytes());
            h.update([0xfd]);
            h.update(format!("{:?}", case.status).as_bytes());
        }
    }
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_junit_xml_report() {
        let xml = r#"
            <testsuite name="auth" tests="2" failures="1">
              <testcase classname="auth.LoginSuite" name="test_login_success" time="0.01" />
              <testcase classname="auth.LoginSuite" name="test_login_expired" time="0.02">
                <failure message="expected 401">stack trace</failure>
              </testcase>
            </testsuite>
        "#;

        let result =
            parse_junit_xml_reports_with_exit(&[("report.xml".into(), xml.into())], Some(1));
        assert_eq!(result.total, 2);
        assert_eq!(result.passed, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(result.parser, "junit-xml");
        assert!(!result.empty);
    }

    #[test]
    fn missing_reports_yields_missing_result() {
        let result = parse_junit_xml_reports_with_exit(&[], Some(0));
        assert_eq!(result.completeness, ResultCompleteness::Missing);
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("no junit xml reports")));
    }
}
