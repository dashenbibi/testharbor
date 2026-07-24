//! Tauri shell — IPC only; business logic in `testharbor-application`.

use std::sync::Arc;
use tauri::State;
use testharbor_application::{
    app_info, app_info_ext, apply_guard_status, apply_patch, approve_test_case, cancel_execution,
    cancel_generation, cleanup_storage, collect_inventory, comment_case_review,
    confirm_custom_command, confirm_profile, create_run as create_application_run,
    create_successor_run, decide_accept_fail, decide_accept_pass, decide_end_without_decision,
    decide_mark_environment, decide_mark_flaky, decide_mark_product_bug, delete_run,
    discard_delivery, discard_run_worktree, disk_usage, end_run, export_patch,
    freeze_worktree_candidate, generate_and_review, get_case_review, get_case_review_detail,
    get_generation_progress, get_last_execution_result, get_run_actions, get_run_audit_history,
    get_run_detail, get_worktree_status, list_claude_models, list_codex_models, list_grok_models,
    list_recent_projects, list_run_events, list_run_history, open_project, poll_execution,
    poll_execution_output as read_execution_output, prepare_execution_rerun, prepare_worktree,
    probe_path, probe_profile_readiness, recover_on_startup, reevaluate_accept_pass,
    reject_test_case, request_case_changes, request_result_test_change,
    retry_generation_with_options, set_finding_disposition, set_intended_tests,
    start_test_execution, stop_current_activity, suggest_profile, write_in_worktree,
    AcceptPassView, ActivityActionResult, AppInfo, AppInfoExt, AppState, ApplyGuardView,
    ApplyPatchResult, ApproveCaseResult, CandidateRevision, CaseReviewDetail, CaseReviewState,
    CleanupResult, CommentCaseResult, ConfirmedTrust, CreatedRunSummary, DeleteRunResult,
    DiskUsage, ExecutionPollResult, ExportPatchResult, FindingDisposition, GenerateTestsRequest,
    GenerateTestsResult, GenerationProgress, InventoryView, OpenProjectResult,
    PrepareWorktreeResult, ProbeResult, ProfileProbeResult, ProfileSuggestion, ProjectSummary,
    RecoveryReport, RejectCaseResult, ResultDecisionResult, RunEventItem, RunFilter,
    RunHistoryItem, SharedAppState, SpecAttachmentInput, StartExecutionResult, SuccessorRunResult,
    TestEnvironmentInput, TestIdentity, TestProfileDraft, UiPreferences, WorktreeStatus,
};

#[tauri::command]
fn get_app_info(state: State<'_, SharedAppState>) -> AppInfo {
    app_info(Some(state.inner()))
}

#[tauri::command]
fn get_app_info_ext(state: State<'_, SharedAppState>) -> AppInfoExt {
    app_info_ext(state.inner())
}

#[tauri::command]
fn set_agent_preferences(
    state: State<'_, SharedAppState>,
    enabled_modes: Vec<String>,
) -> Result<testharbor_application::AgentPreferences, String> {
    state
        .set_agent_preferences(enabled_modes)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_ui_language(
    state: State<'_, SharedAppState>,
    language: String,
) -> Result<UiPreferences, String> {
    state
        .set_ui_language(&language)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_events(
    state: State<'_, SharedAppState>,
    run_id: String,
    limit: u32,
) -> Result<Vec<RunEventItem>, String> {
    list_run_events(state.inner(), &run_id, limit as usize).map_err(|e| e.to_string())
}

#[tauri::command]
fn probe_repo_path(path: String) -> ProbeResult {
    probe_path(path)
}

#[tauri::command]
fn open_repo_project(
    state: State<'_, SharedAppState>,
    path: String,
    create_run: bool,
) -> Result<OpenProjectResult, String> {
    if create_run {
        return Err("open_repo_project(createRun=true) 已移除；请调用 create_project_run".into());
    }
    open_project(state.inner(), path, false).map_err(|e| e.to_string())
}

#[tauri::command]
fn create_project_run(
    state: State<'_, SharedAppState>,
    project_id: String,
) -> Result<CreatedRunSummary, String> {
    create_application_run(state.inner(), &project_id).map_err(|error| error.to_string())
}

#[tauri::command]
fn run_actions(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<Vec<testharbor_domain::UserActionDescriptor>, String> {
    get_run_actions(state.inner(), &run_id)
}

#[tauri::command]
fn stop_run_activity(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ActivityActionResult, String> {
    stop_current_activity(state.inner(), &run_id)
}

#[tauri::command]
fn end_task_run(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ActivityActionResult, String> {
    end_run(state.inner(), &run_id)
}

#[tauri::command]
fn probe_run_profile(
    state: State<'_, SharedAppState>,
    run_id: String,
    include_baseline: bool,
) -> Result<ProfileProbeResult, String> {
    probe_profile_readiness(state.inner(), &run_id, include_baseline)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_projects(state: State<'_, SharedAppState>) -> Result<Vec<ProjectSummary>, String> {
    list_recent_projects(state.inner(), 20).map_err(|e| e.to_string())
}

#[tauri::command]
fn run_audit_history(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<testharbor_application::RunAuditHistory, String> {
    get_run_audit_history(state.inner(), &run_id).map_err(|error| error.to_string())
}

#[tauri::command]
fn prepare_run_worktree(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<PrepareWorktreeResult, String> {
    prepare_worktree(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn worktree_status(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<WorktreeStatus, String> {
    get_worktree_status(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn worktree_write_file(
    state: State<'_, SharedAppState>,
    run_id: String,
    rel_path: String,
    content: String,
) -> Result<(), String> {
    write_in_worktree(state.inner(), &run_id, &rel_path, &content).map_err(|e| e.to_string())
}

#[tauri::command]
fn worktree_freeze(
    state: State<'_, SharedAppState>,
    run_id: String,
    sequence: u32,
) -> Result<CandidateRevision, String> {
    freeze_worktree_candidate(state.inner(), &run_id, sequence).map_err(|e| e.to_string())
}

#[tauri::command]
fn worktree_discard(state: State<'_, SharedAppState>, run_id: String) -> Result<(), String> {
    discard_run_worktree(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn suggest_test_profile(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ProfileSuggestion, String> {
    suggest_profile(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn confirm_test_profile(
    state: State<'_, SharedAppState>,
    run_id: String,
    profile: TestProfileDraft,
) -> Result<ConfirmedTrust, String> {
    confirm_profile(state.inner(), &run_id, profile).map_err(|e| e.to_string())
}

#[tauri::command]
fn confirm_test_command(
    state: State<'_, SharedAppState>,
    run_id: String,
    executable: String,
    args: Vec<String>,
    timeout_ms: u64,
) -> Result<ConfirmedTrust, String> {
    confirm_custom_command(state.inner(), &run_id, executable, args, timeout_ms)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn start_execution(
    state: State<'_, SharedAppState>,
    run_id: String,
    case_id: Option<String>,
) -> Result<StartExecutionResult, String> {
    start_test_execution(state.inner(), &run_id, case_id.as_deref()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn poll_run_execution(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ExecutionPollResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        poll_execution(&state, &run_id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn poll_execution_output(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ExecutionPollResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        read_execution_output(&state, &run_id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn cancel_run_execution(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ExecutionPollResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        cancel_execution(&state, &run_id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn collect_run_inventory(
    state: State<'_, SharedAppState>,
    run_id: String,
    select_all: bool,
) -> Result<InventoryView, String> {
    collect_inventory(state.inner(), &run_id, select_all).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_run_intended(
    state: State<'_, SharedAppState>,
    run_id: String,
    identities: Vec<TestIdentity>,
) -> Result<testharbor_application::IntendedTestManifest, String> {
    set_intended_tests(state.inner(), &run_id, identities).map_err(|e| e.to_string())
}

#[tauri::command]
fn eval_accept_pass(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<AcceptPassView, String> {
    reevaluate_accept_pass(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn generate_tests(
    state: State<'_, SharedAppState>,
    run_id: String,
    objective: String,
    acceptance_criteria: Vec<String>,
    source_anchors: Option<Vec<testharbor_domain::SourceAnchor>>,
    generation_scope: Option<Vec<String>>,
    spec_attachments: Option<Vec<SpecAttachmentInput>>,
    test_strategy: Option<String>,
    cleanup_test_data: Option<bool>,
    agent_mode: Option<String>,
    agent_model: Option<String>,
    reuse_agent_session: bool,
    environment: Option<TestEnvironmentInput>,
) -> Result<GenerateTestsResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        generate_and_review(
            &state,
            GenerateTestsRequest {
                run_id,
                objective,
                acceptance_criteria,
                source_anchors: source_anchors.unwrap_or_default(),
                generation_scope: generation_scope.unwrap_or_default(),
                spec_attachments: spec_attachments.unwrap_or_default(),
                test_strategy,
                cleanup_test_data,
                force_regenerate: false,
                agent_mode,
                reuse_agent_session,
                agent_model,
                environment,
            },
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_grok_models() -> Result<testharbor_application::GrokModelCatalog, String> {
    tauri::async_runtime::spawn_blocking(list_grok_models)
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn get_codex_models() -> Result<testharbor_application::CliModelCatalog, String> {
    tauri::async_runtime::spawn_blocking(list_codex_models)
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn get_claude_models() -> Result<testharbor_application::CliModelCatalog, String> {
    tauri::async_runtime::spawn_blocking(list_claude_models)
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
fn set_run_agent_model(
    state: State<'_, SharedAppState>,
    run_id: String,
    agent_model: Option<String>,
) -> Result<(), String> {
    testharbor_application::set_agent_model(state.inner(), &run_id, agent_model)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn generation_progress(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<GenerationProgress, String> {
    get_generation_progress(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn cancel_test_generation(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<bool, String> {
    cancel_generation(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn retry_test_generation(
    state: State<'_, SharedAppState>,
    run_id: String,
    force_regenerate: Option<bool>,
) -> Result<GenerateTestsResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        retry_generation_with_options(&state, &run_id, force_regenerate.unwrap_or(false))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn last_execution(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ExecutionPollResult, String> {
    get_last_execution_result(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn case_review(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<CaseReviewState, String> {
    get_case_review(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn case_review_detail(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<CaseReviewDetail, String> {
    get_case_review_detail(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_finding(
    state: State<'_, SharedAppState>,
    run_id: String,
    finding_id: String,
    disposition: String,
    risk_reason: Option<String>,
) -> Result<CaseReviewState, String> {
    let d = match disposition.as_str() {
        "open" => FindingDisposition::Open,
        "must_fix" => FindingDisposition::MustFix,
        "accepted_risk" => FindingDisposition::AcceptedRisk,
        "resolved" => FindingDisposition::Resolved,
        _ => return Err(format!("unknown disposition: {disposition}")),
    };
    set_finding_disposition(state.inner(), &run_id, &finding_id, d, risk_reason)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn approve_case(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ApproveCaseResult, String> {
    approve_test_case(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn comment_case(
    state: State<'_, SharedAppState>,
    run_id: String,
    message: String,
) -> Result<CommentCaseResult, String> {
    comment_case_review(state.inner(), &run_id, &message).map_err(|e| e.to_string())
}

#[tauri::command]
fn reject_case(
    state: State<'_, SharedAppState>,
    run_id: String,
    message: String,
) -> Result<RejectCaseResult, String> {
    reject_test_case(state.inner(), &run_id, &message).map_err(|e| e.to_string())
}

#[tauri::command]
fn request_changes(
    state: State<'_, SharedAppState>,
    run_id: String,
    message: String,
) -> Result<CaseReviewState, String> {
    request_case_changes(state.inner(), &run_id, &message).map_err(|e| e.to_string())
}

#[tauri::command]
fn result_accept_pass(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ResultDecisionResult, String> {
    decide_accept_pass(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn result_accept_fail(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ResultDecisionResult, String> {
    decide_accept_fail(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn result_mark_bug(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ResultDecisionResult, String> {
    decide_mark_product_bug(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn result_mark_env(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ResultDecisionResult, String> {
    decide_mark_environment(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn result_mark_flaky(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ResultDecisionResult, String> {
    decide_mark_flaky(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn result_request_test_change(
    state: State<'_, SharedAppState>,
    run_id: String,
    feedback: String,
) -> Result<GenerateTestsResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        request_result_test_change(&state, &run_id, &feedback).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn result_end_no_decision(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ResultDecisionResult, String> {
    decide_end_without_decision(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn result_rerun(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ResultDecisionResult, String> {
    prepare_execution_rerun(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_apply_guard(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ApplyGuardView, String> {
    apply_guard_status(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn apply_run_patch(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ApplyPatchResult, String> {
    apply_patch(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn export_run_patch(
    state: State<'_, SharedAppState>,
    run_id: String,
    complete: bool,
) -> Result<ExportPatchResult, String> {
    export_patch(state.inner(), &run_id, complete).map_err(|e| e.to_string())
}

#[tauri::command]
fn discard_run_delivery(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<ResultDecisionResult, String> {
    discard_delivery(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn create_run_successor(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<SuccessorRunResult, String> {
    create_successor_run(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_history(
    state: State<'_, SharedAppState>,
    project_id: Option<String>,
    filter: String,
    limit: u32,
) -> Result<Vec<RunHistoryItem>, String> {
    let f = match filter.as_str() {
        "in_progress" => RunFilter::InProgress,
        "pending_review" => RunFilter::PendingReview,
        "ready_to_apply" => RunFilter::ReadyToApply,
        "completed" => RunFilter::Completed,
        "failed_or_cancelled" => RunFilter::FailedOrCancelled,
        _ => RunFilter::All,
    };
    list_run_history(state.inner(), project_id, f, limit as usize).map_err(|e| e.to_string())
}

#[tauri::command]
fn run_detail(state: State<'_, SharedAppState>, run_id: String) -> Result<RunHistoryItem, String> {
    get_run_detail(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_task_run(
    state: State<'_, SharedAppState>,
    run_id: String,
) -> Result<DeleteRunResult, String> {
    delete_run(state.inner(), &run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_disk_usage(state: State<'_, SharedAppState>) -> Result<DiskUsage, String> {
    disk_usage(state.inner()).map_err(|e| e.to_string())
}

#[tauri::command]
fn run_cleanup(
    state: State<'_, SharedAppState>,
    remove_terminal_worktrees: bool,
    delete_completed_beyond: Option<u32>,
) -> Result<CleanupResult, String> {
    cleanup_storage(
        state.inner(),
        remove_terminal_worktrees,
        delete_completed_beyond.map(|n| n as usize),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn run_recovery(state: State<'_, SharedAppState>) -> Result<RecoveryReport, String> {
    recover_on_startup(state.inner()).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    std::env::set_var("TESTHARBOR_PROCESS_LAUNCHER_REQUIRED", "1");
    let app_state: SharedAppState = Arc::new(AppState::open_default().unwrap_or_else(|e| {
        panic!("failed to open TestHarbor data store: {e}");
    }));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            get_app_info_ext,
            set_ui_language,
            set_agent_preferences,
            get_codex_models,
            get_claude_models,
            get_grok_models,
            list_events,
            probe_repo_path,
            open_repo_project,
            create_project_run,
            run_actions,
            run_audit_history,
            stop_run_activity,
            end_task_run,
            list_projects,
            prepare_run_worktree,
            worktree_status,
            worktree_write_file,
            worktree_freeze,
            worktree_discard,
            suggest_test_profile,
            confirm_test_profile,
            probe_run_profile,
            confirm_test_command,
            start_execution,
            poll_run_execution,
            poll_execution_output,
            cancel_run_execution,
            collect_run_inventory,
            set_run_intended,
            eval_accept_pass,
            generate_tests,
            set_run_agent_model,
            cancel_test_generation,
            generation_progress,
            retry_test_generation,
            last_execution,
            case_review,
            case_review_detail,
            set_finding,
            approve_case,
            comment_case,
            reject_case,
            request_changes,
            result_accept_pass,
            result_accept_fail,
            result_mark_bug,
            result_mark_env,
            result_mark_flaky,
            result_request_test_change,
            result_end_no_decision,
            result_rerun,
            get_apply_guard,
            apply_run_patch,
            export_run_patch,
            discard_run_delivery,
            create_run_successor,
            list_history,
            run_detail,
            delete_task_run,
            get_disk_usage,
            run_cleanup,
            run_recovery,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
