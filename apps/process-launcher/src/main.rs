use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Deserialize)]
struct LaunchRequest {
    executable: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: HashMap<String, String>,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

#[derive(Serialize)]
struct ProcessIdentity {
    pid: u32,
    pgid: i32,
    start_token: String,
    executable_identity: String,
    status: &'static str,
}

fn fail(message: impl AsRef<str>) -> ! {
    let _ = writeln!(std::io::stderr(), "{}", message.as_ref());
    std::process::exit(125);
}

fn process_start_token(pid: u32) -> String {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .unwrap_or_else(|error| fail(error.to_string()));
    if !output.status.success() {
        fail("could not obtain process start token");
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        fail("empty process start token");
    }
    format!("{pid}:{value}")
}

fn main() {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let request_line = lines
        .next()
        .transpose()
        .unwrap_or_else(|error| fail(error.to_string()))
        .unwrap_or_else(|| fail("missing launch request"));
    let request: LaunchRequest =
        serde_json::from_str(&request_line).unwrap_or_else(|error| fail(error.to_string()));
    if !request.executable.is_absolute() || !request.executable.is_file() {
        fail("executable must be an existing absolute file");
    }
    if !request.cwd.is_dir() {
        fail("cwd must be an existing directory");
    }
    if unsafe { libc::setpgid(0, 0) } != 0 {
        fail(std::io::Error::last_os_error().to_string());
    }
    let pid = std::process::id();
    let mut executable_file =
        std::fs::File::open(&request.executable).unwrap_or_else(|error| fail(error.to_string()));
    let mut hasher = Sha256::new();
    hasher.update(request.executable.to_string_lossy().as_bytes());
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut executable_file, &mut buffer)
            .unwrap_or_else(|error| fail(error.to_string()));
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let executable_identity = format!("sha256:{}", hex::encode(hasher.finalize()));
    let start_token = process_start_token(pid);
    let identity = ProcessIdentity {
        pid,
        pgid: pid as i32,
        start_token,
        executable_identity,
        status: "waiting_for_go",
    };
    println!(
        "{}",
        serde_json::to_string(&identity).unwrap_or_else(|error| fail(error.to_string()))
    );
    std::io::stdout()
        .flush()
        .unwrap_or_else(|error| fail(error.to_string()));
    let go = lines
        .next()
        .transpose()
        .unwrap_or_else(|error| fail(error.to_string()))
        .unwrap_or_default();
    if go.trim() != "GO" {
        fail("launch cancelled before GO");
    }
    let stdout =
        std::fs::File::create(&request.stdout_path).unwrap_or_else(|error| fail(error.to_string()));
    let stderr =
        std::fs::File::create(&request.stderr_path).unwrap_or_else(|error| fail(error.to_string()));
    let mut command = Command::new(&request.executable);
    command
        .args(&request.args)
        .current_dir(&request.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .env_clear();
    for (key, value) in request.env {
        command.env(key, value);
    }
    let error = command.exec();
    fail(format!("exec failed: {error}"));
}
