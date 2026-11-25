//! Thin MCP translation layer for `desktop`, `vision`, `app`, `window`, and `run_actions`
//! routing all execution to [`automata_service::AutomationService`].

use std::sync::Arc;

use automata_linux::coords::{SpaceRect, WindowId};
use automata_service::protocol::{
    ActCommand, Command, ExploreCommand, LaunchWaitArg, Target, VisionCommand, WindowFilter,
    WindowStateArg,
};
use serde_json::{Value, json};

use crate::state::AgentState;
use crate::tools::{opt_bool, opt_i32, opt_str, opt_u64, parse_hwnd_value, req_str};

fn window_id_from_arg(val: Option<&Value>) -> anyhow::Result<Option<WindowId>> {
    match val {
        Some(v) => Ok(Some(WindowId::new(parse_hwnd_value(v)?)?)),
        None => Ok(None),
    }
}

fn target_from_args(args: &Value) -> anyhow::Result<Target> {
    Ok(Target {
        process: opt_str(args, "process").or_else(|| opt_str(args, "process_name")),
        pid: opt_u64(args, "pid").and_then(|p| u32::try_from(p).ok()),
        title: opt_str(args, "title"),
        automation_id: opt_str(args, "automation_id"),
        selector: opt_str(args, "selector"),
        window_id: window_id_from_arg(args.get("hwnd"))?,
    })
}

async fn run_cmd(state: &Arc<AgentState>, cmd: Command) -> anyhow::Result<Value> {
    state
        .service
        .call_async(cmd)
        .await
        .map(|r| r.into_value())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub async fn handle_desktop(args: Value, state: Arc<AgentState>) -> anyhow::Result<Value> {
    let action = req_str(&args, "action")?;
    let target = target_from_args(&args)?;
    let cmd = match action.as_str() {
        "list_windows" => Command::Explore(ExploreCommand::ListWindows {
            filter: WindowFilter {
                process: target.process,
                pid: target.pid,
                title: target.title,
                automation_id: target.automation_id,
                window_id: target.window_id,
            },
        }),
        "focused_window" => Command::Explore(ExploreCommand::FocusedWindow),
        "element_tree" | "get_element_tree" => Command::Explore(ExploreCommand::WindowTree {
            target,
            max_results: opt_u64(&args, "max_results").unwrap_or(10) as usize,
        }),
        "active_window_tree" => Command::Explore(ExploreCommand::ActiveWindowTree {
            selector: target.selector,
            max_results: opt_u64(&args, "max_results").unwrap_or(10) as usize,
        }),
        "find_elements" => {
            let selector = target
                .selector
                .clone()
                .ok_or_else(|| anyhow::anyhow!("find_elements requires 'selector'"))?;
            Command::Explore(ExploreCommand::FindElements {
                target,
                selector,
                include_ancestors: opt_bool(&args, "include_ancestors", true),
                include_siblings: opt_bool(&args, "include_siblings", false),
                max_results: opt_u64(&args, "max_results").unwrap_or(20) as usize,
            })
        }
        other => anyhow::bail!("unsupported desktop action '{other}'"),
    };
    run_cmd(&state, cmd).await
}

pub async fn handle_vision(args: Value, state: Arc<AgentState>) -> anyhow::Result<Value> {
    let action = req_str(&args, "action")?;
    let window_id = window_id_from_arg(args.get("hwnd"))?;
    let capture_only = opt_bool(&args, "capture_only", false);
    let save_screenshot =
        opt_bool(&args, "screenshot", false) || opt_bool(&args, "save_screenshot", false);
    let path = opt_str(&args, "path");
    let region = args.get("region").and_then(|r| {
        Some(SpaceRect::screen(
            opt_i32(r, "x")?,
            opt_i32(r, "y")?,
            opt_i32(r, "width")?,
            opt_i32(r, "height")?,
        ))
    });

    let cmd = match action.as_str() {
        "window_layout" => Command::Vision(VisionCommand::WindowLayout {
            window_id: window_id.ok_or_else(|| {
                anyhow::anyhow!("vision action 'window_layout' requires 'hwnd'")
            })?,
            capture_only,
            save_screenshot,
        }),
        "window_probe" => Command::Vision(VisionCommand::WindowProbe {
            window_id: window_id.ok_or_else(|| {
                anyhow::anyhow!("vision action 'window_probe' requires 'hwnd'")
            })?,
            capture_only,
            save_screenshot,
        }),
        "screen_layout" => Command::Vision(VisionCommand::ScreenLayout {
            capture_only,
            save_screenshot,
        }),
        "capture" => Command::Vision(VisionCommand::Capture {
            window_id,
            region,
            path,
        }),
        other => anyhow::bail!("unsupported vision action '{other}'"),
    };
    run_cmd(&state, cmd).await
}

pub async fn handle_app(args: Value, state: Arc<AgentState>) -> anyhow::Result<Value> {
    let action = opt_str(&args, "action").unwrap_or_else(|| {
        if args.get("target").is_some() || args.get("exe").is_some() || args.get("app").is_some() {
            "launch".into()
        } else {
            "list_installed".into()
        }
    });

    match action.as_str() {
        "list_installed" => {
            let cmd = Command::Act(ActCommand::ListInstalledApps {
                filter: opt_str(&args, "filter"),
            });
            run_cmd(&state, cmd).await
        }
        "launch" => {
            let target = opt_str(&args, "target")
                .or_else(|| opt_str(&args, "exe"))
                .or_else(|| opt_str(&args, "app"))
                .ok_or_else(|| anyhow::anyhow!("app launch requires 'target', 'exe', or 'app'"))?;
            let wait_str = opt_str(&args, "launch_wait")
                .or_else(|| opt_str(&args, "wait"))
                .unwrap_or_else(|| "new_pid".into());
            let wait = match wait_str.as_str() {
                "new_pid" => LaunchWaitArg::NewPid,
                "new_window" => LaunchWaitArg::NewWindow,
                "match_any" => LaunchWaitArg::MatchAny,
                other => anyhow::bail!("invalid launch_wait '{other}'"),
            };
            let timeout_ms = if let Some(ms) = opt_u64(&args, "timeout_ms") {
                ms
            } else {
                let secs = args
                    .get("timeout_secs")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(30.0)
                    .max(0.1);
                (secs * 1000.0).round() as u64
            };
            run_cmd(
                &state,
                Command::Act(ActCommand::LaunchApp {
                    target,
                    wait,
                    timeout_ms,
                }),
            )
            .await
        }
        "focus" => {
            let mut target = target_from_args(&args)?;
            if target.title.is_none() {
                target.title = opt_str(&args, "button");
            }
            let window_index = opt_u64(&args, "window_index").unwrap_or(0) as usize;
            run_cmd(
                &state,
                Command::Act(ActCommand::FocusApp {
                    target,
                    window_index,
                }),
            )
            .await
        }
        "kill" => {
            let pid = opt_u64(&args, "pid").and_then(|p| u32::try_from(p).ok());
            let process = opt_str(&args, "process")
                .or_else(|| opt_str(&args, "process_name"))
                .or_else(|| opt_str(&args, "target"));
            run_cmd(&state, Command::Act(ActCommand::KillApp { pid, process })).await
        }
        "list_taskbar" => handle_desktop(json!({ "action": "list_windows" }), state).await,
        "show_desktop" => {
            anyhow::bail!(
                "show_desktop is Windows-specific and not implemented on Linux; use window(minimize) on target hwnds"
            )
        }
        other => anyhow::bail!("unsupported app action '{other}'"),
    }
}

pub async fn handle_window(args: Value, state: Arc<AgentState>) -> anyhow::Result<Value> {
    let action = req_str(&args, "action")?;
    let target = target_from_args(&args)?;
    let cmd = match action.as_str() {
        "minimize" => Command::Act(ActCommand::WindowState {
            target,
            state: WindowStateArg::Minimize,
        }),
        "maximize" => Command::Act(ActCommand::WindowState {
            target,
            state: WindowStateArg::Maximize,
        }),
        "restore" => Command::Act(ActCommand::WindowState {
            target,
            state: WindowStateArg::Restore,
        }),
        "close" => Command::Act(ActCommand::WindowState {
            target,
            state: WindowStateArg::Close,
        }),
        "focus" => Command::Act(ActCommand::WindowState {
            target,
            state: WindowStateArg::Focus,
        }),
        "set_bounds" => Command::Act(ActCommand::WindowBounds {
            target,
            x: opt_i32(&args, "x"),
            y: opt_i32(&args, "y"),
            width: opt_i32(&args, "width"),
            height: opt_i32(&args, "height"),
        }),
        "get_bounds" => Command::Act(ActCommand::GetWindowBounds { target }),
        "screenshot" => Command::Act(ActCommand::WindowScreenshot {
            target,
            path: opt_str(&args, "path"),
        }),
        other => anyhow::bail!("unsupported window action '{other}'"),
    };
    run_cmd(&state, cmd).await
}

pub async fn handle_run_actions(args: Value, state: Arc<AgentState>) -> anyhow::Result<Value> {
    let window_val = args
        .get("window")
        .ok_or_else(|| anyhow::anyhow!("run_actions requires 'window' object"))?;
    let steps_val = args
        .get("steps")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("run_actions requires 'steps' array"))?;

    let window = target_from_args(window_val)?;
    if window.is_empty() {
        anyhow::bail!("'window' must specify at least 'process', 'pid', 'title', 'hwnd', or 'selector'");
    }
    let reset = opt_bool(&args, "reset", false);
    let timeout_secs = args
        .get("timeout_secs")
        .and_then(|v| v.as_f64())
        .unwrap_or(10.0)
        .max(0.1);

    run_cmd(
        &state,
        Command::Act(ActCommand::RunSteps {
            window,
            steps: steps_val.clone(),
            reset,
            timeout_secs,
        }),
    )
    .await
}
