pub mod platform;
pub mod ui;
pub mod workflows;

use std::borrow::Cow;
use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolResult, Content, JsonObject, Tool};
use rmcp::service::{RequestContext, RoleServer};
use serde_json::{Value, json};

use crate::state::AgentState;

/// Format synthetic HWND as `"0x..."` hex string (matching Windows MCP convention).
#[cfg(test)]
pub fn format_hwnd(hwnd: u64) -> String {
    format!("0x{hwnd:08x}")
}

/// Parse HWND from `"0x..."` hex string, 53-bit-safe decimal string, or JSON number.
pub fn parse_hwnd_value(val: &Value) -> anyhow::Result<u64> {
    if let Some(n) = val.as_u64() {
        anyhow::ensure!(
            n <= automata_linux::MAX_SAFE_JS_HWND,
            "hwnd {n} exceeds 53-bit JavaScript safe integer range (max {}); pass the 0x-prefixed hex string from list_windows",
            automata_linux::MAX_SAFE_JS_HWND
        );
        return Ok(n);
    }
    if let Some(s) = val.as_str() {
        return parse_hwnd_str(s);
    }
    anyhow::bail!("hwnd must be a hex string (0x...) or integer")
}

pub fn parse_hwnd_str(raw: &str) -> anyhow::Result<u64> {
    let trimmed = raw.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return u64::from_str_radix(hex, 16)
            .map_err(|e| anyhow::anyhow!("invalid hex hwnd '{raw}': {e}"));
    }
    let parsed = trimmed
        .parse::<u64>()
        .map_err(|e| anyhow::anyhow!("invalid hwnd '{raw}' (expected 0x-hex or decimal): {e}"))?;
    anyhow::ensure!(
        parsed <= automata_linux::MAX_SAFE_JS_HWND,
        "decimal hwnd {parsed} exceeds 53-bit JavaScript safe integer range (max {}); pass the 0x-prefixed hex string from list_windows",
        automata_linux::MAX_SAFE_JS_HWND
    );
    Ok(parsed)
}

pub fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

pub fn opt_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
    })
}

pub fn opt_i32(args: &Value, key: &str) -> Option<i32> {
    args.get(key)
        .and_then(|v| v.as_i64().and_then(|i| i32::try_from(i).ok()))
}

pub fn opt_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

pub fn req_str(args: &Value, key: &str) -> anyhow::Result<String> {
    opt_str(args, key).ok_or_else(|| anyhow::anyhow!("missing required parameter '{key}'"))
}

pub fn req_i32(args: &Value, key: &str) -> anyhow::Result<i32> {
    opt_i32(args, key).ok_or_else(|| anyhow::anyhow!("missing required integer parameter '{key}'"))
}

pub fn ok_json(val: Value) -> CallToolResult {
    let text = serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string());
    let mut res = CallToolResult::success(vec![Content::text(text)]);
    if val.is_object() {
        res.structured_content = Some(val);
    }
    res
}

pub fn err_result(err: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(err.to_string())])
}

fn make_tool(name: &'static str, description: &'static str, schema: Value) -> Tool {
    let obj: JsonObject = serde_json::from_value(schema).unwrap_or_default();
    Tool::new(Cow::Borrowed(name), Cow::Borrowed(description), Arc::new(obj))
}

pub fn all_tools() -> Vec<Tool> {
    vec![
        make_tool(
            "desktop",
            "Inspect Linux desktop windows and AT-SPI2 accessibility element trees (list_windows, element_tree, find_elements, focused_window, active_window_tree).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list_windows", "element_tree", "get_element_tree", "find_elements", "focused_window", "active_window_tree"]
                    },
                    "process": { "type": "string", "description": "Filter by process name" },
                    "hwnd": { "type": ["string", "integer"], "description": "Synthetic HWND hex string (0x...) or integer" },
                    "pid": { "type": "integer", "description": "Filter by process PID" },
                    "title": { "type": "string", "description": "Window title exact or case-insensitive substring" },
                    "automation_id": { "type": "string" },
                    "selector": { "type": "string", "description": "UI selector filter or query" },
                    "max_results": { "type": "integer" },
                    "include_ancestors": { "type": "boolean", "default": true },
                    "include_siblings": { "type": "boolean", "default": false }
                },
                "required": ["action"]
            }),
        ),
        make_tool(
            "vision",
            "Screenshot and OCR layout analysis for windows or full screen when AT-SPI2 does not expose custom widgets.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["window_layout", "window_probe", "screen_layout", "capture"]
                    },
                    "hwnd": { "type": ["string", "integer"] },
                    "capture_only": { "type": "boolean", "default": false },
                    "screenshot": { "type": "boolean", "default": false },
                    "save_screenshot": { "type": "boolean", "default": false },
                    "path": { "type": "string" },
                    "region": {
                        "type": "object",
                        "properties": {
                            "x": { "type": "integer" },
                            "y": { "type": "integer" },
                            "width": { "type": "integer" },
                            "height": { "type": "integer" }
                        }
                    }
                },
                "required": ["action"]
            }),
        ),
        make_tool(
            "app",
            "List installed Linux desktop applications, launch applications/desktop files, focus or kill running apps.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["launch", "list_installed", "focus", "kill", "list_taskbar", "show_desktop"],
                        "default": "launch"
                    },
                    "target": { "type": "string", "description": "Binary name/path or .desktop ID" },
                    "exe": { "type": "string", "description": "Alias for target" },
                    "app": { "type": "string", "description": "Alias for target" },
                    "launch_wait": { "type": "string", "enum": ["new_pid", "new_window", "match_any"], "default": "new_pid" },
                    "wait": { "type": "string", "enum": ["new_pid", "new_window", "match_any"] },
                    "timeout_secs": { "type": "number", "default": 30 },
                    "timeout_ms": { "type": "integer" },
                    "process": { "type": "string" },
                    "process_name": { "type": "string" },
                    "pid": { "type": "integer" },
                    "title": { "type": "string" },
                    "window_index": { "type": "integer", "default": 0 },
                    "filter": { "type": "string" }
                }
            }),
        ),
        make_tool(
            "window",
            "Manage top-level windows by synthetic HWND (minimize, maximize, restore, close, focus, set_bounds, get_bounds, screenshot).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["minimize", "maximize", "restore", "close", "focus", "set_bounds", "get_bounds", "screenshot"]
                    },
                    "hwnd": { "type": ["string", "integer"] },
                    "process": { "type": "string" },
                    "title": { "type": "string" },
                    "x": { "type": "integer" },
                    "y": { "type": "integer" },
                    "width": { "type": "integer" },
                    "height": { "type": "integer" },
                    "path": { "type": "string" }
                },
                "required": ["action"]
            }),
        ),
        make_tool(
            "run_actions",
            "Execute UI-Automata steps directly against a target window using persistent ShadowDom handle caching inside the MCP server.",
            json!({
                "type": "object",
                "properties": {
                    "window": {
                        "type": "object",
                        "properties": {
                            "process": { "type": "string" },
                            "pid": { "type": "integer" },
                            "title": { "type": "string" },
                            "hwnd": { "type": ["string", "integer"] },
                            "selector": { "type": "string" }
                        }
                    },
                    "steps": { "type": "array", "items": { "type": "object" } },
                    "reset": { "type": "boolean", "default": false },
                    "timeout_secs": { "type": "number", "default": 10 }
                },
                "required": ["window", "steps"]
            }),
        ),
        make_tool(
            "start_workflow",
            "Load and run a complete YAML workflow file or inline YAML string with parameter overrides, streaming phase progress and returning extracted outputs.",
            json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string" },
                    "local_file": { "type": "string" },
                    "yaml": { "type": "string" },
                    "name": { "type": "string" },
                    "params": { "type": "object", "additionalProperties": true },
                    "background": { "type": "boolean", "default": false }
                }
            }),
        ),
        make_tool(
            "workflow",
            "Manage workflows: list available scripts, read YAML, lint YAML, inspect active run status, cancel running workflow, or view recent run logs.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "read", "lint", "status", "cancel", "list_runs", "logs"]
                    },
                    "yaml": { "type": "string" },
                    "name": { "type": "string" },
                    "file": { "type": "string" },
                    "limit": { "type": "integer", "default": 20 }
                },
                "required": ["action"]
            }),
        ),
        make_tool(
            "input",
            "Low-level synthetic mouse and keyboard input (mouse_move, mouse_click, mouse_drag, mouse_scroll, key_press, type_text, get_cursor_pos).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["mouse_move", "mouse_click", "mouse_drag", "mouse_scroll", "key_press", "type_text", "get_cursor_pos"]
                    },
                    "x": { "type": "integer" },
                    "y": { "type": "integer" },
                    "x2": { "type": "integer" },
                    "y2": { "type": "integer" },
                    "delta_x": { "type": "integer", "default": 0 },
                    "delta_y": { "type": "integer", "default": 0 },
                    "button": { "type": "string", "enum": ["left", "right", "middle", "double", "triple"], "default": "left" },
                    "keys": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["action"]
            }),
        ),
        make_tool(
            "clipboard",
            "Read or write plain text on the Linux clipboard (wl-clipboard on Wayland / xclip or xsel on X11).",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["read", "write"] },
                    "text": { "type": "string" }
                },
                "required": ["action"]
            }),
        ),
        make_tool(
            "browser",
            "Control Chromium/Edge over Chrome DevTools Protocol (ensure, tabs, open, activate, close, navigate, dom, eval, screenshot).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["ensure", "tabs", "open", "activate", "close", "navigate", "dom", "eval", "screenshot"]
                    },
                    "tab_id": { "type": "string" },
                    "url": { "type": "string" },
                    "expression": { "type": "string" }
                },
                "required": ["action"]
            }),
        ),
        make_tool(
            "file",
            "Filesystem operations (list, read, read_lines, write, append, copy, move, delete, mkdir, rmdir, stat, checksum, glob).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "read", "read_lines", "write", "append", "copy", "move", "delete", "mkdir", "rmdir", "stat", "checksum", "glob"]
                    },
                    "path": { "type": "string" },
                    "src": { "type": "string" },
                    "dst": { "type": "string" },
                    "content": { "type": "string" },
                    "encoding": { "type": "string", "enum": ["utf-8", "base64"] },
                    "overwrite": { "type": "boolean", "default": false },
                    "offset": { "type": "integer", "default": 0 },
                    "count": { "type": "integer", "default": 100 },
                    "pattern": { "type": "string" },
                    "root": { "type": "string" }
                },
                "required": ["action"]
            }),
        ),
        make_tool(
            "system",
            "Linux host diagnostics, process management, and command execution without shell (ping, whoami, hostname, env, get_path, ipconfig, route_table, list_processes, kill_process, exec).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["ping", "whoami", "hostname", "env", "get_path", "ipconfig", "route_table", "list_processes", "processes", "kill_process", "exec"]
                    },
                    "args": { "type": "array", "items": { "type": "string" } },
                    "cwd": { "type": "string" },
                    "timeout_secs": { "type": "integer", "default": 60 },
                    "pid": { "type": "integer" },
                    "process_name": { "type": "string" }
                },
                "required": ["action"]
            }),
        ),
        make_tool(
            "resources",
            "List and read built-in example workflows and Agent markdown guides.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "read"] },
                    "name": { "type": "string" }
                },
                "required": ["action"]
            }),
        ),
    ]
}

pub async fn dispatch_tool(
    name: &str,
    args: Value,
    state: Arc<AgentState>,
    ctx: RequestContext<RoleServer>,
) -> Result<CallToolResult, McpError> {
    let outcome = match name {
        "desktop" => ui::handle_desktop(args, state).await,
        "vision" => ui::handle_vision(args, state).await,
        "app" => ui::handle_app(args, state).await,
        "window" => ui::handle_window(args, state).await,
        "run_actions" => ui::handle_run_actions(args, state).await,
        "start_workflow" => workflows::handle_start_workflow(args, state, ctx).await,
        "workflow" => workflows::handle_workflow(args, state).await,
        "resources" => workflows::handle_resources(args, state).await,
        "input" => platform::handle_input(args, state).await,
        "clipboard" => platform::handle_clipboard(args, state).await,
        "browser" => platform::handle_browser(args, state).await,
        "file" => platform::handle_file_with_policy(args, state).await,
        "system" => platform::handle_system(args, state).await,
        other => {
            return Err(McpError::invalid_params(
                format!("unknown tool '{other}'"),
                None,
            ));
        }
    };

    Ok(match outcome {
        Ok(val) => ok_json(val),
        Err(e) => err_result(e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_all_13_mcp_tools() {
        let tools = all_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "desktop",
                "vision",
                "app",
                "window",
                "run_actions",
                "start_workflow",
                "workflow",
                "input",
                "clipboard",
                "browser",
                "file",
                "system",
                "resources",
            ]
        );
    }

    #[test]
    fn parses_and_formats_hwnd_hex_and_decimal() {
        assert_eq!(parse_hwnd_str("0x001A04B2").unwrap(), 0x001A04B2);
        assert_eq!(parse_hwnd_str("0XFF").unwrap(), 255);
        assert_eq!(parse_hwnd_str("12345").unwrap(), 12345);
        assert!(parse_hwnd_str("0xZZZ").is_err());
        assert!(parse_hwnd_value(&json!(u64::MAX)).is_err());
        assert!(parse_hwnd_str("9007199254740992").is_err());
        assert_eq!(format_hwnd(0x1a), "0x0000001a");
    }
}
