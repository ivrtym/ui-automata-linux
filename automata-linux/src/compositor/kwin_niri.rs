//! Lightweight Niri socket IPC and KDE KWin Wayland compositor adapters.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::WindowAction;
use crate::compositor::{
    CompositorBackend, CompositorCapabilities, CompositorKind, CompositorWindow,
    block_on_compositor, discover_niri_socket_in_runtime_dir,
};
use crate::runtime::command::run_bounded;

/// Native Niri compositor backend communicating over `$NIRI_SOCKET` Unix domain IPC
/// with `niri msg` fallback.
pub struct NiriBackend;

#[derive(Debug, Deserialize)]
struct NiriWindowRaw {
    id: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    is_focused: bool,
}

impl CompositorBackend for NiriBackend {
    fn kind(&self) -> CompositorKind {
        CompositorKind::Niri
    }

    fn capabilities(&self) -> CompositorCapabilities {
        let windows = self.list_windows();
        let ok = windows.is_ok();
        CompositorCapabilities {
            kind: CompositorKind::Niri,
            available: ok,
            can_list_windows: ok,
            can_report_bounds: false,
            can_report_stacking: ok,
            can_focus: ok,
            can_close: ok,
            can_move_resize: false,
            detail: match windows {
                Ok(w) => format!("niri IPC responsive ({} window(s))", w.len()),
                Err(e) => format!("niri IPC unavailable: {e}"),
            },
        }
    }

    fn list_windows(&self) -> Result<Vec<CompositorWindow>> {
        let json = niri_ipc_json("\"Windows\"", &["msg", "--json", "windows"])?;
        parse_niri_windows(&json)
    }

    fn focus_window(&self, native_id: u64, _pid: Option<u32>, _title: Option<&str>) -> Result<()> {
        let req = format!("{{\"Action\":{{\"FocusWindow\":{{\"id\":{native_id}}}}}}}");
        let id_str = native_id.to_string();
        let _ = niri_ipc_json(&req, &["msg", "action", "focus-window", "--id", &id_str])?;
        Ok(())
    }

    fn close_window(&self, native_id: u64, _pid: Option<u32>, _title: Option<&str>) -> Result<()> {
        let req = format!("{{\"Action\":{{\"CloseWindow\":{{\"id\":{native_id}}}}}}}");
        let id_str = native_id.to_string();
        let _ = niri_ipc_json(&req, &["msg", "action", "close-window", "--id", &id_str])?;
        Ok(())
    }

    fn set_window_state(
        &self,
        native_id: u64,
        pid: Option<u32>,
        title: Option<&str>,
        action: WindowAction,
    ) -> Result<()> {
        match action {
            WindowAction::Close => self.close_window(native_id, pid, title),
            WindowAction::Maximize | WindowAction::Restore => {
                self.focus_window(native_id, pid, title)?;
                let req = "{\"Action\":{\"MaximizeColumn\":{}}}";
                let _ = niri_ipc_json(req, &["msg", "action", "maximize-column"])?;
                Ok(())
            }
            WindowAction::Minimize => anyhow::bail!("niri does not support window minimization"),
        }
    }
}

fn niri_ipc_json(socket_request: &str, cli_args: &[&str]) -> Result<String> {
    let socket = std::env::var("NIRI_SOCKET")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(discover_niri_socket_in_runtime_dir);

    if let Some(sock_path) = socket {
        if let Ok(mut stream) = UnixStream::connect(&sock_path) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            if writeln!(stream, "{socket_request}").is_ok() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() && !line.trim().is_empty() {
                    let parsed: serde_json::Value = serde_json::from_str(&line)?;
                    if let Some(ok_val) = parsed.get("Ok") {
                        // Direct socket wraps reply in {"Ok":{"Windows":[...]}} or {"Ok":"Handled"}
                        if let Some(inner) = ok_val.get("Windows") {
                            return Ok(inner.to_string());
                        }
                        return Ok(ok_val.to_string());
                    }
                }
            }
        }
    }

    let mut cmd = Command::new("niri");
    cmd.args(cli_args);
    let out = run_bounded(cmd, "niri CLI", Duration::from_secs(2), 2 * 1024 * 1024)
        .context("run niri CLI")?;
    if !out.status.success() {
        anyhow::bail!("niri command failed: {:?}", out.status.code());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn parse_niri_windows(json_str: &str) -> Result<Vec<CompositorWindow>> {
    let raw: Vec<NiriWindowRaw> =
        serde_json::from_str(json_str).context("parse niri windows JSON")?;
    let mut windows: Vec<CompositorWindow> = raw
        .into_iter()
        .map(|w| CompositorWindow {
            native_id: w.id,
            title: w.title.unwrap_or_default(),
            app_id: w.app_id.unwrap_or_default(),
            pid: w.pid,
            bounds: None,
            is_focused: w.is_focused,
            is_minimized: false,
            z_order_rank: 0,
            backend: CompositorKind::Niri,
        })
        .collect();
    windows.sort_by_key(|w| (!w.is_focused, w.native_id));
    for (rank, w) in windows.iter_mut().enumerate() {
        w.z_order_rank = rank;
    }
    Ok(windows)
}

/// KDE KWin Wayland backend probing `org.kde.KWin` on the session D-Bus with
/// XWayland fallback for mixed sessions.
pub struct KWinBackend;

impl CompositorBackend for KWinBackend {
    fn kind(&self) -> CompositorKind {
        CompositorKind::KWinWayland
    }

    fn capabilities(&self) -> CompositorCapabilities {
        let dbus_ok = block_on_compositor(async {
            let conn = zbus::Connection::session().await?;
            let proxy = zbus::fdo::DBusProxy::new(&conn).await?;
            let has = proxy.name_has_owner("org.kde.KWin".try_into()?).await?;
            Ok(has)
        })
        .unwrap_or(false);

        CompositorCapabilities {
            kind: CompositorKind::KWinWayland,
            available: dbus_ok,
            can_list_windows: dbus_ok,
            can_report_bounds: dbus_ok,
            can_report_stacking: false,
            can_focus: dbus_ok,
            can_close: dbus_ok,
            can_move_resize: dbus_ok,
            detail: if dbus_ok {
                "org.kde.KWin detected on session D-Bus".to_string()
            } else {
                "org.kde.KWin not present on session D-Bus".to_string()
            },
        }
    }

    fn list_windows(&self) -> Result<Vec<CompositorWindow>> {
        // Use kdotool if present (Wayland + KWin helper), else XWayland EWMH.
        let mut cmd = Command::new("kdotool");
        cmd.args(["search", "--name", ".*"]);
        if let Ok(out) = run_bounded(cmd, "kdotool search", Duration::from_secs(2), 512 * 1024) {
            if out.status.success() {
                let ids: Vec<&str> = std::str::from_utf8(&out.stdout)
                    .unwrap_or("")
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .collect();
                if !ids.is_empty() {
                    let mut windows = Vec::with_capacity(ids.len());
                    for (rank, id_str) in ids.into_iter().enumerate() {
                        let name = Command::new("kdotool")
                            .args(["getwindowname", id_str])
                            .output()
                            .ok()
                            .filter(|o| o.status.success())
                            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                            .unwrap_or_default();
                        windows.push(CompositorWindow {
                            native_id: crate::compositor::hash_id_to_safe_u64(id_str),
                            title: name,
                            app_id: String::new(),
                            pid: None,
                            bounds: None,
                            is_focused: rank == 0,
                            is_minimized: false,
                            z_order_rank: rank,
                            backend: CompositorKind::KWinWayland,
                        });
                    }
                    return Ok(windows);
                }
            }
        }
        crate::compositor::x11::X11Backend.list_windows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_niri_window_array_focus_first() {
        let json = r#"[
            {"id":5,"title":"Docs","app_id":"org.mozilla.firefox","pid":333,"is_focused":false},
            {"id":9,"title":"Terminal","app_id":"foot","pid":444,"is_focused":true}
        ]"#;
        let wins = parse_niri_windows(json).expect("parse niri json");
        assert_eq!(wins.len(), 2);
        assert_eq!(wins[0].native_id, 9);
        assert!(wins[0].is_focused);
        assert_eq!(wins[0].z_order_rank, 0);
        assert_eq!(wins[1].native_id, 5);
    }
}
