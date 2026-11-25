//! Thin MCP translation layer for `input`, `clipboard`, `browser`, `file`, and `system`.

use std::sync::Arc;

use automata_linux::coords::SpacePoint;
use automata_service::protocol::{
    BrowserCommand, ClickButtonArg, ClipboardCommand, Command, FileCommand, InputCommand,
    SystemCommand,
};
use serde_json::Value;

use crate::state::AgentState;
use crate::tools::{opt_i32, opt_str, opt_u64, req_i32, req_str};

fn translate_input_command(args: &Value) -> anyhow::Result<InputCommand> {
    let action = req_str(args, "action")?;
    Ok(match action.as_str() {
        "mouse_move" => InputCommand::MouseMove {
            point: SpacePoint::screen(req_i32(args, "x")?, req_i32(args, "y")?),
        },
        "mouse_click" => {
            let button = match opt_str(args, "button").as_deref().unwrap_or("left") {
                "left" => ClickButtonArg::Left,
                "right" => ClickButtonArg::Right,
                "middle" => ClickButtonArg::Middle,
                "double" => ClickButtonArg::Double,
                "triple" => ClickButtonArg::Triple,
                other => anyhow::bail!("invalid mouse button '{other}'"),
            };
            InputCommand::MouseClick {
                point: SpacePoint::screen(req_i32(args, "x")?, req_i32(args, "y")?),
                button,
            }
        }
        "mouse_drag" => InputCommand::MouseDrag {
            from: SpacePoint::screen(req_i32(args, "x")?, req_i32(args, "y")?),
            to: SpacePoint::screen(req_i32(args, "x2")?, req_i32(args, "y2")?),
        },
        "mouse_scroll" => InputCommand::MouseScroll {
            point: SpacePoint::screen(req_i32(args, "x")?, req_i32(args, "y")?),
            delta_x: opt_i32(args, "delta_x").unwrap_or(0),
            delta_y: opt_i32(args, "delta_y").unwrap_or(0),
        },
        "key_press" => InputCommand::KeyPress {
            keys: req_str(args, "keys")?,
        },
        "type_text" => InputCommand::TypeText {
            text: req_str(args, "text")?,
        },
        "get_cursor_pos" => InputCommand::GetCursorPos,
        other => anyhow::bail!("unsupported input action '{other}'"),
    })
}

fn translate_clipboard_command(args: &Value) -> anyhow::Result<ClipboardCommand> {
    match req_str(args, "action")?.as_str() {
        "read" => Ok(ClipboardCommand::Read),
        "write" => Ok(ClipboardCommand::Write {
            text: req_str(args, "text")?,
        }),
        other => anyhow::bail!("unsupported clipboard action '{other}'"),
    }
}

fn translate_browser_command(args: &Value) -> anyhow::Result<BrowserCommand> {
    match req_str(args, "action")?.as_str() {
        "ensure" => Ok(BrowserCommand::Ensure),
        "tabs" => Ok(BrowserCommand::Tabs),
        "open" => Ok(BrowserCommand::Open {
            url: opt_str(args, "url"),
        }),
        "activate" => Ok(BrowserCommand::Activate {
            tab_id: req_str(args, "tab_id")?,
        }),
        "close" => Ok(BrowserCommand::Close {
            tab_id: req_str(args, "tab_id")?,
        }),
        "navigate" => Ok(BrowserCommand::Navigate {
            tab_id: req_str(args, "tab_id")?,
            url: req_str(args, "url")?,
        }),
        "dom" => Ok(BrowserCommand::Dom {
            tab_id: req_str(args, "tab_id")?,
        }),
        "eval" => Ok(BrowserCommand::Eval {
            tab_id: req_str(args, "tab_id")?,
            expression: req_str(args, "expression")?,
        }),
        "screenshot" => Ok(BrowserCommand::Screenshot {
            tab_id: req_str(args, "tab_id")?,
        }),
        other => anyhow::bail!("unsupported browser action '{other}'"),
    }
}

fn translate_system_command(args: &Value) -> anyhow::Result<SystemCommand> {
    match req_str(args, "action")?.as_str() {
        "ping" => Ok(SystemCommand::Ping),
        "whoami" => Ok(SystemCommand::Whoami),
        "hostname" => Ok(SystemCommand::Hostname),
        "env" => Ok(SystemCommand::Env),
        "get_path" => Ok(SystemCommand::GetPath),
        "ipconfig" => Ok(SystemCommand::IpConfig),
        "route_table" => Ok(SystemCommand::RouteTable),
        "list_processes" | "processes" => Ok(SystemCommand::ListProcesses),
        "kill_process" => Ok(SystemCommand::KillProcess {
            pid: opt_u64(args, "pid").and_then(|p| u32::try_from(p).ok()),
            process_name: opt_str(args, "process_name").or_else(|| opt_str(args, "process")),
        }),
        "exec" => {
            let args_arr = args
                .get("args")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow::anyhow!("system exec requires non-empty 'args' array"))?;
            let argv: Vec<String> = args_arr
                .iter()
                .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))
                .collect();
            Ok(SystemCommand::Exec {
                args: argv,
                timeout_secs: opt_u64(args, "timeout_secs").unwrap_or(60),
            })
        }
        other => anyhow::bail!("unsupported system action '{other}'"),
    }
}

pub async fn handle_input(args: Value, state: Arc<AgentState>) -> anyhow::Result<Value> {
    let cmd = Command::Input(translate_input_command(&args)?);
    state
        .service
        .call_async(cmd)
        .await
        .map(|r| r.into_value())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub async fn handle_clipboard(args: Value, state: Arc<AgentState>) -> anyhow::Result<Value> {
    let cmd = Command::Clipboard(translate_clipboard_command(&args)?);
    state
        .service
        .call_async(cmd)
        .await
        .map(|r| r.into_value())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub async fn handle_browser(args: Value, state: Arc<AgentState>) -> anyhow::Result<Value> {
    let cmd = Command::Browser(translate_browser_command(&args)?);
    state
        .service
        .call_async(cmd)
        .await
        .map(|r| r.into_value())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub async fn handle_file_with_policy(
    args: Value,
    state: Arc<AgentState>,
) -> anyhow::Result<Value> {
    let cmd = Command::File(FileCommand::Raw(args));
    state
        .service
        .call_async(cmd)
        .await
        .map(|r| r.into_value())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub async fn handle_system(args: Value, state: Arc<AgentState>) -> anyhow::Result<Value> {
    let cmd = Command::System(translate_system_command(&args)?);
    state
        .service
        .call_async(cmd)
        .await
        .map(|r| r.into_value())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use automata_service::file_ops::execute_file_request;
    use automata_service::system_ops::{KeyOp, execute_system_command, parse_send_keys};
    use serde_json::json;

    use super::translate_system_command;

    #[test]
    fn parses_windows_send_keys_expressions() {
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
            parse_send_keys("{alt}{F4}").unwrap(),
            vec![KeyOp::Press("alt+F4".into())]
        );
        assert_eq!(
            parse_send_keys("hello{{x}}{enter}").unwrap(),
            vec![
                KeyOp::Text("hello".into()),
                KeyOp::Text("{".into()),
                KeyOp::Text("x".into()),
                KeyOp::Text("}".into()),
                KeyOp::Press("{enter}".into()),
            ]
        );
        assert_eq!(
            parse_send_keys("ctrl+j").unwrap(),
            vec![KeyOp::Press("ctrl+j".into())]
        );
    }

    #[tokio::test]
    async fn file_and_system_handlers_roundtrip() {
        let dir = std::env::temp_dir().join(format!("automata_agent_fs_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        execute_file_request(
            json!({ "action": "mkdir", "path": dir.display().to_string() }),
            true,
        )
        .unwrap();

        let fpath = dir.join("sample.txt");
        let fstr = fpath.display().to_string();
        execute_file_request(
            json!({
                "action": "write",
                "path": fstr,
                "content": "line1\nline2\nline3\n",
                "overwrite": true
            }),
            true,
        )
        .unwrap();

        let read = execute_file_request(json!({ "action": "read", "path": fstr }), true).unwrap();
        assert_eq!(read["encoding"], json!("utf-8"));

        let ping = execute_system_command(&translate_system_command(&json!({ "action": "ping" })).unwrap()).unwrap();
        assert_eq!(ping["pong"], json!(true));

        let exec_res = execute_system_command(
            &translate_system_command(&json!({
                "action": "exec",
                "args": ["sh", "-c", "echo mcp-ok"]
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(exec_res["success"], json!(true));
        assert_eq!(exec_res["stdout"].as_str().unwrap().trim(), "mcp-ok");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
