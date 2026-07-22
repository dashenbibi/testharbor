use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use testharbor_application::{
    app_info_ext, apply_guard_status, apply_patch, approve_test_case, cancel_execution,
    cancel_generation, cleanup_storage, comment_case_review, confirm_profile, create_run,
    decide_accept_fail, decide_accept_pass, decide_end_without_decision, decide_mark_environment,
    decide_mark_flaky, decide_mark_product_bug, delete_run, discard_delivery, disk_usage, end_run,
    export_patch, generate_and_review, get_case_review_detail, get_last_execution_result,
    get_run_actions, get_run_audit_history, get_run_detail, list_claude_models, list_codex_models,
    list_grok_models, list_recent_projects, list_run_events, list_run_history, open_project,
    poll_execution, prepare_execution_rerun, probe_profile_readiness, recover_on_startup,
    reject_test_case, request_case_changes, request_result_test_change,
    retry_generation_with_options, set_agent_model, set_execution_timeout, set_finding_disposition,
    start_test_execution, stop_current_activity, suggest_profile, AppState, GenerateTestsRequest,
    RunFilter, SpecAttachmentInput, TestEnvironmentInput, TestProfileDraft,
};
use testharbor_domain::{FindingDisposition, SourceAnchor, SpecAttachmentKind};

#[derive(Debug, Parser)]
#[command(
    name = "testharbor",
    version,
    about = "TestHarbor local test orchestration CLI"
)]
struct Cli {
    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Override the local data directory for CI or isolated testing.
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Set and remember the language used for human-readable output.
    #[arg(long, global = true, value_enum)]
    language: Option<LanguageArg>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LanguageArg {
    #[value(name = "en")]
    English,
    #[value(name = "zh-CN", alias = "zh", alias = "zh-cn")]
    SimplifiedChinese,
}

impl LanguageArg {
    fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    Info,
    Projects {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    Runs {
        #[command(subcommand)]
        command: RunCommand,
    },
    Generate(GenerateArgs),
    Retry(RetryArgs),
    CancelGeneration(RunIdArgs),
    GenerationProgress(RunIdArgs),
    Model(SetModelArgs),
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },
    Execute {
        #[command(subcommand)]
        command: ExecuteCommand,
    },
    Result {
        #[command(subcommand)]
        command: ResultCommand,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Environment(EnvironmentArgs),
    Init(InitArgs),
    Apply {
        #[command(subcommand)]
        command: ApplyCommand,
    },
    Cleanup(CleanupArgs),
    Recovery,
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    List(ListArgs),
    Open(OpenArgs),
    Probe { path: PathBuf },
}

#[derive(Debug, Subcommand)]
enum RunCommand {
    Create(RunCreateArgs),
    List(RunListArgs),
    Show(RunIdArgs),
    Audit(RunIdArgs),
    Events {
        run_id: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    Delete(RunIdArgs),
    Usage,
    Actions(RunIdArgs),
    Stop(RunIdArgs),
    End(RunIdArgs),
}

#[derive(Debug, Args)]
struct RunCreateArgs {
    #[arg(long)]
    project_id: String,
}

#[derive(Debug, Subcommand)]
enum ReviewCommand {
    Show(RunIdArgs),
    Approve(RunIdArgs),
    Reject(MessageArgs),
    Changes(MessageArgs),
    Comment(MessageArgs),
    Finding(FindingArgs),
}

#[derive(Debug, Subcommand)]
enum ExecuteCommand {
    #[command(hide = true)]
    Profile(RunIdArgs),
    #[command(hide = true)]
    ConfirmProfile {
        run_id: String,
        #[arg(long, value_name = "JSON")]
        file: PathBuf,
    },
    Start(ExecuteStartArgs),
    Timeout(ExecuteTimeoutArgs),
    Poll(RunIdArgs),
    Last(RunIdArgs),
    Cancel(RunIdArgs),
    #[command(hide = true)]
    Probe {
        run_id: String,
        #[arg(long)]
        baseline: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ResultCommand {
    AcceptPass(RunIdArgs),
    AcceptFail(RunIdArgs),
    MarkBug(RunIdArgs),
    MarkEnvironment(RunIdArgs),
    MarkFlaky(RunIdArgs),
    EndWithoutDecision(RunIdArgs),
    Rerun(RunIdArgs),
    Changes(MessageArgs),
}

#[derive(Debug, Subcommand)]
enum ApplyCommand {
    Guard(RunIdArgs),
    Run(RunIdArgs),
    Export {
        run_id: String,
        #[arg(long)]
        complete: bool,
    },
    Discard(RunIdArgs),
}

#[derive(Debug, Subcommand)]
enum ModelsCommand {
    Codex,
    Claude,
    Grok,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Show,
    Language {
        #[arg(value_enum)]
        language: LanguageArg,
    },
    Agents {
        #[arg(long = "enable", value_name = "CLI", required = true)]
        enabled_modes: Vec<String>,
    },
}

#[derive(Debug, Args)]
struct ListArgs {
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Debug, Args)]
struct OpenArgs {
    path: PathBuf,
    #[arg(long)]
    create_run: bool,
}

#[derive(Debug, Args)]
struct RunIdArgs {
    run_id: String,
}

#[derive(Debug, Args)]
struct MessageArgs {
    run_id: String,
    #[arg(long, short = 'm')]
    message: String,
}

#[derive(Debug, Args)]
struct FindingArgs {
    run_id: String,
    finding_id: String,
    #[arg(value_enum)]
    disposition: DispositionArg,
    #[arg(long)]
    risk_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DispositionArg {
    Open,
    MustFix,
    AcceptedRisk,
    Resolved,
}

impl From<DispositionArg> for FindingDisposition {
    fn from(value: DispositionArg) -> Self {
        match value {
            DispositionArg::Open => Self::Open,
            DispositionArg::MustFix => Self::MustFix,
            DispositionArg::AcceptedRisk => Self::AcceptedRisk,
            DispositionArg::Resolved => Self::Resolved,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FilterArg {
    All,
    InProgress,
    PendingReview,
    ReadyToApply,
    Completed,
    FailedOrCancelled,
}

impl From<FilterArg> for RunFilter {
    fn from(value: FilterArg) -> Self {
        match value {
            FilterArg::All => Self::All,
            FilterArg::InProgress => Self::InProgress,
            FilterArg::PendingReview => Self::PendingReview,
            FilterArg::ReadyToApply => Self::ReadyToApply,
            FilterArg::Completed => Self::Completed,
            FilterArg::FailedOrCancelled => Self::FailedOrCancelled,
        }
    }
}

#[derive(Debug, Args)]
struct RunListArgs {
    #[arg(long)]
    project_id: Option<String>,
    #[arg(long, value_enum, default_value_t = FilterArg::All)]
    filter: FilterArg,
    #[arg(long, default_value_t = 50)]
    limit: usize,
}

#[derive(Debug, Args)]
struct GenerateArgs {
    run_id: String,
    /// 测试提示词；保留 --objective 作为兼容别名
    #[arg(long = "prompt", visible_alias = "objective")]
    prompt: String,
    /// 验收项；可重复传入，也可在同一参数中用分号分隔
    #[arg(long = "criteria", short = 'c', required = true)]
    acceptance_criteria: Vec<String>,
    /// 高级兼容参数：代码锚点由 Agent 根据仓库自行发现，通常不需要填写。
    #[arg(long = "anchor", value_name = "KIND=VALUE", hide = true)]
    source_anchors: Vec<String>,
    #[arg(long = "scope", value_name = "GLOB", hide = true)]
    generation_scope: Vec<String>,
    #[arg(long = "attach-text", value_name = "LABEL=TEXT", hide = true)]
    pasted_attachments: Vec<String>,
    #[arg(long = "attach-markdown", value_name = "REPO_PATH", hide = true)]
    markdown_attachments: Vec<String>,
    #[arg(long, default_value = "e2e_api_flow", hide = true)]
    strategy: String,
    #[arg(long, hide = true)]
    no_cleanup: bool,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    reuse_session: bool,
    #[arg(long)]
    follow: bool,
    /// 一段提供给 Agent 的测试环境说明，可包含账号、密码、服务地址和依赖说明。
    #[arg(long)]
    environment: Option<String>,
    #[arg(long, hide = true)]
    environment_file: Option<PathBuf>,
    #[arg(long, hide = true)]
    base_url: Option<String>,
    #[arg(long, hide = true)]
    health_url: Option<String>,
    #[arg(long, hide = true)]
    login_url: Option<String>,
    /// Inherit a masked runtime value from the CLI process environment.
    #[arg(long = "var", value_name = "KEY", hide = true)]
    variables: Vec<String>,
}

#[derive(Debug, Args)]
struct RetryArgs {
    run_id: String,
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct SetModelArgs {
    run_id: String,
    #[arg(long, conflicts_with = "clear")]
    model: Option<String>,
    #[arg(long)]
    clear: bool,
}

#[derive(Debug, Args)]
struct ExecuteStartArgs {
    run_id: String,
    #[arg(long)]
    case_id: Option<String>,
    #[arg(long)]
    wait: bool,
    #[arg(long, default_value_t = 500)]
    interval_ms: u64,
}

#[derive(Debug, Args)]
struct ExecuteTimeoutArgs {
    run_id: String,
    /// Timeout in milliseconds. Use exactly one of --milliseconds, --seconds or --minutes.
    #[arg(long)]
    milliseconds: Option<u64>,
    /// Timeout in seconds. Use exactly one of --milliseconds, --seconds or --minutes.
    #[arg(long)]
    seconds: Option<u64>,
    /// Timeout in minutes. Use exactly one of --milliseconds, --seconds or --minutes.
    #[arg(long)]
    minutes: Option<u64>,
}

#[derive(Debug, Args)]
struct CleanupArgs {
    #[arg(long)]
    remove_terminal_worktrees: bool,
    #[arg(long)]
    keep_completed: Option<usize>,
}

#[derive(Debug, Args)]
struct EnvironmentArgs {
    #[arg(long)]
    base_url: String,
}

#[derive(Debug, Args)]
struct InitArgs {
    /// 项目路径；交互模式下默认当前目录
    path: Option<PathBuf>,
    /// 测试提示词
    #[arg(long = "prompt", visible_alias = "objective")]
    prompt: Option<String>,
    /// 验收项；可重复传入，也可在交互输入中用分号分隔
    #[arg(long = "criteria", short = 'c')]
    acceptance_criteria: Vec<String>,
    /// 一段提供给 Agent 的测试环境说明，可包含账号、密码、服务地址和依赖说明。
    #[arg(long)]
    environment: Option<String>,
    #[arg(long, hide = true)]
    environment_file: Option<PathBuf>,
    /// 创建任务后立即生成测试候选；默认只创建任务
    #[arg(long)]
    generate: bool,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    reuse_session: bool,
}

fn main() {
    std::env::set_var("TESTHARBOR_PROCESS_LAUNCHER_REQUIRED", "1");
    if let Err(error) = run() {
        eprintln!("testharbor: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut cli = Cli::parse();
    let state = match &cli.data_dir {
        Some(path) => AppState::open_at(path)
            .context("failed to open the specified TestHarbor data directory")?,
        None => AppState::open_default().context("failed to open the TestHarbor data directory")?,
    };
    if let Some(language) = cli.language {
        state.set_ui_language(language.code())?;
    } else {
        cli.language = Some(if state.get_ui_preferences().language == "zh-CN" {
            LanguageArg::SimplifiedChinese
        } else {
            LanguageArg::English
        });
    }
    dispatch(&cli, &state)
}

fn localized<'a>(cli: &Cli, english: &'a str, simplified_chinese: &'a str) -> &'a str {
    match cli.language.unwrap_or(LanguageArg::English) {
        LanguageArg::English => english,
        LanguageArg::SimplifiedChinese => simplified_chinese,
    }
}

fn dispatch(cli: &Cli, state: &AppState) -> Result<()> {
    match &cli.command {
        Command::Info => emit(cli, &app_info_ext(state)),
        Command::Projects { command } => dispatch_projects(cli, state, command),
        Command::Runs { command } => dispatch_runs(cli, state, command),
        Command::Generate(args) => generate(cli, state, args),
        Command::Retry(args) => emit(
            cli,
            &retry_generation_with_options(state, &args.run_id, args.force)?,
        ),
        Command::CancelGeneration(args) => emit(cli, &cancel_generation(state, &args.run_id)?),
        Command::GenerationProgress(args) => emit(
            cli,
            &testharbor_application::get_generation_progress(state, &args.run_id)?,
        ),
        Command::Model(args) => set_model(cli, state, args),
        Command::Review { command } => dispatch_review(cli, state, command),
        Command::Execute { command } => dispatch_execute(cli, state, command),
        Command::Result { command } => dispatch_result(cli, state, command),
        Command::Apply { command } => dispatch_apply(cli, state, command),
        Command::Config { command } => dispatch_config(cli, state, command),
        Command::Environment(args) => check_environment(cli, args),
        Command::Init(args) => init_task(cli, state, args),
        Command::Cleanup(args) => emit(
            cli,
            &cleanup_storage(state, args.remove_terminal_worktrees, args.keep_completed)?,
        ),
        Command::Recovery => emit(cli, &recover_on_startup(state)?),
        Command::Models { command } => dispatch_models(cli, command),
    }
}

fn set_model(cli: &Cli, state: &AppState, args: &SetModelArgs) -> Result<()> {
    if !args.clear && args.model.is_none() {
        bail!(
            "{}",
            localized(
                cli,
                "provide --model <MODEL>, or use --clear to remove the per-task model override",
                "请提供 --model <MODEL>，或使用 --clear 清除任务级模型覆盖"
            )
        );
    }
    emit(
        cli,
        &set_agent_model(state, &args.run_id, args.model.clone())?,
    )
}

fn dispatch_projects(cli: &Cli, state: &AppState, command: &ProjectCommand) -> Result<()> {
    match command {
        ProjectCommand::List(args) => {
            let projects = list_recent_projects(state, args.limit)?;
            if cli.json {
                emit(cli, &projects)
            } else {
                for project in projects {
                    println!("{}\t{}\t{}", project.id, project.name, project.root_path);
                }
                Ok(())
            }
        }
        ProjectCommand::Open(args) => {
            if args.create_run {
                bail!(
                    "{}",
                    localized(
                        cli,
                        "projects open --create-run was removed; use runs create --project-id <ID>",
                        "projects open --create-run 已移除；请使用 runs create --project-id <ID>"
                    )
                );
            }
            emit(cli, &open_project(state, &args.path, false)?)
        }
        ProjectCommand::Probe { path } => {
            emit(cli, &testharbor_application::probe_path(path.clone()))
        }
    }
}

fn dispatch_runs(cli: &Cli, state: &AppState, command: &RunCommand) -> Result<()> {
    match command {
        RunCommand::Create(args) => emit(cli, &create_run(state, &args.project_id)?),
        RunCommand::List(args) => {
            let runs = list_run_history(
                state,
                args.project_id.clone(),
                args.filter.into(),
                args.limit,
            )?;
            if cli.json {
                emit(cli, &runs)
            } else {
                for run in runs {
                    println!(
                        "{}\t{}\t{}\t{}",
                        run.run_id, run.workflow_state, run.project_name, run.objective
                    );
                }
                Ok(())
            }
        }
        RunCommand::Show(args) => emit(cli, &get_run_detail(state, &args.run_id)?),
        RunCommand::Audit(args) => emit(cli, &get_run_audit_history(state, &args.run_id)?),
        RunCommand::Events { run_id, limit } => emit(cli, &list_run_events(state, run_id, *limit)?),
        RunCommand::Delete(args) => emit(cli, &delete_run(state, &args.run_id)?),
        RunCommand::Usage => emit(cli, &disk_usage(state)?),
        RunCommand::Actions(args) => emit(
            cli,
            &get_run_actions(state, &args.run_id).map_err(anyhow::Error::msg)?,
        ),
        RunCommand::Stop(args) => emit(
            cli,
            &stop_current_activity(state, &args.run_id).map_err(anyhow::Error::msg)?,
        ),
        RunCommand::End(args) => emit(
            cli,
            &end_run(state, &args.run_id).map_err(anyhow::Error::msg)?,
        ),
    }
}

fn generate(cli: &Cli, state: &AppState, args: &GenerateArgs) -> Result<()> {
    let environment = build_environment(args)?;
    let source_anchors = args
        .source_anchors
        .iter()
        .map(|value| {
            let (kind, anchor) = value
                .split_once('=')
                .with_context(|| format!("代码锚点必须是 KIND=VALUE: {value}"))?;
            Ok(SourceAnchor {
                kind: kind.trim().to_string(),
                value: anchor.trim().to_string(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut spec_attachments = args
        .pasted_attachments
        .iter()
        .map(|value| {
            let (label, body) = value
                .split_once('=')
                .with_context(|| format!("粘贴附件必须是 LABEL=TEXT: {value}"))?;
            Ok(SpecAttachmentInput {
                kind: SpecAttachmentKind::PastedText,
                label_or_path: label.trim().to_string(),
                body: Some(body.to_string()),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    spec_attachments.extend(
        args.markdown_attachments
            .iter()
            .map(|path| SpecAttachmentInput {
                kind: SpecAttachmentKind::RepoMarkdown,
                label_or_path: path.clone(),
                body: None,
            }),
    );
    let request = GenerateTestsRequest {
        run_id: args.run_id.clone(),
        objective: args.prompt.clone(),
        acceptance_criteria: normalize_criteria(&args.acceptance_criteria),
        source_anchors,
        generation_scope: args.generation_scope.clone(),
        spec_attachments,
        test_strategy: Some(args.strategy.clone()),
        cleanup_test_data: Some(!args.no_cleanup),
        force_regenerate: false,
        agent_mode: args.agent.clone(),
        reuse_agent_session: args.reuse_session,
        agent_model: args.model.clone(),
        environment,
    };
    let result = if args.follow {
        generate_with_progress(cli, state, request)?
    } else {
        generate_and_review(state, request)?
    };
    if cli.json {
        emit(cli, &result)
    } else {
        println!(
            "{}: agent={} files={} revision=r{}",
            localized(cli, "Generation complete", "生成完成"),
            result.agent_mode,
            result.written_files.len(),
            result.candidate.sequence
        );
        println!("run_id={}", args.run_id);
        println!(
            "{}: {}",
            localized(cli, "Review findings", "评审发现"),
            result.review.findings.len()
        );
        Ok(())
    }
}

fn generate_with_progress(
    cli: &Cli,
    state: &AppState,
    request: GenerateTestsRequest,
) -> Result<testharbor_application::GenerateTestsResult> {
    thread::scope(|scope| {
        let run_id = request.run_id.clone();
        let worker = scope.spawn(|| generate_and_review(state, request));
        let mut stdout_tail = String::new();
        let mut stderr_tail = String::new();
        while !worker.is_finished() {
            if let Ok(progress) = testharbor_application::get_generation_progress(state, &run_id) {
                print_progress_delta(cli, &progress.stdout_tail, &mut stdout_tail, "stdout");
                print_progress_delta(cli, &progress.stderr_tail, &mut stderr_tail, "stderr");
            }
            thread::sleep(Duration::from_millis(300));
        }
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("生成线程异常退出"))?
            .map_err(Into::into)
    })
}

fn print_progress_delta(cli: &Cli, current: &str, previous: &mut String, stream: &str) {
    if current.is_empty() || current == previous {
        return;
    }
    let delta = current
        .strip_prefix(previous.as_str())
        .unwrap_or(current)
        .trim_end();
    if !delta.is_empty() {
        if cli.json {
            println!(
                "{}",
                serde_json::json!({
                    "type": "generation_progress",
                    "stream": stream,
                    "text": delta,
                })
            );
        } else {
            print!("{delta}\n");
        }
        let _ = std::io::stdout().flush();
    }
    *previous = current.to_string();
}

fn build_environment(args: &GenerateArgs) -> Result<Option<TestEnvironmentInput>> {
    build_environment_from_inputs(
        args.environment.as_deref(),
        args.environment_file.as_deref(),
        &args.variables,
        args.base_url.as_deref(),
        args.health_url.as_deref(),
        args.login_url.as_deref(),
    )
}

fn build_environment_from_inputs(
    environment: Option<&str>,
    environment_file: Option<&Path>,
    variables: &[String],
    base_url: Option<&str>,
    health_url: Option<&str>,
    login_url: Option<&str>,
) -> Result<Option<TestEnvironmentInput>> {
    let file_context = environment_file
        .map(std::fs::read_to_string)
        .transpose()
        .context("读取环境信息文件失败")?;
    let context = [file_context.as_deref(), environment]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let variables = parse_variables(variables)?;
    let has_environment = !context.is_empty()
        || !variables.is_empty()
        || base_url.is_some()
        || health_url.is_some()
        || login_url.is_some();
    Ok(has_environment.then_some(TestEnvironmentInput {
        context,
        base_url: base_url.unwrap_or_default().to_string(),
        health_url: health_url.map(str::to_string),
        login_url: login_url.map(str::to_string),
        startup_command: None,
        startup_cwd: None,
        username: None,
        password: None,
        variables,
    }))
}

fn parse_variables(values: &[String]) -> Result<HashMap<String, String>> {
    values
        .iter()
        .map(|key| {
            let key = key.trim();
            if key.is_empty() {
                bail!("环境变量名不能为空");
            }
            let value =
                std::env::var(key).with_context(|| format!("运行时环境变量未设置：{key}"))?;
            Ok((key.to_string(), value))
        })
        .collect()
}

fn dispatch_review(cli: &Cli, state: &AppState, command: &ReviewCommand) -> Result<()> {
    match command {
        ReviewCommand::Show(args) => emit(cli, &get_case_review_detail(state, &args.run_id)?),
        ReviewCommand::Approve(args) => emit(cli, &approve_test_case(state, &args.run_id)?),
        ReviewCommand::Reject(args) => {
            emit(cli, &reject_test_case(state, &args.run_id, &args.message)?)
        }
        ReviewCommand::Changes(args) => emit(
            cli,
            &request_case_changes(state, &args.run_id, &args.message)?,
        ),
        ReviewCommand::Comment(args) => emit(
            cli,
            &comment_case_review(state, &args.run_id, &args.message)?,
        ),
        ReviewCommand::Finding(args) => emit(
            cli,
            &set_finding_disposition(
                state,
                &args.run_id,
                &args.finding_id,
                args.disposition.into(),
                args.risk_reason.clone(),
            )?,
        ),
    }
}

fn dispatch_execute(cli: &Cli, state: &AppState, command: &ExecuteCommand) -> Result<()> {
    match command {
        ExecuteCommand::Profile(args) => emit(cli, &suggest_profile(state, &args.run_id)?),
        ExecuteCommand::ConfirmProfile { run_id, file } => {
            let profile: TestProfileDraft = serde_json::from_slice(
                &std::fs::read(file)
                    .with_context(|| format!("读取 Profile 文件失败: {}", file.display()))?,
            )
            .context("Profile JSON 格式无效")?;
            emit(cli, &confirm_profile(state, run_id, profile)?)
        }
        ExecuteCommand::Start(args) => {
            if state.get_trust_grant(&args.run_id).is_none() {
                let profile = suggest_profile(state, &args.run_id)?;
                confirm_profile(state, &args.run_id, profile.profile)?;
            }
            let started = start_test_execution(state, &args.run_id, args.case_id.as_deref())?;
            if !args.wait {
                emit(cli, &started)
            } else {
                println!(
                    "{}: {}",
                    localized(cli, "Execution started", "执行已启动"),
                    started.execution_id
                );
                wait_for_execution(cli, state, &args.run_id, args.interval_ms)
            }
        }
        ExecuteCommand::Timeout(args) => {
            let units = [
                args.milliseconds.is_some(),
                args.seconds.is_some(),
                args.minutes.is_some(),
            ]
            .into_iter()
            .filter(|present| *present)
            .count();
            if units != 1 {
                bail!("execute timeout requires exactly one of --milliseconds, --seconds or --minutes");
            }
            let timeout_ms = if let Some(milliseconds) = args.milliseconds {
                milliseconds
            } else if let Some(seconds) = args.seconds {
                seconds
                    .checked_mul(1_000)
                    .context("seconds value is too large")?
            } else {
                args.minutes
                    .expect("validated exactly one timeout unit")
                    .checked_mul(60_000)
                    .context("minutes value is too large")?
            };
            emit(
                cli,
                &set_execution_timeout(state, &args.run_id, timeout_ms)?,
            )
        }
        ExecuteCommand::Poll(args) => emit(cli, &poll_execution(state, &args.run_id)?),
        ExecuteCommand::Last(args) => emit(cli, &get_last_execution_result(state, &args.run_id)?),
        ExecuteCommand::Cancel(args) => emit(cli, &cancel_execution(state, &args.run_id)?),
        ExecuteCommand::Probe { run_id, baseline } => {
            emit(cli, &probe_profile_readiness(state, run_id, *baseline)?)
        }
    }
}

fn wait_for_execution(cli: &Cli, state: &AppState, run_id: &str, interval_ms: u64) -> Result<()> {
    loop {
        let poll = poll_execution(state, run_id)?;
        if poll.generic_result.is_some() {
            return emit(cli, &poll);
        }
        if !cli.json {
            println!(
                "执行中: task={} phase={:?}",
                poll.task_id, poll.outcome.phase
            );
        }
        thread::sleep(Duration::from_millis(interval_ms.max(100).min(30_000)));
    }
}

fn dispatch_result(cli: &Cli, state: &AppState, command: &ResultCommand) -> Result<()> {
    if let ResultCommand::Changes(args) = command {
        return emit(
            cli,
            &request_result_test_change(state, &args.run_id, &args.message)?,
        );
    }
    let result = match command {
        ResultCommand::AcceptPass(args) => decide_accept_pass(state, &args.run_id)?,
        ResultCommand::AcceptFail(args) => decide_accept_fail(state, &args.run_id)?,
        ResultCommand::MarkBug(args) => decide_mark_product_bug(state, &args.run_id)?,
        ResultCommand::MarkEnvironment(args) => decide_mark_environment(state, &args.run_id)?,
        ResultCommand::MarkFlaky(args) => decide_mark_flaky(state, &args.run_id)?,
        ResultCommand::EndWithoutDecision(args) => {
            decide_end_without_decision(state, &args.run_id)?
        }
        ResultCommand::Rerun(args) => prepare_execution_rerun(state, &args.run_id)?,
        ResultCommand::Changes(_) => unreachable!("handled above"),
    };
    emit(cli, &result)
}

fn dispatch_apply(cli: &Cli, state: &AppState, command: &ApplyCommand) -> Result<()> {
    match command {
        ApplyCommand::Guard(args) => emit(cli, &apply_guard_status(state, &args.run_id)?),
        ApplyCommand::Run(args) => emit(cli, &apply_patch(state, &args.run_id)?),
        ApplyCommand::Export { run_id, complete } => {
            emit(cli, &export_patch(state, run_id, *complete)?)
        }
        ApplyCommand::Discard(args) => emit(cli, &discard_delivery(state, &args.run_id)?),
    }
}

fn dispatch_models(cli: &Cli, command: &ModelsCommand) -> Result<()> {
    match command {
        ModelsCommand::Codex => emit(
            cli,
            &list_codex_models().map_err(|error| anyhow::anyhow!(error))?,
        ),
        ModelsCommand::Claude => emit(
            cli,
            &list_claude_models().map_err(|error| anyhow::anyhow!(error))?,
        ),
        ModelsCommand::Grok => emit(
            cli,
            &list_grok_models().map_err(|error| anyhow::anyhow!(error))?,
        ),
    }
}

fn dispatch_config(cli: &Cli, state: &AppState, command: &ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Show => emit(cli, &app_info_ext(state)),
        ConfigCommand::Language { language } => emit(cli, &state.set_ui_language(language.code())?),
        ConfigCommand::Agents { enabled_modes } => {
            emit(cli, &state.set_agent_preferences(enabled_modes.clone())?)
        }
    }
}

fn check_environment(cli: &Cli, args: &EnvironmentArgs) -> Result<()> {
    let endpoint = service_endpoint(&args.base_url).with_context(|| {
        format!(
            "{}: {}",
            localized(cli, "unable to parse service URL", "无法解析服务地址"),
            args.base_url
        )
    })?;
    if tcp_reachable(&endpoint) {
        return emit(
            cli,
            &serde_json::json!({"reachable": true, "endpoint": endpoint}),
        );
    }
    bail!("{}", match cli.language.unwrap_or(LanguageArg::English) {
        LanguageArg::English => format!("service is not running: {endpoint}. The CLI does not execute free-form startup commands; declare a ServiceProfile in structured TestProfile JSON and use execute confirm-profile/probe"),
        LanguageArg::SimplifiedChinese => format!("服务未运行: {endpoint}。CLI 不执行自由文本启动命令；请在结构化 TestProfile JSON 中声明 ServiceProfile，并通过 execute confirm-profile/probe"),
    })
}

fn init_task(cli: &Cli, state: &AppState, args: &InitArgs) -> Result<()> {
    if cli.json
        && (args.path.is_none() || args.prompt.is_none() || args.acceptance_criteria.is_empty())
    {
        bail!(
            "{}",
            localized(
                cli,
                "init with --json requires a path, --prompt, and at least one --criteria",
                "--json 模式下 init 必须提供路径、--prompt 和至少一个 --criteria"
            )
        );
    }
    let path = match &args.path {
        Some(path) => path.clone(),
        None => PathBuf::from(prompt(localized(cli, "Project path", "项目路径"), ".")?),
    };
    let opened = open_project(state, &path, false)?;
    let created_run = create_run(state, &opened.project.id)?;
    let objective = match &args.prompt {
        Some(prompt) => prompt.clone(),
        None if args.generate => prompt(localized(cli, "Test objective", "测试目标"), "")?,
        None => String::new(),
    };
    let criteria = if args.acceptance_criteria.is_empty() && args.generate {
        split_criteria(&prompt(
            localized(
                cli,
                "Acceptance criteria (separate with semicolons)",
                "验收标准（用分号分隔）",
            ),
            "",
        )?)
    } else {
        normalize_criteria(&args.acceptance_criteria)
    };
    let environment = if args.generate {
        match &args.environment {
            Some(environment) => Some(environment.clone()),
            None => {
                let value = prompt(
                    localized(
                        cli,
                        "Test environment details (optional)",
                        "测试环境说明（可选）",
                    ),
                    "",
                )?;
                (!value.trim().is_empty()).then_some(value)
            }
        }
    } else {
        args.environment.clone()
    };
    if args.generate {
        if objective.trim().is_empty() || criteria.is_empty() {
            bail!(
                "{}",
                localized(
                    cli,
                    "--generate requires a test objective and at least one acceptance criterion",
                    "--generate 需要测试目标和至少一个验收标准"
                )
            );
        }
        let result = generate_and_review(
            state,
            GenerateTestsRequest {
                run_id: created_run.run_id.clone(),
                objective,
                acceptance_criteria: criteria,
                source_anchors: vec![],
                generation_scope: vec![],
                spec_attachments: vec![],
                test_strategy: Some("e2e_api_flow".into()),
                cleanup_test_data: Some(true),
                force_regenerate: false,
                agent_mode: args.agent.clone(),
                reuse_agent_session: args.reuse_session,
                agent_model: args.model.clone(),
                environment: build_environment_from_inputs(
                    environment.as_deref(),
                    args.environment_file.as_deref(),
                    &[],
                    None,
                    None,
                    None,
                )?,
            },
        )?;
        return emit(cli, &result);
    }
    if cli.json {
        return emit(
            cli,
            &serde_json::json!({"project": opened, "run": created_run}),
        );
    }
    println!(
        "{}: {}",
        localized(cli, "Project opened", "项目已打开"),
        opened.project.root_path
    );
    println!(
        "{}: {}",
        localized(cli, "Task created", "任务已创建"),
        created_run.run_id
    );
    println!(
        "{}: testharbor generate {} --prompt ... --criteria ...",
        localized(cli, "Next", "下一步"),
        created_run.run_id
    );
    Ok(())
}

fn prompt(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let value = line.trim();
    Ok(if value.is_empty() {
        default.to_string()
    } else {
        value.to_string()
    })
}

fn split_criteria(input: &str) -> Vec<String> {
    input
        .split([';', '；', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn normalize_criteria(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| split_criteria(value))
        .collect()
}

fn service_endpoint(base_url: &str) -> Option<String> {
    let value = base_url.trim();
    let authority = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .unwrap_or(value)
        .split('/')
        .next()?
        .trim();
    let port = value.starts_with("https://").then_some(443).unwrap_or(80);
    Some(if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:{port}")
    })
}

fn tcp_reachable(endpoint: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    endpoint
        .to_socket_addrs()
        .ok()
        .into_iter()
        .flatten()
        .any(|address| TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok())
}

fn emit<T: Serialize>(cli: &Cli, value: &T) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_variables, service_endpoint};

    #[test]
    fn reads_masked_runtime_variables_from_process_environment() {
        std::env::set_var("TESTHARBOR_TEST_MASKED_VALUE", "a=b");
        let values = vec!["TESTHARBOR_TEST_MASKED_VALUE".into()];
        let parsed = parse_variables(&values).expect("valid environment variables");
        assert_eq!(
            parsed.get("TESTHARBOR_TEST_MASKED_VALUE"),
            Some(&"a=b".to_string())
        );
        std::env::remove_var("TESTHARBOR_TEST_MASKED_VALUE");
    }

    #[test]
    fn rejects_malformed_environment_variables() {
        assert!(parse_variables(&["TESTHARBOR_ENV_THAT_DOES_NOT_EXIST_42".into()]).is_err());
        assert!(parse_variables(&["".into()]).is_err());
    }

    #[test]
    fn service_endpoint_does_not_assume_a_fixed_application_port() {
        assert_eq!(
            service_endpoint("http://localhost:8300/api/health").as_deref(),
            Some("localhost:8300")
        );
        assert_eq!(
            service_endpoint("https://example.test/health").as_deref(),
            Some("example.test:443")
        );
    }
}
