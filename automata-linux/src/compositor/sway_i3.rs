//! Sway / i3 IPC compositor backend (`swaymsg` / `i3-msg -t get_tree`).

use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::WindowAction;
use crate::compositor::{
    CompositorBackend, CompositorCapabilities, CompositorKind, CompositorWindow,
};
use crate::runtime::command::run_bounded;

/// Offset separating native Wayland Sway container IDs from 32-bit X11 XIDs.
pub const SWAY_CONTAINER_ID_BASE: u64 = 1 << 32;

pub struct SwayI3Backend;

#[derive(Debug, Deserialize)]
struct TreeNode {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    window: Option<u64>,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    visible: Option<bool>,
    #[serde(default)]
    rect: Option<TreeRect>,
    #[serde(default)]
    window_properties: Option<WindowProps>,
    #[serde(default)]
    nodes: Vec<TreeNode>,
    #[serde(default)]
    floating_nodes: Vec<TreeNode>,
}

#[derive(Debug, Deserialize)]
struct TreeRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Debug, Deserialize)]
struct WindowProps {
    #[serde(default)]
    class: Option<String>,
}

fn ipc_binary() -> &'static str {
    if std::env::var("SWAYSOCK")
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        "swaymsg"
    } else {
        "i3-msg"
    }
}

impl CompositorBackend for SwayI3Backend {
    fn kind(&self) -> CompositorKind {
        CompositorKind::SwayI3
    }

    fn capabilities(&self) -> CompositorCapabilities {
        let bin = ipc_binary();
        let mut cmd = Command::new(bin);
        cmd.args(["-t", "get_tree"]);
        let ok = run_bounded(cmd, "sway/i3 get_tree probe", Duration::from_secs(2), 2 * 1024 * 1024)
            .map(|o| o.status.success())
            .unwrap_or(false);
        CompositorCapabilities {
            kind: CompositorKind::SwayI3,
            available: ok,
            can_list_windows: ok,
            can_report_bounds: ok,
            can_report_stacking: true,
            can_focus: ok,
            can_close: ok,
            can_move_resize: ok,
            detail: if ok {
                format!("{bin} -t get_tree responsive")
            } else {
                format!("{bin} unavailable")
            },
        }
    }

    fn list_windows(&self) -> Result<Vec<CompositorWindow>> {
        let bin = ipc_binary();
        let mut cmd = Command::new(bin);
        cmd.args(["-t", "get_tree"]);
        let out = run_bounded(cmd, "sway/i3 get_tree", Duration::from_secs(3), 4 * 1024 * 1024)?;
        if !out.status.success() {
            anyhow::bail!("{bin} -t get_tree failed");
        }
        parse_i3_sway_tree(&String::from_utf8_lossy(&out.stdout))
    }

    fn focus_window(&self, native_id: u64, _pid: Option<u32>, title: Option<&str>) -> Result<()> {
        let selector = criteria_for(native_id, title)?;
        run_ipc(&format!("{selector} focus"))
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
        let selector = criteria_for(native_id, title)?;
        run_ipc(&format!(
            "{selector} floating enable, move absolute position {x} px {y} px, resize set {width} px {height} px"
        ))
    }

    fn close_window(&self, native_id: u64, _pid: Option<u32>, title: Option<&str>) -> Result<()> {
        let selector = criteria_for(native_id, title)?;
        run_ipc(&format!("{selector} kill"))
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
            WindowAction::Maximize => {
                let selector = criteria_for(native_id, title)?;
                run_ipc(&format!("{selector} fullscreen enable"))
            }
            WindowAction::Restore => {
                let selector = criteria_for(native_id, title)?;
                run_ipc(&format!("{selector} fullscreen disable"))
            }
            WindowAction::Minimize => {
                let selector = criteria_for(native_id, title)?;
                run_ipc(&format!("{selector} move scratchpad"))
            }
        }
    }
}

fn criteria_for(native_id: u64, title: Option<&str>) -> Result<String> {
    if native_id >= SWAY_CONTAINER_ID_BASE {
        return Ok(format!("[con_id={}]", native_id - SWAY_CONTAINER_ID_BASE));
    }
    if native_id > 0 {
        return Ok(format!("[id={native_id}]"));
    }
    if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
        let escaped = t.replace('"', "\\\"");
        return Ok(format!("[title=\"{escaped}\"]"));
    }
    anyhow::bail!("no con_id, X11 id, or title to target Sway/i3 window")
}

fn run_ipc(command_str: &str) -> Result<()> {
    let bin = ipc_binary();
    let mut cmd = Command::new(bin);
    cmd.arg(command_str);
    let out = run_bounded(cmd, "sway/i3 ipc", Duration::from_secs(2), 64 * 1024)?;
    if out.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "{bin} command failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
    }
}

pub fn parse_i3_sway_tree(json_str: &str) -> Result<Vec<CompositorWindow>> {
    let root: TreeNode = serde_json::from_str(json_str).context("parse Sway/i3 JSON tree")?;
    let mut windows = Vec::new();
    collect_windows(&root, &mut windows);
    windows.sort_by_key(|w| (!w.is_focused, w.native_id));
    for (rank, w) in windows.iter_mut().enumerate() {
        w.z_order_rank = rank;
    }
    Ok(windows)
}

fn collect_windows(node: &TreeNode, out: &mut Vec<CompositorWindow>) {
    let is_leaf_window = node.nodes.is_empty()
        && node.floating_nodes.is_empty()
        && (node.pid.is_some() || node.window.is_some() || node.app_id.is_some());

    if is_leaf_window {
        let native_id = node
            .window
            .unwrap_or_else(|| SWAY_CONTAINER_ID_BASE + node.id);
        let bounds = node
            .rect
            .as_ref()
            .filter(|r| r.width > 0 && r.height > 0)
            .map(|r| (r.x, r.y, r.width, r.height));
        let app_id = node
            .app_id
            .clone()
            .or_else(|| node.window_properties.as_ref().and_then(|p| p.class.clone()))
            .unwrap_or_default();

        out.push(CompositorWindow {
            native_id,
            title: node.name.clone().unwrap_or_default(),
            app_id,
            pid: node.pid,
            bounds,
            is_focused: node.focused,
            is_minimized: node.visible == Some(false),
            z_order_rank: out.len(),
            backend: CompositorKind::SwayI3,
        });
    }

    for child in &node.nodes {
        collect_windows(child, out);
    }
    for child in &node.floating_nodes {
        collect_windows(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sway_tree_leaves_and_container_ids() {
        let json = r#"{
            "id": 1,
            "nodes": [{
                "id": 12,
                "name": "Terminal",
                "pid": 777,
                "app_id": "foot",
                "focused": true,
                "visible": true,
                "rect": {"x": 10, "y": 20, "width": 800, "height": 600},
                "nodes": [],
                "floating_nodes": []
            }],
            "floating_nodes": []
        }"#;
        let wins = parse_i3_sway_tree(json).expect("parse sway tree");
        assert_eq!(wins.len(), 1);
        assert_eq!(wins[0].native_id, SWAY_CONTAINER_ID_BASE + 12);
        assert_eq!(wins[0].pid, Some(777));
        assert_eq!(wins[0].bounds, Some((10, 20, 800, 600)));
        assert!(wins[0].is_focused);
    }
}
