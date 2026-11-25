pub mod background;
pub mod guard;
pub mod x11_xtest;
pub mod xkb;

pub use background::*;
pub use guard::*;
pub use xkb::*;

use std::process::Command;

use crate::backend::{
    InputBackend, InputKind, has_x11_display, resolve_input_backend, uinput_writable,
    ydotoold_running,
};
use crate::{Result, UiError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickType {
    Left,
    Double,
    Triple,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAxis {
    Vertical,
    Horizontal,
}

fn run_xdotool(args: &[&str]) -> Result<()> {
    let out = Command::new("xdotool")
        .args(args)
        .output()
        .map_err(|e| UiError::Platform(format!("failed to spawn xdotool: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(UiError::Platform(format!(
            "xdotool {:?} failed ({}): {}",
            args,
            out.status,
            err.trim()
        ))
        .into());
    }
    Ok(())
}

fn run_ydotool(args: &[&str]) -> Result<()> {
    let out = Command::new("ydotool")
        .args(args)
        .output()
        .map_err(|e| UiError::Platform(format!("failed to spawn ydotool: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let hint = if !ydotoold_running() && !uinput_writable() {
            " (hint: start `ydotoold` and ensure read/write permission on /dev/uinput, e.g. `input` group)"
        } else {
            ""
        };
        return Err(UiError::Platform(format!(
            "ydotool {:?} failed ({}): {}{}",
            args,
            out.status,
            err.trim(),
            hint
        ))
        .into());
    }
    Ok(())
}

fn ydotool_abs_scale() -> f64 {
    let display = crate::backend::resolve_display_backend().ok();
    let desktop_env = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let override_raw = std::env::var("AUTOMATA_YDOTOOL_SCALE").ok();
    crate::coords::CoordResolver::ydotool_abs_scale(
        display,
        desktop_env.as_deref(),
        override_raw.as_deref(),
    )
}

fn ydotool_abs_move(x: i32, y: i32) -> Result<()> {
    // Skip redundant `--absolute` moves (and their `(0, 0)` reset) when already on target.
    if let Ok((cx, cy)) = get_cursor_pos() {
        if (cx - x).abs() <= 4 && (cy - y).abs() <= 4 {
            return Ok(());
        }
    }
    let scale = ydotool_abs_scale();
    let sx = ((x as f64) * scale).round() as i32;
    let sy = ((y as f64) * scale).round() as i32;
    run_ydotool(&[
        "mousemove",
        "--absolute",
        "-x",
        &sx.to_string(),
        "-y",
        &sy.to_string(),
    ])
}

fn warn_if_x11_grabbed() {
    if !has_x11_display() {
        return;
    }
    if let Some(grab) = xkb::probe_x11_grabs() {
        if grab.any_grabbed() {
            log::warn!(
                "Active X11 input grab detected before synthetic input (pointer={}, keyboard={}); an open popup menu or drag may intercept events",
                grab.pointer_grabbed,
                grab.keyboard_grabbed
            );
        }
    }
}

/// Move the mouse cursor to screen coordinates `(x, y)`.
pub fn mouse_move(x: i32, y: i32) -> Result<()> {
    match resolve_input_backend(InputKind::Pointer)? {
        InputBackend::X11XTest => x11_xtest::mouse_move(x, y),
        InputBackend::Xdotool => {
            run_xdotool(&["mousemove", "--sync", &x.to_string(), &y.to_string()])
        }
        InputBackend::Ydotool => ydotool_abs_move(x, y),
        InputBackend::Wtype => Err(UiError::Platform("wtype does not support pointer move".into()).into()),
    }
}

/// Click and drag with the left button from `(x1, y1)` to `(x2, y2)`.
pub fn mouse_drag(x1: i32, y1: i32, x2: i32, y2: i32) -> Result<()> {
    match resolve_input_backend(InputKind::Pointer)? {
        InputBackend::X11XTest => {
            warn_if_x11_grabbed();
            x11_xtest::mouse_drag(x1, y1, x2, y2)
        }
        InputBackend::Xdotool => {
            warn_if_x11_grabbed();
            run_xdotool(&[
                "mousemove",
                "--sync",
                &x1.to_string(),
                &y1.to_string(),
                "mousedown",
                "1",
                "mousemove",
                "--sync",
                &x2.to_string(),
                &y2.to_string(),
                "mouseup",
                "1",
            ])
        }
        InputBackend::Ydotool => {
            // ydotool click bitmask: 0x00 = left, 0x40 = down, 0x80 = up
            ydotool_abs_move(x1, y1)?;
            run_ydotool(&["click", "0x40"])?;
            let move_res = ydotool_abs_move(x2, y2);
            let up_res = run_ydotool(&["click", "0x80"]);
            move_res?;
            up_res
        }
        InputBackend::Wtype => Err(UiError::Platform("wtype does not support mouse_drag".into()).into()),
    }
}

/// Move to `(x, y)` and perform a click of the given kind.
pub fn mouse_click(x: i32, y: i32, kind: ClickType) -> Result<()> {
    match resolve_input_backend(InputKind::Pointer)? {
        InputBackend::X11XTest => {
            warn_if_x11_grabbed();
            x11_xtest::mouse_click(x, y, kind)
        }
        InputBackend::Xdotool => {
            warn_if_x11_grabbed();
            run_xdotool(&["mousemove", "--sync", &x.to_string(), &y.to_string()])?;
            match kind {
                ClickType::Left => run_xdotool(&["click", "1"]),
                ClickType::Double => run_xdotool(&["click", "--repeat", "2", "--delay", "40", "1"]),
                ClickType::Triple => run_xdotool(&["click", "--repeat", "3", "--delay", "40", "1"]),
                ClickType::Right => run_xdotool(&["click", "3"]),
                ClickType::Middle => run_xdotool(&["click", "2"]),
            }
        }
        InputBackend::Ydotool => {
            ydotool_abs_move(x, y)?;
            let code = match kind {
                ClickType::Left | ClickType::Double | ClickType::Triple => "0xC0",
                ClickType::Right => "0xC1",
                ClickType::Middle => "0xC2",
            };
            let repeat = match kind {
                ClickType::Double => 2,
                ClickType::Triple => 3,
                _ => 1,
            };
            for _ in 0..repeat {
                run_ydotool(&["click", code])?;
            }
            Ok(())
        }
        InputBackend::Wtype => Err(UiError::Platform("wtype does not support mouse_click".into()).into()),
    }
}

/// Scroll mouse wheel at `(x, y)` by `delta_x` / `delta_y` ticks (positive = up/right).
pub fn mouse_scroll(x: i32, y: i32, delta_x: i32, delta_y: i32) -> Result<()> {
    if delta_x == 0 && delta_y == 0 {
        return Ok(());
    }
    mouse_move(x, y)?;
    if resolve_input_backend(InputKind::Pointer)? == InputBackend::Ydotool {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if delta_y != 0 {
        scroll_axis_ticks(delta_y, ScrollAxis::Vertical)?;
    }
    if delta_x != 0 {
        scroll_axis_ticks(delta_x, ScrollAxis::Horizontal)?;
    }
    Ok(())
}

pub(crate) fn scroll_axis_ticks(delta: i32, axis: ScrollAxis) -> Result<()> {
    if delta == 0 {
        return Ok(());
    }
    match resolve_input_backend(InputKind::Pointer)? {
        InputBackend::X11XTest => x11_xtest::scroll_axis_ticks(delta, axis),
        InputBackend::Xdotool => {
            let button = match (axis, delta > 0) {
                (ScrollAxis::Vertical, true) => "4",
                (ScrollAxis::Vertical, false) => "5",
                (ScrollAxis::Horizontal, true) => "7",
                (ScrollAxis::Horizontal, false) => "6",
            };
            let count = delta.unsigned_abs().max(1).to_string();
            run_xdotool(&["click", "--repeat", &count, button])
        }
        InputBackend::Ydotool => {
            // Emit discrete ±1 wheel ticks over a short interval rather than one giant jump,
            // matching physical mouse wheels so Chromium/Electron scroll thresholds fire reliably.
            let step = delta.signum();
            let (dx, dy) = match axis {
                ScrollAxis::Vertical => (0, step),
                ScrollAxis::Horizontal => (step, 0),
            };
            let dx_str = dx.to_string();
            let dy_str = dy.to_string();
            let ticks = delta.unsigned_abs().max(1);
            for i in 0..ticks {
                run_ydotool(&["mousemove", "--wheel", "--", &dx_str, &dy_str])?;
                if i + 1 < ticks {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            }
            Ok(())
        }
        InputBackend::Wtype => Err(UiError::Platform("wtype does not support scroll".into()).into()),
    }
}

/// Normalise Windows-style key expressions (e.g. `"{ENTER}"`, `"ctrl+j"`, `"alt+f4"`)
/// into X11 keysyms accepted by `xdotool key` / `wtype`.
pub fn normalise_key(expr: &str) -> String {
    let trimmed = expr.trim();
    let inner = trimmed
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(trimmed);

    if inner.contains('+') {
        return inner
            .split('+')
            .map(map_single_key)
            .collect::<Vec<_>>()
            .join("+");
    }

    map_single_key(inner)
}

fn map_single_key(token: &str) -> String {
    match token.trim().to_ascii_lowercase().as_str() {
        "enter" | "return" => "Return".into(),
        "tab" => "Tab".into(),
        "esc" | "escape" => "Escape".into(),
        "space" => "space".into(),
        "backspace" | "bksp" => "BackSpace".into(),
        "delete" | "del" => "Delete".into(),
        "insert" | "ins" => "Insert".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "pgup" | "pageup" | "page_up" => "Prior".into(),
        "pgdn" | "pagedown" | "page_down" => "Next".into(),
        "up" => "Up".into(),
        "down" => "Down".into(),
        "left" => "Left".into(),
        "right" => "Right".into(),
        "ctrl" | "control" => "ctrl".into(),
        "alt" => "alt".into(),
        "shift" => "shift".into(),
        "win" | "super" | "meta" => "super".into(),
        "f1" => "F1".into(),
        "f2" => "F2".into(),
        "f3" => "F3".into(),
        "f4" => "F4".into(),
        "f5" => "F5".into(),
        "f6" => "F6".into(),
        "f7" => "F7".into(),
        "f8" => "F8".into(),
        "f9" => "F9".into(),
        "f10" => "F10".into(),
        "f11" => "F11".into(),
        "f12" => "F12".into(),
        other => other.to_string(),
    }
}

/// Press a key or key combination (supports `{ENTER}`, `ctrl+j`, etc.).
pub fn key_press(key: &str) -> Result<()> {
    let mapped = normalise_key(key);
    warn_if_x11_grabbed();
    match resolve_input_backend(InputKind::Keyboard)? {
        InputBackend::X11XTest => {
            if let Ok(()) = x11_xtest::key_press_chord(&mapped) {
                return Ok(());
            }
            run_xdotool(&["key", "--clearmodifiers", &mapped])
        }
        InputBackend::Xdotool => run_xdotool(&["key", "--clearmodifiers", &mapped]),
        InputBackend::Wtype => {
            let mut args: Vec<String> = Vec::new();
            let parts: Vec<&str> = mapped.split('+').collect();
            for p in &parts[..parts.len().saturating_sub(1)] {
                args.push("-M".into());
                args.push((*p).to_string());
            }
            if let Some(last) = parts.last() {
                args.push("-k".into());
                args.push((*last).to_string());
            }
            let out = Command::new("wtype").args(&args).output()?;
            if out.status.success() {
                return Ok(());
            }
            let err = String::from_utf8_lossy(&out.stderr);
            Err(UiError::Platform(format!("wtype key failed: {}", err.trim())).into())
        }
        InputBackend::Ydotool => {
            let codes: Vec<u16> = mapped
                .split('+')
                .map(map_evdev_keycode)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut events: Vec<String> = Vec::with_capacity(codes.len() * 2);
            for &c in &codes {
                events.push(format!("{c}:1"));
            }
            for &c in codes.iter().rev() {
                events.push(format!("{c}:0"));
            }
            let mut args = vec!["key"];
            let refs: Vec<&str> = events.iter().map(String::as_str).collect();
            args.extend(refs);
            run_ydotool(&args)
        }
    }
}

fn map_evdev_keycode(token: &str) -> std::result::Result<u16, UiError> {
    match token.trim().to_ascii_lowercase().as_str() {
        "escape" | "esc" => Ok(1),
        "1" => Ok(2),
        "2" => Ok(3),
        "3" => Ok(4),
        "4" => Ok(5),
        "5" => Ok(6),
        "6" => Ok(7),
        "7" => Ok(8),
        "8" => Ok(9),
        "9" => Ok(10),
        "0" => Ok(11),
        "backspace" => Ok(14),
        "tab" => Ok(15),
        "q" => Ok(16),
        "w" => Ok(17),
        "e" => Ok(18),
        "r" => Ok(19),
        "t" => Ok(20),
        "y" => Ok(21),
        "u" => Ok(22),
        "i" => Ok(23),
        "o" => Ok(24),
        "p" => Ok(25),
        "return" | "enter" => Ok(28),
        "ctrl" | "control" => Ok(29),
        "a" => Ok(30),
        "s" => Ok(31),
        "d" => Ok(32),
        "f" => Ok(33),
        "g" => Ok(34),
        "h" => Ok(35),
        "j" => Ok(36),
        "k" => Ok(37),
        "l" => Ok(38),
        "shift" => Ok(42),
        "z" => Ok(44),
        "x" => Ok(45),
        "c" => Ok(46),
        "v" => Ok(47),
        "b" => Ok(48),
        "n" => Ok(49),
        "m" => Ok(50),
        "alt" => Ok(56),
        "space" => Ok(57),
        "f1" => Ok(59),
        "f2" => Ok(60),
        "f3" => Ok(61),
        "f4" => Ok(62),
        "f5" => Ok(63),
        "f6" => Ok(64),
        "f7" => Ok(65),
        "f8" => Ok(66),
        "f9" => Ok(67),
        "f10" => Ok(68),
        "f11" => Ok(87),
        "f12" => Ok(88),
        "home" => Ok(102),
        "up" => Ok(103),
        "prior" => Ok(104),
        "left" => Ok(105),
        "right" => Ok(106),
        "end" => Ok(107),
        "down" => Ok(108),
        "next" => Ok(109),
        "insert" => Ok(110),
        "delete" => Ok(111),
        "super" => Ok(125),
        other => other.parse::<u16>().map_err(|_| {
            UiError::Platform(format!("unsupported ydotool key token: {other:?}"))
        }),
    }
}

/// Type literal text via synthetic keyboard input.
pub fn type_text(text: &str) -> Result<()> {
    match resolve_input_backend(InputKind::Keyboard)? {
        InputBackend::X11XTest => {
            if text.is_ascii() {
                if x11_xtest::type_ascii_text(text).is_ok() {
                    return Ok(());
                }
            }
            if crate::backend::command_exists("xdotool") && has_x11_display() {
                return run_xdotool(&["type", "--clearmodifiers", "--delay", "12", "--", text]);
            }
            crate::clipboard::clipboard_write(text)?;
            std::thread::sleep(std::time::Duration::from_millis(60));
            key_press("ctrl+v")
        }
        InputBackend::Xdotool => run_xdotool(&["type", "--clearmodifiers", "--delay", "12", "--", text]),
        InputBackend::Ydotool => {
            if text.is_ascii() {
                run_ydotool(&["type", "--", text])
            } else {
                // ydotool `type` cannot emit non-ASCII UTF-8 (e.g. CJK); stage via clipboard + Ctrl+V.
                crate::clipboard::clipboard_write(text)?;
                std::thread::sleep(std::time::Duration::from_millis(60));
                key_press("ctrl+v")
            }
        }
        InputBackend::Wtype => {
            let out = Command::new("wtype").args(["--", text]).output()?;
            if out.status.success() {
                return Ok(());
            }
            let err = String::from_utf8_lossy(&out.stderr);
            Err(UiError::Platform(format!("wtype type failed: {}", err.trim())).into())
        }
    }
}

/// Query current cursor coordinates `(x, y)` if `xdotool` is available.
pub fn get_cursor_pos() -> Result<(i32, i32)> {
    match resolve_input_backend(InputKind::CursorQuery)? {
        InputBackend::X11XTest => x11_xtest::query_pointer(),
        InputBackend::Xdotool => {
            let out = Command::new("xdotool")
                .args(["getmouselocation", "--shell"])
                .output()?;
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                let mut x = None;
                let mut y = None;
                for line in s.lines() {
                    if let Some(v) = line.strip_prefix("X=") {
                        x = v.trim().parse::<i32>().ok();
                    } else if let Some(v) = line.strip_prefix("Y=") {
                        y = v.trim().parse::<i32>().ok();
                    }
                }
                if let (Some(px), Some(py)) = (x, y) {
                    return Ok((px, py));
                }
            }
            Err(UiError::Platform("xdotool getmouselocation failed".into()).into())
        }
        InputBackend::Ydotool | InputBackend::Wtype => Err(UiError::Platform(
            "cursor coordinate query requires xdotool on X11/XWayland".into(),
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::normalise_key;

    #[test]
    fn normalises_braced_and_combo_keys() {
        assert_eq!(normalise_key("{ENTER}"), "Return");
        assert_eq!(normalise_key("{TAB}"), "Tab");
        assert_eq!(normalise_key("{ESC}"), "Escape");
        assert_eq!(normalise_key("ctrl+j"), "ctrl+j");
        assert_eq!(normalise_key("alt+f4"), "alt+F4");
        assert_eq!(normalise_key("Ctrl+Shift+Enter"), "ctrl+shift+Return");
    }

    #[test]
    fn maps_common_evdev_keycodes() {
        assert_eq!(super::map_evdev_keycode("Return").unwrap(), 28);
        assert_eq!(super::map_evdev_keycode("ctrl").unwrap(), 29);
        assert_eq!(super::map_evdev_keycode("v").unwrap(), 47);
        assert!(super::map_evdev_keycode("unknown_key_xyz").is_err());
    }
}
