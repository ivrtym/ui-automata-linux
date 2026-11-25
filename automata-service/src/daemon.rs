//! Local per-user (`0700` dir / `0600` socket) Unix Domain Socket daemon
//! allowing repeated `automata` CLI invocations and scripts to share one
//! persistent [`AutomationService`] session, `ShadowDom`, and `Recorder`.

use std::fs::{self, DirBuilder};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::protocol::{Command, CommandResult};
use crate::{AutomationService, ServiceError};

#[derive(Debug, Serialize, Deserialize)]
struct UdsRequest {
    id: u64,
    cmd: Command,
}

#[derive(Debug, Serialize, Deserialize)]
struct UdsResponse {
    id: u64,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<CommandResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ServiceError>,
}

/// Default per-user Unix socket path (`$XDG_RUNTIME_DIR/automata/service.sock`).
pub fn default_socket_path() -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        let t = runtime.trim();
        if !t.is_empty() {
            return PathBuf::from(t).join("automata").join("service.sock");
        }
    }
    let uid = current_uid();
    std::env::temp_dir()
        .join(format!("automata-{uid}"))
        .join("service.sock")
}

pub fn pid_path_for_socket(sock: &Path) -> PathBuf {
    sock.with_extension("pid")
}

/// Default per-user systemd unit path (`~/.config/systemd/user/automata-service.service`).
pub fn default_systemd_unit_path() -> PathBuf {
    if let Ok(cfg) = std::env::var("XDG_CONFIG_HOME") {
        let t = cfg.trim();
        if !t.is_empty() {
            return PathBuf::from(t)
                .join("systemd")
                .join("user")
                .join("automata-service.service");
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config")
        .join("systemd")
        .join("user")
        .join("automata-service.service")
}

/// Generates a clean `systemd --user` unit file for running `automata daemon` persistently
/// outside transient terminal or command process groups.
pub fn render_systemd_user_unit(
    exe_path: &Path,
    sock_path: &Path,
    workflows_dir: Option<&Path>,
) -> String {
    let mut exec = format!(
        "{} daemon start --foreground --socket {}",
        exe_path.display(),
        sock_path.display()
    );
    if let Some(wf) = workflows_dir {
        exec.push_str(&format!(" --workflows-dir {}", wf.display()));
    }
    format!(
        "[Unit]\n\
Description=UI-Automata Linux Persistent Automation Service\n\
After=graphical-session.target\n\
PartOf=graphical-session.target\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart={exec}\n\
Restart=on-failure\n\
RestartSec=2s\n\
\n\
[Install]\n\
WantedBy=graphical-session.target\n"
    )
}

fn current_uid() -> u32 {
    fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0)
}

fn ensure_private_socket_dir(dir: &Path) -> Result<(), ServiceError> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(ServiceError::PermissionDenied(format!(
            "socket directory '{}' cannot be a symlink",
            dir.display()
        )));
    }
    let self_uid = current_uid();
    if meta.uid() != self_uid {
        return Err(ServiceError::PermissionDenied(format!(
            "socket directory '{}' is not owned by current uid {self_uid}",
            dir.display()
        )));
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Checks whether a live `automata` daemon is accepting connections on `sock_path`.
pub fn is_daemon_alive(sock_path: &Path) -> bool {
    UnixStream::connect(sock_path).is_ok()
}

/// Sends a single [`Command`] to a running daemon over `sock_path`.
pub fn call_daemon(sock_path: &Path, cmd: Command) -> Result<CommandResult, ServiceError> {
    let mut stream = UnixStream::connect(sock_path).map_err(|e| {
        ServiceError::Platform(format!(
            "failed to connect to daemon socket '{}': {e}",
            sock_path.display()
        ))
    })?;
    let req = UdsRequest { id: 1, cmd };
    let line = serde_json::to_string(&req)
        .map_err(|e| ServiceError::Internal(format!("serialize daemon request: {e}")))?;
    writeln!(stream, "{line}")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut resp_line = String::new();
    let n = reader.read_line(&mut resp_line)?;
    if n == 0 {
        return Err(ServiceError::Platform(
            "daemon closed socket before sending response".into(),
        ));
    }
    let resp: UdsResponse = serde_json::from_str(&resp_line)
        .map_err(|e| ServiceError::Internal(format!("invalid daemon JSON response: {e}")))?;
    if resp.ok {
        resp.result.ok_or_else(|| {
            ServiceError::Internal("daemon response missing result payload".into())
        })
    } else {
        Err(resp
            .error
            .unwrap_or_else(|| ServiceError::Platform("unknown daemon error".into())))
    }
}

/// Stops a running daemon via its companion `.pid` file (`SIGTERM`) and waits briefly.
pub fn stop_daemon(sock_path: &Path) -> Result<bool, ServiceError> {
    let pid_file = pid_path_for_socket(sock_path);
    let Ok(raw_pid) = fs::read_to_string(&pid_file) else {
        if sock_path.exists() && !is_daemon_alive(sock_path) {
            let _ = fs::remove_file(sock_path);
        }
        return Ok(false);
    };
    let pid: u32 = raw_pid
        .trim()
        .parse()
        .map_err(|e| ServiceError::InvalidInput(format!("corrupt pid file: {e}")))?;

    let status = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()?;
    if status.success() {
        for _ in 0..20 {
            if !Path::new(&format!("/proc/{pid}")).exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = fs::remove_file(sock_path);
        let _ = fs::remove_file(&pid_file);
        return Ok(true);
    }
    Ok(false)
}

/// Runs the blocking `0700 / 0600` Unix Domain Socket server loop serving `service`
/// until `shutdown_flag` flips `true`.
pub fn run_daemon_server(
    svc: AutomationService,
    sock_path: &Path,
    shutdown_flag: Arc<AtomicBool>,
) -> Result<(), ServiceError> {
    let parent = sock_path
        .parent()
        .ok_or_else(|| ServiceError::InvalidInput("socket path must have a parent dir".into()))?;
    ensure_private_socket_dir(parent)?;

    if sock_path.exists() {
        if is_daemon_alive(sock_path) {
            return Err(ServiceError::Conflict(format!(
                "automata daemon is already listening on '{}'",
                sock_path.display()
            )));
        }
        // Safe cleanup of stale socket from an earlier exited process.
        fs::remove_file(sock_path)?;
    }

    let listener = UnixListener::bind(sock_path)?;
    fs::set_permissions(sock_path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;

    let pid_path = pid_path_for_socket(sock_path);
    let _ = fs::write(&pid_path, format!("{}\n", std::process::id()));
    let _ = fs::set_permissions(&pid_path, fs::Permissions::from_mode(0o600));

    struct CleanupGuard {
        sock: PathBuf,
        pid: PathBuf,
    }
    impl Drop for CleanupGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.sock);
            let _ = fs::remove_file(&self.pid);
        }
    }
    let _cleanup = CleanupGuard {
        sock: sock_path.to_path_buf(),
        pid: pid_path,
    };

    while !shutdown_flag.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let svc_conn = svc.clone();
                std::thread::spawn(move || {
                    handle_uds_client(svc_conn, stream);
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            Err(err) => {
                return Err(ServiceError::Platform(format!(
                    "daemon accept failed: {err}"
                )));
            }
        }
    }
    Ok(())
}

fn handle_uds_client(svc: AutomationService, stream: UnixStream) {
    let _ = stream.set_nonblocking(false);
    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line_res in reader.lines() {
        let Ok(line) = line_res else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<UdsRequest>(&line) {
            Ok(req) => match svc.call(req.cmd) {
                Ok(result) => UdsResponse {
                    id: req.id,
                    ok: true,
                    result: Some(result),
                    error: None,
                },
                Err(error) => UdsResponse {
                    id: req.id,
                    ok: false,
                    result: None,
                    error: Some(error),
                },
            },
            Err(e) => UdsResponse {
                id: 0,
                ok: false,
                result: None,
                error: Some(ServiceError::InvalidInput(format!("invalid JSON request: {e}"))),
            },
        };
        if let Ok(encoded) = serde_json::to_string(&resp) {
            if writeln!(writer, "{encoded}").is_err() || writer.flush().is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SystemCommand;
    use crate::service::temp_service_config;
    use automata_linux::diagnostics::CapabilitiesSnapshot;

    struct DummyActor;
    impl crate::actor::UiActorOps for DummyActor {
        fn capabilities(&self) -> CapabilitiesSnapshot {
            unreachable!()
        }
        fn reset_shadow_dom(&mut self) {}
        fn resolve_window_bounds(&mut self, _: automata_linux::coords::WindowId) -> Result<(i32, i32, i32, i32), ServiceError> {
            Ok((0, 0, 100, 100))
        }
        fn list_windows(&mut self, _: &crate::protocol::WindowFilter) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!([]))
        }
        fn focused_window(&mut self) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!(null))
        }
        fn window_tree(&mut self, _: &crate::protocol::Target, _: usize) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
        fn active_window_tree(&mut self, _: Option<&str>, _: usize) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
        fn find_elements(&mut self, _: &crate::protocol::Target, _: &str, _: bool, _: bool, _: usize) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!([]))
        }
        fn capture_bundle(&mut self, _: &crate::protocol::Target, _: &Path, _: usize, _: usize, _: bool) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
        fn batch_explore(&mut self, _: &crate::protocol::Target, _: &[crate::protocol::BatchProbeStep], _: bool, _: Option<&Path>) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
        fn vision(&mut self, _: &crate::protocol::VisionCommand) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
        fn launch_app(&mut self, _: &str, _: ui_automata::LaunchWait, _: std::time::Duration) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
        fn focus_app(&mut self, _: &crate::protocol::Target, _: usize) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
        fn window_state(&mut self, _: &crate::protocol::Target, _: crate::protocol::WindowStateArg) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
        fn window_bounds(&mut self, _: &crate::protocol::Target, _: Option<i32>, _: Option<i32>, _: Option<i32>, _: Option<i32>) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
        fn get_window_bounds(&mut self, _: &crate::protocol::Target) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
        fn window_screenshot(&mut self, _: &crate::protocol::Target, _: Option<&str>) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
        fn run_steps(&mut self, _: &crate::protocol::Target, _: &[serde_json::Value], _: bool, _: std::time::Duration) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({"ok": true}))
        }
        fn browser(&mut self, _: &crate::protocol::BrowserCommand) -> Result<serde_json::Value, ServiceError> {
            Ok(serde_json::json!({}))
        }
    }

    #[test]
    fn uds_daemon_sets_0700_0600_perms_and_shares_recorder_session() {
        let tmp = std::env::temp_dir().join(format!("automata_uds_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let sock_dir = tmp.join("run");
        let sock_path = sock_dir.join("service.sock");

        let svc = AutomationService::open_with_actor(temp_service_config(tmp.clone(), true), || {
            Box::new(DummyActor)
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_srv = shutdown.clone();
        let sock_srv = sock_path.clone();

        let srv_thread = std::thread::spawn(move || {
            run_daemon_server(svc, &sock_srv, shutdown_srv).unwrap();
        });

        for _ in 0..50 {
            if is_daemon_alive(&sock_path) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(is_daemon_alive(&sock_path));
        assert_eq!(fs::metadata(&sock_dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&sock_path).unwrap().permissions().mode() & 0o777, 0o600);

        // Two separate client calls share session & recorder across UDS!
        let pong = call_daemon(&sock_path, Command::System(SystemCommand::Ping)).unwrap();
        assert_eq!(pong.data["pong"], true);

        call_daemon(
            &sock_path,
            Command::Session(crate::protocol::SessionCommand::RecordStart {
                name: "uds_shared_flow".into(),
                target: Some(crate::protocol::Target::process("calc")),
            }),
        )
        .unwrap();

        call_daemon(
            &sock_path,
            Command::Act(crate::protocol::ActCommand::Invoke {
                window: crate::protocol::Target::process("calc"),
                selector: ">> [role=button][name='1']".into(),
                timeout_secs: 2.0,
            }),
        )
        .unwrap();

        let exported = call_daemon(
            &sock_path,
            Command::Session(crate::protocol::SessionCommand::RecordStop { output_path: None }),
        )
        .unwrap();
        assert_eq!(exported.data["valid"], true);
        assert_eq!(exported.data["steps_count"], 1);

        shutdown.store(true, Ordering::Relaxed);
        srv_thread.join().unwrap();
        assert!(!sock_path.exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn renders_systemd_user_unit_with_socket_and_workflows() {
        let unit = render_systemd_user_unit(
            Path::new("/home/user/.ui-automata/automata"),
            Path::new("/run/user/1000/automata/service.sock"),
            Some(Path::new("/home/user/.ui-automata/workflows")),
        );
        assert!(unit.contains("ExecStart=/home/user/.ui-automata/automata daemon start --foreground --socket /run/user/1000/automata/service.sock --workflows-dir /home/user/.ui-automata/workflows"));
        assert!(unit.contains("WantedBy=graphical-session.target"));
    }
}
