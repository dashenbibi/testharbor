//! End-to-end dogfood path on a real (temp) Go module.

use std::process::Command;
use std::thread;
use std::time::Duration;
use testharbor_application::{
    apply_patch, approve_test_case, collect_inventory, confirm_profile, decide_accept_pass,
    generate_and_review, open_project, poll_execution, set_finding_disposition,
    start_test_execution, suggest_profile, AppState, GenerateTestsRequest,
};
use testharbor_domain::FindingDisposition;
use testharbor_trust::suggest_go_profile;

fn init_go_pkg(dir: &std::path::Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "e2e@test"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "e2e"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "commit.gpgsign", "false"])
        .current_dir(dir)
        .status()
        .unwrap();
    std::fs::write(
        dir.join("go.mod"),
        "module example.com/e2eflow\n\ngo 1.21\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("lib.go"),
        "package e2eflow\n\nfunc Add(a, b int) int { return a + b }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("existing_test.go"),
        "package e2eflow\n\nimport \"testing\"\n\nfunc TestPreExisting(t *testing.T) {}\n",
    )
    .unwrap();
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
fn e2e_generate_inventory_test_accept_pass_gate() {
    if Command::new("go").arg("version").output().is_err() {
        eprintln!("skip e2e: go not installed");
        return;
    }

    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_go_pkg(repo.path());

    let state = AppState::open_at(data.path()).unwrap();
    let opened = open_project(&state, repo.path(), true).unwrap();
    let run_id = opened.created_run.unwrap().run_id;

    // Freeze Profile/Trust before TestIntent and generation.
    let profile = suggest_go_profile(repo.path()).expect("go profile");
    confirm_profile(&state, &run_id, profile).unwrap();

    // 1) Generate + agent review (mock)
    let gen = generate_and_review(
        &state,
        GenerateTestsRequest {
            run_id: run_id.clone(),
            objective: "cover Add".into(),
            acceptance_criteria: vec!["TestHarborGenerated exists and passes".into()],
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
    assert!(
        gen.written_files
            .iter()
            .any(|f| f.contains("testharbor_generated")),
        "expected generated file"
    );

    // 2) Collect inventory + intended (before approve for hash alignment)
    let inv = collect_inventory(&state, &run_id, true).unwrap();
    assert!(
        inv.inventory
            .identities
            .iter()
            .any(|i| i.name == "TestHarborGenerated"),
        "inventory missing generated test: {:?}",
        inv.inventory.identities
    );
    let intended = inv.intended.as_ref().expect("intended manifest");
    assert_eq!(
        intended
            .identities
            .iter()
            .map(|identity| identity.name.as_str())
            .collect::<Vec<_>>(),
        vec!["TestHarborGenerated"],
        "intended tests should be limited to this generated candidate"
    );

    // 3) Resolve findings and approve case
    for f in gen.review.findings {
        let d = if matches!(f.disposition, FindingDisposition::MustFix) {
            FindingDisposition::Resolved
        } else {
            FindingDisposition::AcceptedRisk
        };
        let reason = if d == FindingDisposition::AcceptedRisk {
            Some("e2e".into())
        } else {
            None
        };
        let _ = set_finding_disposition(&state, &run_id, &f.id, d, reason);
    }
    // Re-approve after intended is set so approval binds intended hash
    let _ = approve_test_case(&state, &run_id).unwrap();

    // 4) Trust + go test
    let _ = suggest_profile(&state, &run_id).unwrap();

    start_test_execution(&state, &run_id, None).unwrap();
    let mut accept_enabled = false;
    let mut saw_structured = false;
    let mut observed_cases = Vec::new();
    let mut analysis_verdict = None;
    let mut review_rec = None;
    for _ in 0..600 {
        let p = poll_execution(&state, &run_id).unwrap();
        if let Some(s) = &p.structured {
            saw_structured = true;
            observed_cases = s
                .packages
                .iter()
                .flat_map(|package| package.cases.iter())
                .map(|case| case.name.clone())
                .collect::<Vec<_>>();
            assert!(
                s.total >= 1,
                "expected at least one test in structured result"
            );
        }
        if let Some(ap) = &p.accept_pass {
            if ap.enabled {
                accept_enabled = true;
            }
        }
        if let Some(a) = &p.analysis {
            analysis_verdict = Some(a.suggested_verdict.clone());
        }
        if let Some(r) = &p.result_review {
            review_rec = Some(r.recommendation.clone());
        }
        if p.generic_result.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    assert!(saw_structured, "expected go-test-json structured result");
    assert!(
        observed_cases
            .iter()
            .any(|name| name == "TestHarborGenerated"),
        "expected generated test to run, observed {observed_cases:?}"
    );
    assert!(
        observed_cases.iter().any(|name| name == "TestPreExisting"),
        "formal execution must run the full frozen Profile command, observed {observed_cases:?}"
    );
    // Green mock test + intended observed should allow AcceptPass (with case approval)
    assert!(
        accept_enabled,
        "expected AcceptPass enabled after green go test with intended"
    );
    assert!(
        analysis_verdict.is_some(),
        "every exited formal execution requires Analyzer"
    );
    assert!(
        review_rec.is_some(),
        "every exited formal execution requires an independent Result Reviewer"
    );
    assert!(state.get_analysis(&run_id).is_some());
    assert!(state.get_result_agent_review(&run_id).is_some());

    let decision = decide_accept_pass(&state, &run_id).unwrap();
    assert_eq!(decision.workflow_state, "ReadyToApply");
    let applied = apply_patch(&state, &run_id).unwrap();
    assert!(!applied.applied.applied_commit.is_empty());
    assert!(repo.path().join("testharbor_generated_test.go").exists());
}
