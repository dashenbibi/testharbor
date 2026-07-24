//! CLI adapters for Codex, Claude Code, and Grok (optional; require network/auth).

use crate::{
    schema::{
        AgentFinalEnvelope, AnalysisPayload, GeneratePayload, ResultReviewPayload, ReviewPayload,
        SCHEMA_VERSION,
    },
    AgentError, AgentMode, AnalyzeRequest, AnalyzeResult, GenerateRequest, GenerateResult, Result,
    ResultReviewAgentResult, ResultReviewRequest, ReviewRequest, ReviewResult,
};
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const DEFAULT_CODEX_TOTAL_TIMEOUT: Duration = Duration::from_secs(900);
const DEFAULT_CODEX_IDLE_TIMEOUT: Duration = Duration::from_secs(180);
const DEFAULT_GROK_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_GROK_PREFLIGHT_IDLE_TIMEOUT: Duration = Duration::from_secs(8);
const DEFAULT_MODEL_CATALOG_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliProvider {
    Codex,
    Claude,
    Grok,
}

impl CliProvider {
    fn mode(self) -> AgentMode {
        match self {
            Self::Codex => AgentMode::Codex,
            Self::Claude => AgentMode::Claude,
            Self::Grok => AgentMode::Grok,
        }
    }
}

#[derive(Debug, Error)]
pub enum CodexError {
    #[error("codex not found or not usable: {0}")]
    Unavailable(String),
    #[error("codex failed: {0}")]
    Failed(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("generation cancelled by user")]
    Cancelled,
}

static GENERATION_CANCELLATIONS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    OnceLock::new();

fn generation_cancellations() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    GENERATION_CANCELLATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_generation(run_id: &str) -> Arc<AtomicBool> {
    let token = Arc::new(AtomicBool::new(false));
    if let Ok(mut active) = generation_cancellations().lock() {
        active.insert(run_id.to_string(), Arc::clone(&token));
    }
    token
}

fn unregister_generation(run_id: &str) {
    if let Ok(mut active) = generation_cancellations().lock() {
        active.remove(run_id);
    }
}

/// Request cancellation of the active Codex generation/review for a Run.
pub fn cancel_generation(run_id: &str) -> bool {
    let token = generation_cancellations()
        .lock()
        .ok()
        .and_then(|active| active.get(run_id).cloned());
    if let Some(token) = token {
        token.store(true, Ordering::Release);
        true
    } else {
        false
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexCapabilities {
    pub version: String,
    pub has_exec: bool,
    pub has_output_schema: bool,
    pub has_ephemeral: bool,
    pub has_sandbox: bool,
    pub authenticated: bool,
    pub auth_status: String,
}

fn codex_total_timeout() -> Duration {
    std::env::var("TESTHARBOR_CODEX_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|millis| Duration::from_millis(millis.clamp(1_000, 3_600_000)))
        .unwrap_or(DEFAULT_CODEX_TOTAL_TIMEOUT)
}

fn codex_idle_timeout() -> Duration {
    std::env::var("TESTHARBOR_CODEX_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|millis| Duration::from_millis(millis.clamp(1_000, 900_000)))
        .unwrap_or(DEFAULT_CODEX_IDLE_TIMEOUT)
}

fn grok_preflight_timeout() -> Duration {
    std::env::var("TESTHARBOR_GROK_PREFLIGHT_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|millis| Duration::from_millis(millis.clamp(1_000, 60_000)))
        .unwrap_or(DEFAULT_GROK_PREFLIGHT_TIMEOUT)
}

/// Run a child process while continuously draining both pipes. Keeping the
/// pipes unread while waiting on the child can deadlock once their buffers fill.
#[cfg(test)]
fn run_command_with_timeout_logged(
    program: &str,
    args: &[&str],
    stdin_input: Option<&str>,
    total_timeout: Duration,
    idle_timeout: Duration,
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
) -> std::result::Result<Output, CodexError> {
    run_command_with_timeout_logged_cancelable(
        program,
        args,
        stdin_input,
        total_timeout,
        idle_timeout,
        stdout_log,
        stderr_log,
        None,
    )
}

fn run_command_with_timeout_logged_cancelable(
    program: &str,
    args: &[&str],
    stdin_input: Option<&str>,
    total_timeout: Duration,
    idle_timeout: Duration,
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    cancel_token: Option<Arc<AtomicBool>>,
) -> std::result::Result<Output, CodexError> {
    let owned_args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    run_command_with_timeout_logged_cancelable_owned(
        program,
        &owned_args,
        stdin_input,
        total_timeout,
        idle_timeout,
        stdout_log,
        stderr_log,
        cancel_token,
    )
}

fn run_command_with_timeout_logged_cancelable_owned(
    program: &str,
    args: &[String],
    stdin_input: Option<&str>,
    total_timeout: Duration,
    idle_timeout: Duration,
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    cancel_token: Option<Arc<AtomicBool>>,
) -> std::result::Result<Output, CodexError> {
    run_command_with_timeout_logged_cancelable_owned_in_dir(
        program,
        args,
        stdin_input,
        total_timeout,
        idle_timeout,
        stdout_log,
        stderr_log,
        cancel_token,
        None,
    )
}

fn run_command_with_timeout_logged_cancelable_owned_in_dir(
    program: &str,
    args: &[String],
    stdin_input: Option<&str>,
    total_timeout: Duration,
    idle_timeout: Duration,
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    cancel_token: Option<Arc<AtomicBool>>,
    current_dir: Option<&Path>,
) -> std::result::Result<Output, CodexError> {
    run_command_with_timeout_logged_cancelable_owned_in_dir_with_env(
        program,
        args,
        stdin_input,
        total_timeout,
        idle_timeout,
        stdout_log,
        stderr_log,
        cancel_token,
        current_dir,
        None,
    )
}

fn run_command_with_timeout_logged_cancelable_owned_in_dir_with_env(
    program: &str,
    args: &[String],
    stdin_input: Option<&str>,
    total_timeout: Duration,
    idle_timeout: Duration,
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    cancel_token: Option<Arc<AtomicBool>>,
    current_dir: Option<&Path>,
    environment: Option<&HashMap<String, String>>,
) -> std::result::Result<Output, CodexError> {
    let prompt_bytes = stdin_input
        .map(str::len)
        .or_else(|| {
            args.windows(2)
                .find(|pair| pair[0] == "-p" || pair[0] == "--prompt")
                .map(|pair| pair[1].len())
        })
        .unwrap_or(0);
    let call_log = stdout_log.clone().or_else(|| stderr_log.clone());
    let cancel_file = stdout_log
        .as_ref()
        .or(stderr_log.as_ref())
        .and_then(|path| path.parent())
        .map(|dir| dir.join("generation.cancel"));
    let mut command = Command::new(program);
    let is_agent_cli = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "codex" | "claude" | "grok"));
    if is_agent_cli && environment.map_or(true, |values| !values.contains_key("PATH")) {
        if let Some(path) = login_shell_path() {
            command.env("PATH", path);
        }
    }
    if Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "grok")
    {
        if let Some(proxy) = grok_proxy_url() {
            command
                .env("HTTP_PROXY", &proxy)
                .env("HTTPS_PROXY", &proxy)
                .env("ALL_PROXY", &proxy);
        }
    }
    command.args(args);
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    if let Some(environment) = environment {
        command.envs(environment);
    }
    command
        .stdin(if stdin_input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = command
        .spawn()
        .map_err(|e| CodexError::Unavailable(e.to_string()))?;
    let pid = child.id();
    let stdin_writer = stdin_input.map(|input| {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let input = input.as_bytes().to_vec();
        thread::spawn(move || {
            let _ = stdin.write_all(&input);
            let _ = stdin.flush();
        })
    });
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let last_activity = Arc::new(Mutex::new(Instant::now()));
    let stdout_activity = Arc::clone(&last_activity);
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut log = stdout_log
            .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok());
        let mut chunk = [0u8; 8192];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    if let Ok(mut activity) = stdout_activity.lock() {
                        *activity = Instant::now();
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(file) = log.as_mut() {
                        let _ = file.write_all(&chunk[..count]);
                        let _ = file.flush();
                    }
                }
                Err(_) => break,
            }
        }
        bytes
    });
    let stderr_activity = Arc::clone(&last_activity);
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut log = stderr_log
            .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok());
        let mut chunk = [0u8; 8192];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    if let Ok(mut activity) = stderr_activity.lock() {
                        *activity = Instant::now();
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(file) = log.as_mut() {
                        let _ = file.write_all(&chunk[..count]);
                        let _ = file.flush();
                    }
                }
                Err(_) => break,
            }
        }
        bytes
    });

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancel_token
            .as_ref()
            .is_some_and(|token| token.load(Ordering::Acquire))
            || cancel_file.as_ref().is_some_and(|path| path.is_file())
        {
            #[cfg(unix)]
            unsafe {
                let _ = libc::kill(-(pid as i32), libc::SIGTERM);
                thread::sleep(Duration::from_millis(200));
                let _ = libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            if let Some(writer) = stdin_writer {
                let _ = writer.join();
            }
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(CodexError::Cancelled);
        }
        let idle_elapsed = last_activity
            .lock()
            .map(|activity| activity.elapsed())
            .unwrap_or_default();
        let timeout_reason = if started.elapsed() >= total_timeout {
            Some(format!(
                "reached total runtime limit after {}s",
                total_timeout.as_secs()
            ))
        } else if idle_elapsed >= idle_timeout {
            Some(format!(
                "no stdout/stderr activity for {}s",
                idle_timeout.as_secs()
            ))
        } else {
            None
        };
        if let Some(reason) = timeout_reason {
            #[cfg(unix)]
            unsafe {
                let _ = libc::kill(-(pid as i32), libc::SIGTERM);
                thread::sleep(Duration::from_millis(200));
                let _ = libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            if let Some(writer) = stdin_writer {
                let _ = writer.join();
            }
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(CodexError::Failed(reason));
        }
        thread::sleep(Duration::from_millis(100));
    };

    if let Some(writer) = stdin_writer {
        let _ = writer.join();
    }

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let output = Output {
        status,
        stdout,
        stderr,
    };
    if let Some(path) = call_log.as_deref() {
        let _ = append_agent_call_record(
            path,
            program,
            prompt_bytes,
            output.stdout.len(),
            output.stderr.len(),
            started.elapsed(),
            output.status.success(),
        );
    }
    Ok(output)
}

fn append_agent_call_record(
    path: &Path,
    program: &str,
    prompt_bytes: usize,
    stdout_bytes: usize,
    stderr_bytes: usize,
    elapsed: Duration,
    success: bool,
) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let event = serde_json::json!({
        "type": "testharbor.agent_call",
        "program": Path::new(program).file_name().and_then(|name| name.to_str()).unwrap_or(program),
        "prompt_bytes": prompt_bytes,
        "stdout_bytes": stdout_bytes,
        "stderr_bytes": stderr_bytes,
        "duration_ms": elapsed.as_millis(),
        "success": success,
    });
    writeln!(file, "{event}")?;
    file.flush()
}

fn run_codex_prompt(
    args: &[&str],
    prompt: &str,
    model: Option<&str>,
) -> std::result::Result<Output, CodexError> {
    let program = codex_command_path()?;
    let mut full_args = args.to_vec();
    if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
        if full_args.get(1) == Some(&"resume") && !full_args.is_empty() {
            let insert_at = full_args.len() - 1;
            full_args.splice(insert_at..insert_at, ["--model", model]);
        } else {
            full_args.extend(["--model", model]);
        }
    }
    full_args.push("-");
    run_command_with_timeout_logged_cancelable(
        program.to_str().unwrap_or("codex"),
        &full_args,
        Some(prompt),
        codex_total_timeout(),
        codex_idle_timeout(),
        None,
        None,
        None,
    )
}

fn run_codex_with_progress(
    args: &[&str],
    prompt: &str,
    progress_dir: Option<&str>,
    cancel_token: Option<Arc<AtomicBool>>,
    model: Option<&str>,
    environment: Option<&HashMap<String, String>>,
) -> std::result::Result<Output, CodexError> {
    let program = codex_command_path()?;
    let (stdout_log, stderr_log) = progress_dir
        .map(PathBuf::from)
        .map(|dir| {
            (
                Some(dir.join("generation.stdout.jsonl")),
                Some(dir.join("generation.stderr.log")),
            )
        })
        .unwrap_or((None, None));
    let mut full_args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
        if full_args.get(1).map(String::as_str) == Some("resume") && !full_args.is_empty() {
            let insert_at = full_args.len() - 1;
            full_args.splice(
                insert_at..insert_at,
                ["--model".to_string(), model.to_string()],
            );
        } else {
            full_args.extend(["--model".to_string(), model.to_string()]);
        }
    }
    full_args.push("-".into());
    run_command_with_timeout_logged_cancelable_owned_in_dir_with_env(
        program.to_str().unwrap_or("codex"),
        &full_args,
        Some(prompt),
        codex_total_timeout(),
        codex_idle_timeout(),
        stdout_log,
        stderr_log,
        cancel_token,
        None,
        environment,
    )
}

fn resolve_cli_path(
    name: &str,
    override_env: &str,
    display_name: &str,
) -> std::result::Result<PathBuf, CodexError> {
    if let Ok(path) = std::env::var(override_env) {
        let path = PathBuf::from(path.trim());
        if path.is_file() {
            return Ok(path);
        }
    }
    if let Some(path) = std::env::var_os("PATH").and_then(|value| {
        std::env::split_paths(&value)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    }) {
        return Ok(path);
    }

    // Packaged GUI apps often start without the user's terminal PATH. Ask
    // the user's login shell to resolve the same command instead of requiring
    // an absolute path in the settings UI.
    #[cfg(unix)]
    {
        for shell in login_shells() {
            if let Ok(output) = Command::new(shell)
                .args(["-lc", "command -v \"$1\"", "testharbor-resolve", name])
                .output()
            {
                if let Some(path) = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .rev()
                    .map(str::trim)
                    .find(|line| Path::new(line).is_file())
                {
                    return Ok(PathBuf::from(path));
                }
            }
        }
    }

    Err(CodexError::Unavailable(format!(
        "未找到 {display_name}。请安装 {name}，或设置 {override_env}"
    )))
}

fn codex_command_path() -> std::result::Result<PathBuf, CodexError> {
    resolve_cli_path("codex", "TESTHARBOR_CODEX_BIN", "Codex CLI")
}

fn claude_command_path() -> std::result::Result<PathBuf, CodexError> {
    resolve_cli_path("claude", "TESTHARBOR_CLAUDE_BIN", "Claude Code CLI")
}

#[cfg(unix)]
fn login_shells() -> Vec<OsString> {
    let mut shells = Vec::new();
    if let Some(shell) = std::env::var_os("SHELL") {
        shells.push(shell);
    }
    for shell in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
        let shell = OsString::from(shell);
        if !shells.iter().any(|candidate| candidate == &shell) {
            shells.push(shell);
        }
    }
    shells
}

#[cfg(unix)]
fn login_shell_path() -> Option<OsString> {
    login_shells().into_iter().find_map(|shell| {
        let output = Command::new(shell)
            .args(["-lc", "printf '%s' \"$PATH\""])
            .output()
            .ok()?;
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!path.is_empty()).then(|| OsString::from(path))
    })
}

#[cfg(not(unix))]
fn login_shell_path() -> Option<OsString> {
    None
}

fn run_claude_with_progress(
    args: &[String],
    prompt: &str,
    current_dir: &Path,
    progress_dir: Option<&str>,
    cancel_token: Option<Arc<AtomicBool>>,
    environment: Option<&HashMap<String, String>>,
) -> std::result::Result<Output, CodexError> {
    let (stdout_log, stderr_log) = progress_dir
        .map(PathBuf::from)
        .map(|dir| {
            (
                Some(dir.join("generation.stdout.jsonl")),
                Some(dir.join("generation.stderr.log")),
            )
        })
        .unwrap_or((None, None));
    let mut full_args = args.to_vec();
    full_args.extend(["-p".into(), prompt.to_string()]);
    let program = claude_command_path()?;
    run_command_with_timeout_logged_cancelable_owned_in_dir_with_env(
        program.to_str().unwrap_or("claude"),
        &full_args,
        None,
        codex_total_timeout(),
        codex_idle_timeout(),
        stdout_log,
        stderr_log,
        cancel_token,
        Some(current_dir),
        environment,
    )
}

fn run_claude_prompt(
    args: &[String],
    prompt: &str,
    current_dir: &Path,
) -> std::result::Result<Output, CodexError> {
    run_claude_with_progress(args, prompt, current_dir, None, None, None)
}

fn claude_model_args(args: &mut Vec<String>, model: Option<&str>) {
    let model = model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("TESTHARBOR_CLAUDE_MODEL").ok());
    if let Some(model) = model {
        args.extend(["--model".into(), model]);
    }
}

fn claude_generate_args(
    session_id: Option<&str>,
    worktree_path: &str,
    model: Option<&str>,
    _focused_revision: bool,
) -> Vec<String> {
    let mut args = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--dangerously-skip-permissions".into(),
        "--effort".into(),
        claude_effort(),
        "--exclude-dynamic-system-prompt-sections".into(),
        "--add-dir".into(),
        worktree_path.into(),
        "--tools".into(),
        "Read,Grep,Glob,Edit,Write".into(),
    ];
    claude_model_args(&mut args, model);
    if let Some(session_id) = session_id {
        args.extend(["--resume".into(), session_id.into()]);
    }
    args
}

fn claude_json_args(
    cwd: &str,
    schema: &str,
    model: Option<&str>,
    _session_id: Option<&str>,
    tools: &str,
) -> Vec<String> {
    let mut args = vec![
        "--output-format".into(),
        "json".into(),
        "--json-schema".into(),
        schema.into(),
        "--dangerously-skip-permissions".into(),
        "--effort".into(),
        claude_effort(),
        "--exclude-dynamic-system-prompt-sections".into(),
    ];
    if tools.trim().is_empty() {
        args.extend(["--tools".into(), "".into()]);
    } else {
        args.extend([
            "--add-dir".into(),
            cwd.into(),
            "--tools".into(),
            tools.into(),
        ]);
    }
    claude_model_args(&mut args, model);
    args
}

fn claude_effort() -> String {
    std::env::var("TESTHARBOR_CLAUDE_EFFORT")
        .ok()
        .filter(|value| matches!(value.trim(), "low" | "medium" | "high" | "max"))
        .unwrap_or_else(|| "low".into())
}

fn extract_claude_session_id(stdout: &[u8]) -> Option<String> {
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(id) = value
            .get("session_id")
            .or_else(|| value.get("sessionId"))
            .and_then(|value| value.as_str())
        {
            return Some(id.to_string());
        }
    }
    None
}

fn extract_claude_text(stdout: &[u8]) -> String {
    let raw = String::from_utf8_lossy(stdout).trim().to_string();
    let mut candidates = Vec::new();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
        candidates.push(value);
    }
    for line in raw.lines() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            candidates.push(value);
        }
    }
    for value in candidates.iter().rev() {
        if let Some(structured) = value.get("structured_output") {
            return structured.to_string();
        }
        if let Some(result) = value.get("result") {
            if let Some(text) = result.as_str() {
                return text.to_string();
            }
            return result.to_string();
        }
        if let Some(text) = value.get("text").and_then(|text| text.as_str()) {
            return text.to_string();
        }
    }
    raw
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeCapabilities {
    pub version: String,
    pub has_print: bool,
    pub has_stream_json: bool,
    pub has_json_schema: bool,
    pub has_resume: bool,
    pub has_tools: bool,
    pub authenticated: bool,
    pub auth_status: String,
}

pub fn probe_claude() -> std::result::Result<ClaudeCapabilities, CodexError> {
    let program = claude_command_path()?;
    let version_output = Command::new(&program)
        .arg("--version")
        .output()
        .map_err(|e| CodexError::Unavailable(e.to_string()))?;
    if !version_output.status.success() {
        return Err(CodexError::Unavailable(
            String::from_utf8_lossy(&version_output.stderr)
                .trim()
                .to_string(),
        ));
    }
    let version = String::from_utf8_lossy(&version_output.stdout)
        .trim()
        .to_string();
    let help = Command::new(&program)
        .arg("--help")
        .output()
        .map_err(|e| CodexError::Unavailable(e.to_string()))?;
    if !help.status.success() {
        return Err(CodexError::Unavailable(
            String::from_utf8_lossy(&help.stderr).trim().to_string(),
        ));
    }
    let help_text = String::from_utf8_lossy(&help.stdout);
    let auth = Command::new(&program)
        .args(["auth", "status"])
        .output()
        .map_err(|e| CodexError::Unavailable(e.to_string()))?;
    let auth_value = serde_json::from_slice::<serde_json::Value>(&auth.stdout).ok();
    let authenticated = auth.status.success()
        && auth_value
            .as_ref()
            .and_then(|value| value.get("loggedIn"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
    let auth_status = if authenticated {
        auth_value
            .and_then(|value| {
                let method = value.get("authMethod")?.as_str()?;
                Some(format!("Claude Code 已登录（{method}）"))
            })
            .unwrap_or_else(|| "Claude Code 已登录".into())
    } else {
        let detail = String::from_utf8_lossy(&auth.stderr).trim().to_string();
        if detail.is_empty() {
            "Claude Code 尚未登录，请运行 `claude auth login`".into()
        } else {
            detail
        }
    };
    Ok(ClaudeCapabilities {
        version,
        has_print: help_text.contains("--print") || help_text.contains("-p, --print"),
        has_stream_json: help_text.contains("stream-json"),
        has_json_schema: help_text.contains("--json-schema"),
        has_resume: help_text.contains("--resume"),
        has_tools: help_text.contains("--tools"),
        authenticated,
        auth_status,
    })
}

fn grok_command_path() -> std::result::Result<PathBuf, CodexError> {
    if let Ok(path) = std::env::var("TESTHARBOR_GROK_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }
    if let Some(path) = std::env::var_os("PATH").and_then(|value| {
        std::env::split_paths(&value)
            .map(|dir| dir.join("grok"))
            .find(|candidate| candidate.is_file())
    }) {
        return Ok(path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let path = PathBuf::from(home).join(".grok/bin/grok");
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(CodexError::Unavailable(
        "未找到 Grok CLI。请安装 grok，或设置 TESTHARBOR_GROK_BIN".into(),
    ))
}

fn grok_proxy_url() -> Option<String> {
    for key in [
        "TESTHARBOR_GROK_PROXY",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let output = Command::new("scutil").arg("--proxy").output().ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let https_enabled =
            scutil_proxy_value(&text, "HTTPSEnable").is_some_and(|value| value == "1");
        let http_enabled =
            scutil_proxy_value(&text, "HTTPEnable").is_some_and(|value| value == "1");
        let (host_key, port_key) = if https_enabled {
            ("HTTPSProxy", "HTTPSPort")
        } else if http_enabled {
            ("HTTPProxy", "HTTPPort")
        } else {
            return None;
        };
        let host = scutil_proxy_value(&text, host_key)?;
        let port = scutil_proxy_value(&text, port_key)?;
        return Some(format!("http://{host}:{port}"));
    }

    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn scutil_proxy_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.trim() == key).then(|| value.trim().to_string())
        })
        .filter(|value| !value.is_empty())
}

fn run_grok_with_progress(
    args: &[String],
    prompt: &str,
    progress_dir: Option<&str>,
    cancel_token: Option<Arc<AtomicBool>>,
    environment: Option<&HashMap<String, String>>,
) -> std::result::Result<Output, CodexError> {
    let (stdout_log, stderr_log) = progress_dir
        .map(PathBuf::from)
        .map(|dir| {
            (
                Some(dir.join("generation.stdout.jsonl")),
                Some(dir.join("generation.stderr.log")),
            )
        })
        .unwrap_or((None, None));
    let mut full_args = vec!["-p".to_string(), prompt.to_string()];
    full_args.extend(args.iter().cloned());
    let program = grok_command_path()?;
    run_command_with_timeout_logged_cancelable_owned_in_dir_with_env(
        program.to_str().unwrap_or("grok"),
        &full_args,
        None,
        codex_total_timeout(),
        codex_idle_timeout(),
        stdout_log,
        stderr_log,
        cancel_token,
        None,
        environment,
    )
}

fn run_grok_prompt(args: &[String], prompt: &str) -> std::result::Result<Output, CodexError> {
    run_grok_with_progress(args, prompt, None, None, None)
}

fn extract_grok_text(stdout: &[u8]) -> String {
    let raw = String::from_utf8_lossy(stdout).trim().to_string();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
        if let Some(text) = value.get("text") {
            if let Some(text) = text.as_str() {
                return text.to_string();
            }
            if text.is_object() {
                return text.to_string();
            }
        }
        if value.is_object() {
            return raw;
        }
    }
    raw
}

fn extract_grok_session_id(stdout: &[u8]) -> Option<String> {
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(id) = value
            .get("sessionId")
            .or_else(|| value.get("session_id"))
            .and_then(|value| value.as_str())
        {
            return Some(id.to_string());
        }
    }
    None
}

fn extract_thread_id(stdout: &[u8]) -> Option<String> {
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let event_type = value.get("type").and_then(|v| v.as_str());
        if event_type != Some("thread.started") && event_type != Some("session.started") {
            continue;
        }
        if let Some(id) = value
            .get("thread_id")
            .or_else(|| value.get("session_id"))
            .and_then(|v| v.as_str())
        {
            return Some(id.to_string());
        }
    }
    None
}

pub fn probe_codex() -> std::result::Result<CodexCapabilities, CodexError> {
    let program = codex_command_path()?;
    let output = Command::new(&program)
        .args(["--version"])
        .output()
        .map_err(|e| CodexError::Unavailable(e.to_string()))?;
    if !output.status.success() {
        return Err(CodexError::Unavailable(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let help = Command::new(&program)
        .args(["exec", "--help"])
        .output()
        .map_err(|e| CodexError::Unavailable(e.to_string()))?;
    if !help.status.success() {
        return Err(CodexError::Unavailable(
            String::from_utf8_lossy(&help.stderr).trim().to_string(),
        ));
    }
    let help_txt = String::from_utf8_lossy(&help.stdout);
    let auth = Command::new(&program)
        .args(["login", "status"])
        .output()
        .map_err(|e| CodexError::Unavailable(e.to_string()))?;
    let auth_status = if auth.status.success() {
        let stdout = String::from_utf8_lossy(&auth.stdout).trim().to_string();
        if stdout.is_empty() {
            "Codex 已登录".to_string()
        } else {
            stdout
        }
    } else {
        let stderr = String::from_utf8_lossy(&auth.stderr).trim().to_string();
        return Err(CodexError::Unavailable(if stderr.is_empty() {
            "Codex 尚未登录，请先运行 `codex login`".to_string()
        } else {
            stderr
        }));
    };
    Ok(CodexCapabilities {
        version,
        has_exec: help_txt.contains("Run Codex non-interactively")
            || help_txt.contains("Usage: codex exec"),
        has_output_schema: help_txt.contains("output-schema"),
        has_ephemeral: help_txt.contains("ephemeral"),
        has_sandbox: help_txt.contains("sandbox"),
        authenticated: true,
        auth_status,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrokCapabilities {
    pub version: String,
    pub has_headless: bool,
    pub has_json: bool,
    pub has_resume: bool,
    pub has_tools: bool,
    pub has_no_subagents: bool,
    pub has_disable_web_search: bool,
    pub authenticated: bool,
    pub auth_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliModelCatalog {
    pub models: Vec<String>,
    pub default_model: Option<String>,
}

pub type GrokModelCatalog = CliModelCatalog;

#[derive(Debug, Deserialize)]
struct CodexModelEntry {
    slug: String,
    #[serde(default)]
    visibility: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexModelCatalogResponse {
    models: Vec<CodexModelEntry>,
}

pub fn list_codex_models() -> std::result::Result<CliModelCatalog, CodexError> {
    let program = codex_command_path()?;
    let output = run_command_with_timeout_logged_cancelable_owned(
        program.to_str().unwrap_or("codex"),
        &["debug".into(), "models".into()],
        None,
        DEFAULT_MODEL_CATALOG_TIMEOUT,
        DEFAULT_MODEL_CATALOG_TIMEOUT,
        None,
        None,
        None,
    )?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CodexError::Failed(if detail.is_empty() {
            "Codex 模型列表获取失败".into()
        } else {
            format!("Codex 模型列表获取失败：{detail}")
        }));
    }
    parse_codex_model_catalog(&String::from_utf8_lossy(&output.stdout))
}

fn parse_codex_model_catalog(output: &str) -> std::result::Result<CliModelCatalog, CodexError> {
    let catalog = serde_json::from_str::<CodexModelCatalogResponse>(output)
        .map_err(|error| CodexError::Failed(format!("Codex 模型列表解析失败：{error}")))?;
    let mut models = catalog
        .models
        .into_iter()
        .filter(|model| model.visibility.as_deref().unwrap_or("list") == "list")
        .map(|model| model.slug)
        .filter(|model| !model.trim().is_empty())
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    Ok(CliModelCatalog {
        models,
        default_model: None,
    })
}

pub fn list_claude_models() -> std::result::Result<CliModelCatalog, CodexError> {
    let program = claude_command_path()?;
    let output = run_command_with_timeout_logged_cancelable_owned(
        program.to_str().unwrap_or("claude"),
        &["--help".into()],
        None,
        DEFAULT_MODEL_CATALOG_TIMEOUT,
        DEFAULT_MODEL_CATALOG_TIMEOUT,
        None,
        None,
        None,
    )?;
    let help = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(CliModelCatalog {
        models: parse_claude_model_aliases(&help),
        default_model: None,
    })
}

fn parse_claude_model_aliases(help: &str) -> Vec<String> {
    let Some(model_start) = help.find("--model") else {
        return Vec::new();
    };
    let model_help = help[model_start..]
        .split_once("\n  -")
        .map(|(section, _)| section)
        .unwrap_or(&help[model_start..]);
    let mut models = Vec::new();
    let mut rest = model_help;
    while let Some(start) = rest.find('\'') {
        if start > 0 && rest.as_bytes()[start - 1].is_ascii_alphanumeric() {
            rest = &rest[start + 1..];
            continue;
        }
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('\'') else {
            break;
        };
        let model = after_start[..end].trim();
        if !model.is_empty() && !models.iter().any(|item| item == model) {
            models.push(model.to_string());
        }
        rest = &after_start[end + 1..];
    }
    models
}

pub fn probe_grok() -> std::result::Result<GrokCapabilities, CodexError> {
    let program = grok_command_path()?;
    let version_output = Command::new(&program)
        .arg("--version")
        .output()
        .map_err(|e| CodexError::Unavailable(e.to_string()))?;
    if !version_output.status.success() {
        return Err(CodexError::Unavailable(
            String::from_utf8_lossy(&version_output.stderr)
                .trim()
                .to_string(),
        ));
    }
    let version = String::from_utf8_lossy(&version_output.stdout)
        .trim()
        .to_string();
    let help = Command::new(&program)
        .args(["--help"])
        .output()
        .map_err(|e| CodexError::Unavailable(e.to_string()))?;
    let help_text = String::from_utf8_lossy(&help.stdout);
    let (authenticated, auth_status) = grok_auth_status();
    Ok(GrokCapabilities {
        version,
        has_headless: help_text.contains("--single") || help_text.contains("-p, --single"),
        has_json: help_text.contains("--output-format"),
        has_resume: help_text.contains("--resume"),
        has_tools: help_text.contains("--tools"),
        has_no_subagents: help_text.contains("--no-subagents"),
        has_disable_web_search: help_text.contains("--disable-web-search"),
        authenticated,
        auth_status,
    })
}

pub fn list_grok_models() -> std::result::Result<GrokModelCatalog, CodexError> {
    let program = grok_command_path()?;
    let output = run_command_with_timeout_logged_cancelable_owned(
        program.to_str().unwrap_or("grok"),
        &["models".to_string()],
        None,
        grok_preflight_timeout(),
        DEFAULT_GROK_PREFLIGHT_IDLE_TIMEOUT,
        None,
        None,
        None,
    )?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CodexError::Failed(if detail.is_empty() {
            "Grok 模型列表获取失败".into()
        } else {
            format!("Grok 模型列表获取失败：{detail}")
        }));
    }
    let catalog = parse_grok_model_catalog(&String::from_utf8_lossy(&output.stdout));
    Ok(catalog)
}

fn parse_grok_model_catalog(output: &str) -> GrokModelCatalog {
    let mut models = Vec::new();
    let mut default_model = None;
    let mut reading_models = false;
    for line in output.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Default model:") {
            let value = value.trim();
            if !value.is_empty() {
                default_model = Some(value.to_string());
            }
            continue;
        }
        if line == "Available models:" {
            reading_models = true;
            continue;
        }
        if reading_models {
            let model = line
                .strip_prefix('*')
                .or_else(|| line.strip_prefix('-'))
                .map(str::trim)
                .and_then(|value| value.split_whitespace().next())
                .filter(|value| !value.is_empty());
            if let Some(model) = model {
                if !models.iter().any(|item| item == model) {
                    models.push(model.to_string());
                }
            }
        }
    }
    if let Some(default_model) = &default_model {
        if !models.iter().any(|model| model == default_model) {
            models.insert(0, default_model.clone());
        }
    }
    GrokModelCatalog {
        models,
        default_model,
    }
}

fn grok_auth_status() -> (bool, String) {
    if std::env::var("XAI_API_KEY").is_ok_and(|value| !value.trim().is_empty()) {
        return (true, "已设置 XAI_API_KEY".into());
    }

    let Some(home) = std::env::var_os("HOME") else {
        return (
            false,
            "Grok 尚未登录，请运行 `grok login` 或设置 XAI_API_KEY".into(),
        );
    };
    let auth_path = PathBuf::from(home).join(".grok/auth.json");
    let Ok(raw) = std::fs::read_to_string(&auth_path) else {
        return (
            false,
            "Grok 尚未登录，请运行 `grok login` 或设置 XAI_API_KEY".into(),
        );
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return (
            false,
            "Grok 登录凭据文件无法解析，请重新运行 `grok login`".into(),
        );
    };
    let expires_at = value.as_object().and_then(|entries| {
        entries
            .values()
            .find_map(|entry| entry.get("expires_at").and_then(serde_json::Value::as_str))
    });
    match expires_at.and_then(|value| DateTime::parse_from_rfc3339(value).ok()) {
        Some(expires_at) if expires_at.with_timezone(&Utc) > Utc::now() => (
            true,
            format!("Grok 登录凭据有效至 {}", expires_at.to_rfc3339()),
        ),
        Some(_) => (
            true,
            "Grok 已配置登录凭据，短期访问令牌将由 CLI 自动刷新".into(),
        ),
        None => (true, "Grok 已配置登录凭据，将在首次调用时验证".into()),
    }
}

fn ensure_grok_model_service() -> std::result::Result<(), CodexError> {
    let program = grok_command_path()?;
    let args = vec!["models".to_string()];
    let output = run_command_with_timeout_logged_cancelable_owned(
        program.to_str().unwrap_or("grok"),
        &args,
        None,
        grok_preflight_timeout(),
        DEFAULT_GROK_PREFLIGHT_IDLE_TIMEOUT,
        None,
        None,
        None,
    )
    .map_err(|error| match error {
        CodexError::Failed(reason) => {
            CodexError::Failed(format!("Grok 模型服务连接失败：{reason}"))
        }
        other => other,
    })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let detail = if detail.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        detail
    };
    Err(CodexError::Failed(if detail.is_empty() {
        "Grok 模型服务返回失败，请检查网络或 CLI 配置".into()
    } else {
        format!("Grok 模型服务返回失败：{detail}")
    }))
}

pub fn codex_generate(req: &GenerateRequest) -> Result<GenerateResult> {
    generate_with_provider(req, CliProvider::Codex)
}

pub fn claude_generate(req: &GenerateRequest) -> Result<GenerateResult> {
    generate_with_provider(req, CliProvider::Claude)
}

pub fn grok_generate(req: &GenerateRequest) -> Result<GenerateResult> {
    generate_with_provider(req, CliProvider::Grok)
}

fn generate_with_provider(req: &GenerateRequest, provider: CliProvider) -> Result<GenerateResult> {
    match provider {
        CliProvider::Codex => {
            let capabilities = probe_codex()?;
            if !capabilities.has_exec
                || !capabilities.has_sandbox
                || !capabilities.has_ephemeral
                || !capabilities.authenticated
            {
                return Err(AgentError::Other(
                    "Codex CLI 缺少受控 workspace-write sandbox 能力".into(),
                ));
            }
        }
        CliProvider::Claude => {
            let capabilities = probe_claude()?;
            if !capabilities.has_print
                || !capabilities.has_stream_json
                || !capabilities.has_tools
                || !capabilities.authenticated
            {
                return Err(AgentError::Other(format!(
                    "Claude Code CLI 不可用：{}",
                    capabilities.auth_status
                )));
            }
        }
        CliProvider::Grok => {
            let capabilities = probe_grok()?;
            if !capabilities.has_headless
                || !capabilities.has_json
                || !capabilities.has_tools
                || !capabilities.has_no_subagents
                || !capabilities.has_disable_web_search
            {
                return Err(AgentError::Other(
                    "Grok CLI 缺少 TestHarbor 所需的 headless JSON 或 resume 能力".into(),
                ));
            }
            if !capabilities.authenticated {
                return Err(AgentError::Other(capabilities.auth_status));
            }
            ensure_grok_model_service()?;
        }
    }
    let strategy = req
        .test_strategy
        .as_deref()
        .unwrap_or("e2e_api_flow")
        .to_lowercase();
    let cleanup = req.cleanup_test_data.unwrap_or(true);
    let external_target = req.environment.as_ref().filter(|environment| {
        !environment.base_url.trim().is_empty() || !environment.context.trim().is_empty()
    });
    let runtime_boundary = external_target
        .map(|environment| {
            let mut variable_names = environment.variables.keys().cloned().collect::<Vec<_>>();
            variable_names.sort();
            let context_note = if environment.context.trim().is_empty() {
                "No free-form environment instructions were supplied.".to_string()
            } else {
                format!(
                    "USER-PROVIDED ENVIRONMENT INSTRUCTIONS:\n{}",
                    environment.context.trim()
                )
            };
            format!(
                "RUNTIME TARGET: resolve the running service from the explicit target, the user instructions, and the repository's own configuration. TestHarbor does not provide a universal TESTHARBOR_* runtime contract. Do not invent or require TESTHARBOR_BASE_URL, TESTHARBOR_DB_DSN, TESTHARBOR_DB_USER, TESTHARBOR_DB_PASSWORD, TESTHARBOR_EXPECTED_DB_NAME, TESTHARBOR_ADMIN_NAME, TESTHARBOR_ADMIN_PASSWORD, TESTHARBOR_USER_NAME, or TESTHARBOR_USER_PASSWORD. Read the repository's actual config files, .env files, README/startup scripts, and native config loader; use the real project keys and values. Only use an environment variable when the repository already defines it or SmartTest explicitly supplied that exact variable. Do not fail in a synthetic preflight merely because one of the names above is absent. Do not execute startup/shutdown commands unless the confirmed ServiceProfile authorizes it, and do not scan unrelated secret files. Explicit service URL (if supplied): {}. Health check: {}. Login endpoint: {}. Runtime variable names explicitly supplied to the child process: {}.\n{}",
                environment.base_url,
                environment.health_url.as_deref().unwrap_or("base URL"),
                environment.login_url.as_deref().unwrap_or("discover from repository/API docs"),
                if variable_names.is_empty() { "none".into() } else { variable_names.join(", ") },
                context_note,
            )
        })
        .unwrap_or_else(|| "RUNTIME TARGET: no external service is declared. Do not discover or execute service startup/shutdown commands; use only repository-native test harnesses already invoked by the confirmed Profile.".into());
    let strategy_instructions = match strategy.as_str() {
        "unit" => {
            "Test strategy: UNIT. Generate focused package-level unit tests with mocks/stubs where appropriate. Do not turn this into an end-to-end test.\n".to_string()
        }
        "integration" => format!(
            "Test strategy: INTEGRATION. Generate repository-native integration tests that exercise real module collaboration and persistence boundaries where the repository already has test infrastructure. Use isolated test data. Test data cleanup: {}.\n",
            if cleanup { "required via the framework's cleanup hooks, deferred cleanup, transactions, delete statements, or cache-key cleanup" } else { "do not delete automatically; namespace data clearly and make it traceable" }
        ),
        _ => format!(
            "Test strategy: E2E API FLOW. Generate automated end-to-end flow tests, not narrow unit tests. Prefer the repository's existing API/HTTP/CLI test harness. Exercise the business flow through the production-facing router and middleware chain, then verify resulting database/cache/state changes through existing stores, test DB handles, fixtures, or repository test helpers. Include setup of isolated test data, authentication/login or the repository's real credential helper when the target API is protected, API calls for each operation/query step, assertions on API responses plus DB/cache side effects, and negative/edge checks required by the acceptance criteria. Test data cleanup: {}.\n\
             HARD GENERATION CONTRACT: these are completion conditions, not suggestions. Before writing the test, prove that the real route, authentication path, and database/cache boundary can be exercised. A direct controller call, `gin.New()` with APIs mounted directly, an inline fixed-response route, an in-memory module replacement, a fabricated token/header, or a status-only smoke test is not an E2E implementation. For a protected path, the test must acquire a real login/token/session/API key and emit an `auth` step before the first protected request. For every required persistence or cache invariant, the test must perform a real runtime read/write and assert the observed state. Use only the runtime endpoint or repository-native harness declared by TestHarbor; never discover or execute service lifecycle commands. If the controlled service or local harness is unavailable, report the missing capability instead of silently generating a lower-level substitute. Do not finish with a candidate that the review would have to reinterpret as a unit or limited-harness test.\n",
            if cleanup { "required; use the framework's cleanup hooks, deferred cleanup, transactions, delete statements, or cache-key cleanup so reruns are safe" } else { "user disabled automatic cleanup; namespace data with unique run/test IDs and avoid broad destructive cleanup" }
        ),
    };
    let strategy_instructions = if external_target.is_some() {
        strategy_instructions.replace(
            "If the repository lacks an executable harness for one of these required boundaries, do not silently generate a lower-level substitute: stop and report the missing harness as a generation blocker.",
            "When a local harness is unavailable, discover and use the real configured/running service and its API-level state instead of blocking solely on the missing local harness.",
        )
    } else {
        strategy_instructions
    };
    let runtime_config_contract = "RUNTIME CONFIG CONTRACT: when the user supplies a service URL, account, password, tenant, or other environment detail in the free-form environment instructions, identify and use it as generation input without inventing a SmartTest-specific variable name. Database, Redis, queue, and other dependency settings must come from the repository's existing configuration or native test helpers. Never add a mandatory TESTHARBOR_* environment preflight for values that the repository config already provides. Keep credentials out of generated source and emitted evidence; use the repository's normal runtime configuration or an explicitly supplied variable at execution time.";
    let strategy_instructions =
        format!("{strategy_instructions}\n{runtime_config_contract}\n{runtime_boundary}");
    let revision_feedback = req
        .change_request
        .as_deref()
        .map(|message| {
            format!(
                "\nThis is a targeted revision of the existing generated tests. First inspect the current test files and preserve all unrelated scenarios, steps, assertions, cleanup, and evidence. Modify only what is necessary to address the listed review issues; do not regenerate the entire test suite or replace unrelated coverage. Address this request explicitly:\n{message}\n"
            )
        })
        .unwrap_or_default();
    // Let Codex write only in the isolated worktree with workspace-write.
    let prompt = if let Some(change_request) = req
        .change_request
        .as_deref()
        .filter(|message| !message.trim().is_empty())
    {
        format!(
             "You are making a targeted revision to already generated tests in a repository worktree.\n\
             Objective: {}\n\
             Repository language hint: {}. Treat this only as a starting hint; inspect the repository, its manifest, README, configuration, and existing test tooling to determine the actual framework and test command.\n\
             Acceptance criteria: {}\n\
             Test strategy and hard generation contract:\n{}\n\
             Review request (fix only these issues):\n{}\n\
             First inspect only the existing changed test files and the directly relevant test helper or route definition needed to fix the request. Use targeted file reads/searches; do not scan the whole repository, reread unrelated production files, or regenerate the test suite.\n\
             Modify only the existing test files in the configured writable test paths. Preserve all unrelated cases, business steps, authentication, assertions, HTTP request/response evidence, and cleanup. Make the smallest change that fixes the listed issues. Do not modify product code, dependency manifests, lockfiles, or configuration.\n\
             Keep every affected business step represented by a literal TESTHARBOR_STEP contract with case_id, type, title, input, action, output, and assertion. Preserve real API/database/cache behavior and never replace it with a stub or fabricated response. Run only the targeted compile/test command needed to verify the changed test files before finishing.",
            req.objective,
            req.language_hint,
            req.acceptance_criteria.join("; "),
            strategy_instructions,
            change_request,
        )
    } else {
        format!(
        "You are generating tests for a repository worktree.\n\
         Objective: {}\n\
         Repository language hint: {}. Treat this only as a starting hint; inspect the repository, its manifest, README, configuration, and existing test tooling to determine the actual framework and test command.\n\
         Acceptance criteria:\n- {}\n\
         {}\
         {}\
         Write or update only files in the repository's existing test layout and the configured writable test paths. Detect the language, framework, test naming convention, and runner from repository files; do not assume Go, a particular directory, or a particular framework. Never modify product code, application modules, dependency manifests, lockfiles, generated source, or configuration files. If the test exposes a product defect, keep the product code unchanged and report/assert the defect in the test or review summary. Do not run commands that rewrite dependencies or source files outside the writable test paths.\n\
         Prefer table-driven tests matching repo style when useful.\n\
         Runtime-only test data, generated reports, screenshots, request/response dumps, and temporary files must go under `.testharbor/runs/{}` in this worktree. Never place runtime artifacts in the repository root or beside product source files; TestHarbor keeps `.testharbor/` in the worktree-local Git exclude list. Test SOURCE files must NEVER be placed under `.testharbor/`: that directory is git-ignored, so a test written there is invisible to candidate detection and the run fails with no candidate changes. Write test source files into the repository's existing test layout (for example `tests/`, `test/`, or beside existing tests) using the repository's test naming convention, so they appear in the candidate diff.\n\
         For every generated end-to-end/integration test, begin with bounded reconnaissance: inspect the changed-file list, project manifest, relevant route registrations, public interfaces, persistence models, runtime config selection, and existing test helpers; use targeted searches and read only files needed to establish evidence. Do not scan unrelated directories or reread the entire repository. Runtime config selection means checking the running process command line, startup scripts, README, `.env`, `config.yml`, `config.yaml`, `*-config.yml`, `*-config.yaml`, and `dev*.yml`; do not stop at empty `.tmp` templates. The flow must name real operations discovered in this repository and put them in execution order; do not collapse it into generic phases such as `准备测试上下文`, `调用接口`, or `断言结果`. Include every setup, create, update, configure, publish, query, state-verification, rollback, and business cleanup operation that is actually required by the target. Never invent domain entities, endpoint names, infrastructure components, or workflow steps that are not supported by repository evidence.\n\
         For protected APIs, trace the real route registration and authentication middleware before writing the test. Include a concrete authentication step before the first protected API: call the repository's real login/token/session endpoint or use its existing authenticated test helper, then carry the resulting token/cookie/session through subsequent requests. Emit this as a `TESTHARBOR_STEP` with `type` `auth` and verify the authentication result. If the flow supports unauthenticated or insufficient-permission behavior, include a separate 401/403 assertion case. A fake header such as `Authorization: Bearer test-token` is not login and must not be used as the only authentication evidence.\n\
         Do not define inline `r.GET`/`r.POST` handlers that merely return fixed responses, replace the production router with local route stubs, or call controller methods directly while claiming API E2E coverage. The test must exercise the real router/controller/middleware chain. If the repository has no local runnable router or auth harness, discover the running service from project configuration and use its real route and authentication path; if it cannot be started, report the exact command and error instead of fabricating an equivalent route.\n\
         Expose that same human-readable flow as static source contracts so a reviewer can see every step before execution. For each logical step, write a literal `TESTHARBOR_STEP` contract as a static source comment in the test file: the exact text `TESTHARBOR_STEP` followed by one compact JSON object must appear in the generated source itself, placed next to the step it describes. A runtime print/echo/log statement that constructs the marker at runtime does NOT satisfy this requirement, because the contract is invisible in the source before the test runs and the review view cannot parse it; you may additionally emit the same contract at runtime for execution-time evidence, but the static source comment is required. Do not construct the step JSON dynamically from runtime values; write the full contract text statically. The following JSON is a contract example, not source-language-specific code:\n\
         TESTHARBOR_STEP {{\"id\":\"00-setup\",\"type\":\"setup\",\"title\":\"准备隔离测试数据\",\"input\":\"目标业务要求的实体和参数\",\"action\":\"使用仓库现有测试工具创建隔离数据\",\"output\":\"后续步骤所需的实体标识或上下文\",\"assertion\":\"数据已创建且不会污染其他测试\"}}\n\
         The `type` field is required and must identify the real execution mechanism: `setup`, `auth`, `api`, `mysql`, `redis`, `database`, `cache`, `queue`, `filesystem`, `assert`, `cleanup`, or `business`. Use `auth` for login/token/session/API-key acquisition, `api` for HTTP/REST/RPC calls, `mysql` for MySQL/SQL persistence operations, and `redis` for Redis/cache operations.\n\
         Emit one TESTHARBOR_STEP for setup, every concrete API/business operation, every database/cache/state verification, rollback or deletion, and final test-data cleanup when applicable. Cleanup must execute after all business verification steps; if it is registered near setup with a deferred cleanup hook, its emitted runtime step must still be the final step. The title must describe the real business action, and input/output/assertion must be understandable to a non-code reviewer. Use stable ordered ids such as `00-setup`, `01-main-operation`, and `99-cleanup` so the UI can track execution order.\n\
         For a module-level objective or a change touching multiple behaviors, generate a test suite with independent business scenarios instead of one oversized test. Before each scenario, emit a literal TESTHARBOR_CASE contract using this shape:\n\
         TESTHARBOR_CASE {{\"id\":\"main-happy-path\",\"title\":\"主要成功场景\",\"objective\":\"验证目标规定的主要业务链路\",\"priority\":\"P0\"}}\n\
         Use one stable case id per scenario, give every case isolated data and cleanup, and cover meaningful variants such as happy path, validation failure, permission failure, idempotency, state inconsistency, and dependency failure when they are relevant. If the objective truly describes one scenario, emit one case.\n\
         Derive the case count from the objective, acceptance criteria, changed files, and actual business behaviors discovered in the repository. Create one independent case for each materially different business scenario, acceptance criterion, failure branch, permission/state rule, persistence/cache invariant, or dependency interaction that needs separate setup and assertions. Broad module/service/API scopes should therefore produce multiple cases, but do not invent artificial cases or pad the suite to a fixed number when the repository proves the scope is one atomic behavior. Map every acceptance criterion to one or more cases, count the TESTHARBOR_CASE markers before finishing, and never return one smoke case when the scope clearly contains multiple independent behaviors.\n\
         Implement each case as a separately enumerable top-level test or a named test subtest so the Test CLI can report its identity independently; do not hide all scenarios behind one undifferentiated test function.\n\
         Add the case_id field to every TESTHARBOR_STEP and use the same case_id in HTTP evidence. For example:\n\
         TESTHARBOR_STEP {{\"case_id\":\"main-happy-path\",\"id\":\"00-setup\",\"type\":\"setup\",\"title\":\"准备隔离测试数据\",\"input\":\"目标业务要求的实体和参数\",\"action\":\"使用仓库现有测试工具创建隔离数据\",\"output\":\"后续步骤所需的实体标识或上下文\",\"assertion\":\"数据已创建且不会污染其他测试\"}}\n\
         For every real HTTP/API operation, also emit the actual sanitized wire evidence at runtime using exactly one compact JSON line with these prefixes:\n\
         TESTHARBOR_HTTP_REQUEST {{\"case_id\":\"main-happy-path\",\"step_id\":\"01-main-operation\",\"method\":\"<actual-method>\",\"url\":\"<actual-url>\",\"headers\":{{\"Content-Type\":\"application/json\"}},\"body\":{{\"<actual-field>\":\"<actual-value>\"}}}}\n\
         TESTHARBOR_HTTP_RESPONSE {{\"case_id\":\"main-happy-path\",\"step_id\":\"01-main-operation\",\"status\":200,\"headers\":{{\"Content-Type\":\"application/json\"}},\"body\":{{\"<actual-field>\":\"<actual-value>\"}}}}\n\
         Both HTTP records must include the same case_id and step_id as the corresponding TESTHARBOR_CASE and TESTHARBOR_STEP.\n\
         Use the actual final method, URL, query/body, response status, response headers, and response body observed by the test. A `TESTHARBOR_HTTP_RESPONSE` containing only `status` is invalid: every response record must include a `body` field with the actual decoded response object, array, string, or an explicit `null`/empty value when the endpoint has no body. Read and decode the real response before emitting the marker; do not replace it with a status-only placeholder or invent a response. The evidence may be constructed dynamically because it must contain runtime values, but never fabricate values. Redact authorization headers, cookies, tokens, passwords, API keys, and other secrets. If a step is not HTTP/API based, do not invent HTTP evidence; its TESTHARBOR_STEP input/output and database/cache evidence are sufficient.\n\
         Before finishing, inspect the generated test files and verify that every concrete business step has a literal TESTHARBOR_STEP contract present as a static source comment in the file (a runtime-only echo/print/log does not count), then ensure new tests compile with the project toolchain.",
        req.objective,
        req.language_hint,
        req.acceptance_criteria.join("\n- "),
        strategy_instructions,
        revision_feedback,
        req.run_id.as_deref().unwrap_or("current"),
        )
    };
    let prompt = if external_target.is_some() {
        prompt.replace(
            "If the repository has no runnable production router or auth harness, report that limitation in the generated test and review instead of silently fabricating an equivalent route.",
            "When a local router or auth harness is unavailable, discover and call the real configured service and authentication path; do not replace it with a stub.",
        )
    } else {
        prompt
    };
    let prompt = format!(
        "{prompt}\nFINAL OUTPUT CONTRACT: after editing, return exactly one complete JSON envelope with schema_version=1, task_kind=generate_tests, test_intent_hash={}, scope_hash={}, evidence_refs={:?}, status=completed, a non-empty summary, and payload {{written_files:[repository-relative paths], proposed_intended_names:[exact stable test identities], notes:[], test_runner:{{executable:\"<repository-native executable>\",args:[\"<arg>\"],cwd:\".\",parser:\"exit-code\",timeout_ms:3600000}}}}. Inspect the project, its language/tooling, configuration, README, and existing tests to select the best executable runner; do not assume any specific language or framework. Missing login credentials, a running service, a dedicated E2E harness, a test database, or persistence fixtures must not stop test generation: record those limits in summary/notes and still generate the best candidate. If a likely test entrypoint exists, return the best-effort command even when it is only command-level exit-code based; use null only when no plausible test command or test entrypoint can be found at all, or when TestHarbor already supplied a compatible confirmed runner. Do not return shell pipelines or a single shell command string: executable and args must be separate. Bare payloads, Markdown fences, missing intended identities, or mismatched hashes/evidence refs are invalid.",
        req.test_intent_hash, req.scope_hash, req.evidence_refs
    );

    let out_dir = req
        .progress_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(&req.worktree_path).join(".testharbor-agent"));
    std::fs::create_dir_all(&out_dir)?;
    let last_msg = out_dir.join("generate-last.json");

    let cancellation = req.run_id.as_deref().map(register_generation);
    let output = match provider {
        CliProvider::Codex => {
            let last_msg_arg = last_msg.to_str().unwrap_or("generate-last.json");
            let args = generate_exec_args(
                req.session_id.as_deref(),
                &req.worktree_path,
                last_msg_arg,
                req.agent_model.as_deref(),
            );
            run_codex_with_progress(
                &args,
                &prompt,
                req.progress_dir.as_deref(),
                cancellation.clone(),
                None,
                req.environment.as_ref().map(|env| &env.variables),
            )
        }
        CliProvider::Claude => {
            let args = claude_generate_args(
                req.session_id.as_deref(),
                &req.worktree_path,
                req.agent_model.as_deref(),
                req.change_request.is_some(),
            );
            run_claude_with_progress(
                &args,
                &prompt,
                Path::new(&req.worktree_path),
                req.progress_dir.as_deref(),
                cancellation.clone(),
                req.environment.as_ref().map(|env| &env.variables),
            )
        }
        CliProvider::Grok => {
            let args = grok_generate_args(
                req.session_id.as_deref(),
                &req.worktree_path,
                req.agent_model.as_deref(),
            );
            run_grok_with_progress(
                &args,
                &prompt,
                req.progress_dir.as_deref(),
                cancellation.clone(),
                req.environment.as_ref().map(|env| &env.variables),
            )
        }
    };
    if let Some(run_id) = req.run_id.as_deref() {
        unregister_generation(run_id);
    }
    let output = output?;
    let session_id = match provider {
        CliProvider::Codex => extract_thread_id(&output.stdout),
        CliProvider::Claude => extract_claude_session_id(&output.stdout),
        CliProvider::Grok => extract_grok_session_id(&output.stdout),
    }
    .or_else(|| req.session_id.clone());

    if !output.status.success() {
        return Err(AgentError::Codex(CodexError::Failed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        )));
    }

    let text = match provider {
        CliProvider::Codex => std::fs::read_to_string(&last_msg)
            .map_err(|error| AgentError::OutputInvalid(error.to_string()))?,
        CliProvider::Claude => extract_claude_text(&output.stdout),
        CliProvider::Grok => extract_grok_text(&output.stdout),
    };
    let envelope =
        parse_json_candidate::<AgentFinalEnvelope<GeneratePayload>>(&text).ok_or_else(|| {
            AgentError::OutputInvalid("could not parse complete generate envelope".into())
        })?;
    validate_envelope(
        &envelope,
        &req.test_intent_hash,
        &req.scope_hash,
        &req.evidence_refs,
        "generate_tests",
    )?;
    if envelope.payload.proposed_intended_names.is_empty() {
        return Err(AgentError::OutputInvalid(
            "generate envelope must propose at least one intended identity".into(),
        ));
    }
    Ok(GenerateResult {
        mode: provider.mode(),
        summary: envelope.summary,
        written_files: envelope.payload.written_files,
        proposed_intended_names: envelope.payload.proposed_intended_names,
        test_runner: envelope.payload.test_runner,
        raw_path: match provider {
            CliProvider::Codex => last_msg.to_str().map(|s| s.to_string()),
            CliProvider::Claude | CliProvider::Grok => req
                .progress_dir
                .as_deref()
                .map(|dir| Path::new(dir).join("generation.stdout.jsonl"))
                .and_then(|path| path.to_str().map(|s| s.to_string())),
        },
        session_id,
    })
}

fn grok_generate_args(
    session_id: Option<&str>,
    worktree_path: &str,
    model: Option<&str>,
) -> Vec<String> {
    let mut args = grok_model_args(model);
    args.extend([
        "--cwd".into(),
        worktree_path.into(),
        "--output-format".into(),
        "streaming-json".into(),
        "--always-approve".into(),
        "--no-alt-screen".into(),
        "--no-memory".into(),
        "--no-subagents".into(),
        "--disable-web-search".into(),
        "--max-turns".into(),
        grok_max_turns(),
        "--tools".into(),
        "read_file,grep,list_dir,write_file,edit_file".into(),
    ]);
    if let Some(session_id) = session_id {
        args.push("--resume".into());
        args.push(session_id.into());
    }
    args
}

fn grok_max_turns() -> String {
    std::env::var("TESTHARBOR_AGENT_MAX_TURNS")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .map(|value| value.clamp(4, 64).to_string())
        .unwrap_or_else(|| "24".into())
}

fn generate_exec_args<'a>(
    _session_id: Option<&'a str>,
    worktree_path: &'a str,
    last_msg: &'a str,
    model: Option<&'a str>,
) -> Vec<&'a str> {
    // A resumed Codex thread does not reliably re-assert cwd/sandbox across
    // CLI versions. Generation therefore starts an ephemeral constrained turn;
    // TestHarbor carries revision feedback explicitly in the request bundle.
    let mut args = vec![
        "exec",
        "--sandbox",
        "workspace-write",
        "--cd",
        worktree_path,
        "--skip-git-repo-check",
        "--ephemeral",
        "--json",
        "--output-last-message",
        last_msg,
    ];
    if let Some(model) = model {
        args.splice(1..1, ["--model", model]);
    }
    args
}

pub fn codex_review(req: &ReviewRequest) -> Result<ReviewResult> {
    review_with_provider(req, CliProvider::Codex)
}

pub fn claude_review(req: &ReviewRequest) -> Result<ReviewResult> {
    review_with_provider(req, CliProvider::Claude)
}

pub fn grok_review(req: &ReviewRequest) -> Result<ReviewResult> {
    review_with_provider(req, CliProvider::Grok)
}

fn repair_review_patch_bundle(bundle: &Path) -> Result<()> {
    let full_patch_path = bundle.join("diff.patch");
    let agent_patch_path = bundle.join("agent-review.patch");
    let Ok(full_patch) = std::fs::read(&full_patch_path) else {
        return Ok(());
    };
    let needs_refresh = std::fs::read(&agent_patch_path)
        .map(|agent_patch| agent_patch != full_patch)
        .unwrap_or(true);
    if needs_refresh {
        std::fs::write(&agent_patch_path, &full_patch)?;
    }

    let manifest_path = bundle.join("manifest.json");
    if let Ok(body) = std::fs::read_to_string(&manifest_path) {
        if let Ok(mut manifest) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(object) = manifest.as_object_mut() {
                object.insert(
                    "agent_review_patch_complete".into(),
                    serde_json::Value::Bool(true),
                );
                let bytes = serde_json::to_vec(&manifest)
                    .map_err(|error| AgentError::OutputInvalid(error.to_string()))?;
                std::fs::write(&manifest_path, bytes)?;
            }
        }
    }
    Ok(())
}

fn review_with_provider(req: &ReviewRequest, provider: CliProvider) -> Result<ReviewResult> {
    match provider {
        CliProvider::Codex => {
            let caps = probe_codex()?;
            if !caps.has_output_schema || !caps.has_sandbox || !caps.has_ephemeral {
                return Err(AgentError::Codex(CodexError::Unavailable(
                    "codex missing --output-schema".into(),
                )));
            }
        }
        CliProvider::Claude => {
            let capabilities = probe_claude()?;
            if !capabilities.has_print
                || !capabilities.has_json_schema
                || !capabilities.has_tools
                || !capabilities.authenticated
            {
                return Err(AgentError::Other(format!(
                    "Claude Code CLI 不可用：{}",
                    capabilities.auth_status
                )));
            }
        }
        CliProvider::Grok => {
            let capabilities = probe_grok()?;
            if !capabilities.has_headless
                || !capabilities.has_json
                || !capabilities.has_tools
                || !capabilities.has_no_subagents
                || !capabilities.has_disable_web_search
                || !capabilities.authenticated
            {
                return Err(AgentError::Other(format!(
                    "Grok CLI 不可用：{}",
                    capabilities.auth_status
                )));
            }
        }
    }

    let bundle = Path::new(&req.bundle_dir);
    repair_review_patch_bundle(bundle)?;
    let schema_path = bundle.join("review-schema.json");
    let last_msg = bundle.join("review-last.json");
    write_review_schema(&schema_path)?;

    let revision_feedback = req
        .change_request
        .as_deref()
        .map(|message| format!("\nHuman revision request to verify:\n{message}\n"))
        .unwrap_or_default();
    let prompt = format!(
        "Review the generated tests using only files in this bundle directory. Read test-intent.json, changed-files.json, agent-review.patch, and validation-summary.json once, then make the decision; do not search the repository or inspect unrelated files. The objective, acceptance criteria, and test intent hash are in test-intent.json; do not repeat or reconstruct them from other files. agent-review.patch is the complete agent-facing patch for every path listed in changed-files.json; inspect the entire patch before deciding. This is a generation-time review: validation-summary.json contains static generation validation, not execution output. Do not require runtime HTTP evidence at this stage.\n\
         {}\
         Changed paths: {}\n\
         For every protected API path, verify that the generated test reaches the real router and authentication middleware. A request with a fabricated Authorization header is not an authentication step. Require a concrete login/token/session/API-key acquisition step and a corresponding `TESTHARBOR_STEP` of type `auth`; flag missing authentication, inline route stubs, fixed-response handlers, direct controller calls, or mocked middleware as a major finding. For every `TESTHARBOR_HTTP_RESPONSE`, inspect the generated source and verify that it reads the actual response status, headers, and decoded body before emitting the marker. A source marker that contains only `status`, omits `body`/`headers`, hardcodes a response, or never captures the real response is a major finding. Do not flag the absence of runtime response records in validation-summary.json here; execution-time evidence is checked after the test runs.\n\
         Respond with exactly one complete JSON envelope matching the schema. Set schema_version=1, task_kind=review_tests, status=completed, and echo test_intent_hash={}, scope_hash={}, evidence_refs={:?} exactly. The payload contains recommendation APPROVE|CHANGES|BLOCK|NEEDS_HUMAN and findings. Bare payloads are invalid. Do not use Markdown fences, prose, or multiple JSON objects. Use Simplified Chinese for the human-facing summary, finding messages, suggestions, and other explanatory text. Keep recommendation enum values, file paths, identifiers, and code symbols unchanged.",
        revision_feedback,
        req.changed_paths.join(", "),
        req.test_intent_hash,
        req.scope_hash,
        req.evidence_refs
    );

    let cancellation = req.run_id.as_deref().map(register_generation);
    let output = match provider {
        CliProvider::Codex => run_codex_with_progress(
            &codex_structured_args(
                req.session_id.as_deref(),
                &req.bundle_dir,
                schema_path.to_str().unwrap_or("review-schema.json"),
                last_msg.to_str().unwrap_or("review-last.json"),
            ),
            &prompt,
            req.progress_dir.as_deref(),
            cancellation,
            req.agent_model.as_deref(),
            None,
        ),
        CliProvider::Claude => {
            let schema = std::fs::read_to_string(&schema_path)?;
            let args = claude_json_args(
                &req.bundle_dir,
                &schema,
                req.agent_model.as_deref(),
                req.session_id.as_deref(),
                "Read,Grep,Glob",
            );
            run_claude_with_progress(
                &args,
                &prompt,
                Path::new(&req.bundle_dir),
                req.progress_dir.as_deref(),
                cancellation,
                None,
            )
        }
        CliProvider::Grok => {
            let schema = std::fs::read_to_string(&schema_path)?;
            let args = grok_json_args(
                &req.bundle_dir,
                &schema,
                req.agent_model.as_deref(),
                req.session_id.as_deref(),
            );
            run_grok_with_progress(
                &args,
                &prompt,
                req.progress_dir.as_deref(),
                cancellation,
                None,
            )
        }
    };
    if let Some(run_id) = req.run_id.as_deref() {
        unregister_generation(run_id);
    }
    let output = output?;

    if !output.status.success() {
        return Err(AgentError::Codex(CodexError::Failed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        )));
    }

    let text = match provider {
        CliProvider::Codex => std::fs::read_to_string(&last_msg)
            .map_err(|e| AgentError::OutputInvalid(e.to_string()))?,
        CliProvider::Claude => extract_claude_text(&output.stdout),
        CliProvider::Grok => extract_grok_text(&output.stdout),
    };
    parse_review_output(
        &text,
        &req.test_intent_hash,
        &req.scope_hash,
        &req.evidence_refs,
        provider.mode(),
    )
}

fn grok_json_args(
    cwd: &str,
    schema: &str,
    model: Option<&str>,
    _session_id: Option<&str>,
) -> Vec<String> {
    let mut args = grok_model_args(model);
    args.extend([
        "--cwd".into(),
        cwd.into(),
        "--output-format".into(),
        "json".into(),
        "--json-schema".into(),
        schema.into(),
        "--always-approve".into(),
        "--no-alt-screen".into(),
        "--no-memory".into(),
        "--no-subagents".into(),
        "--disable-web-search".into(),
        "--max-turns".into(),
        grok_max_turns(),
        "--tools".into(),
        "read_file,grep,list_dir".into(),
    ]);
    args
}

fn codex_structured_args<'a>(
    _session_id: Option<&'a str>,
    cwd: &'a str,
    schema: &'a str,
    last_message: &'a str,
) -> Vec<&'a str> {
    vec![
        "exec",
        "--sandbox",
        "read-only",
        "--cd",
        cwd,
        "--skip-git-repo-check",
        "--ephemeral",
        "--output-schema",
        schema,
        "--json",
        "--output-last-message",
        last_message,
    ]
}

fn grok_model_args(model: Option<&str>) -> Vec<String> {
    let model = model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("TESTHARBOR_GROK_MODEL")
                .ok()
                .filter(|model| !model.trim().is_empty())
        });
    let Some(model) = model else {
        return vec![];
    };
    vec!["--model".into(), model]
}

fn write_review_schema(path: &Path) -> Result<()> {
    let schema = agent_envelope_schema(
        "review_tests",
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["recommendation", "findings"],
            "properties": {
                "recommendation": {
                    "type": "string",
                    "enum": ["APPROVE", "CHANGES", "BLOCK", "NEEDS_HUMAN"]
                },
                "findings": findings_schema()
            }
        }),
    );
    std::fs::write(path, serde_json::to_vec_pretty(&schema).unwrap())?;
    Ok(())
}

fn parse_review_output(
    text: &str,
    expected_hash: &str,
    expected_scope_hash: &str,
    expected_evidence_refs: &[String],
    mode: AgentMode,
) -> Result<ReviewResult> {
    let env = parse_json_candidate::<AgentFinalEnvelope<ReviewPayload>>(text).ok_or_else(|| {
        AgentError::OutputInvalid(format!(
            "could not parse complete review envelope: {}",
            output_preview(text)
        ))
    })?;
    validate_envelope(
        &env,
        expected_hash,
        expected_scope_hash,
        expected_evidence_refs,
        "review_tests",
    )?;
    if !matches!(
        env.payload.recommendation.as_str(),
        "APPROVE" | "CHANGES" | "BLOCK" | "NEEDS_HUMAN"
    ) {
        return Err(AgentError::OutputInvalid(
            "review recommendation is outside the contract enum".into(),
        ));
    }
    Ok(ReviewResult {
        mode,
        recommendation: env.payload.recommendation,
        summary: env.summary,
        findings: env.payload.findings,
        test_intent_hash: env.test_intent_hash,
    })
}

fn validate_envelope<T>(
    envelope: &AgentFinalEnvelope<T>,
    expected_hash: &str,
    expected_scope_hash: &str,
    expected_evidence_refs: &[String],
    expected_task_kind: &str,
) -> Result<()> {
    if envelope.schema_version != SCHEMA_VERSION
        || envelope.task_kind != expected_task_kind
        || envelope.test_intent_hash != expected_hash
        || envelope.scope_hash != expected_scope_hash
        || envelope.evidence_refs != expected_evidence_refs
        || envelope.status != "completed"
        || envelope.summary.trim().is_empty()
    {
        return Err(AgentError::OutputInvalid(format!(
            "agent envelope mismatch: expected schema={} task={} intent={} scope={} evidence_refs={:?} status=completed",
            SCHEMA_VERSION,
            expected_task_kind,
            expected_hash,
            expected_scope_hash,
            expected_evidence_refs
        )));
    }
    Ok(())
}

/// Normalize the final response boundary shared by all CLI providers.
///
/// Codex writes the last message to a file, while Grok returns a wrapper or
/// sometimes markdown around the model's JSON. The workflow must not care
/// which CLI produced the response, so adapters accept only a JSON object that
/// can deserialize into the provider-neutral contract and ignore transport
/// wrappers around it.
fn parse_json_candidate<T: DeserializeOwned>(text: &str) -> Option<T> {
    let mut candidates = Vec::new();
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        candidates.push(trimmed.to_string());
    }

    for block in fenced_json_blocks(text) {
        candidates.push(block);
    }

    // A CLI may prepend a short status line or emit JSONL events. Try each
    // complete object from the end, where the final response normally lives.
    candidates.extend(json_object_candidates(text));

    for candidate in candidates {
        if let Ok(value) = serde_json::from_str::<T>(&candidate) {
            return Some(value);
        }
    }
    None
}

fn fenced_json_blocks(text: &str) -> Vec<String> {
    text.lines()
        .collect::<Vec<_>>()
        .split(|line| line.trim_start().starts_with("~~~"))
        .filter_map(|lines| {
            if lines.len() < 2 {
                return None;
            }
            let body = lines[1..lines.len().saturating_sub(1)].join("\n");
            (!body.trim().is_empty()).then_some(body)
        })
        .collect()
}

fn json_object_candidates(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut objects = Vec::new();
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (index, byte) in bytes.iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' if depth == 0 => {
                start = Some(index);
                depth = 1;
            }
            b'{' if depth > 0 => depth += 1,
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(begin) = start.take() {
                        objects.push(String::from_utf8_lossy(&bytes[begin..=index]).to_string());
                    }
                }
            }
            _ => {}
        }
    }
    objects.reverse();
    objects
}

fn output_preview(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview = compact.chars().take(240).collect::<String>();
    if compact.chars().count() > 240 {
        format!("{preview}…")
    } else {
        preview
    }
}

/// Analyzer Agent via Codex/Grok (read-only; structured JSON).
pub fn codex_analyze(req: &AnalyzeRequest) -> Result<AnalyzeResult> {
    analyze_with_provider(req, CliProvider::Codex)
}

pub fn claude_analyze(req: &AnalyzeRequest) -> Result<AnalyzeResult> {
    analyze_with_provider(req, CliProvider::Claude)
}

pub fn grok_analyze(req: &AnalyzeRequest) -> Result<AnalyzeResult> {
    analyze_with_provider(req, CliProvider::Grok)
}

fn analyze_with_provider(req: &AnalyzeRequest, provider: CliProvider) -> Result<AnalyzeResult> {
    match provider {
        CliProvider::Codex => {
            let caps = probe_codex()?;
            if !caps.has_output_schema || !caps.has_sandbox || !caps.has_ephemeral {
                return Err(AgentError::Codex(CodexError::Unavailable(
                    "codex missing --output-schema".into(),
                )));
            }
        }
        CliProvider::Claude => {
            let capabilities = probe_claude()?;
            if !capabilities.has_print
                || !capabilities.has_json_schema
                || !capabilities.has_tools
                || !capabilities.authenticated
            {
                return Err(AgentError::Other(format!(
                    "Claude Code CLI 不可用：{}",
                    capabilities.auth_status
                )));
            }
        }
        CliProvider::Grok => {
            let capabilities = probe_grok()?;
            if !capabilities.has_headless
                || !capabilities.has_json
                || !capabilities.has_tools
                || !capabilities.has_no_subagents
                || !capabilities.has_disable_web_search
                || !capabilities.authenticated
            {
                return Err(AgentError::Other(format!(
                    "Grok CLI 不可用：{}",
                    capabilities.auth_status
                )));
            }
        }
    }

    let out_dir = std::env::temp_dir().join(format!(
        "testharbor-analyze-{}",
        &req.test_intent_hash.chars().take(12).collect::<String>()
    ));
    std::fs::create_dir_all(&out_dir)?;
    let schema_path = out_dir.join("analyze-schema.json");
    let last_msg = out_dir.join("analyze-last.json");
    write_analyze_schema(&schema_path)?;

    let prompt = format!(
        "You are the Analyzer Agent for test execution results.\n\
         Classify the outcome as one of: PASS | TEST_BUG | PRODUCT_BUG | ENVIRONMENT_ERROR | FLAKY | UNKNOWN.\n\
         Objective: {}\n\
         Acceptance: {}\n\
         Evidence: exit={:?} parser={} completeness={} consistency={} total={} passed={} failed={} empty={} \
         intended={}/{} observed failed={} missing={} failure_phase={:?}\n\
         stdout excerpt:\n{}\n\
         stderr excerpt:\n{}\n\
         Respond with exactly one complete JSON envelope matching the schema. Set schema_version=1, task_kind=analyze_result, status=completed, and echo test_intent_hash={}, scope_hash={}, evidence_refs={:?} exactly. The payload contains suggested_verdict, confidence (high|medium|low), evidence_notes[], findings[]. Bare payloads are invalid. Do not use Markdown fences, prose, or multiple JSON objects. Do not invent evidence not present above.",
        req.objective,
        req.acceptance_criteria.join("; "),
        req.exit_code,
        req.parser,
        req.completeness,
        req.consistency,
        req.total,
        req.passed,
        req.failed,
        req.empty,
        req.intended_observed,
        req.intended_count,
        req.intended_failed,
        req.intended_missing,
        req.failure_phase,
        req.stdout_excerpt.chars().take(2400).collect::<String>(),
        req.stderr_excerpt.chars().take(1200).collect::<String>(),
        req.test_intent_hash,
        req.scope_hash,
        req.evidence_refs
    );

    let output = match provider {
        CliProvider::Codex => run_codex_prompt(
            &codex_structured_args(
                req.session_id.as_deref(),
                out_dir.to_str().unwrap_or("."),
                schema_path.to_str().unwrap_or("analyze-schema.json"),
                last_msg.to_str().unwrap_or("analyze-last.json"),
            ),
            &prompt,
            req.agent_model.as_deref(),
        )?,
        CliProvider::Claude => {
            let schema = std::fs::read_to_string(&schema_path)?;
            let args = claude_json_args(
                out_dir.to_str().unwrap_or("."),
                &schema,
                req.agent_model.as_deref(),
                req.session_id.as_deref(),
                "",
            );
            run_claude_prompt(&args, &prompt, &out_dir)?
        }
        CliProvider::Grok => {
            let schema = std::fs::read_to_string(&schema_path)?;
            let args = grok_json_args(
                out_dir.to_str().unwrap_or("."),
                &schema,
                req.agent_model.as_deref(),
                req.session_id.as_deref(),
            );
            run_grok_prompt(&args, &prompt)?
        }
    };

    if !output.status.success() {
        return Err(AgentError::Codex(CodexError::Failed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        )));
    }

    let text = match provider {
        CliProvider::Codex => std::fs::read_to_string(&last_msg)
            .map_err(|e| AgentError::OutputInvalid(e.to_string()))?,
        CliProvider::Claude => extract_claude_text(&output.stdout),
        CliProvider::Grok => extract_grok_text(&output.stdout),
    };
    parse_analyze_output(
        &text,
        &req.test_intent_hash,
        &req.scope_hash,
        &req.evidence_refs,
        provider.mode(),
    )
}

/// Result Reviewer Agent via Codex/Grok — independent check of analyzer + hard evidence.
pub fn codex_review_result(req: &ResultReviewRequest) -> Result<ResultReviewAgentResult> {
    review_result_with_provider(req, CliProvider::Codex)
}

pub fn claude_review_result(req: &ResultReviewRequest) -> Result<ResultReviewAgentResult> {
    review_result_with_provider(req, CliProvider::Claude)
}

pub fn grok_review_result(req: &ResultReviewRequest) -> Result<ResultReviewAgentResult> {
    review_result_with_provider(req, CliProvider::Grok)
}

fn review_result_with_provider(
    req: &ResultReviewRequest,
    provider: CliProvider,
) -> Result<ResultReviewAgentResult> {
    match provider {
        CliProvider::Codex => {
            let caps = probe_codex()?;
            if !caps.has_output_schema || !caps.has_sandbox || !caps.has_ephemeral {
                return Err(AgentError::Codex(CodexError::Unavailable(
                    "codex missing --output-schema".into(),
                )));
            }
        }
        CliProvider::Claude => {
            let capabilities = probe_claude()?;
            if !capabilities.has_print
                || !capabilities.has_json_schema
                || !capabilities.has_tools
                || !capabilities.authenticated
            {
                return Err(AgentError::Other(format!(
                    "Claude Code CLI 不可用：{}",
                    capabilities.auth_status
                )));
            }
        }
        CliProvider::Grok => {
            let capabilities = probe_grok()?;
            if !capabilities.has_headless
                || !capabilities.has_json
                || !capabilities.has_tools
                || !capabilities.has_no_subagents
                || !capabilities.has_disable_web_search
                || !capabilities.authenticated
            {
                return Err(AgentError::Other(format!(
                    "Grok CLI 不可用：{}",
                    capabilities.auth_status
                )));
            }
        }
    }

    let out_dir = std::env::temp_dir().join(format!(
        "testharbor-result-review-{}",
        &req.test_intent_hash.chars().take(12).collect::<String>()
    ));
    std::fs::create_dir_all(&out_dir)?;
    let schema_path = out_dir.join("result-review-schema.json");
    let last_msg = out_dir.join("result-review-last.json");
    write_result_review_schema(&schema_path)?;

    let prompt = format!(
        "You are the Result Reviewer. Independently check the Analyzer against hard evidence.\n\
         Recommendation must be one of: ACCEPT_PASS | ACCEPT_FAIL | RERUN | CHANGE_TEST | NEEDS_HUMAN.\n\
         Objective: {}\n\
         Analyzer verdict={} confidence={} summary={}\n\
         Evidence: exit={:?} completeness={} consistency={} total={} failed={} empty={} \
         intended={}/{} accept_pass_enabled={} accept_pass_reasons={:?}\n\
         Flag analyzer_overreach=true if Analyzer over-infers beyond evidence.\n\
         AcceptPass Guard is authoritative when enabled — do not recommend ACCEPT_PASS if Guard is false.\n\
         Respond with exactly one complete JSON envelope matching the schema. Set schema_version=1, task_kind=review_result, status=completed, and echo test_intent_hash={}, scope_hash={}, evidence_refs={:?} exactly. Bare payloads are invalid. Do not use Markdown fences, prose, or multiple JSON objects. Use Simplified Chinese for summary, evidence notes, and finding messages/suggestions. Keep verdict and recommendation enum values in English for machine parsing.",
        req.objective,
        req.analyzer_verdict,
        req.analyzer_confidence,
        req.analyzer_summary,
        req.exit_code,
        req.completeness,
        req.consistency,
        req.total,
        req.failed,
        req.empty,
        req.intended_observed,
        req.intended_count,
        req.accept_pass_enabled,
        req.accept_pass_reasons,
        req.test_intent_hash,
        req.scope_hash,
        req.evidence_refs
    );

    let output = match provider {
        CliProvider::Codex => run_codex_prompt(
            &codex_structured_args(
                None,
                out_dir.to_str().unwrap_or("."),
                schema_path.to_str().unwrap_or("result-review-schema.json"),
                last_msg.to_str().unwrap_or("result-review-last.json"),
            ),
            &prompt,
            req.agent_model.as_deref(),
        )?,
        CliProvider::Claude => {
            let schema = std::fs::read_to_string(&schema_path)?;
            let args = claude_json_args(
                out_dir.to_str().unwrap_or("."),
                &schema,
                req.agent_model.as_deref(),
                None,
                "",
            );
            run_claude_prompt(&args, &prompt, &out_dir)?
        }
        CliProvider::Grok => {
            let schema = std::fs::read_to_string(&schema_path)?;
            let args = grok_json_args(
                out_dir.to_str().unwrap_or("."),
                &schema,
                req.agent_model.as_deref(),
                None,
            );
            run_grok_prompt(&args, &prompt)?
        }
    };

    if !output.status.success() {
        return Err(AgentError::Codex(CodexError::Failed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        )));
    }

    let text = match provider {
        CliProvider::Codex => std::fs::read_to_string(&last_msg)
            .map_err(|e| AgentError::OutputInvalid(e.to_string()))?,
        CliProvider::Claude => extract_claude_text(&output.stdout),
        CliProvider::Grok => extract_grok_text(&output.stdout),
    };
    parse_result_review_output(
        &text,
        &req.test_intent_hash,
        &req.scope_hash,
        &req.evidence_refs,
        provider.mode(),
    )
}

fn write_analyze_schema(path: &Path) -> Result<()> {
    let schema = agent_envelope_schema(
        "analyze_result",
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["suggested_verdict", "confidence", "evidence_notes", "findings"],
            "properties": {
                "suggested_verdict": {
                    "type": "string",
                    "enum": ["PASS", "TEST_BUG", "PRODUCT_BUG", "ENVIRONMENT_ERROR", "FLAKY", "UNKNOWN"]
                },
                "confidence": {
                    "type": "string",
                    "enum": ["high", "medium", "low"]
                },
                "evidence_notes": {"type": "array", "items": {"type": "string"}},
                "findings": findings_schema()
            }
        }),
    );
    std::fs::write(path, serde_json::to_vec_pretty(&schema).unwrap())?;
    Ok(())
}

fn write_result_review_schema(path: &Path) -> Result<()> {
    let schema = agent_envelope_schema(
        "review_result",
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["recommendation", "analyzer_overreach", "findings"],
            "properties": {
                "recommendation": {
                    "type": "string",
                    "enum": ["ACCEPT_PASS", "ACCEPT_FAIL", "RERUN", "CHANGE_TEST", "NEEDS_HUMAN"]
                },
                "analyzer_overreach": {"type": "boolean"},
                "findings": findings_schema()
            }
        }),
    );
    std::fs::write(path, serde_json::to_vec_pretty(&schema).unwrap())?;
    Ok(())
}

fn agent_envelope_schema(task_kind: &str, payload_schema: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["schema_version", "task_kind", "test_intent_hash", "scope_hash", "evidence_refs", "status", "summary", "payload"],
        "properties": {
            "schema_version": {"type": "integer", "enum": [SCHEMA_VERSION]},
            "task_kind": {"type": "string", "enum": [task_kind]},
            "test_intent_hash": {"type": "string"},
            "scope_hash": {"type": "string"},
            "evidence_refs": {"type": "array", "items": {"type": "string"}},
            "status": {"type": "string", "enum": ["completed"]},
            "summary": {"type": "string"},
            "payload": payload_schema
        }
    })
}

fn findings_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["severity", "category", "file", "line", "message", "suggestion"],
            "properties": {
                "severity": {"type": "string"},
                "category": {"type": "string"},
                "file": {"type": ["string", "null"]},
                "line": {"type": ["integer", "null"], "minimum": 1},
                "message": {"type": "string"},
                "suggestion": {"type": ["string", "null"]}
            }
        }
    })
}

fn parse_analyze_output(
    text: &str,
    expected_hash: &str,
    expected_scope_hash: &str,
    expected_evidence_refs: &[String],
    mode: AgentMode,
) -> Result<AnalyzeResult> {
    let env =
        parse_json_candidate::<AgentFinalEnvelope<AnalysisPayload>>(text).ok_or_else(|| {
            AgentError::OutputInvalid(format!(
                "could not parse complete analyze envelope: {}",
                output_preview(text)
            ))
        })?;
    validate_envelope(
        &env,
        expected_hash,
        expected_scope_hash,
        expected_evidence_refs,
        "analyze_result",
    )?;
    if !matches!(
        env.payload.suggested_verdict.as_str(),
        "PASS" | "TEST_BUG" | "PRODUCT_BUG" | "ENVIRONMENT_ERROR" | "FLAKY" | "UNKNOWN"
    ) || !matches!(env.payload.confidence.as_str(), "high" | "medium" | "low")
    {
        return Err(AgentError::OutputInvalid(
            "analyzer enum is outside the contract".into(),
        ));
    }
    Ok(AnalyzeResult {
        mode,
        suggested_verdict: env.payload.suggested_verdict,
        confidence: env.payload.confidence,
        summary: env.summary,
        evidence_notes: env.payload.evidence_notes,
        findings: env.payload.findings,
        test_intent_hash: env.test_intent_hash,
    })
}

fn parse_result_review_output(
    text: &str,
    expected_hash: &str,
    expected_scope_hash: &str,
    expected_evidence_refs: &[String],
    mode: AgentMode,
) -> Result<ResultReviewAgentResult> {
    let env =
        parse_json_candidate::<AgentFinalEnvelope<ResultReviewPayload>>(text).ok_or_else(|| {
            AgentError::OutputInvalid(format!(
                "could not parse complete result-review envelope: {}",
                output_preview(text)
            ))
        })?;
    validate_envelope(
        &env,
        expected_hash,
        expected_scope_hash,
        expected_evidence_refs,
        "review_result",
    )?;
    if !matches!(
        env.payload.recommendation.as_str(),
        "ACCEPT_PASS" | "ACCEPT_FAIL" | "RERUN" | "CHANGE_TEST" | "NEEDS_HUMAN"
    ) {
        return Err(AgentError::OutputInvalid(
            "result-review recommendation is outside the contract enum".into(),
        ));
    }
    Ok(ResultReviewAgentResult {
        mode,
        recommendation: env.payload.recommendation,
        analyzer_overreach: env.payload.analyzer_overreach,
        summary: env.summary,
        findings: env.payload.findings,
        test_intent_hash: env.test_intent_hash,
    })
}

#[allow(dead_code)]
fn _schema_version() -> u32 {
    SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_truncated_review_patch_from_full_diff() {
        let bundle =
            std::env::temp_dir().join(format!("testharbor-agent-review-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("diff.patch"), "full patch\nwith all lines\n").unwrap();
        std::fs::write(bundle.join("agent-review.patch"), "truncated\n").unwrap();
        std::fs::write(bundle.join("manifest.json"), r#"{"hash":"x"}"#).unwrap();

        repair_review_patch_bundle(&bundle).unwrap();

        assert_eq!(
            std::fs::read_to_string(bundle.join("agent-review.patch")).unwrap(),
            "full patch\nwith all lines\n"
        );
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(bundle.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["agent_review_patch_complete"], true);
        std::fs::remove_dir_all(bundle).unwrap();
    }

    fn assert_strict_objects(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                if map.get("type").and_then(|v| v.as_str()) == Some("object") {
                    assert_eq!(
                        map.get("additionalProperties"),
                        Some(&serde_json::Value::Bool(false)),
                        "object schema must set additionalProperties=false: {value}"
                    );
                }
                for child in map.values() {
                    assert_strict_objects(child);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    assert_strict_objects(child);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn output_schemas_are_strict_openai_json_schemas() {
        let dir = tempfile::tempdir().unwrap();
        for (name, write) in [
            (
                "review-schema.json",
                write_review_schema as fn(&Path) -> Result<()>,
            ),
            ("analyze-schema.json", write_analyze_schema),
            ("result-review-schema.json", write_result_review_schema),
        ] {
            let path = dir.path().join(name);
            write(&path).unwrap();
            let schema: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_strict_objects(&schema);
        }
    }

    #[test]
    fn parses_codex_models_from_cli_catalog() {
        let catalog = parse_codex_model_catalog(
            r#"{"models":[
                {"slug":"gpt-current","visibility":"list","supported_in_api":true},
                {"slug":"gpt-hidden","visibility":"hide","supported_in_api":true},
                {"slug":"codex-spark","visibility":"list","supported_in_api":false}
            ]}"#,
        )
        .unwrap();
        assert_eq!(catalog.models, ["codex-spark", "gpt-current"]);
    }

    #[test]
    fn parses_claude_model_names_from_help() {
        let models = parse_claude_model_aliases(
            "  --model <model> Provide an alias 'opus' or 'sonnet' or a model's full name (e.g. 'claude-opus-5').\n  -n, --name <name>",
        );
        assert_eq!(models, ["opus", "sonnet", "claude-opus-5"]);
    }

    #[test]
    fn probe_may_fail_offline() {
        // Should not panic
        let _ = probe_codex();
        let _ = probe_grok();
    }

    #[test]
    fn extracts_thread_id_from_jsonl() {
        let stdout = br#"{"type":"turn.started"}
{"type":"thread.started","thread_id":"019f6a7e-b361-70a0-8c72-61b190313d60"}
{"type":"turn.completed"}
"#;
        assert_eq!(
            extract_thread_id(stdout).as_deref(),
            Some("019f6a7e-b361-70a0-8c72-61b190313d60")
        );
    }

    #[test]
    fn resumed_generation_still_uses_fresh_constrained_sandbox() {
        let args = generate_exec_args(Some("session-123"), "/tmp/worktree", "/tmp/last.json", None);
        assert_eq!(args[0], "exec");
        assert!(args.contains(&"--skip-git-repo-check"));
        assert!(args.contains(&"--sandbox"));
        assert!(args.contains(&"--cd"));
        assert!(args.contains(&"--ephemeral"));
        assert!(!args.contains(&"resume"));
        assert!(!args.contains(&"session-123"));
    }

    #[test]
    fn codex_generation_passes_selected_model_on_new_and_resumed_sessions() {
        let fresh = generate_exec_args(None, "/tmp/worktree", "/tmp/last.json", Some("gpt-5"));
        assert!(fresh.windows(2).any(|pair| pair == ["--model", "gpt-5"]));
        let resumed = generate_exec_args(
            Some("session-123"),
            "/tmp/worktree",
            "/tmp/last.json",
            Some("gpt-5"),
        );
        assert!(resumed.windows(2).any(|pair| pair == ["--model", "gpt-5"]));
    }

    #[test]
    fn grok_generation_uses_headless_resume_flags() {
        let args = grok_generate_args(Some("session-123"), "/tmp/worktree", None);
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--resume" && pair[1] == "session-123"));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--cwd" && pair[1] == "/tmp/worktree"));
        assert!(args
            .windows(2)
            .any(|pair| { pair[0] == "--output-format" && pair[1] == "streaming-json" }));
        assert!(args.contains(&"--always-approve".into()));
    }

    #[test]
    fn claude_generation_uses_print_stream_and_resume_flags() {
        let args = claude_generate_args(
            Some("claude-session-123"),
            "/tmp/worktree",
            Some("sonnet"),
            false,
        );
        assert!(args
            .windows(2)
            .any(|pair| { pair[0] == "--output-format" && pair[1] == "stream-json" }));
        assert!(!args.contains(&"--include-partial-messages".into()));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--effort" && pair[1] == "low"));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--tools" && pair[1].contains("Read")));
        assert!(!args.iter().any(|arg| arg.contains("Bash")));
        assert!(args
            .windows(2)
            .any(|pair| { pair[0] == "--resume" && pair[1] == "claude-session-123" }));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--model" && pair[1] == "sonnet"));
        assert!(args.contains(&"--dangerously-skip-permissions".into()));
    }

    #[test]
    fn readonly_agent_does_not_resume_generation_session() {
        let args = codex_structured_args(
            Some("generation-session"),
            "/tmp/bundle",
            "/tmp/schema.json",
            "/tmp/last.json",
        );
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--sandbox", "read-only"]));
        assert!(args.contains(&"--ephemeral"));
        assert!(!args.contains(&"resume"));
        assert!(!args.contains(&"generation-session"));
    }

    #[test]
    fn extracts_grok_text_and_session_id() {
        let stdout = br#"{"type":"text","data":"hello"}
{"type":"end","sessionId":"grok-session-123"}
"#;
        assert_eq!(
            extract_grok_session_id(stdout).as_deref(),
            Some("grok-session-123")
        );
        let json = br#"{"text":"{\"ok\":true}","sessionId":"grok-session-123"}"#;
        assert_eq!(extract_grok_text(json), r#"{"ok":true}"#);
    }

    #[test]
    fn extracts_claude_json_wrapper_and_session_id() {
        let stdout = br#"{"type":"result","session_id":"claude-session-123","result":"{\"recommendation\":\"APPROVE\",\"findings\":[]}"}"#;
        assert_eq!(
            extract_claude_session_id(stdout).as_deref(),
            Some("claude-session-123")
        );
        assert_eq!(
            extract_claude_text(stdout),
            r#"{"recommendation":"APPROVE","findings":[]}"#
        );
    }

    #[test]
    fn parses_complete_review_envelope_from_cli_wrappers() {
        let evidence = vec!["candidate:one".to_string()];
        let text = "Review complete:\n~~~json\n{\"schema_version\":1,\"task_kind\":\"review_tests\",\"test_intent_hash\":\"intent-1\",\"scope_hash\":\"scope-1\",\"evidence_refs\":[\"candidate:one\"],\"status\":\"completed\",\"summary\":\"需要修改\",\"payload\":{\"recommendation\":\"CHANGES\",\"findings\":[]}}\n~~~";
        let parsed =
            parse_review_output(text, "intent-1", "scope-1", &evidence, AgentMode::Grok).unwrap();
        assert_eq!(parsed.recommendation, "CHANGES");
        assert_eq!(parsed.test_intent_hash, "intent-1");
    }

    #[test]
    fn rejects_bare_payload_and_accepts_envelope_from_jsonl_tail() {
        assert!(parse_review_output(
            "{\"recommendation\":\"APPROVE\",\"findings\":[]}",
            "intent-2",
            "scope-2",
            &[],
            AgentMode::Codex
        )
        .is_err());
        let text = "{\"type\":\"status\",\"data\":\"done\"}\n{\"schema_version\":1,\"task_kind\":\"review_tests\",\"test_intent_hash\":\"intent-2\",\"scope_hash\":\"scope-2\",\"evidence_refs\":[],\"status\":\"completed\",\"summary\":\"通过\",\"payload\":{\"recommendation\":\"APPROVE\",\"findings\":[]}}";
        let parsed =
            parse_review_output(text, "intent-2", "scope-2", &[], AgentMode::Codex).unwrap();
        assert_eq!(parsed.recommendation, "APPROVE");
        assert_eq!(parsed.test_intent_hash, "intent-2");
        assert!(
            parse_review_output(text, "different-intent", "scope-2", &[], AgentMode::Codex)
                .is_err()
        );
        assert!(
            parse_review_output(text, "intent-2", "different-scope", &[], AgentMode::Codex)
                .is_err()
        );
    }

    #[test]
    fn parses_grok_model_catalog_without_hardcoded_models() {
        let catalog = parse_grok_model_catalog(
            "You are logged in with grok.com.\n\nDefault model: grok-4.5\n\nAvailable models:\n  * grok-4.5 (default)\n  * grok-next\n",
        );
        assert_eq!(catalog.default_model.as_deref(), Some("grok-4.5"));
        assert_eq!(catalog.models, ["grok-4.5", "grok-next"]);
    }

    #[cfg(unix)]
    #[test]
    fn command_drains_large_output_without_deadlock() {
        let output = run_command_with_timeout_logged(
            "sh",
            &["-c", "yes testharbor | head -n 20000"],
            None,
            Duration::from_secs(5),
            Duration::from_secs(5),
            None,
            None,
        )
        .unwrap();
        assert!(output.status.success());
        assert!(output.stdout.len() > 64_000);
    }

    #[cfg(unix)]
    #[test]
    fn command_writes_stdin_and_closes_pipe() {
        let output = run_command_with_timeout_logged(
            "cat",
            &[],
            Some("testharbor prompt\n"),
            Duration::from_secs(5),
            Duration::from_secs(5),
            None,
            None,
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "testharbor prompt\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_timeout_stops_child() {
        let err = run_command_with_timeout_logged(
            "sh",
            &["-c", "sleep 5"],
            None,
            Duration::from_millis(250),
            Duration::from_millis(250),
            None,
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("runtime limit")
                || err.to_string().contains("no stdout/stderr activity")
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_streams_output_before_exit() {
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");
        let stderr_path = dir.path().join("stderr.log");
        let thread_stdout = stdout_path.clone();
        let thread_stderr = stderr_path.clone();
        let handle = thread::spawn(move || {
            run_command_with_timeout_logged(
                "sh",
                &["-c", "printf 'first\\n'; sleep 1; printf 'second\\n'"],
                None,
                Duration::from_secs(3),
                Duration::from_secs(3),
                Some(thread_stdout),
                Some(thread_stderr),
            )
            .unwrap()
        });

        thread::sleep(Duration::from_millis(250));
        let live = std::fs::read_to_string(&stdout_path).unwrap_or_default();
        assert!(live.contains("first"));
        assert!(!live.contains("second"));

        let output = handle.join().unwrap();
        assert!(output.status.success());
        let complete = std::fs::read_to_string(stdout_path).unwrap();
        assert!(complete.contains("second"));
    }

    #[cfg(unix)]
    #[test]
    fn command_activity_extends_idle_deadline() {
        let output = run_command_with_timeout_logged(
            "sh",
            &[
                "-c",
                "printf 'one\\n'; sleep 0.2; printf 'two\\n'; sleep 0.2; printf 'three\\n'",
            ],
            None,
            Duration::from_secs(2),
            Duration::from_millis(300),
            None,
            None,
        )
        .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("three"));
    }

    #[cfg(unix)]
    #[test]
    fn command_idle_timeout_is_distinct() {
        let err = run_command_with_timeout_logged(
            "sh",
            &["-c", "printf 'started\\n'; sleep 5"],
            None,
            Duration::from_secs(3),
            Duration::from_millis(250),
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no stdout/stderr activity"));
    }
}
