//! X11 / XWayland compositor backend powered by `x11rb` + `wmctrl` fallbacks.

use std::process::Command;

use anyhow::Result;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, Window};

use crate::WindowAction;
use crate::compositor::{
    CompositorBackend, CompositorCapabilities, CompositorKind, CompositorWindow,
};

pub struct X11Backend;

impl CompositorBackend for X11Backend {
    fn kind(&self) -> CompositorKind {
        CompositorKind::X11
    }

    fn capabilities(&self) -> CompositorCapabilities {
        let can_connect = x11rb::connect(None).is_ok();
        CompositorCapabilities {
            kind: CompositorKind::X11,
            available: can_connect,
            can_list_windows: can_connect,
            can_report_bounds: can_connect,
            can_report_stacking: can_connect,
            can_focus: can_connect,
            can_close: can_connect,
            can_move_resize: can_connect,
            detail: if can_connect {
                "x11rb connected to DISPLAY".to_string()
            } else {
                "unable to connect to X11 DISPLAY".to_string()
            },
        }
    }

    fn list_windows(&self) -> Result<Vec<CompositorWindow>> {
        let (conn, screen_num) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen_num].root;

        let atom_client_stacking = conn
            .intern_atom(false, b"_NET_CLIENT_LIST_STACKING")?
            .reply()?
            .atom;
        let atom_client_list = conn.intern_atom(false, b"_NET_CLIENT_LIST")?.reply()?.atom;
        let atom_active = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
            .reply()?
            .atom;
        let atom_pid = conn.intern_atom(false, b"_NET_WM_PID")?.reply()?.atom;
        let atom_net_name = conn.intern_atom(false, b"_NET_WM_NAME")?.reply()?.atom;
        let atom_utf8 = conn.intern_atom(false, b"UTF8_STRING")?.reply()?.atom;

        let active_xid = read_u32_prop(&conn, root, atom_active);

        let mut xids = read_window_list_prop(&conn, root, atom_client_stacking);
        let has_stacking = !xids.is_empty();
        if !has_stacking {
            xids = read_window_list_prop(&conn, root, atom_client_list);
        } else {
            // `_NET_CLIENT_LIST_STACKING` is bottom-to-top; reverse to topmost-first.
            xids.reverse();
        }

        let mut out = Vec::with_capacity(xids.len());
        for (rank, xid) in xids.into_iter().enumerate() {
            let pid = read_u32_prop(&conn, xid, atom_pid);
            let title = read_text_prop(&conn, xid, atom_net_name, atom_utf8)
                .or_else(|| read_text_prop(&conn, xid, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()))
                .unwrap_or_default();
            let app_id = read_text_prop(&conn, xid, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into())
                .unwrap_or_default();
            let bounds = read_window_bounds(&conn, root, xid);

            out.push(CompositorWindow {
                native_id: u64::from(xid),
                title,
                app_id,
                pid,
                bounds,
                is_focused: active_xid == Some(xid),
                is_minimized: false,
                z_order_rank: rank,
                backend: CompositorKind::X11,
            });
        }
        Ok(out)
    }

    fn focus_window(&self, native_id: u64, pid: Option<u32>, title: Option<&str>) -> Result<()> {
        if native_id > 0 && native_id <= u64::from(u32::MAX) {
            let hex = format!("{native_id:#x}");
            if run_ok("wmctrl", &["-ia", &hex]) || run_ok("xdotool", &["windowactivate", "--sync", &native_id.to_string()]) {
                return Ok(());
            }
        }
        if let Some(p) = pid {
            if let Some(xid) = find_xid_by_pid(p) {
                if run_ok("wmctrl", &["-ia", &xid]) {
                    return Ok(());
                }
            }
        }
        if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
            if run_ok("wmctrl", &["-a", t]) {
                return Ok(());
            }
        }
        anyhow::bail!("X11 focus_window failed via wmctrl and xdotool")
    }

    fn set_window_bounds(
        &self,
        native_id: u64,
        title: Option<&str>,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let spec = format!("0,{x},{y},{width},{height}");
        if native_id > 0 && native_id <= u64::from(u32::MAX) {
            let hex = format!("{native_id:#x}");
            if run_ok("wmctrl", &["-ir", &hex, "-e", &spec]) {
                return Ok(());
            }
        }
        if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
            if run_ok("wmctrl", &["-r", t, "-e", &spec]) {
                return Ok(());
            }
        }
        anyhow::bail!("X11 set_window_bounds failed via wmctrl")
    }

    fn close_window(&self, native_id: u64, pid: Option<u32>, title: Option<&str>) -> Result<()> {
        if native_id > 0 && native_id <= u64::from(u32::MAX) {
            let hex = format!("{native_id:#x}");
            if run_ok("wmctrl", &["-ic", &hex]) {
                return Ok(());
            }
        }
        if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
            if run_ok("wmctrl", &["-c", t]) {
                return Ok(());
            }
        }
        if let Some(p) = pid {
            if run_ok("xdotool", &["search", "--pid", &p.to_string(), "windowclose"]) {
                return Ok(());
            }
        }
        anyhow::bail!("X11 close_window failed via wmctrl and xdotool")
    }

    fn set_window_state(
        &self,
        native_id: u64,
        pid: Option<u32>,
        title: Option<&str>,
        action: WindowAction,
    ) -> Result<()> {
        let hex = (native_id > 0 && native_id <= u64::from(u32::MAX)).then(|| format!("{native_id:#x}"));
        match action {
            WindowAction::Close => self.close_window(native_id, pid, title),
            WindowAction::Minimize => {
                if let Some(ref h) = hex {
                    if run_ok("wmctrl", &["-ir", h, "-b", "add,hidden"]) {
                        return Ok(());
                    }
                }
                if let Some(p) = pid {
                    if run_ok("xdotool", &["search", "--pid", &p.to_string(), "windowminimize"]) {
                        return Ok(());
                    }
                }
                anyhow::bail!("X11 minimize failed")
            }
            WindowAction::Maximize => {
                if let Some(ref h) = hex {
                    if run_ok("wmctrl", &["-ir", h, "-b", "add,maximized_vert,maximized_horz"]) {
                        return Ok(());
                    }
                }
                if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
                    if run_ok("wmctrl", &["-r", t, "-b", "add,maximized_vert,maximized_horz"]) {
                        return Ok(());
                    }
                }
                anyhow::bail!("X11 maximize failed")
            }
            WindowAction::Restore => {
                if let Some(ref h) = hex {
                    let _ = run_ok("wmctrl", &["-ir", h, "-b", "remove,maximized_vert,maximized_horz,hidden"]);
                } else if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
                    let _ = run_ok("wmctrl", &["-r", t, "-b", "remove,maximized_vert,maximized_horz,hidden"]);
                }
                self.focus_window(native_id, pid, title)
            }
        }
    }
}

fn run_ok(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn find_xid_by_pid(pid: u32) -> Option<String> {
    let out = Command::new("wmctrl").args(["-lp"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let pid_str = pid.to_string();
    String::from_utf8_lossy(&out.stdout).lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let xid = parts.next()?;
        let _desktop = parts.next()?;
        let row_pid = parts.next()?;
        (row_pid == pid_str).then(|| xid.to_string())
    })
}

fn read_window_list_prop<C: Connection>(conn: &C, root: Window, prop: u32) -> Vec<u32> {
    let Ok(cookie) = conn.get_property(false, root, prop, AtomEnum::WINDOW, 0, 4096) else {
        return Vec::new();
    };
    let Ok(reply) = cookie.reply() else {
        return Vec::new();
    };
    reply
        .value32()
        .map(|it| it.collect())
        .unwrap_or_default()
}

fn read_u32_prop<C: Connection>(conn: &C, win: Window, prop: u32) -> Option<u32> {
    let reply = conn
        .get_property(false, win, prop, AtomEnum::ANY, 0, 4)
        .ok()?
        .reply()
        .ok()?;
    reply.value32()?.next()
}

fn read_text_prop<C: Connection>(conn: &C, win: Window, prop: u32, ty: u32) -> Option<String> {
    let reply = conn
        .get_property(false, win, prop, ty, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    if reply.value.is_empty() {
        return None;
    }
    // WM_CLASS contains two nul-separated strings (`instance\0class\0`).
    let cleaned: String = String::from_utf8_lossy(&reply.value)
        .split('\0')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(".");
    (!cleaned.is_empty()).then_some(cleaned)
}

fn read_window_bounds<C: Connection>(
    conn: &C,
    root: Window,
    win: Window,
) -> Option<(i32, i32, i32, i32)> {
    let geom = conn.get_geometry(win).ok()?.reply().ok()?;
    let coords = conn
        .translate_coordinates(win, root, 0, 0)
        .ok()?
        .reply()
        .ok()?;
    Some((
        i32::from(coords.dst_x),
        i32::from(coords.dst_y),
        i32::from(geom.width),
        i32::from(geom.height),
    ))
}
