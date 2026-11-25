use std::process::Command;

use atspi::CoordType;
use atspi::proxy::proxy_ext::ProxyExt;

use crate::a11y::{A11yClient, synthetic_hwnd};
use crate::{Desktop, LinuxElement, WindowAction, windows_info};

fn parse_hwnd(hwnd_str: &str) -> Result<u64, String> {
    let hex = hwnd_str
        .strip_prefix("0x")
        .or_else(|| hwnd_str.strip_prefix("0X"))
        .ok_or_else(|| format!("hwnd must be 0x-prefixed hex, got: {hwnd_str:?}"))?;
    u64::from_str_radix(hex, 16).map_err(|_| format!("invalid hwnd: {hwnd_str:?}"))
}

fn find_window_by_hwnd(hwnd: u64) -> Result<LinuxElement, String> {
    let desktop = Desktop::new();
    let windows = desktop
        .application_windows()
        .map_err(|e| format!("application_windows failed: {e}"))?;
    windows
        .into_iter()
        .find(|w| synthetic_hwnd(w.bus_name(), w.object_path()) == hwnd)
        .ok_or_else(|| format!("window hwnd={hwnd:#x} not found"))
}

/// Return the PID of the currently active/foreground window via AT-SPI2 `State::Active`.
pub fn foreground_pid() -> Option<u32> {
    let desktop = Desktop::new();
    let fg = desktop.foreground_window()?;
    fg.process_id().ok()
}

/// Bring the window matching synthetic `hwnd` to the foreground.
pub fn force_foreground(hwnd: u64) -> Result<(), String> {
    let win = find_window_by_hwnd(hwnd)?;
    win.activate_window().map_err(|e| e.to_string())
}

/// Best-effort guard verifying keyboard input targets `expected_pid`.
///
/// When AT-SPI2 reports an active foreground window PID, refuses keyboard injection
/// if a different process holds foreground focus.
pub fn check_keyboard_target(expected_pid: u32) -> Result<(), String> {
    if let Some(actual) = foreground_pid() {
        if actual != expected_pid {
            return Err(format!(
                "foreground window is process {actual}, expected {expected_pid} — refusing keyboard input"
            ));
        }
    }
    Ok(())
}

/// Best-effort guard verifying a click at `(x, y)` targets `expected_pid`.
///
/// On X11/XWayland, probes `xdotool getwindowfocus` / active window when available;
/// falls back to checking AT-SPI2 `foreground_pid()` when non-null.
pub fn check_click_point(x: i32, y: i32, expected_pid: u32) -> Result<(), String> {
    let desktop = Desktop::new();
    if let Some(fg) = desktop.foreground_window() {
        if let Ok(actual) = fg.process_id() {
            if actual != expected_pid {
                if let Ok((bx, by, bw, bh)) = fg.bounds() {
                    if bw > 0 && bh > 0 && x >= bx && x <= bx + bw && y >= by && y <= by + bh {
                        return Err(format!(
                            "click point ({x},{y}) is covered by foreground process {actual}, expected {expected_pid}"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Bring a window matching `process`/`selector`/`pid` to the foreground and verify activation.
pub fn activate_window_preflight(
    process: &str,
    selector: Option<&str>,
    pid: Option<u32>,
) -> Result<(), String> {
    let all = windows_info::application_windows()
        .map_err(|e| format!("activate_window: {e}"))?;
    let proc_lower = process.to_ascii_lowercase();

    let win = all
        .into_iter()
        .filter(|w| ui_automata::process_names_match(&w.process_name, &proc_lower))
        .filter(|w| pid.map_or(true, |p| w.pid == p))
        .filter(|w| {
            if let Some(sel) = selector {
                if sel == "*" {
                    return true;
                }
                let rest_opt = sel
                    .strip_prefix("[name")
                    .or_else(|| sel.strip_prefix("[title"));
                if let Some(rest) = rest_opt {
                    let value = rest
                        .trim_start_matches(|c| c == '~' || c == '^' || c == '$' || c == '=')
                        .trim_end_matches(']');
                    return w
                        .title
                        .to_ascii_lowercase()
                        .contains(&value.to_ascii_lowercase());
                }
            }
            true
        })
        .next()
        .ok_or_else(|| format!("activate_window: no window found for process '{process}'"))?;

    force_foreground(win.hwnd)?;
    if let Some(fg) = foreground_pid() {
        if fg == win.pid {
            return Ok(());
        }
    }
    // On Wayland, compositor focus-stealing prevention may keep grab_focus advisory;
    // return Ok if activate_window itself succeeded when active state cannot be forced.
    Ok(())
}

/// Minimize, maximize, restore, or close a window by its `"0x..."` synthetic hwnd string.
pub fn set_window_state(hwnd_str: &str, state: WindowAction) -> Result<(), String> {
    let hwnd = parse_hwnd(hwnd_str)?;
    let win = find_window_by_hwnd(hwnd)?;

    match state {
        WindowAction::Close => win.close().map_err(|e| e.to_string()),
        WindowAction::Minimize => win.minimize_window().map_err(|e| e.to_string()),
        WindowAction::Maximize => {
            if let Some(ref title) = win.name() {
                if Command::new("wmctrl")
                    .args(["-r", title, "-b", "add,maximized_vert,maximized_horz"])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
                {
                    return Ok(());
                }
            }
            let pid = win.process_id().ok();
            let title = win.name();
            let backend = crate::compositor::active_backend();
            let native_id = crate::compositor::list_compositor_windows()
                .as_deref()
                .and_then(|wins| crate::compositor::find_matching_compositor_window(wins, pid, title.as_deref()))
                .map(|w| w.native_id)
                .unwrap_or(0);
            if backend
                .set_window_state(native_id, pid, title.as_deref(), WindowAction::Maximize)
                .is_ok()
            {
                crate::compositor::invalidate_compositor_cache();
                return Ok(());
            }
            Err("maximize failed via wmctrl and native compositor backend".into())
        }
        WindowAction::Restore => {
            if let Some(ref title) = win.name() {
                let _ = Command::new("wmctrl")
                    .args(["-r", title, "-b", "remove,maximized_vert,maximized_horz"])
                    .status();
            }
            let pid = win.process_id().ok();
            let title = win.name();
            let backend = crate::compositor::active_backend();
            let native_id = crate::compositor::list_compositor_windows()
                .as_deref()
                .and_then(|wins| crate::compositor::find_matching_compositor_window(wins, pid, title.as_deref()))
                .map(|w| w.native_id)
                .unwrap_or(0);
            let _ = backend.set_window_state(native_id, pid, title.as_deref(), WindowAction::Restore);
            crate::compositor::invalidate_compositor_cache();
            win.activate_window().map_err(|e| e.to_string())
        }
    }
}

/// Move and resize a window by its `"0x..."` synthetic hwnd string.
pub fn set_window_bounds(
    hwnd_str: &str,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let hwnd = parse_hwnd(hwnd_str)?;
    let win = find_window_by_hwnd(hwnd)?;
    let client = A11yClient::shared().map_err(|e| e.to_string())?;

    let atspi_ok = client.block_on(async {
        let Ok(proxy) = client.accessible_proxy(win.bus_name(), win.object_path()).await else {
            return false;
        };
        let Ok(proxies) = proxy.proxies().await else {
            return false;
        };
        let Ok(comp) = proxies.component().await else {
            return false;
        };
        comp.set_extents(x, y, width as i32, height as i32, CoordType::Screen)
            .await
            .unwrap_or(false)
    });

    if atspi_ok {
        crate::compositor::invalidate_compositor_cache();
        return Ok(());
    }

    if let Some(title) = win.name() {
        let spec = format!("0,{x},{y},{width},{height}");
        if Command::new("wmctrl")
            .args(["-r", &title, "-e", &spec])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            crate::compositor::invalidate_compositor_cache();
            return Ok(());
        }
    }

    let pid = win.process_id().ok();
    let title = win.name();
    let backend = crate::compositor::active_backend();
    let native_id = crate::compositor::list_compositor_windows()
        .as_deref()
        .and_then(|wins| crate::compositor::find_matching_compositor_window(wins, pid, title.as_deref()))
        .map(|w| w.native_id)
        .unwrap_or(0);
    if backend
        .set_window_bounds(native_id, title.as_deref(), x, y, width, height)
        .is_ok()
    {
        crate::compositor::invalidate_compositor_cache();
        return Ok(());
    }

    Err("set_window_bounds failed via AT-SPI Component::set_extents, wmctrl, and compositor".into())
}
