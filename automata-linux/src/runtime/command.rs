use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::{Result, UiError};

const ETXTBSY_ERRNO: i32 = 26;
const TEXT_FILE_BUSY_RETRIES: u32 = 5;
const TEXT_FILE_BUSY_BACKOFF_MS: u64 = 5;

fn is_text_file_busy(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(ETXTBSY_ERRNO)
}

/// Spawn `cmd` retrying transient `ETXTBSY` races (e.g. immediately after an atomic binary replace).
pub fn spawn_retrying_busy(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    let mut attempt = 0u32;
    loop {
        match cmd.spawn() {
            Err(err) if is_text_file_busy(&err) && attempt < TEXT_FILE_BUSY_RETRIES => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(
                    TEXT_FILE_BUSY_BACKOFF_MS * u64::from(attempt),
                ));
            }
            res => return res,
        }
    }
}

/// Launch a detached GUI/background child with null stdio, its own Unix process group
/// (`setpgid(0, 0)`), and a dedicated reaper thread so exited children never accumulate
/// as `<defunct>` zombies inside long-lived MCP processes.
pub fn spawn_detached(mut cmd: Command, desc: &str) -> Result<u32> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = spawn_retrying_busy(&mut cmd)
        .map_err(|e| UiError::Platform(format!("failed to launch {desc}: {e}")))?;
    let pid = child.id();
    if let Err(e) = std::thread::Builder::new()
        .name(format!("reap-{pid}"))
        .spawn(move || {
            let _ = child.wait();
        })
    {
        log::warn!("failed to spawn reaper thread for launched pid={pid}: {e}");
    }
    Ok(pid)
}

#[derive(Debug, Clone)]
pub struct BoundedCommandOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

fn read_capped<R: std::io::Read + Send + 'static>(
    mut reader: R,
    max_bytes: usize,
) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        let mut truncated = false;
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let remaining = max_bytes.saturating_sub(out.len());
            if remaining > 0 {
                let take = remaining.min(n);
                out.extend_from_slice(&buf[..take]);
            }
            if n > remaining {
                truncated = true;
            }
        }
        (out, truncated)
    })
}

/// Run `cmd` synchronously with strict `timeout` and capped stdout/stderr capture (`max_bytes`).
/// Kills and reaps the child if the deadline expires.
pub fn run_bounded(
    mut cmd: Command,
    desc: &str,
    timeout: Duration,
    max_bytes: usize,
) -> Result<BoundedCommandOutput> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_retrying_busy(&mut cmd)
        .map_err(|e| UiError::Platform(format!("failed to run {desc}: {e}")))?;

    let stdout_handle = child.stdout.take().map(|r| read_capped(r, max_bytes));
    let stderr_handle = child.stderr.take().map(|r| read_capped(r, max_bytes));

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(h) = stdout_handle {
                let _ = h.join();
            }
            if let Some(h) = stderr_handle {
                let _ = h.join();
            }
            return Err(UiError::Platform(format!("{desc} timed out after {timeout:?}")).into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    let (stdout, out_trunc) = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let (stderr, err_trunc) = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    Ok(BoundedCommandOutput {
        status,
        stdout,
        stderr,
        truncated: out_trunc || err_trunc,
    })
}

#[cfg(test)]
mod tests {
    use super::{run_bounded, spawn_detached};
    use std::process::Command;
    use std::time::Duration;

    #[test]
    fn spawn_detached_reaps_child_process() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 0"]);
        let pid = spawn_detached(cmd, "detached test").unwrap();
        let proc_dir = std::path::PathBuf::from(format!("/proc/{pid}"));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while proc_dir.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!proc_dir.exists(), "child pid={pid} stayed in /proc");
    }

    #[test]
    fn run_bounded_caps_output_and_enforces_timeout() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf 'abcdefghij'"]);
        let out = run_bounded(cmd, "cap test", Duration::from_secs(2), 4).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"abcd");
        assert!(out.truncated);

        let mut slow = Command::new("sh");
        slow.args(["-c", "sleep 5"]);
        let err = run_bounded(slow, "timeout test", Duration::from_millis(80), 1024).unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }
}
