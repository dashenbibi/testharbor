//! Test drivers: generic exit-code (and later Go/JUnit).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use testharbor_domain::{ResultCompleteness, ResultConsistency};
use testharbor_process_supervisor::{
    CommandSpec, ProcessError, ProcessOutcome, ProcessSupervisor, TaskPhase,
};
use testharbor_trust::TestProfileDraft;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, DriverError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenericTestResult {
    pub task_id: String,
    pub outcome: ProcessOutcome,
    pub completeness: ResultCompleteness,
    pub consistency: ResultConsistency,
    /// UI must not call this "tests passed" — command-level only.
    pub command_succeeded: bool,
    pub summary: String,
}

pub fn profile_to_command_spec(profile: &TestProfileDraft) -> CommandSpec {
    CommandSpec {
        executable: profile.command.executable.clone(),
        args: profile.command.args.clone(),
        cwd: profile.command.cwd.clone().into(),
        env: HashMap::new(),
        timeout_ms: Some(profile.timeout_ms),
    }
}

/// Start a generic test command (does not wait).
pub fn start_generic(
    supervisor: &ProcessSupervisor,
    task_id: &str,
    profile: &TestProfileDraft,
    log_dir: &Path,
) -> Result<ProcessOutcome> {
    start_generic_with_env(supervisor, task_id, profile, log_dir, HashMap::new())
}

/// Start a generic test command with runtime-only environment variables.
pub fn start_generic_with_env(
    supervisor: &ProcessSupervisor,
    task_id: &str,
    profile: &TestProfileDraft,
    log_dir: &Path,
    env: HashMap<String, String>,
) -> Result<ProcessOutcome> {
    start_generic_with_env_and_identity(supervisor, task_id, profile, log_dir, env, |_| Ok(()))
}

pub fn start_generic_with_env_and_identity<F>(
    supervisor: &ProcessSupervisor,
    task_id: &str,
    profile: &TestProfileDraft,
    log_dir: &Path,
    env: HashMap<String, String>,
    persist_identity: F,
) -> Result<ProcessOutcome>
where
    F: FnOnce(&ProcessOutcome) -> std::result::Result<(), ProcessError>,
{
    // Go's build cache is content-addressed and contains no test data. Keep it
    // in Go's normal shared cache instead of duplicating hundreds of MB inside
    // every TestHarbor Run's evidence directory.
    let mut spec = profile_to_command_spec(profile);
    spec.env.extend(env);
    Ok(supervisor.start_with_identity_callback(task_id, &spec, log_dir, persist_identity)?)
}

/// Map process outcome → generic opaque result (INV-09).
pub fn normalize_generic(task_id: &str, outcome: ProcessOutcome) -> GenericTestResult {
    let (completeness, command_succeeded, summary) = match outcome.phase {
        TaskPhase::Exited => {
            let code = outcome.exit_code.unwrap_or(-1);
            if code == 0 {
                (
                    ResultCompleteness::Opaque,
                    true,
                    "命令成功退出 (exit 0)。Generic/exit-code 模式不能证明测试已收集或通过。"
                        .into(),
                )
            } else {
                (
                    ResultCompleteness::Opaque,
                    false,
                    format!("命令失败 (exit {code})。结果仅为命令级，非结构化测试通过。"),
                )
            }
        }
        TaskPhase::TimedOut => (ResultCompleteness::Partial, false, "命令超时终止。".into()),
        TaskPhase::Cancelled => (ResultCompleteness::Partial, false, "命令已取消。".into()),
        TaskPhase::Failed | TaskPhase::Starting | TaskPhase::Running => (
            ResultCompleteness::Missing,
            false,
            format!("进程状态: {:?}", outcome.phase),
        ),
    };

    GenericTestResult {
        task_id: task_id.to_string(),
        outcome,
        completeness,
        consistency: ResultConsistency::Consistent,
        command_succeeded,
        summary,
    }
}

pub fn wait_generic(supervisor: &ProcessSupervisor, task_id: &str) -> Result<GenericTestResult> {
    let outcome = supervisor.wait(task_id, 50)?;
    Ok(normalize_generic(task_id, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use testharbor_process_supervisor::new_task_id;
    use testharbor_trust::suggest_echo_profile;

    #[test]
    fn echo_is_opaque_success_not_structured_pass() {
        let sup = ProcessSupervisor::new();
        let dir = tempfile::tempdir().unwrap();
        let profile = suggest_echo_profile(dir.path());
        let id = new_task_id();
        start_generic(&sup, &id, &profile, dir.path()).unwrap();
        let r = wait_generic(&sup, &id).unwrap();
        assert!(r.command_succeeded);
        assert_eq!(r.completeness, ResultCompleteness::Opaque);
        assert!(r.summary.contains("不能证明") || r.summary.contains("命令成功"));
    }
}
