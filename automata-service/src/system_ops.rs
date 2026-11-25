//! System, SendKeys/Input, and Clipboard command implementations shared across
//! CLI, Rust callers, and MCP.

use std::process::Command;
use std::time::Duration;

use automata_linux::coords::{CoordResolver, ScreenPoint, WindowId};
use automata_linux::doctor::EnvProfile;
use automata_linux::input::{
    get_cursor_pos, key_press, mouse_click, mouse_drag, mouse_move, mouse_scroll, type_text,
};
use automata_linux::runtime::command::run_bounded;
use automata_linux::{clipboard_read, clipboard_write};
use serde_json::{Value, json};

const DEFAULT_MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

use crate::ServiceError;
use crate::protocol::{ClipboardCommand, InputCommand, SystemCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOp {
    Press(String),
    Text(String),
}

/// Parses SendKeys-compatible expressions (`{ctrl}v`, `{ctrl}(ac)`, `{alt}{F4}`, `hello{{x}}{enter}`, `ctrl+j`)
/// into atomic [`KeyOp`] operations.
pub fn parse_send_keys(expr: &str) -> Result<Vec<KeyOp>, ServiceError> {
    if !expr.contains('{') && !expr.contains('}') {
        return Ok(vec![KeyOp::Press(expr.to_string())]);
    }
    let chars: Vec<char> = expr.chars().collect();
    let mut ops = Vec::new();
    let mut mods: Vec<String> = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        if chars[i] == '{' {
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                ops.push(KeyOp::Text("{".into()));
                i += 2;
                continue;
            }
            let Some(rel_end) = chars[i + 1..].iter().position(|&c| c == '}') else {
                return Err(ServiceError::InvalidInput(format!(
                    "unclosed '{{' in key expression '{expr}'"
                )));
            };
            let token: String = chars[i + 1..i + 1 + rel_end].iter().collect();
            let lower = token.trim().to_ascii_lowercase();
            match lower.as_str() {
                "ctrl" | "control" => mods.push("ctrl".into()),
                "alt" => mods.push("alt".into()),
                "shift" => mods.push("shift".into()),
                "win" | "super" | "meta" => mods.push("super".into()),
                _ => {
                    if mods.is_empty() {
                        ops.push(KeyOp::Press(format!("{{{token}}}")));
                    } else {
                        let mut combo = mods.clone();
                        combo.push(token);
                        ops.push(KeyOp::Press(combo.join("+")));
                        mods.clear();
                    }
                }
            }
            i += rel_end + 2;
            continue;
        }

        if chars[i] == '}' && i + 1 < chars.len() && chars[i + 1] == '}' {
            ops.push(KeyOp::Text("}".into()));
            i += 2;
            continue;
        }

        if !mods.is_empty() && chars[i] == '(' {
            let Some(rel_end) = chars[i + 1..].iter().position(|&c| c == ')') else {
                return Err(ServiceError::InvalidInput(format!(
                    "unclosed '(' modifier group in key expression '{expr}'"
                )));
            };
            for &ch in &chars[i + 1..i + 1 + rel_end] {
                let mut combo = mods.clone();
                combo.push(ch.to_string());
                ops.push(KeyOp::Press(combo.join("+")));
            }
            mods.clear();
            i += rel_end + 2;
            continue;
        }

        if !mods.is_empty() {
            let mut combo = mods.clone();
            combo.push(chars[i].to_string());
            ops.push(KeyOp::Press(combo.join("+")));
            mods.clear();
            i += 1;
            continue;
        }

        let start = i;
        while i < chars.len() && chars[i] != '{' && chars[i] != '}' {
            i += 1;
        }
        let lit: String = chars[start..i].iter().collect();
        if !lit.is_empty() {
            ops.push(KeyOp::Text(lit));
        }
    }

    if !mods.is_empty() {
        return Err(ServiceError::InvalidInput(format!(
            "dangling modifier without target key in '{expr}'"
        )));
    }

    Ok(ops)
}

/// Resolves any [`InputCommand`]'s [`automata_linux::coords::SpacePoint`] using `window_bounds`
/// and injects mouse/keyboard events cleanly.
pub fn execute_input_command<F>(
    cmd: &InputCommand,
    mut window_bounds: F,
) -> Result<Value, ServiceError>
where
    F: FnMut(WindowId) -> Result<(i32, i32, i32, i32), ServiceError>,
{
    let mut to_screen = |pt: automata_linux::coords::SpacePoint| -> Result<ScreenPoint, ServiceError> {
        let mut inner_err: Option<ServiceError> = None;
        let res = CoordResolver::to_screen_point(pt, |wid| match window_bounds(wid) {
            Ok(b) => Ok(b),
            Err(e) => {
                inner_err = Some(e);
                Err(automata_linux::UiError::Platform("window bounds lookup failed".into()))
            }
        });
        if let Some(err) = inner_err {
            return Err(err);
        }
        res.map_err(ServiceError::from)
    };

    match cmd {
        InputCommand::MouseMove { point } => {
            let sp = to_screen(*point)?;
            mouse_move(sp.x, sp.y)?;
            Ok(json!({ "ok": true, "x": sp.x, "y": sp.y }))
        }
        InputCommand::MouseClick { point, button } => {
            let sp = to_screen(*point)?;
            let mapped = match button {
                crate::protocol::ClickButtonArg::Left => automata_linux::input::ClickType::Left,
                crate::protocol::ClickButtonArg::Right => automata_linux::input::ClickType::Right,
                crate::protocol::ClickButtonArg::Middle => automata_linux::input::ClickType::Middle,
                crate::protocol::ClickButtonArg::Double => automata_linux::input::ClickType::Double,
                crate::protocol::ClickButtonArg::Triple => automata_linux::input::ClickType::Triple,
            };
            mouse_click(sp.x, sp.y, mapped)?;
            Ok(json!({ "ok": true, "x": sp.x, "y": sp.y, "button": button }))
        }
        InputCommand::MouseDrag { from, to } => {
            let s1 = to_screen(*from)?;
            let s2 = to_screen(*to)?;
            mouse_drag(s1.x, s1.y, s2.x, s2.y)?;
            Ok(json!({ "ok": true }))
        }
        InputCommand::MouseScroll {
            point,
            delta_x,
            delta_y,
        } => {
            let sp = to_screen(*point)?;
            mouse_scroll(sp.x, sp.y, *delta_x, *delta_y)?;
            Ok(json!({ "ok": true }))
        }
        InputCommand::KeyPress { keys } => {
            for op in parse_send_keys(keys)? {
                match op {
                    KeyOp::Press(k) => key_press(&k)?,
                    KeyOp::Text(t) => type_text(&t)?,
                }
            }
            Ok(json!({ "ok": true, "keys": keys }))
        }
        InputCommand::TypeText { text } => {
            type_text(text)?;
            Ok(json!({ "ok": true, "len": text.chars().count() }))
        }
        InputCommand::GetCursorPos => {
            let (x, y) = get_cursor_pos()?;
            Ok(json!({ "x": x, "y": y }))
        }
    }
}

pub fn execute_clipboard_command(cmd: &ClipboardCommand) -> Result<Value, ServiceError> {
    match cmd {
        ClipboardCommand::Read => {
            let text = clipboard_read()?;
            Ok(json!({ "text": text }))
        }
        ClipboardCommand::Write { text } => {
            clipboard_write(text)?;
            Ok(json!({ "ok": true, "len": text.len() }))
        }
    }
}

pub fn execute_system_command(cmd: &SystemCommand) -> Result<Value, ServiceError> {
    match cmd {
        SystemCommand::Ping => Ok(json!({ "pong": true })),
        SystemCommand::Whoami => {
            let username = std::env::var("USER")
                .or_else(|_| std::env::var("LOGNAME"))
                .unwrap_or_else(|_| "unknown".into());
            let home_dir = std::env::var("HOME").unwrap_or_default();
            Ok(json!({ "username": username, "home_dir": home_dir }))
        }
        SystemCommand::Hostname => {
            let host = std::fs::read_to_string("/etc/hostname")
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "localhost".into());
            Ok(json!({ "hostname": host }))
        }
        SystemCommand::Env => {
            let vars: std::collections::BTreeMap<String, String> = std::env::vars().collect();
            Ok(serde_json::to_value(vars).map_err(|e| ServiceError::Internal(e.to_string()))?)
        }
        SystemCommand::GetPath => {
            let path = std::env::var("PATH").unwrap_or_default();
            let entries: Vec<&str> = path.split(':').filter(|s| !s.is_empty()).collect();
            Ok(json!({ "PATH": path, "entries": entries }))
        }
        SystemCommand::IpConfig => {
            let mut cmd = Command::new("ip");
            cmd.args(["-j", "addr"]);
            if let Ok(out) = run_bounded(cmd, "ip -j addr", Duration::from_secs(5), DEFAULT_MAX_OUTPUT_BYTES) {
                if out.status.success() {
                    if let Ok(val) = serde_json::from_slice::<Value>(&out.stdout) {
                        return Ok(val);
                    }
                }
            }
            let mut fallback = Command::new("ip");
            fallback.args(["addr"]);
            let out = run_bounded(fallback, "ip addr", Duration::from_secs(5), DEFAULT_MAX_OUTPUT_BYTES)?;
            Ok(json!({ "raw": String::from_utf8_lossy(&out.stdout) }))
        }
        SystemCommand::RouteTable => {
            let mut cmd = Command::new("ip");
            cmd.args(["-j", "route"]);
            if let Ok(out) = run_bounded(cmd, "ip -j route", Duration::from_secs(5), DEFAULT_MAX_OUTPUT_BYTES) {
                if out.status.success() {
                    if let Ok(val) = serde_json::from_slice::<Value>(&out.stdout) {
                        return Ok(val);
                    }
                }
            }
            let mut fallback = Command::new("ip");
            fallback.args(["route"]);
            let out = run_bounded(fallback, "ip route", Duration::from_secs(5), DEFAULT_MAX_OUTPUT_BYTES)?;
            Ok(json!({ "raw": String::from_utf8_lossy(&out.stdout) }))
        }
        SystemCommand::ListProcesses => {
            let procs: Vec<Value> = automata_linux::process::list_processes()?
                .into_iter()
                .map(|(pid, name)| json!({ "pid": pid, "name": name }))
                .collect();
            Ok(Value::Array(procs))
        }
        SystemCommand::KillProcess { pid, process_name } => {
            if let Some(p) = pid {
                automata_linux::process::kill_process_by_pid(*p)?;
                return Ok(json!({ "killed_pids": [p] }));
            }
            if let Some(name) = process_name {
                let killed = automata_linux::process::kill_processes_by_name(name)?;
                return Ok(json!({ "killed_pids": killed }));
            }
            Err(ServiceError::InvalidInput(
                "kill_process requires 'pid' or 'process_name'".into(),
            ))
        }
        SystemCommand::Exec { args, timeout_secs } => {
            if args.is_empty() {
                return Err(ServiceError::InvalidInput(
                    "system exec 'args' array cannot be empty".into(),
                ));
            }
            let timeout = Duration::from_secs(*timeout_secs);
            let mut cmd = Command::new(&args[0]);
            if args.len() > 1 {
                cmd.args(&args[1..]);
            }
            let out = run_bounded(
                cmd,
                &format!("system.exec {}", args[0]),
                timeout,
                DEFAULT_MAX_OUTPUT_BYTES,
            )?;
            Ok(json!({
                "exit_code": out.status.code(),
                "success": out.status.success(),
                "stdout": String::from_utf8_lossy(&out.stdout),
                "stderr": String::from_utf8_lossy(&out.stderr),
                "truncated": out.truncated,
            }))
        }
        SystemCommand::Doctor { profile } => {
            let prof = match profile.as_deref().unwrap_or("auto").to_ascii_lowercase().as_str() {
                "x11" => EnvProfile::X11,
                "wayland" => EnvProfile::Wayland,
                "xvfb" => EnvProfile::VirtualX11,
                "ocr" => EnvProfile::Ocr,
                "all" => EnvProfile::All,
                _ => EnvProfile::Auto,
            };
            let report = automata_linux::doctor::inspect_environment(prof);
            serde_json::to_value(report).map_err(|e| ServiceError::Internal(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_send_keys_expressions() {
        assert_eq!(
            parse_send_keys("{ctrl}v").unwrap(),
            vec![KeyOp::Press("ctrl+v".into())]
        );
        assert_eq!(
            parse_send_keys("{ctrl}(ac)").unwrap(),
            vec![
                KeyOp::Press("ctrl+a".into()),
                KeyOp::Press("ctrl+c".into()),
            ]
        );
        assert_eq!(
            parse_send_keys("hello{enter}").unwrap(),
            vec![KeyOp::Text("hello".into()), KeyOp::Press("{enter}".into())]
        );
    }
}
