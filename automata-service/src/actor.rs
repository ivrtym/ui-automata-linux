//! Dedicated OS-thread UI/Browser Actor isolating blocking AT-SPI2 / X11 /
//! Portal / CDP handles from Tokio worker threads while retaining `ShadowDom`.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use automata_linux::coords::WindowId;
use automata_linux::diagnostics::CapabilitiesSnapshot;
use automata_linux::{
    A11yClient, ConnectionMode, Desktop, LinuxElement, WindowAction, build_element_tree_with,
    capture_window, capture_window_tree_snapshot, find_elements_with, find_matching_window_with,
    force_foreground, inspect_window_info, set_window_bounds, set_window_state, vision_layout_ex,
    window_probe,
};
use serde_json::{Value, json};
use ui_automata::{
    AnchorDef, Browser as _, Executor, Plan, RetryPolicy, SelectorPath, ShadowDom, Step,
    WorkflowState,
};

use crate::ServiceError;
use crate::protocol::{
    BatchProbeStep, BrowserCommand, ClickButtonArg, Target, VisionCommand, WindowFilter,
    WindowStateArg,
};
use crate::session::WindowTargetKey;

pub fn window_info_to_json(info: &automata_linux::WindowInfo) -> Value {
    let wid = WindowId::new(info.hwnd)
        .map(|w| w.to_hex())
        .unwrap_or_else(|_| format!("{:#x}", info.hwnd));
    json!({
        "hwnd": wid,
        "hwnd_dec": info.hwnd,
        "pid": info.pid,
        "process": info.process_name,
        "process_name": info.process_name,
        "title": info.title,
        "role": info.control_type,
        "control_type": info.control_type,
        "is_active": info.is_active,
        "is_minimized": info.is_minimized,
        "bounds": json!({ "x": info.x, "y": info.y, "width": info.width, "height": info.height }),
    })
}

/// Abstract operations executed inside the serialized UI Actor thread.
///
/// Implementing this trait with a lightweight fake enables deterministic
/// contract testing of `AutomationService` without a live display server.
pub trait UiActorOps: Send {
    fn capabilities(&self) -> CapabilitiesSnapshot;
    fn reset_shadow_dom(&mut self);
    fn resolve_window_bounds(&mut self, id: WindowId) -> Result<(i32, i32, i32, i32), ServiceError>;
    fn list_windows(&mut self, filter: &WindowFilter) -> Result<Value, ServiceError>;
    fn focused_window(&mut self) -> Result<Value, ServiceError>;
    fn window_tree(&mut self, target: &Target, max_results: usize) -> Result<Value, ServiceError>;
    fn active_window_tree(
        &mut self,
        selector: Option<&str>,
        max_results: usize,
    ) -> Result<Value, ServiceError>;
    fn find_elements(
        &mut self,
        target: &Target,
        selector: &str,
        include_ancestors: bool,
        include_siblings: bool,
        max_results: usize,
    ) -> Result<Value, ServiceError>;
    fn capture_bundle(
        &mut self,
        target: &Target,
        out_dir: &Path,
        max_nodes: usize,
        max_depth: usize,
        include_ocr: bool,
    ) -> Result<Value, ServiceError>;
    fn batch_explore(
        &mut self,
        target: &Target,
        steps: &[BatchProbeStep],
        stop_on_error: bool,
        bundle_dir: Option<&Path>,
    ) -> Result<Value, ServiceError>;
    fn vision(&mut self, cmd: &VisionCommand) -> Result<Value, ServiceError>;
    fn launch_app(
        &mut self,
        target: &str,
        wait: ui_automata::LaunchWait,
        timeout: Duration,
    ) -> Result<Value, ServiceError>;
    fn focus_app(&mut self, target: &Target, window_index: usize) -> Result<Value, ServiceError>;
    fn window_state(&mut self, target: &Target, state: WindowStateArg) -> Result<Value, ServiceError>;
    fn window_bounds(
        &mut self,
        target: &Target,
        x: Option<i32>,
        y: Option<i32>,
        width: Option<i32>,
        height: Option<i32>,
    ) -> Result<Value, ServiceError>;
    fn get_window_bounds(&mut self, target: &Target) -> Result<Value, ServiceError>;
    fn window_screenshot(
        &mut self,
        target: &Target,
        path: Option<&str>,
    ) -> Result<Value, ServiceError>;
    fn run_steps(
        &mut self,
        window: &Target,
        steps: &[Value],
        reset: bool,
        timeout: Duration,
    ) -> Result<Value, ServiceError>;
    fn browser(&mut self, cmd: &BrowserCommand) -> Result<Value, ServiceError>;
    fn shutdown(&mut self) {}
}

/// Production Linux UI Actor holding persistent `Desktop`, `Executor`, and `ShadowDom`.
pub struct LinuxUiActorOps {
    executor: Executor<Desktop>,
    active_window: Option<WindowTargetKey>,
}

impl LinuxUiActorOps {
    pub fn new(mode: ConnectionMode) -> Self {
        let desktop = Desktop::with_connection_mode(mode);
        Self {
            executor: Executor::new(desktop),
            active_window: None,
        }
    }

    fn resolve_target_hwnd(&self, target: &Target) -> Result<u64, ServiceError> {
        if let Some(id) = target.window_id {
            return Ok(id.as_u64());
        }
        let win = find_matching_window_with(
            &self.executor.desktop,
            target.pid,
            target.title.as_deref(),
            target.automation_id.as_deref(),
            target.process.as_deref(),
            None,
        )?;
        win.hwnd()
            .ok_or_else(|| ServiceError::WindowNotFound("matched window has no synthetic hwnd".into()))
    }
}

impl UiActorOps for LinuxUiActorOps {
    fn capabilities(&self) -> CapabilitiesSnapshot {
        CapabilitiesSnapshot::probe()
    }

    fn reset_shadow_dom(&mut self) {
        self.executor.dom = ShadowDom::new();
        self.active_window = None;
    }

    fn resolve_window_bounds(&mut self, id: WindowId) -> Result<(i32, i32, i32, i32), ServiceError> {
        let win = find_matching_window_with(
            &self.executor.desktop,
            None,
            None,
            None,
            None,
            Some(id.as_u64()),
        )?;
        let client = A11yClient::shared()?;
        let info = inspect_window_info(&win, &client);
        Ok((info.x, info.y, info.width, info.height))
    }

    fn list_windows(&mut self, filter: &WindowFilter) -> Result<Value, ServiceError> {
        let client = A11yClient::shared()?;
        let windows = self.executor.desktop.application_windows()?;
        let mut list = Vec::new();
        for w in windows {
            let info = inspect_window_info(&w, &client);
            if let Some(ref p) = filter.process {
                if !ui_automata::process_names_match(&info.process_name, p) {
                    continue;
                }
            }
            if let Some(target_pid) = filter.pid {
                if info.pid != target_pid {
                    continue;
                }
            }
            if let Some(id) = filter.window_id {
                if info.hwnd != id.as_u64() {
                    continue;
                }
            }
            if let Some(ref t) = filter.title {
                if !info.title.to_lowercase().contains(&t.to_lowercase()) {
                    continue;
                }
            }
            list.push(window_info_to_json(&info));
        }
        Ok(Value::Array(list))
    }

    fn focused_window(&mut self) -> Result<Value, ServiceError> {
        let client = A11yClient::shared()?;
        match self.executor.desktop.foreground_window() {
            Some(w) => Ok(window_info_to_json(&inspect_window_info(&w, &client))),
            None => Ok(json!({ "focused": null })),
        }
    }

    fn window_tree(&mut self, target: &Target, max_results: usize) -> Result<Value, ServiceError> {
        build_element_tree_with(
            &self.executor.desktop,
            target.pid,
            target.title.as_deref(),
            target.automation_id.as_deref(),
            target.process.as_deref(),
            target.window_id.map(|w| w.as_u64()),
            max_results,
            target.selector.as_deref(),
        )
        .map_err(ServiceError::from)
    }

    fn active_window_tree(
        &mut self,
        selector: Option<&str>,
        max_results: usize,
    ) -> Result<Value, ServiceError> {
        let fg = self
            .executor
            .desktop
            .foreground_window()
            .ok_or_else(|| ServiceError::WindowNotFound("no focused foreground window found".into()))?;
        let target_hwnd = fg
            .hwnd()
            .ok_or_else(|| ServiceError::WindowNotFound("focused window has no synthetic hwnd".into()))?;
        build_element_tree_with(
            &self.executor.desktop,
            None,
            None,
            None,
            None,
            Some(target_hwnd),
            max_results,
            selector,
        )
        .map_err(ServiceError::from)
    }

    fn find_elements(
        &mut self,
        target: &Target,
        selector: &str,
        include_ancestors: bool,
        include_siblings: bool,
        max_results: usize,
    ) -> Result<Value, ServiceError> {
        let res = find_elements_with(
            &self.executor.desktop,
            target.pid,
            target.title.as_deref(),
            target.automation_id.as_deref(),
            target.process.as_deref(),
            target.window_id.map(|w| w.as_u64()),
            selector,
            include_ancestors,
            include_siblings,
            max_results,
        )?;
        serde_json::to_value(res).map_err(|e| ServiceError::Internal(e.to_string()))
    }

    fn capture_bundle(
        &mut self,
        target: &Target,
        out_dir: &Path,
        max_nodes: usize,
        max_depth: usize,
        include_ocr: bool,
    ) -> Result<Value, ServiceError> {
        capture_bundle_into_dir(
            &self.executor.desktop,
            target,
            out_dir,
            max_nodes,
            max_depth,
            true,
            true,
            include_ocr,
        )
    }

    fn batch_explore(
        &mut self,
        target: &Target,
        steps: &[BatchProbeStep],
        stop_on_error: bool,
        bundle_dir: Option<&Path>,
    ) -> Result<Value, ServiceError> {
        let mut results = Vec::with_capacity(steps.len());
        let mut succeeded = 0usize;
        let mut failed = 0usize;

        for (idx, item) in steps.iter().enumerate() {
            let mut action_error: Option<String> = None;
            let mut action_result: Option<Value> = None;

            if let Some(ref step_val) = item.step {
                match self.run_steps(target, std::slice::from_ref(step_val), false, Duration::from_secs(10)) {
                    Ok(v) => action_result = Some(v),
                    Err(e) => action_error = Some(e.to_string()),
                }
            }

            if item.settle_ms > 0 {
                std::thread::sleep(Duration::from_millis(item.settle_ms));
            }

            let step_bundle = if item.capture_tree || item.capture_screenshot || item.capture_ocr {
                if let Some(root_dir) = bundle_dir {
                    let safe_name = sanitize_step_slug(&item.name);
                    let step_dir = root_dir.join(format!("{idx:02}_{safe_name}"));
                    match capture_bundle_into_dir(
                        &self.executor.desktop,
                        target,
                        &step_dir,
                        5_000,
                        32,
                        item.capture_tree,
                        item.capture_screenshot,
                        item.capture_ocr,
                    ) {
                        Ok(b) => Some(b),
                        Err(e) => {
                            if action_error.is_none() {
                                action_error = Some(format!("capture failed: {e}"));
                            }
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let query_hits = if let Some(ref sel) = item.query {
                match self.find_elements(target, sel, false, false, item.max_results) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        if action_error.is_none() {
                            action_error = Some(format!("query failed: {e}"));
                        }
                        None
                    }
                }
            } else {
                None
            };

            let step_ok = action_error.is_none();
            if step_ok {
                succeeded += 1;
            } else {
                failed += 1;
            }

            results.push(json!({
                "index": idx,
                "name": item.name,
                "ok": step_ok,
                "error": action_error,
                "action_result": action_result,
                "query_hits": query_hits,
                "bundle": step_bundle,
            }));

            if !step_ok && stop_on_error {
                break;
            }
        }

        Ok(json!({
            "ok": failed == 0,
            "total": steps.len(),
            "executed": results.len(),
            "succeeded": succeeded,
            "failed": failed,
            "bundle_dir": bundle_dir.map(|p| p.display().to_string()),
            "steps": results,
        }))
    }

    fn vision(&mut self, cmd: &VisionCommand) -> Result<Value, ServiceError> {
        match cmd {
            VisionCommand::WindowLayout {
                window_id,
                capture_only,
                save_screenshot,
            } => {
                let out = vision_layout_ex(
                    Some(window_id.as_u64()),
                    *capture_only,
                    *save_screenshot,
                )?;
                serde_json::to_value(out).map_err(|e| ServiceError::Internal(e.to_string()))
            }
            VisionCommand::WindowProbe {
                window_id,
                capture_only,
                save_screenshot,
            } => {
                let out = window_probe(window_id.as_u64(), *capture_only, *save_screenshot)?;
                serde_json::to_value(out).map_err(|e| ServiceError::Internal(e.to_string()))
            }
            VisionCommand::ScreenLayout {
                capture_only,
                save_screenshot,
            } => {
                let out = vision_layout_ex(None, *capture_only, *save_screenshot)?;
                serde_json::to_value(out).map_err(|e| ServiceError::Internal(e.to_string()))
            }
            VisionCommand::Capture {
                window_id,
                region,
                path,
            } => {
                let resolved_path = path.clone().unwrap_or_else(|| {
                    let ts = chrono::Utc::now().timestamp_millis();
                    format!("/tmp/automata_capture_{ts}.png")
                });
                let screen_rect = if let Some(r) = region {
                    Some(automata_linux::coords::CoordResolver::to_screen_rect(
                        *r,
                        |wid| {
                            self.resolve_window_bounds(wid).map_err(|e| {
                                automata_linux::UiError::Platform(e.to_string())
                            })
                        },
                    )?)
                } else {
                    None
                };
                let p = std::path::Path::new(&resolved_path);
                let (ox, oy) = match (window_id, screen_rect) {
                    (Some(h), Some(sr)) => {
                        automata_linux::capture_window_region(h.as_u64(), sr.x, sr.y, sr.width, sr.height, p)?
                    }
                    (Some(h), None) => automata_linux::capture_window(h.as_u64(), p)?,
                    (None, Some(sr)) => {
                        automata_linux::capture_region(sr.x, sr.y, sr.width, sr.height, p)?;
                        (sr.x, sr.y)
                    }
                    (None, None) => {
                        automata_linux::capture_screen(p)?;
                        (0, 0)
                    }
                };
                Ok(json!({ "path": resolved_path, "origin_x": ox, "origin_y": oy }))
            }
        }
    }

    fn launch_app(
        &mut self,
        target: &str,
        wait: ui_automata::LaunchWait,
        timeout: Duration,
    ) -> Result<Value, ServiceError> {
        let (pid, hwnd) = self.executor.desktop.launch_and_wait(target, wait, timeout)?;
        Ok(json!({
            "pid": pid,
            "hwnd": hwnd.map(|h| format!("{h:#x}")),
            "hwnd_dec": hwnd,
        }))
    }

    fn focus_app(&mut self, target: &Target, window_index: usize) -> Result<Value, ServiceError> {
        if let Some(id) = target.window_id {
            force_foreground(id.as_u64()).map_err(ServiceError::classify_message)?;
            return Ok(json!({ "focused": true, "hwnd": id.to_hex() }));
        }
        let client = A11yClient::shared()?;
        let windows = self.executor.desktop.application_windows()?;
        let mut matches: Vec<LinuxElement> = windows
            .into_iter()
            .filter(|w| {
                let info = inspect_window_info(w, &client);
                if let Some(p) = target.pid {
                    if info.pid != p {
                        return false;
                    }
                }
                if let Some(ref proc) = target.process {
                    if !ui_automata::process_names_match(&info.process_name, proc) {
                        return false;
                    }
                }
                if let Some(ref t) = target.title {
                    if !info.title.to_lowercase().contains(&t.to_lowercase()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        if matches.is_empty() {
            return Err(ServiceError::WindowNotFound(
                "no matching window found to focus".into(),
            ));
        }
        let chosen = matches.remove(window_index.min(matches.len() - 1));
        let h = chosen
            .hwnd()
            .ok_or_else(|| ServiceError::WindowNotFound("matched window has no synthetic hwnd".into()))?;
        force_foreground(h).map_err(ServiceError::classify_message)?;
        Ok(json!({ "focused": true, "hwnd": format!("{h:#x}") }))
    }

    fn window_state(&mut self, target: &Target, state: WindowStateArg) -> Result<Value, ServiceError> {
        let hwnd_u64 = self.resolve_target_hwnd(target)?;
        let hwnd_hex = format!("{hwnd_u64:#x}");
        match WindowAction::try_from(state) {
            Ok(action) => {
                set_window_state(&hwnd_hex, action).map_err(ServiceError::classify_message)?;
                let action_str = match state {
                    WindowStateArg::Minimize => "minimize",
                    WindowStateArg::Maximize => "maximize",
                    WindowStateArg::Restore => "restore",
                    WindowStateArg::Close => "close",
                    WindowStateArg::Focus => "focus",
                };
                Ok(json!({ "ok": true, "hwnd": hwnd_hex, "action": action_str }))
            }
            Err(()) => {
                force_foreground(hwnd_u64).map_err(ServiceError::classify_message)?;
                Ok(json!({ "ok": true, "hwnd": hwnd_hex, "action": "focus" }))
            }
        }
    }

    fn window_bounds(
        &mut self,
        target: &Target,
        x: Option<i32>,
        y: Option<i32>,
        width: Option<i32>,
        height: Option<i32>,
    ) -> Result<Value, ServiceError> {
        let hwnd_u64 = self.resolve_target_hwnd(target)?;
        let hwnd_hex = format!("{hwnd_u64:#x}");
        let win = find_matching_window_with(
            &self.executor.desktop,
            None,
            None,
            None,
            None,
            Some(hwnd_u64),
        )?;
        let client = A11yClient::shared()?;
        let cur = inspect_window_info(&win, &client);
        let nx = x.unwrap_or(cur.x);
        let ny = y.unwrap_or(cur.y);
        let nw = u32::try_from(width.unwrap_or(cur.width).max(1))
            .map_err(|e| ServiceError::InvalidInput(e.to_string()))?;
        let nh = u32::try_from(height.unwrap_or(cur.height).max(1))
            .map_err(|e| ServiceError::InvalidInput(e.to_string()))?;
        set_window_bounds(&hwnd_hex, nx, ny, nw, nh).map_err(ServiceError::classify_message)?;
        Ok(json!({
            "ok": true,
            "hwnd": hwnd_hex,
            "action": "set_bounds",
            "x": nx,
            "y": ny,
            "width": nw,
            "height": nh,
        }))
    }

    fn get_window_bounds(&mut self, target: &Target) -> Result<Value, ServiceError> {
        let hwnd_u64 = self.resolve_target_hwnd(target)?;
        let win = find_matching_window_with(
            &self.executor.desktop,
            None,
            None,
            None,
            None,
            Some(hwnd_u64),
        )?;
        let client = A11yClient::shared()?;
        Ok(window_info_to_json(&inspect_window_info(&win, &client)))
    }

    fn window_screenshot(
        &mut self,
        target: &Target,
        path: Option<&str>,
    ) -> Result<Value, ServiceError> {
        let hwnd_u64 = self.resolve_target_hwnd(target)?;
        let hwnd_hex = format!("{hwnd_u64:#x}");
        let out_path = path.map(str::to_string).unwrap_or_else(|| {
            let ts = chrono::Utc::now().timestamp_millis();
            format!("/tmp/automata_window_{ts}.png")
        });
        let (ox, oy) =
            automata_linux::capture_window(hwnd_u64, std::path::Path::new(&out_path))?;
        Ok(json!({ "hwnd": hwnd_hex, "path": out_path, "origin_x": ox, "origin_y": oy }))
    }

    fn run_steps(
        &mut self,
        window: &Target,
        raw_steps: &[Value],
        reset: bool,
        timeout: Duration,
    ) -> Result<Value, ServiceError> {
        if window.is_empty() {
            return Err(ServiceError::InvalidInput(
                "'window' target must specify process, pid, title, window_id, or selector".into(),
            ));
        }

        let steps_yaml = serde_json::to_string(raw_steps)
            .map_err(|e| ServiceError::InvalidInput(format!("serialize steps failed: {e}")))?;
        let steps: Vec<Step> = serde_yaml::from_str(&steps_yaml)
            .map_err(|e| ServiceError::InvalidInput(format!("invalid step definition: {e}")))?;

        let key = WindowTargetKey::from(window);
        let needs_reset = reset || self.active_window.as_ref() != Some(&key);
        if needs_reset {
            self.executor.dom = ShadowDom::new();
            self.active_window = Some(key);

            let mut resolved_pid = window.pid;
            let mut resolved_proc = window.process.clone();
            let mut resolved_sel = window.selector.clone();

            if let Some(wid) = window.window_id {
                if let Ok(win) = find_matching_window_with(
                    &self.executor.desktop,
                    None,
                    None,
                    None,
                    None,
                    Some(wid.as_u64()),
                ) {
                    if resolved_pid.is_none() {
                        resolved_pid = ui_automata::Element::process_id(&win).ok();
                    }
                    if resolved_proc.is_none() {
                        resolved_proc = win.process_name();
                    }
                    if resolved_sel.is_none() {
                        if let Some(w_title) = ui_automata::Element::name(&win) {
                            resolved_sel =
                                Some(format!("[name='{}']", w_title.replace('\'', "\\'")));
                        }
                    }
                }
            } else if resolved_sel.is_none() {
                if let Some(ref t) = window.title {
                    resolved_sel = Some(format!("[name*='{}']", t.replace('\'', "\\'")));
                }
            }

            let sel_path = SelectorPath::parse(resolved_sel.as_deref().unwrap_or("*"))
                .map_err(|e| ServiceError::InvalidInput(format!("invalid window selector: {e}")))?;
            let mut anchor = AnchorDef::root("window", sel_path);
            anchor.pid = resolved_pid;
            anchor.process_name = resolved_proc;
            self.executor.mount(vec![anchor])?;
        }

        let unmount: Vec<String> = Vec::new();
        let plan = Plan {
            name: "run_actions",
            steps: &steps,
            recovery_handlers: Vec::new(),
            max_recoveries: 0,
            unmount: &unmount,
            default_timeout: timeout,
            default_retry: RetryPolicy::None,
        };

        let mut wf_state = WorkflowState::new(true);
        self.executor.run(&plan, &mut wf_state)?;
        let outputs = wf_state.output.into_map();
        Ok(json!({
            "ok": true,
            "steps_run": steps.len(),
            "outputs": outputs,
            "locals": wf_state.locals,
            "shadow_dom_reused": !needs_reset,
        }))
    }

    fn browser(&mut self, cmd: &BrowserCommand) -> Result<Value, ServiceError> {
        let b = self.executor.desktop.linux_browser();
        match cmd {
            BrowserCommand::Ensure => {
                b.ensure()?;
                Ok(json!({ "ok": true }))
            }
            BrowserCommand::Tabs => {
                let tabs = b.tabs()?;
                let out: Vec<Value> = tabs
                    .into_iter()
                    .map(|(id, info)| json!({ "id": id, "title": info.title, "url": info.url }))
                    .collect();
                Ok(Value::Array(out))
            }
            BrowserCommand::Open { url } => {
                let tab_id = b.open_tab(url.as_deref())?;
                Ok(json!({ "tab_id": tab_id }))
            }
            BrowserCommand::Activate { tab_id } => {
                b.activate_tab(tab_id)?;
                Ok(json!({ "ok": true, "tab_id": tab_id }))
            }
            BrowserCommand::Close { tab_id } => {
                b.close_tab(tab_id)?;
                Ok(json!({ "ok": true, "tab_id": tab_id }))
            }
            BrowserCommand::Navigate { tab_id, url } => {
                b.navigate(tab_id, url)?;
                Ok(json!({ "ok": true, "tab_id": tab_id, "url": url }))
            }
            BrowserCommand::Dom { tab_id } => Ok(b.dom_tree(tab_id)?),
            BrowserCommand::Eval { tab_id, expression } => {
                let result = b.eval(tab_id, expression)?;
                Ok(json!({ "result": result }))
            }
            BrowserCommand::Screenshot { tab_id } => {
                let data_b64 = b.screenshot(tab_id)?;
                Ok(json!({ "encoding": "base64", "mime_type": "image/png", "data": data_b64 }))
            }
        }
    }

    fn shutdown(&mut self) {
        self.executor.desktop.close_persistent_connections();
    }
}

/// Converts single-action helper requests (`Invoke`/`SetValue`/`ClickSelector`) into a step JSON value.
pub fn single_action_step(selector: &str, kind: SingleStepKind<'_>) -> Value {
    match kind {
        SingleStepKind::Invoke => json!({
            "intent": format!("Invoke {selector}"),
            "action": { "type": "Invoke", "scope": "window", "selector": selector },
            "expect": { "type": "Always" },
        }),
        SingleStepKind::SetValue(v) => json!({
            "intent": format!("Set value on {selector}"),
            "action": { "type": "SetValue", "scope": "window", "selector": selector, "value": v },
            "expect": { "type": "Always" },
        }),
        SingleStepKind::Click(button) => json!({
            "intent": format!("Click {selector}"),
            "action": { "type": "Click", "scope": "window", "selector": selector, "button": button },
            "expect": { "type": "Always" },
        }),
    }
}

pub enum SingleStepKind<'a> {
    Invoke,
    SetValue(&'a str),
    Click(ClickButtonArg),
}

fn sanitize_step_slug(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let trimmed = s.trim_matches('_');
    if trimmed.is_empty() {
        "step".into()
    } else {
        trimmed.chars().take(40).collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_bundle_into_dir(
    desktop: &Desktop,
    target: &Target,
    out_dir: &Path,
    max_nodes: usize,
    max_depth: usize,
    include_tree: bool,
    include_screenshot: bool,
    include_ocr: bool,
) -> Result<Value, ServiceError> {
    std::fs::create_dir_all(out_dir)?;

    let (win_info, snap_opt) = if include_tree {
        let (info, snap) = capture_window_tree_snapshot(
            desktop,
            target.pid,
            target.title.as_deref(),
            target.automation_id.as_deref(),
            target.process.as_deref(),
            target.window_id.map(|w| w.as_u64()),
            max_nodes,
            max_depth,
        )?;
        (info, Some(snap))
    } else {
        let win = find_matching_window_with(
            desktop,
            target.pid,
            target.title.as_deref(),
            target.automation_id.as_deref(),
            target.process.as_deref(),
            target.window_id.map(|w| w.as_u64()),
        )?;
        let client = A11yClient::shared()?;
        (inspect_window_info(&win, &client), None)
    };

    let win_json = window_info_to_json(&win_info);
    let win_path = out_dir.join("window.json");
    std::fs::write(&win_path, serde_json::to_vec_pretty(&win_json).map_err(|e| ServiceError::Internal(e.to_string()))?)?;

    let mut tree_path_opt: Option<String> = None;
    let mut node_count = 0usize;
    let mut truncated = false;
    let mut truncation_reason: Option<String> = None;
    if let Some(snap) = snap_opt {
        let tree_path = out_dir.join("tree.json");
        std::fs::write(
            &tree_path,
            serde_json::to_vec(&snap.root).map_err(|e| ServiceError::Internal(e.to_string()))?,
        )?;
        node_count = snap.node_count;
        truncated = snap.truncated;
        truncation_reason = snap.truncation_reason;
        tree_path_opt = Some(tree_path.display().to_string());
    }

    let mut screenshot_path_opt: Option<String> = None;
    let mut origin_opt: Option<(i32, i32)> = None;
    if include_screenshot || include_ocr {
        let shot_path = out_dir.join("screenshot.png");
        let (ox, oy) = capture_window(win_info.hwnd, &shot_path)?;
        screenshot_path_opt = Some(shot_path.display().to_string());
        origin_opt = Some((ox, oy));
    }

    let mut ocr_path_opt: Option<String> = None;
    let mut ocr_blocks_count = 0usize;
    if include_ocr {
        let layout = vision_layout_ex(Some(win_info.hwnd), false, false)?;
        ocr_blocks_count = layout.blocks.len();
        let ocr_path = out_dir.join("ocr.json");
        let ocr_val = serde_json::to_value(&layout).map_err(|e| ServiceError::Internal(e.to_string()))?;
        std::fs::write(&ocr_path, serde_json::to_vec_pretty(&ocr_val).map_err(|e| ServiceError::Internal(e.to_string()))?)?;
        ocr_path_opt = Some(ocr_path.display().to_string());
    }

    let manifest = json!({
        "ok": true,
        "bundle_dir": out_dir.display().to_string(),
        "captured_at": chrono::Utc::now().to_rfc3339(),
        "window": win_json,
        "window_path": win_path.display().to_string(),
        "tree_path": tree_path_opt,
        "node_count": node_count,
        "truncated": truncated,
        "truncation_reason": truncation_reason,
        "screenshot_path": screenshot_path_opt,
        "origin": origin_opt.map(|(x, y)| json!({"x": x, "y": y})),
        "ocr_path": ocr_path_opt,
        "ocr_blocks_count": ocr_blocks_count,
    });
    std::fs::write(
        out_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).map_err(|e| ServiceError::Internal(e.to_string()))?,
    )?;
    Ok(manifest)
}

type ActorJob = Box<dyn FnOnce(&mut dyn UiActorOps) + Send>;

/// Handle to the dedicated serialized UI/Browser OS worker thread.
pub struct UiActorHandle {
    tx: mpsc::Sender<ActorJob>,
}

impl UiActorHandle {
    pub fn spawn_with_factory<F>(factory: F) -> Self
    where
        F: FnOnce() -> Box<dyn UiActorOps> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<ActorJob>();
        std::thread::Builder::new()
            .name("automata-ui-actor".into())
            .spawn(move || {
                let mut ops = factory();
                while let Ok(job) = rx.recv() {
                    job(&mut *ops);
                }
                ops.shutdown();
            })
            .expect("failed to spawn automata-ui-actor thread");
        Self { tx }
    }

    pub fn spawn_linux(mode: ConnectionMode) -> Self {
        Self::spawn_with_factory(move || Box::new(LinuxUiActorOps::new(mode)))
    }

    /// Synchronous blocking call onto the UI Actor thread (safe from any OS or runtime context).
    pub fn call_sync<T, F>(&self, f: F) -> Result<T, ServiceError>
    where
        T: Send + 'static,
        F: FnOnce(&mut dyn UiActorOps) -> Result<T, ServiceError> + Send + 'static,
    {
        let (resp_tx, resp_rx) = mpsc::channel();
        let job: ActorJob = Box::new(move |ops| {
            let _ = resp_tx.send(f(ops));
        });
        self.tx
            .send(job)
            .map_err(|_| ServiceError::Internal("UI actor thread terminated".into()))?;
        resp_rx
            .recv()
            .map_err(|_| ServiceError::Internal("UI actor dropped response".into()))?
    }

    /// Async non-blocking `.await` onto the UI Actor thread.
    pub async fn call_async<T, F>(&self, f: F) -> Result<T, ServiceError>
    where
        T: Send + 'static,
        F: FnOnce(&mut dyn UiActorOps) -> Result<T, ServiceError> + Send + 'static,
    {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let job: ActorJob = Box::new(move |ops| {
            let _ = resp_tx.send(f(ops));
        });
        self.tx
            .send(job)
            .map_err(|_| ServiceError::Internal("UI actor thread terminated".into()))?;
        resp_rx
            .await
            .map_err(|_| ServiceError::Internal("UI actor dropped response".into()))?
    }
}
