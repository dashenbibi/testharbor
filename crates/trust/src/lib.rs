//! TrustGrant: user-confirmed command/profile authorization.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvValueSource {
    FixedNonSecret,
    InheritAllowed,
    PromptSecret,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvPolicyEntry {
    pub key: String,
    pub source: EnvValueSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TcpReadiness {
    pub host: String,
    pub port: u16,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceProfileDraft {
    pub startup: CommandIdentity,
    pub timeout_ms: u64,
    pub readiness: TcpReadiness,
    #[serde(default)]
    pub required_env_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandIdentity {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub parser: String,
    /// Optional version string from probe.
    pub executable_version: Option<String>,
    #[serde(default)]
    pub executable_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestProfileDraft {
    pub name: String,
    #[serde(default)]
    pub format: Option<CommandIdentity>,
    #[serde(default)]
    pub collect: Option<CommandIdentity>,
    #[serde(default)]
    pub compile: Option<CommandIdentity>,
    /// Formal full-scope Test command. Kept as `command` for IPC compatibility.
    pub command: CommandIdentity,
    pub timeout_ms: u64,
    #[serde(default)]
    pub service: Option<ServiceProfileDraft>,
    #[serde(default)]
    pub env_policy: Vec<EnvPolicyEntry>,
    #[serde(default)]
    pub network_unrestricted: bool,
}

impl TestProfileDraft {
    pub fn commands(&self) -> impl Iterator<Item = &CommandIdentity> {
        self.format
            .iter()
            .chain(self.collect.iter())
            .chain(self.compile.iter())
            .chain(std::iter::once(&self.command))
            .chain(self.service.iter().map(|service| &service.startup))
    }

    pub fn all_command_snapshots_hash(&self) -> String {
        let hashes = self
            .commands()
            .map(command_snapshot_hash)
            .collect::<Vec<_>>();
        let refs = hashes.iter().map(String::as_str).collect::<Vec<_>>();
        canonical_hash(&refs)
    }

    pub fn validate(&self, repository_root: &Path) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("profile name is required".into());
        }
        let root = repository_root
            .canonicalize()
            .map_err(|error| format!("invalid repository root: {error}"))?;
        for command in self.commands() {
            let executable = Path::new(&command.executable);
            if !executable.is_absolute() || !executable.is_file() {
                return Err(format!(
                    "profile executable must be an existing absolute path: {}",
                    command.executable
                ));
            }
            let cwd = Path::new(&command.cwd)
                .canonicalize()
                .map_err(|error| format!("invalid command cwd {}: {error}", command.cwd))?;
            if !cwd.starts_with(&root) {
                return Err(format!("command cwd escapes repository: {}", command.cwd));
            }
            if command.parser.trim().is_empty() {
                return Err("command parser is required".into());
            }
        }
        if self.timeout_ms == 0 {
            return Err("test timeout must be greater than zero".into());
        }
        if let Some(service) = &self.service {
            if service.timeout_ms == 0 || service.readiness.timeout_ms == 0 {
                return Err("service and readiness timeouts must be greater than zero".into());
            }
            if service.readiness.host.trim().is_empty() {
                return Err("service readiness host is required".into());
            }
            for key in &service.required_env_keys {
                if !self.env_policy.iter().any(|entry| entry.key == *key) {
                    return Err(format!(
                        "service required env key is absent from env policy: {key}"
                    ));
                }
            }
        }
        let mut keys = std::collections::BTreeSet::new();
        for entry in &self.env_policy {
            if entry.key.trim().is_empty() || !keys.insert(entry.key.as_str()) {
                return Err("environment policy keys must be non-empty and unique".into());
            }
        }
        Ok(())
    }

    pub fn normalize(mut self) -> Result<Self, String> {
        for command in self
            .format
            .iter_mut()
            .chain(self.collect.iter_mut())
            .chain(self.compile.iter_mut())
            .chain(std::iter::once(&mut self.command))
            .chain(self.service.iter_mut().map(|service| &mut service.startup))
        {
            command.executable = std::fs::canonicalize(&command.executable)
                .map_err(|error| format!("invalid executable {}: {error}", command.executable))?
                .display()
                .to_string();
            command.executable_identity =
                Some(executable_file_identity(Path::new(&command.executable))?);
            command.cwd = std::fs::canonicalize(&command.cwd)
                .map_err(|error| format!("invalid cwd {}: {error}", command.cwd))?
                .display()
                .to_string();
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustGrant {
    pub id: String,
    pub project_id: String,
    pub trust_scope_hash: String,
    pub profile: TestProfileDraft,
    pub created_at: DateTime<Utc>,
}

pub fn command_snapshot_hash(cmd: &CommandIdentity) -> String {
    let mut h = Sha256::new();
    h.update(cmd.executable.as_bytes());
    h.update([0xff]);
    for a in &cmd.args {
        h.update(a.as_bytes());
        h.update([0xff]);
    }
    h.update(cmd.cwd.as_bytes());
    h.update([0xff]);
    h.update(cmd.parser.as_bytes());
    if let Some(v) = &cmd.executable_version {
        h.update(v.as_bytes());
    }
    if let Some(identity) = &cmd.executable_identity {
        h.update([0xfe]);
        h.update(identity.as_bytes());
    }
    hex::encode(h.finalize())
}

pub fn executable_file_identity(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("invalid executable {}: {error}", path.display()))?;
    let mut file = std::fs::File::open(&canonical)
        .map_err(|error| format!("cannot read executable {}: {error}", canonical.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash executable {}: {error}", canonical.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

pub fn trust_scope_hash(
    repository_identity: &str,
    cmd: &CommandIdentity,
    network_unrestricted: bool,
) -> String {
    let mut h = Sha256::new();
    h.update(repository_identity.as_bytes());
    h.update([0xff]);
    h.update(command_snapshot_hash(cmd).as_bytes());
    h.update([0xff]);
    h.update([network_unrestricted as u8]);
    hex::encode(h.finalize())
}

pub fn profile_trust_scope_hash(repository_identity: &str, profile: &TestProfileDraft) -> String {
    // Bind the complete structured profile, including stage/service timeouts,
    // TCP readiness, required env keys and parser identities. No executable
    // authorization survives a semantic Profile change.
    let profile_json = serde_json::to_string(profile).unwrap_or_default();
    canonical_hash(&[repository_identity, &profile_json])
}

fn canonical_hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
        h.update([0xff]);
    }
    hex::encode(h.finalize())
}

/// Resolve executable via `which`-like search on PATH.
pub fn resolve_executable(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() && p.is_file() {
        return Some(p.to_path_buf());
    }

    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // GUI apps on macOS/Linux often inherit a minimal PATH instead of the
    // user's terminal PATH. Ask the login shell for the same command lookup
    // without requiring users to paste absolute tool paths into TestHarbor.
    #[cfg(unix)]
    {
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
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
                return Some(PathBuf::from(path));
            }
        }
    }

    #[cfg(windows)]
    {
        if let Ok(output) = Command::new("where.exe").arg(name).output() {
            if let Some(path) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .find(|line| Path::new(line).is_file())
            {
                return Some(PathBuf::from(path));
            }
        }
    }

    None
}

pub fn probe_version(executable: &Path) -> Option<String> {
    let out = std::process::Command::new(executable)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        // try -version
        let out2 = std::process::Command::new(executable)
            .arg("-version")
            .output()
            .ok()?;
        if !out2.status.success() {
            return None;
        }
        return Some(
            String::from_utf8_lossy(&out2.stdout)
                .trim()
                .lines()
                .next()?
                .to_string(),
        );
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .lines()
            .next()?
            .to_string(),
    )
}

/// Build a default Go profile draft if `go` is available.
pub fn suggest_go_profile(repo_root: &Path) -> Option<TestProfileDraft> {
    let repo_root = repo_root.canonicalize().ok()?;
    let go = resolve_executable("go")?;
    let ver = probe_version(&go);
    Some(TestProfileDraft {
        name: "go-test-json".into(),
        format: None,
        collect: Some(CommandIdentity {
            executable: go.display().to_string(),
            args: vec!["list".into(), "./...".into()],
            cwd: repo_root.display().to_string(),
            parser: "go-list".into(),
            executable_version: ver.clone(),
            executable_identity: None,
        }),
        compile: Some(CommandIdentity {
            executable: go.display().to_string(),
            args: vec!["test".into(), "-run=^$".into(), "./...".into()],
            cwd: repo_root.display().to_string(),
            parser: "exit-code".into(),
            executable_version: ver.clone(),
            executable_identity: None,
        }),
        command: CommandIdentity {
            executable: go.display().to_string(),
            args: vec![
                "test".into(),
                "-json".into(),
                "-count=1".into(),
                "./...".into(),
            ],
            cwd: repo_root.display().to_string(),
            parser: "go-test-json".into(),
            executable_version: ver,
            executable_identity: None,
        },
        timeout_ms: 600_000,
        service: None,
        env_policy: vec![],
        network_unrestricted: false,
    })
}

/// Generic `true`/`echo` profile for smoke tests when go missing.
pub fn suggest_echo_profile(repo_root: &Path) -> TestProfileDraft {
    let repo_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let echo = resolve_executable("echo").unwrap_or_else(|| PathBuf::from("echo"));
    TestProfileDraft {
        name: "echo-smoke".into(),
        format: None,
        collect: None,
        compile: None,
        command: CommandIdentity {
            executable: echo.display().to_string(),
            args: vec!["testharbor-ok".into()],
            cwd: repo_root.display().to_string(),
            parser: "exit-code".into(),
            executable_version: None,
            executable_identity: None,
        },
        timeout_ms: 10_000,
        service: None,
        env_policy: vec![],
        network_unrestricted: false,
    }
}

pub fn create_grant(
    project_id: &str,
    repository_identity: &str,
    profile: TestProfileDraft,
) -> TrustGrant {
    let scope = profile_trust_scope_hash(repository_identity, &profile);
    TrustGrant {
        id: uuid_v4(),
        project_id: project_id.to_string(),
        trust_scope_hash: scope,
        profile,
        created_at: Utc::now(),
    }
}

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Avoid uuid dep in trust if not needed — use domain style via simple random
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("tg-{t:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_with_args() {
        let a = CommandIdentity {
            executable: "go".into(),
            args: vec!["test".into()],
            cwd: "/r".into(),
            parser: "exit-code".into(),
            executable_version: None,
            executable_identity: None,
        };
        let mut b = a.clone();
        b.args.push("./...".into());
        assert_ne!(command_snapshot_hash(&a), command_snapshot_hash(&b));
    }

    #[test]
    fn profile_scope_binds_timeout_and_service_readiness() {
        let mut profile = suggest_echo_profile(Path::new("/tmp"));
        let original = profile_trust_scope_hash("repo", &profile);
        profile.timeout_ms += 1;
        assert_ne!(original, profile_trust_scope_hash("repo", &profile));

        profile.service = Some(ServiceProfileDraft {
            startup: profile.command.clone(),
            timeout_ms: 1_000,
            readiness: TcpReadiness {
                host: "127.0.0.1".into(),
                port: 8080,
                timeout_ms: 500,
            },
            required_env_keys: vec![],
        });
        let service_scope = profile_trust_scope_hash("repo", &profile);
        profile.service.as_mut().unwrap().readiness.port = 8081;
        assert_ne!(service_scope, profile_trust_scope_hash("repo", &profile));
    }
}
