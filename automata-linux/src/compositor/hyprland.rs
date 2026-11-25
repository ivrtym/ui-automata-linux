//! Hyprland Wayland compositor backend powered by `hyprctl -j` / `hyprctl dispatch`.

use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::WindowAction;
use crate::compositor::{
    CompositorBackend, CompositorCapabilities, CompositorKind, CompositorWindow,
    hash_id_to_safe_u64,
};
use crate::runtime::command::run_bounded;

pub struct HyprlandBackend;

#[derive(Debug, Deserialize)]
struct HyprClient {
    address: String,
    #[serde(default)]
    at: [i32; 2],
    #[serde(default)]
    size: [i32; 2],
    #[serde(default)]
    title: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    pid: i32,
    #[serde(default, rename = "focusHistoryID")]
    focus_history_id: i32,
    #[serde(default)]
    mapped: bool,
    #[serde(default)]
    hidden: bool,
}

impl CompositorBackend for HyprlandBackend {
    fn kind(&self) -> CompositorKind {
        CompositorKind::Hyprland
    }

    fn capabilities(&self) -> CompositorCapabilities {
        let mut cmd = Command::new("hyprctl");
        cmd.args(["clients", "-j"]);
        let ok = run_bounded(cmd, "hyprctl clients probe", Duration::from_secs(2), 1024 * 1024)
            .map(|o| o.status.success())
            .unwrap_or(false);
        CompositorCapabilities {
            kind: CompositorKind::Hyprland,
            available: ok,
            can_list_windows: ok,
            can_report_bounds: ok,
            can_report_stacking: ok,
            can_focus: ok,
            can_close: ok,
            can_move_resize: ok,
            detail: if ok {
                "hyprctl -j responsive".to_string()
            } else {
                "hyprctl -j unavailable".to_string()
            },
        }
    }

    fn list_windows(&self) -> Result<Vec<CompositorWindow>> {
        let mut cmd = Command::new("hyprctl");
        cmd.args(["clients", "-j"]);
        let out = run_bounded(cmd, "hyprctl clients -j", Duration::from_secs(3), 2 * 1024 * 1024)
            .context("run hyprctl clients -j")?;
        if !out.status.success() {
            anyhow::bail!("hyprctl clients -j exited with {:?}", out.status.code());
        }
        parse_hyprland_clients(&String::from_utf8_lossy(&out.stdout))
    }

    fn focus_window(&self, native_id: u64, pid: Option<u32>, title: Option<&str>) -> Result<()> {
        let addr = find_hypr_address(native_id, pid, title)?;
        hypr_dispatch(&["focuswindow", &format!("address:{addr}")])
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
        let addr = find_hypr_address(native_id, None, title)?;
        hypr_dispatch(&[
            "movewindowpixel",
            &format!("exact {x} {y},address:{addr}"),
        ])?;
        hypr_dispatch(&[
            "resizewindowpixel",
            &format!("exact {width} {height},address:{addr}"),
        ])
    }

    fn close_window(&self, native_id: u64, pid: Option<u32>, title: Option<&str>) -> Result<()> {
        let addr = find_hypr_address(native_id, pid, title)?;
        hypr_dispatch(&["closewindow", &format!("address:{addr}")])
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
                hypr_dispatch(&["fullscreen", "1"])
            }
            WindowAction::Minimize => {
                anyhow::bail!("Hyprland does not expose native window minimization")
            }
        }
    }
}

fn find_hypr_address(native_id: u64, pid: Option<u32>, title: Option<&str>) -> Result<String> {
    let mut cmd = Command::new("hyprctl");
    cmd.args(["clients", "-j"]);
    let out = run_bounded(cmd, "hyprctl clients lookup", Duration::from_secs(3), 2 * 1024 * 1024)?;
    let clients: Vec<HyprClient> = serde_json::from_slice(&out.stdout)?;
    clients
        .into_iter()
        .find(|c| {
            hash_id_to_safe_u64(&c.address) == native_id
                || (pid.is_some() && u32::try_from(c.pid).ok() == pid)
                || title.is_some_and(|t| c.title == t)
        })
        .map(|c| c.address)
        .context("matching Hyprland client not found")
}

fn hypr_dispatch(args: &[&str]) -> Result<()> {
    let mut cmd = Command::new("hyprctl");
    cmd.arg("dispatch").args(args);
    let out = run_bounded(cmd, "hyprctl dispatch", Duration::from_secs(2), 64 * 1024)?;
    if out.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "hyprctl dispatch failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
    }
}

pub fn parse_hyprland_clients(json_str: &str) -> Result<Vec<CompositorWindow>> {
    let mut raw: Vec<HyprClient> =
        serde_json::from_str(json_str).context("parse hyprctl clients -j JSON")?;
    raw.retain(|c| c.mapped && !c.hidden);
    raw.sort_by_key(|c| c.focus_history_id);

    Ok(raw
        .into_iter()
        .enumerate()
        .map(|(rank, c)| {
            let bounds = (c.size[0] > 0 && c.size[1] > 0)
                .then_some((c.at[0], c.at[1], c.size[0], c.size[1]));
            CompositorWindow {
                native_id: hash_id_to_safe_u64(&c.address),
                title: c.title,
                app_id: c.class,
                pid: u32::try_from(c.pid).ok().filter(|&p| p > 0),
                bounds,
                is_focused: c.focus_history_id == 0,
                is_minimized: c.hidden,
                z_order_rank: rank,
                backend: CompositorKind::Hyprland,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hyprland_clients_sorted_by_focus_history() {
        let json = r#"[
            {"address":"0xaaa","at":[500,100],"size":[600,400],"title":"Back","class":"foot","pid":11,"focusHistoryID":1,"mapped":true,"hidden":false},
            {"address":"0xbbb","at":[0,0],"size":[800,600],"title":"Front","class":"firefox","pid":22,"focusHistoryID":0,"mapped":true,"hidden":false}
        ]"#;
        let wins = parse_hyprland_clients(json).expect("parse hyprland");
        assert_eq!(wins.len(), 2);
        assert_eq!(wins[0].title, "Front");
        assert!(wins[0].is_focused);
        assert_eq!(wins[0].z_order_rank, 0);
        assert_eq!(wins[1].title, "Back");
        assert!(!wins[1].is_focused);
        assert_eq!(wins[1].bounds, Some((500, 100, 600, 400)));
    }
}
