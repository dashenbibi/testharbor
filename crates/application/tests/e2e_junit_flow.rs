//! End-to-end structured JUnit result path without external pytest/maven deps.

use std::process::Command;
use std::thread;
use std::time::Duration;
use testharbor_application::{
    approve_test_case, confirm_profile, decide_accept_pass, generate_and_review, get_run_detail,
    open_project, poll_execution, set_finding_disposition, start_test_execution, AppState,
    GenerateTestsRequest,
};
use testharbor_domain::FindingDisposition;
use testharbor_trust::{resolve_executable, CommandIdentity, TestProfileDraft};

fn init_repo(dir: &std::path::Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "junit@test"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "junit"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "commit.gpgsign", "false"])
        .current_dir(dir)
        .status()
        .unwrap();
    std::fs::write(dir.join("pyproject.toml"), "[tool.pytest.ini_options]\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .status()
        .unwrap();
}

#[test]
fn e2e_junit_xml_structured_result_without_accept_pass() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());

    let state = AppState::open_at(data.path()).unwrap();
    let opened = open_project(&state, repo.path(), true).unwrap();
    let run_id = opened.created_run.unwrap().run_id;

    let python = resolve_executable("python3").expect("python3 is required for junit e2e");
    let script = r#"from pathlib import Path
Path('.testharbor').mkdir(exist_ok=True)
Path('.testharbor/junit.xml').write_text('''<testsuite name="current" tests="1" failures="0">
  <testcase classname="current.Suite" name="current_pass" time="0.01" />
</testsuite>
''')
"#;
    let profile = TestProfileDraft {
        name: "junit-python".into(),
        format: None,
        collect: None,
        compile: None,
        command: CommandIdentity {
            executable: python.display().to_string(),
            args: vec!["-c".into(), script.into()],
            cwd: repo.path().display().to_string(),
            parser: "junit-xml:.testharbor/junit.xml".into(),
            executable_version: None,
            executable_identity: None,
        },
        timeout_ms: 30_000,
        service: None,
        env_policy: vec![],
        network_unrestricted: false,
    };
    confirm_profile(&state, &run_id, profile).unwrap();

    let gen = generate_and_review(
        &state,
        GenerateTestsRequest {
            run_id: run_id.clone(),
            objective: "cover auth flow with junit report".into(),
            acceptance_criteria: vec!["JUnit report contains current_pass".into()],
            test_strategy: Some("unit".into()),
            cleanup_test_data: Some(true),
            force_regenerate: false,
            agent_mode: Some("mock".into()),
            reuse_agent_session: false,
            agent_model: None,
            environment: None,
            ..Default::default()
        },
    )
    .unwrap();

    for finding in gen.review.findings {
        let disposition = if matches!(finding.disposition, FindingDisposition::MustFix) {
            FindingDisposition::Resolved
        } else {
            FindingDisposition::AcceptedRisk
        };
        let reason = (disposition == FindingDisposition::AcceptedRisk).then(|| "e2e".into());
        let _ = set_finding_disposition(&state, &run_id, &finding.id, disposition, reason);
    }
    approve_test_case(&state, &run_id).unwrap();

    let worktree = get_run_detail(&state, &run_id)
        .unwrap()
        .worktree_path
        .expect("worktree path");
    let stale_dir = std::path::Path::new(&worktree).join(".testharbor");
    std::fs::create_dir_all(&stale_dir).unwrap();
    std::fs::write(
        stale_dir.join("junit.xml"),
        r#"<testsuite name="old"><testcase classname="old" name="old_fail"><failure /></testcase></testsuite>"#,
    )
    .unwrap();

    start_test_execution(&state, &run_id, None).unwrap();

    let mut final_result = None;
    for _ in 0..120 {
        let polled = poll_execution(&state, &run_id).unwrap();
        if polled.generic_result.is_some() {
            final_result = Some(polled);
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let final_result = final_result.expect("execution should finish");
    let structured = final_result
        .structured
        .as_ref()
        .expect("junit structured result");
    assert_eq!(structured.parser, "junit-xml");
    assert_eq!(structured.total, 1);
    assert_eq!(structured.passed, 1);
    assert_eq!(structured.failed, 0);
    assert!(structured
        .packages
        .iter()
        .flat_map(|package| package.cases.iter())
        .any(|case| case.name == "current_pass"));
    assert!(
        !structured
            .packages
            .iter()
            .flat_map(|package| package.cases.iter())
            .any(|case| case.name == "old_fail"),
        "stale junit xml should be cleared before execution"
    );
    assert_eq!(
        final_result.accept_pass.as_ref().map(|view| view.enabled),
        Some(false)
    );
    assert_eq!(
        final_result
            .result_review
            .as_ref()
            .map(|review| review.recommendation.as_str()),
        Some("NEEDS_HUMAN")
    );

    let err = decide_accept_pass(&state, &run_id).unwrap_err();
    assert!(err.to_string().contains("no intended manifest"));
}
