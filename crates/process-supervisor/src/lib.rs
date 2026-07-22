//! Process supervisor: argv execution, process groups, log files, cancel/timeout.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("task already finished")]
    AlreadyFinished,
    #[error("{0}")]
    Other(String),
}

#[cfg(unix)]
fn resolve_launcher_path() -> Result<Option<PathBuf>> {
    if let Some(value) = std::env::var_os("TESTHARBOR_PROCESS_LAUNCHER") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(Some(path));
        }
        return Err(ProcessError::Other(format!(
            "configured testharbor-process-launcher does not exist: {}",
            path.display()
        )));
    }
    let current = std::env::current_exe()?;
    let Some(parent) = current.parent() else {
        return Ok(None);
    };
    let sibling = parent.join("testharbor-process-launcher");
    if sibling.is_file() {
        return Ok(Some(sibling));
    }
    // Cargo test binaries live in target/{profile}/deps while a separately
    // built launcher lives one directory above.
    if parent.file_name().is_some_and(|name| name == "deps") {
        if let Some(profile_dir) = parent.parent() {
            let candidate = profile_dir.join("testharbor-process-launcher");
            if candidate.is_file() {
                return Ok(Some(candidate));
            }
        }
    }
    Ok(None)
}

pub type Result<T> = std::result::Result<T, ProcessError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Non-secret env overrides (key -> value).
    pub env: HashMap<String, String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase {
    Starting,
    Running,
    Exited,
    TimedOut,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessOutcome {
    pub phase: TaskPhase,
    pub exit_code: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    pub pid: Option<u32>,
    pub pgid: Option<i32>,
    #[serde(default)]
    pub start_token: Option<String>,
    #[serde(default)]
    pub executable_identity: Option<String>,
}

struct LiveTask {
    child: Option<Child>,
    pgid: Option<i32>,
    outcome: ProcessOutcome,
    timeout: Option<Duration>,
    started_instant: Instant,
}

#[derive(Serialize)]
struct LauncherRequest<'a> {
    executable: &'a str,
    args: &'a [String],
    cwd: &'a Path,
    env: HashMap<String, String>,
    stdout_path: &'a Path,
    stderr_path: &'a Path,
}

#[derive(Deserialize)]
struct LauncherIdentity {
    pid: u32,
    pgid: i32,
    start_token: String,
    executable_identity: String,
}

/// In-memory supervisor (one per app process).
pub struct ProcessSupervisor {
    inner: Mutex<HashMap<String, LiveTask>>,
}

impl Default for ProcessSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSupervisor {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn has_task(&self, task_id: &str) -> bool {
        self.inner.lock().unwrap().contains_key(task_id)
    }

    /// Spawn command; returns task id and initial outcome (Running or Failed).
    pub fn start(
        &self,
        task_id: &str,
        spec: &CommandSpec,
        log_dir: &Path,
    ) -> Result<ProcessOutcome> {
        self.start_with_identity_callback(task_id, spec, log_dir, |_| Ok(()))
    }

    /// Start through the Unix launcher handshake when available. The callback
    /// runs after PID/PGID/start-token discovery and before the launcher
    /// receives GO, allowing callers to durably persist ProcessIdentity first.
    pub fn start_with_identity_callback<F>(
        &self,
        task_id: &str,
        spec: &CommandSpec,
        log_dir: &Path,
        persist_identity: F,
    ) -> Result<ProcessOutcome>
    where
        F: FnOnce(&ProcessOutcome) -> Result<()>,
    {
        #[cfg(unix)]
        if let Some(launcher) = resolve_launcher_path()? {
            return self.start_via_launcher(task_id, spec, log_dir, &launcher, persist_identity);
        }

        // Development/test fallback. Packaged desktop/CLI set
        // TESTHARBOR_PROCESS_LAUNCHER_REQUIRED and fail closed if the sidecar is
        // absent.
        if std::env::var_os("TESTHARBOR_PROCESS_LAUNCHER_REQUIRED").is_some() {
            return Err(ProcessError::Other(
                "testharbor-process-launcher is required but was not found".into(),
            ));
        }
        let outcome = self.start_direct(task_id, spec, log_dir)?;
        persist_identity(&outcome)?;
        Ok(outcome)
    }

    fn start_direct(
        &self,
        task_id: &str,
        spec: &CommandSpec,
        log_dir: &Path,
    ) -> Result<ProcessOutcome> {
        let executable = Path::new(&spec.executable);
        if !executable.is_absolute() || !executable.is_file() {
            return Err(ProcessError::Other(format!(
                "executable must be an existing absolute path: {}",
                spec.executable
            )));
        }
        {
            let tasks = self.inner.lock().unwrap();
            if tasks.contains_key(task_id) {
                return Err(ProcessError::Other(format!("duplicate task id: {task_id}")));
            }
            let active = tasks
                .values()
                .filter(|task| {
                    matches!(task.outcome.phase, TaskPhase::Starting | TaskPhase::Running)
                })
                .count();
            if active >= 2 {
                return Err(ProcessError::Other(
                    "global process concurrency limit (2) reached".into(),
                ));
            }
        }
        std::fs::create_dir_all(log_dir)?;
        let stdout_path = log_dir.join("stdout.partial");
        let stderr_path = log_dir.join("stderr.partial");
        let stdout_file = File::create(&stdout_path)?;
        let stderr_file = File::create(&stderr_path)?;

        #[cfg(unix)]
        let mut cmd = {
            let mut command = Command::new(&spec.executable);
            command.args(&spec.args);
            command
        };
        #[cfg(not(unix))]
        #[cfg(not(windows))]
        let mut cmd = {
            let mut command = Command::new(&spec.executable);
            command.args(&spec.args);
            command
        };
        #[cfg(windows)]
        let mut cmd = {
            let script_path = log_dir.join("run.ps1");
            std::fs::write(
                &script_path,
                "$args = ConvertFrom-Json $env:TESTHARBOR_ARGS_JSON; & $env:TESTHARBOR_EXECUTABLE @args; $code = $LASTEXITCODE; Set-Content -NoNewline -Path $env:TESTHARBOR_EXIT_STATUS -Value $code; exit $code",
            )?;
            let mut command = Command::new("powershell.exe");
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                script_path.to_string_lossy().as_ref(),
            ]);
            command
        };

        cmd.current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .env_clear();

        // Minimal environment for reliability.
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }
        if let Ok(home) = std::env::var("HOME") {
            cmd.env("HOME", home);
        }
        if let Ok(tmp) = std::env::var("TMPDIR") {
            cmd.env("TMPDIR", tmp);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            let exit_status_path = log_dir.join("exit.status");
            let args_json = serde_json::to_string(&spec.args)
                .map_err(|error| ProcessError::Other(error.to_string()))?;
            cmd.env("TESTHARBOR_EXIT_STATUS", &exit_status_path)
                .env("TESTHARBOR_EXECUTABLE", &spec.executable)
                .env("TESTHARBOR_ARGS_JSON", args_json);
        }

        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                // New process group so we can kill the whole tree.
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let started_at = Utc::now();
        let started_instant = Instant::now();
        let executable_identity = executable_identity(&spec.executable);

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let outcome = ProcessOutcome {
                    phase: TaskPhase::Failed,
                    exit_code: None,
                    started_at,
                    finished_at: Some(Utc::now()),
                    stdout_path,
                    stderr_path,
                    pid: None,
                    pgid: None,
                    start_token: None,
                    executable_identity: executable_identity.clone(),
                };
                // Write spawn error to stderr file for UI.
                let _ = std::fs::write(&outcome.stderr_path, format!("spawn failed: {e}\n"));
                let mut map = self.inner.lock().unwrap();
                map.insert(
                    task_id.to_string(),
                    LiveTask {
                        child: None,
                        pgid: None,
                        outcome: outcome.clone(),
                        timeout: None,
                        started_instant,
                    },
                );
                return Ok(outcome);
            }
        };

        let pid = child.id();
        #[cfg(unix)]
        let pgid = Some(pid as i32);
        #[cfg(not(unix))]
        let pgid = None;

        let outcome = ProcessOutcome {
            phase: TaskPhase::Running,
            exit_code: None,
            started_at,
            finished_at: None,
            stdout_path,
            stderr_path,
            pid: Some(pid),
            pgid,
            start_token: process_start_token(pid).or_else(|| {
                Some(format!(
                    "unverified:{pid}:{}",
                    started_at.timestamp_nanos_opt().unwrap_or_default()
                ))
            }),
            executable_identity,
        };

        let mut map = self.inner.lock().unwrap();
        map.insert(
            task_id.to_string(),
            LiveTask {
                child: Some(child),
                pgid,
                outcome: outcome.clone(),
                timeout: spec.timeout_ms.map(Duration::from_millis),
                started_instant,
            },
        );
        Ok(outcome)
    }

    #[cfg(unix)]
    fn start_via_launcher<F>(
        &self,
        task_id: &str,
        spec: &CommandSpec,
        log_dir: &Path,
        launcher: &Path,
        persist_identity: F,
    ) -> Result<ProcessOutcome>
    where
        F: FnOnce(&ProcessOutcome) -> Result<()>,
    {
        let executable = Path::new(&spec.executable);
        if !executable.is_absolute() || !executable.is_file() || !spec.cwd.is_dir() {
            return Err(ProcessError::Other(
                "launcher requires existing absolute executable and cwd".into(),
            ));
        }
        {
            let tasks = self.inner.lock().unwrap();
            if tasks.contains_key(task_id) {
                return Err(ProcessError::Other(format!("duplicate task id: {task_id}")));
            }
            let active = tasks
                .values()
                .filter(|task| {
                    matches!(task.outcome.phase, TaskPhase::Starting | TaskPhase::Running)
                })
                .count();
            if active >= 2 {
                return Err(ProcessError::Other(
                    "global process concurrency limit (2) reached".into(),
                ));
            }
        }
        std::fs::create_dir_all(log_dir)?;
        let stdout_path = log_dir.join("stdout.partial");
        let stderr_path = log_dir.join("stderr.partial");
        let mut environment = HashMap::new();
        for key in ["PATH", "HOME", "TMPDIR"] {
            if let Ok(value) = std::env::var(key) {
                environment.insert(key.to_string(), value);
            }
        }
        environment.extend(spec.env.clone());
        let request = LauncherRequest {
            executable: &spec.executable,
            args: &spec.args,
            cwd: &spec.cwd,
            env: environment,
            stdout_path: &stdout_path,
            stderr_path: &stderr_path,
        };
        let mut child = Command::new(launcher)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProcessError::Other("launcher stdin unavailable".into()))?;
        serde_json::to_writer(&mut stdin, &request)
            .map_err(|error| ProcessError::Other(error.to_string()))?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProcessError::Other("launcher stdout unavailable".into()))?;
        let mut reader = BufReader::new(stdout);
        let mut identity_line = String::new();
        reader.read_line(&mut identity_line)?;
        let identity: LauncherIdentity = serde_json::from_str(identity_line.trim())
            .map_err(|error| ProcessError::Other(format!("invalid launcher identity: {error}")))?;
        if identity.pid != child.id() || identity.pgid != identity.pid as i32 {
            let _ = child.kill();
            return Err(ProcessError::Other(
                "launcher returned inconsistent process identity".into(),
            ));
        }
        let started_at = Utc::now();
        let outcome = ProcessOutcome {
            phase: TaskPhase::Starting,
            exit_code: None,
            started_at,
            finished_at: None,
            stdout_path,
            stderr_path,
            pid: Some(identity.pid),
            pgid: Some(identity.pgid),
            start_token: Some(identity.start_token),
            executable_identity: Some(identity.executable_identity),
        };
        if let Err(error) = persist_identity(&outcome) {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        stdin.write_all(b"GO\n")?;
        stdin.flush()?;
        drop(stdin);
        let mut running = outcome;
        running.phase = TaskPhase::Running;
        self.inner.lock().unwrap().insert(
            task_id.to_string(),
            LiveTask {
                child: Some(child),
                pgid: running.pgid,
                outcome: running.clone(),
                timeout: spec.timeout_ms.map(Duration::from_millis),
                started_instant: Instant::now(),
            },
        );
        Ok(running)
    }

    /// Reconnect to a process started by another TestHarbor process.
    ///
    /// The child handle cannot be transferred between processes, so the
    /// attached task uses its persisted PID/PGID and log paths. Completion
    /// without a wait handle intentionally keeps exit_code unknown; callers
    /// must not treat that as a successful command.
    pub fn attach(&self, task_id: &str, outcome: ProcessOutcome) -> Result<()> {
        let pid = outcome.pid.ok_or_else(|| {
            ProcessError::Other("cannot attach execution without a process id".into())
        })?;
        let expected_start = outcome.start_token.as_deref().ok_or_else(|| {
            ProcessError::Other("cannot attach execution without a start token".into())
        })?;
        if expected_start.starts_with("unverified:")
            || process_start_token(pid).as_deref() != Some(expected_start)
            || outcome.executable_identity.as_deref() != process_executable_identity(pid).as_deref()
        {
            return Err(ProcessError::Other(
                "process identity mismatch; refusing to attach or signal possible reused PID"
                    .into(),
            ));
        }
        let elapsed = Utc::now()
            .signed_duration_since(outcome.started_at)
            .to_std()
            .unwrap_or_default();
        let started_instant = Instant::now()
            .checked_sub(elapsed)
            .unwrap_or_else(Instant::now);
        self.inner.lock().unwrap().insert(
            task_id.to_string(),
            LiveTask {
                child: None,
                pgid: outcome.pgid,
                outcome,
                timeout: None,
                started_instant,
            },
        );
        Ok(())
    }

    /// Poll task; applies timeout kill if needed.
    pub fn poll(&self, task_id: &str) -> Result<ProcessOutcome> {
        let mut map = self.inner.lock().unwrap();
        let task = map
            .get_mut(task_id)
            .ok_or_else(|| ProcessError::NotFound(task_id.to_string()))?;

        if matches!(
            task.outcome.phase,
            TaskPhase::Exited | TaskPhase::TimedOut | TaskPhase::Cancelled | TaskPhase::Failed
        ) {
            return Ok(task.outcome.clone());
        }

        // Timeout?
        if let Some(limit) = task.timeout {
            if task.started_instant.elapsed() >= limit {
                Self::kill_tree(task.pgid, task.child.as_mut())?;
                if let Some(mut child) = task.child.take() {
                    let _ = child.wait();
                }
                task.outcome.phase = TaskPhase::TimedOut;
                task.outcome.finished_at = Some(Utc::now());
                finalize_logs(&task.outcome);
                return Ok(task.outcome.clone());
            }
        }

        if let Some(child) = task.child.as_mut() {
            match child.try_wait()? {
                Some(status) => {
                    task.outcome.phase = TaskPhase::Exited;
                    task.outcome.exit_code = status.code();
                    task.outcome.finished_at = Some(Utc::now());
                    task.child = None;
                    finalize_logs(&task.outcome);
                }
                None => {
                    task.outcome.phase = TaskPhase::Running;
                }
            }
        } else if let Some(pid) = task.outcome.pid {
            if !is_process_alive(pid) {
                task.outcome.phase = TaskPhase::Exited;
                task.outcome.exit_code = None;
                task.outcome.finished_at = Some(Utc::now());
                // There is no wait handle in an attached process, so keep
                // exit_code=None and let the application fail closed.
                finalize_logs(&task.outcome);
            }
        }

        Ok(task.outcome.clone())
    }

    /// Block until finished or timeout (internal polling).
    pub fn wait(&self, task_id: &str, poll_ms: u64) -> Result<ProcessOutcome> {
        loop {
            let o = self.poll(task_id)?;
            if !matches!(o.phase, TaskPhase::Running | TaskPhase::Starting) {
                return Ok(o);
            }
            thread::sleep(Duration::from_millis(poll_ms));
        }
    }

    pub fn cancel(&self, task_id: &str) -> Result<ProcessOutcome> {
        let mut map = self.inner.lock().unwrap();
        let task = map
            .get_mut(task_id)
            .ok_or_else(|| ProcessError::NotFound(task_id.to_string()))?;

        if matches!(
            task.outcome.phase,
            TaskPhase::Exited | TaskPhase::TimedOut | TaskPhase::Cancelled | TaskPhase::Failed
        ) {
            return Ok(task.outcome.clone());
        }

        Self::kill_tree(task.pgid, task.child.as_mut())?;
        if let Some(mut child) = task.child.take() {
            let _ = child.wait();
        }
        task.outcome.phase = TaskPhase::Cancelled;
        task.outcome.finished_at = Some(Utc::now());
        finalize_logs(&task.outcome);
        Ok(task.outcome.clone())
    }

    pub fn get(&self, task_id: &str) -> Result<ProcessOutcome> {
        self.poll(task_id)
    }

    #[cfg(unix)]
    fn kill_tree(pgid: Option<i32>, child: Option<&mut Child>) -> Result<()> {
        if let Some(pg) = pgid {
            // Negative pgid = process group
            unsafe {
                let _ = libc::kill(-pg, libc::SIGTERM);
            }
            thread::sleep(Duration::from_millis(200));
            unsafe {
                let _ = libc::kill(-pg, libc::SIGKILL);
            }
        } else if let Some(c) = child {
            let _ = c.kill();
        }
        Ok(())
    }

    #[cfg(windows)]
    fn kill_tree(_pgid: Option<i32>, child: Option<&mut Child>) -> Result<()> {
        if let Some(c) = child {
            let pid = c.id().to_string();
            let _ = Command::new("taskkill")
                .args(["/PID", &pid, "/T", "/F"])
                .status();
            let _ = c.kill();
        }
        Ok(())
    }

    #[cfg(all(not(unix), not(windows)))]
    fn kill_tree(_pgid: Option<i32>, child: Option<&mut Child>) -> Result<()> {
        if let Some(c) = child {
            let _ = c.kill();
        }
        Ok(())
    }
}

fn executable_identity(executable: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let path = std::fs::canonicalize(executable).ok()?;
    let mut file = File::open(&path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(format!("sha256:{}", hex::encode(hasher.finalize())))
}

#[cfg(unix)]
fn process_start_token(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then(|| format!("{pid}:{value}"))
}

#[cfg(unix)]
fn process_executable_identity(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty())
        .then(|| executable_identity(&path))
        .flatten()
}

#[cfg(unix)]
pub fn current_process_identity() -> Result<(u32, String, String)> {
    let pid = std::process::id();
    let start_token = process_start_token(pid)
        .ok_or_else(|| ProcessError::Other("cannot resolve current process start token".into()))?;
    let executable = std::env::current_exe()?;
    let executable_identity = executable_identity(executable.to_string_lossy().as_ref())
        .ok_or_else(|| ProcessError::Other("cannot resolve current executable identity".into()))?;
    Ok((pid, start_token, executable_identity))
}

#[cfg(unix)]
pub fn process_identity_matches(pid: u32, start_token: &str, executable_identity: &str) -> bool {
    process_start_token(pid).as_deref() == Some(start_token)
        && process_executable_identity(pid).as_deref() == Some(executable_identity)
}

#[cfg(not(unix))]
fn process_start_token(_pid: u32) -> Option<String> {
    None
}

#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}")])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn is_process_alive(_pid: u32) -> bool {
    false
}

fn finalize_logs(outcome: &ProcessOutcome) {
    // Rename .partial → final if present (best effort).
    for path in [&outcome.stdout_path, &outcome.stderr_path] {
        if let Some(s) = path.to_str() {
            if s.ends_with(".partial") {
                let final_path = PathBuf::from(s.trim_end_matches(".partial"));
                let _ = std::fs::rename(path, &final_path);
            }
        }
    }
}

/// Read up to max_bytes from a log file (supports .partial or finalized).
pub fn read_log_tail(path: &Path, max_bytes: u64) -> Result<String> {
    let candidates = [
        path.to_path_buf(),
        PathBuf::from(path.to_string_lossy().trim_end_matches(".partial")),
    ];
    for p in &candidates {
        if p.exists() {
            let mut f = File::open(p)?;
            let meta = f.metadata()?;
            let len = meta.len();
            if len > max_bytes {
                f.seek_relative_from_end((len - max_bytes) as i64)?;
            }
            let mut buf = String::new();
            f.read_to_string(&mut buf)?;
            return Ok(buf);
        }
    }
    Ok(String::new())
}

trait SeekRelative {
    fn seek_relative_from_end(&mut self, offset_from_start_skip: i64) -> std::io::Result<()>;
}

impl SeekRelative for File {
    fn seek_relative_from_end(&mut self, skip: i64) -> std::io::Result<()> {
        use std::io::Seek;
        self.seek(std::io::SeekFrom::Start(skip as u64))?;
        Ok(())
    }
}

pub fn new_task_id() -> String {
    Uuid::new_v4().to_string()
}

/// Shared supervisor handle.
pub type SharedSupervisor = Arc<ProcessSupervisor>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_echo_success() {
        let sup = ProcessSupervisor::new();
        let dir = tempfile::tempdir().unwrap();
        let spec = CommandSpec {
            executable: "/bin/echo".into(),
            args: vec!["hello-testharbor".into()],
            cwd: dir.path().to_path_buf(),
            env: HashMap::new(),
            timeout_ms: Some(5_000),
        };
        let id = new_task_id();
        let started = sup.start(&id, &spec, dir.path()).unwrap();
        assert!(matches!(
            started.phase,
            TaskPhase::Running | TaskPhase::Exited | TaskPhase::Failed
        ));
        let done = sup.wait(&id, 20).unwrap();
        assert_eq!(done.phase, TaskPhase::Exited);
        assert_eq!(done.exit_code, Some(0));
        let out = read_log_tail(&started.stdout_path, 10_000).unwrap();
        assert!(
            out.contains("hello-testharbor") || {
                // may have been renamed
                let p = PathBuf::from(
                    started
                        .stdout_path
                        .to_string_lossy()
                        .trim_end_matches(".partial"),
                );
                std::fs::read_to_string(p)
                    .unwrap_or_default()
                    .contains("hello-testharbor")
            }
        );
    }

    #[test]
    fn cancel_long_sleep() {
        let sup = ProcessSupervisor::new();
        let dir = tempfile::tempdir().unwrap();
        let spec = CommandSpec {
            executable: "/bin/sleep".into(),
            args: vec!["30".into()],
            cwd: dir.path().to_path_buf(),
            env: HashMap::new(),
            timeout_ms: Some(60_000),
        };
        let id = new_task_id();
        sup.start(&id, &spec, dir.path()).unwrap();
        thread::sleep(Duration::from_millis(100));
        let o = sup.cancel(&id).unwrap();
        assert_eq!(o.phase, TaskPhase::Cancelled);
    }

    #[test]
    fn attach_can_poll_and_cancel_a_task_started_by_another_supervisor() {
        let owner = ProcessSupervisor::new();
        let attached = ProcessSupervisor::new();
        let dir = tempfile::tempdir().unwrap();
        let spec = CommandSpec {
            executable: "/bin/sleep".into(),
            args: vec!["30".into()],
            cwd: dir.path().to_path_buf(),
            env: HashMap::new(),
            timeout_ms: Some(60_000),
        };
        let id = new_task_id();
        let started = owner.start(&id, &spec, dir.path()).unwrap();
        attached.attach(&id, started).unwrap();

        assert!(matches!(
            attached.poll(&id).unwrap().phase,
            TaskPhase::Running | TaskPhase::Starting
        ));
        assert_eq!(attached.cancel(&id).unwrap().phase, TaskPhase::Cancelled);
    }

    #[test]
    fn timeout_kills() {
        let sup = ProcessSupervisor::new();
        let dir = tempfile::tempdir().unwrap();
        let spec = CommandSpec {
            executable: "/bin/sleep".into(),
            args: vec!["30".into()],
            cwd: dir.path().to_path_buf(),
            env: HashMap::new(),
            timeout_ms: Some(200),
        };
        let id = new_task_id();
        sup.start(&id, &spec, dir.path()).unwrap();
        let done = sup.wait(&id, 50).unwrap();
        assert_eq!(done.phase, TaskPhase::TimedOut);
    }
}
