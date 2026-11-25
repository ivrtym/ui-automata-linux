use std::process::Command;
use std::sync::Arc;

use atspi::proxy::proxy_ext::ProxyExt;
use atspi::{CoordType, Interface, Role, ScrollType, State};
use ui_automata::{AutomataError, ClickType};

use crate::a11y::editable::{self, EditableSetOutcome};
use crate::a11y::encoding::{clean_optional_text, strip_placeholder_text};
use crate::a11y::{A11yClient, extract_ref, normalize_role, synthetic_hwnd};
use crate::{Locator, Selector, UiError, input, process, window};

/// Linux AT-SPI2 UI element handle wrapping a D-Bus `(bus_name, object_path)` reference.
#[derive(Clone)]
pub struct LinuxElement {
    pub(crate) client: Arc<A11yClient>,
    pub(crate) bus_name: String,
    pub(crate) path: String,
}

/// Alias matching `automata_windows::UIElement` for cross-crate ergonomics.
pub type UIElement = LinuxElement;

impl LinuxElement {
    pub fn new(client: Arc<A11yClient>, bus_name: String, path: String) -> Self {
        Self {
            client,
            bus_name,
            path,
        }
    }

    pub fn bus_name(&self) -> &str {
        &self.bus_name
    }

    pub fn object_path(&self) -> &str {
        &self.path
    }

    pub fn hwnd(&self) -> Option<u64> {
        // If this element is a top-level window (parent is Application root), use its own ref;
        // otherwise walk up to its top-level window so descendants share the owning window's handle.
        if self.is_top_level_or_no_parent() {
            return Some(synthetic_hwnd(&self.bus_name, &self.path));
        }
        let mut cur = self.clone();
        for _ in 0..32 {
            let Some(p) = cur.parent_elem() else {
                return Some(synthetic_hwnd(&cur.bus_name, &cur.path));
            };
            if p.is_application_root() {
                return Some(synthetic_hwnd(&cur.bus_name, &cur.path));
            }
            cur = p;
        }
        Some(synthetic_hwnd(&self.bus_name, &self.path))
    }

    fn is_application_root(&self) -> bool {
        self.path == "/org/a11y/atspi/accessible/root"
            || self.client.block_on(async {
                let Ok(proxy) = self.client.accessible_proxy(&self.bus_name, &self.path).await else {
                    return false;
                };
                matches!(proxy.get_role().await, Ok(Role::Application))
            })
    }

    fn is_top_level_or_no_parent(&self) -> bool {
        self.client.block_on(async {
            let Ok(proxy) = self.client.accessible_proxy(&self.bus_name, &self.path).await else {
                return true;
            };
            let Ok(role) = proxy.get_role().await else {
                return true;
            };
            matches!(
                role,
                Role::Frame | Role::Window | Role::Dialog | Role::Alert | Role::FileChooser
            )
        })
    }

    pub fn name(&self) -> Option<String> {
        self.client.block_on(async {
            let proxy = self.client.accessible_proxy(&self.bus_name, &self.path).await.ok()?;
            clean_optional_text(proxy.name().await.ok())
        })
    }

    pub fn name_or_empty(&self) -> String {
        self.name().unwrap_or_default()
    }

    pub fn role(&self) -> String {
        self.client.block_on(async {
            let Ok(proxy) = self.client.accessible_proxy(&self.bus_name, &self.path).await else {
                return String::new();
            };
            let role = match proxy.get_role().await {
                Ok(r) => r,
                Err(_) => return proxy.get_role_name().await.unwrap_or_default(),
            };
            let editable = if role == Role::Text {
                proxy
                    .get_state()
                    .await
                    .map(|s| s.contains(State::Editable))
                    .unwrap_or(false)
            } else {
                false
            };
            normalize_role(role, editable)
        })
    }

    pub fn raw_role_name(&self) -> String {
        self.client.block_on(async {
            let Ok(proxy) = self.client.accessible_proxy(&self.bus_name, &self.path).await else {
                return String::new();
            };
            match proxy.get_role().await {
                Ok(r) => r.name().to_string(),
                Err(_) => proxy.get_role_name().await.unwrap_or_default(),
            }
        })
    }

    pub fn automation_id(&self) -> Option<String> {
        self.client.block_on(async {
            let proxy = self.client.accessible_proxy(&self.bus_name, &self.path).await.ok()?;
            proxy.accessible_id().await.ok().filter(|s| !s.is_empty())
        })
    }

    pub fn description(&self) -> Option<String> {
        self.client.block_on(async {
            let proxy = self.client.accessible_proxy(&self.bus_name, &self.path).await.ok()?;
            proxy.description().await.ok().filter(|s| !s.is_empty())
        })
    }

    pub fn attributes(&self) -> std::collections::HashMap<String, String> {
        self.client.block_on(async {
            let Ok(proxy) = self.client.accessible_proxy(&self.bus_name, &self.path).await else {
                return Default::default();
            };
            proxy.get_attributes().await.unwrap_or_default()
        })
    }

    pub fn states(&self) -> Result<atspi::StateSet, UiError> {
        self.client.block_on(async {
            let proxy = self.client.accessible_proxy(&self.bus_name, &self.path).await?;
            proxy
                .get_state()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))
        })
    }

    pub fn text(&self) -> Result<String, UiError> {
        self.client.block_on(async {
            let proxy = self.client.accessible_proxy(&self.bus_name, &self.path).await?;
            let ifaces = proxy
                .get_interfaces()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;

            if ifaces.contains(Interface::Text) {
                if let Ok(proxies) = proxy.proxies().await {
                    if let Ok(tp) = proxies.text().await {
                        if let Ok(len) = tp.character_count().await {
                            if len > 0 {
                                if let Ok(txt) = tp.get_text(0, len).await {
                                    if !txt.is_empty() {
                                        return Ok(txt);
                                    }
                                }
                            } else if len == 0 {
                                // Empty editable field should return "" rather than placeholder name
                                if ifaces.contains(Interface::EditableText) {
                                    return Ok(String::new());
                                }
                            }
                        }
                    }
                }
            }

            if ifaces.contains(Interface::Value) {
                if let Ok(proxies) = proxy.proxies().await {
                    if let Ok(vp) = proxies.value().await {
                        if let Ok(t) = vp.text().await {
                            if !t.is_empty() {
                                return Ok(t);
                            }
                        }
                        if let Ok(v) = vp.current_value().await {
                            return Ok(v.to_string());
                        }
                    }
                }
            }

            Ok(strip_placeholder_text(&proxy.name().await.unwrap_or_default()))
        })
    }

    pub fn inner_text(&self) -> Result<String, UiError> {
        let mut parts = Vec::new();
        for child in self.children_elems()? {
            if let Some(name) = child.name() {
                parts.push(name);
            } else if let Ok(t) = child.text() {
                let cleaned = strip_placeholder_text(&t);
                if !cleaned.is_empty() {
                    parts.push(cleaned);
                }
            }
        }
        Ok(parts.join("\n"))
    }

    pub fn is_enabled(&self) -> Result<bool, UiError> {
        let states = self.states()?;
        Ok(states.contains(State::Enabled) || states.contains(State::Sensitive))
    }

    pub fn is_visible(&self) -> Result<bool, UiError> {
        let states = self.states()?;
        Ok(states.contains(State::Visible) && states.contains(State::Showing))
    }

    pub fn is_minimized(&self) -> bool {
        self.states()
            .map(|s| s.contains(State::Iconified))
            .unwrap_or(false)
    }

    pub fn process_id(&self) -> Result<u32, UiError> {
        self.client
            .pid_for_bus(&self.bus_name)
            .map_err(|e| UiError::Platform(e.to_string()))
    }

    pub fn process_name(&self) -> Option<String> {
        let pid = self.process_id().ok()?;
        process::get_process_name(pid as i32).ok()
    }

    pub(crate) fn raw_bounds(&self) -> Result<(i32, i32, i32, i32), UiError> {
        self.client.block_on(async {
            let proxy = self.client.accessible_proxy(&self.bus_name, &self.path).await?;
            let proxies = proxy
                .proxies()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;
            let comp = proxies
                .component()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;
            comp.get_extents(CoordType::Screen)
                .await
                .map_err(|e| UiError::Platform(e.to_string()))
        })
    }

    pub(crate) fn owning_primary_window(&self) -> Option<Self> {
        if self.is_primary_window_frame() {
            return Some(self.clone());
        }
        let mut cur = self.clone();
        for _ in 0..32 {
            let parent = cur.parent_elem()?;
            if parent.is_application_root() {
                return None;
            }
            if parent.is_primary_window_frame() {
                return Some(parent);
            }
            cur = parent;
        }
        None
    }

    /// Returns the compositor `(dx, dy)` screen-origin offset to add when AT-SPI2
    /// reports window-relative `(0, 0)` bounds for a native Wayland window tree.
    pub(crate) fn wayland_window_offset(&self) -> (i32, i32) {
        if crate::backend::resolve_display_backend().ok()
            != Some(crate::backend::DisplayBackend::Wayland)
        {
            return (0, 0);
        }
        let Some(frame) = self.owning_primary_window() else {
            return (0, 0);
        };
        let Ok(frame_raw) = frame.raw_bounds() else {
            return (0, 0);
        };
        if frame_raw.0 != 0 || frame_raw.1 != 0 || frame_raw.2 <= 0 || frame_raw.3 <= 0 {
            return (0, 0);
        }
        let comp = crate::compositor::compositor_bounds_for_pid_title(
            frame.process_id().ok(),
            frame.name().as_deref(),
        );
        let shifted = apply_wayland_frame_offset((0, 0, frame_raw.2, frame_raw.3), frame_raw, comp, true, false);
        (shifted.0, shifted.1)
    }

    pub fn bounds(&self) -> Result<(i32, i32, i32, i32), UiError> {
        let rect = self.raw_bounds()?;
        if rect.2 <= 0 || rect.3 <= 0 {
            return Ok(rect);
        }
        if crate::backend::resolve_display_backend().ok()
            != Some(crate::backend::DisplayBackend::Wayland)
        {
            return Ok(rect);
        }

        if self.is_primary_window_frame() {
            if rect.0 == 0 && rect.1 == 0 {
                let comp = crate::compositor::compositor_bounds_for_pid_title(
                    self.process_id().ok(),
                    self.name().as_deref(),
                );
                return Ok(apply_wayland_frame_offset(rect, rect, comp, true, true));
            }
            return Ok(rect);
        }

        let Some(frame) = self.owning_primary_window() else {
            return Ok(rect);
        };
        let Ok(frame_raw) = frame.raw_bounds() else {
            return Ok(rect);
        };
        if frame_raw.0 != 0 || frame_raw.1 != 0 || frame_raw.2 <= 0 || frame_raw.3 <= 0 {
            return Ok(rect);
        }
        let comp = crate::compositor::compositor_bounds_for_pid_title(
            frame.process_id().ok(),
            frame.name().as_deref(),
        );
        Ok(apply_wayland_frame_offset(rect, frame_raw, comp, true, false))
    }

    pub fn children_elems(&self) -> Result<Vec<Self>, UiError> {
        let refs = self
            .client
            .children_refs(&self.bus_name, &self.path)
            .map_err(|e| UiError::Platform(e.to_string()))?;
        let mut children: Vec<Self> = refs
            .into_iter()
            .map(|(bus, path)| LinuxElement::new(Arc::clone(&self.client), bus, path))
            .collect();

        // Qt / GTK popup menus and modal dialogs often live as direct siblings of the main
        // frame under the AT-SPI Application node. Expose active non-window sibling overlays
        // as children of the top-level window so scoped workflows can interact with them.
        if self.is_primary_window_frame() {
            if let Some(app) = self.parent_elem().filter(|p| p.is_application_root()) {
                if let Ok(app_children) = self.client.children_refs(&app.bus_name, &app.path) {
                    if app_children.len() > 1 {
                        for (s_bus, s_path) in app_children {
                            if s_path == self.path && s_bus == self.bus_name {
                                continue;
                            }
                            let sib = LinuxElement::new(Arc::clone(&self.client), s_bus, s_path);
                            if sib.is_overlay_sibling() {
                                children.push(sib);
                            }
                        }
                    }
                }
            }
        }

        Ok(children)
    }

    fn is_primary_window_frame(&self) -> bool {
        self.client.block_on(async {
            let Ok(proxy) = self.client.accessible_proxy(&self.bus_name, &self.path).await else {
                return false;
            };
            matches!(proxy.get_role().await, Ok(Role::Frame | Role::Window))
        })
    }

    fn is_overlay_sibling(&self) -> bool {
        let is_non_window = self.client.block_on(async {
            let Ok(proxy) = self.client.accessible_proxy(&self.bus_name, &self.path).await else {
                return false;
            };
            let Ok(role) = proxy.get_role().await else {
                return false;
            };
            !matches!(role, Role::Frame | Role::Window | Role::Application)
        });
        if !is_non_window {
            return false;
        }
        matches!(self.bounds(), Ok((_, _, w, h)) if w > 0 && h > 0)
            && self.is_visible().unwrap_or(false)
    }

    pub fn parent_elem(&self) -> Option<Self> {
        self.client.block_on(async {
            let proxy = self.client.accessible_proxy(&self.bus_name, &self.path).await.ok()?;
            let parent_ref = proxy.parent().await.ok()?;
            let (bus, path) = extract_ref(&parent_ref)?;
            Some(LinuxElement::new(Arc::clone(&self.client), bus, path))
        })
    }

    pub fn has_parent_live(&self) -> bool {
        self.client.block_on(async {
            let Ok(proxy) = self.client.accessible_proxy(&self.bus_name, &self.path).await else {
                return false;
            };
            let Ok(states) = proxy.get_state().await else {
                return false;
            };
            if states.contains(State::Defunct) {
                return false;
            }
            let Ok(parent_ref) = proxy.parent().await else {
                return false;
            };
            extract_ref(&parent_ref).is_some()
        })
    }

    pub fn invoke(&self) -> Result<(), UiError> {
        self.client.block_on(async {
            let proxy = self.client.accessible_proxy(&self.bus_name, &self.path).await?;
            let proxies = proxy
                .proxies()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;
            let action = proxies
                .action()
                .await
                .map_err(|e| UiError::Platform(format!("Action interface unavailable: {e}")))?;
            let ok = action
                .do_action(0)
                .await
                .map_err(|e| UiError::Platform(format!("do_action(0) failed: {e}")))?;
            if !ok {
                return Err(UiError::Platform("do_action(0) returned false".into()));
            }
            Ok(())
        })
    }

    pub fn set_value(&self, value: &str) -> Result<(), UiError> {
        let _ = self.set_value_verified(value)?;
        Ok(())
    }

    /// Sets `EditableText` or numeric `Value` and returns structured readback
    /// verification details (including password protection and UTF-16 vs UTF-8
    /// verification outcome).
    pub fn set_value_verified(&self, value: &str) -> Result<EditableSetOutcome, UiError> {
        editable::set_editable_or_numeric_value(&self.client, &self.bus_name, &self.path, value)
    }

    pub fn focus(&self) -> Result<(), UiError> {
        self.client.block_on(async {
            let proxy = self.client.accessible_proxy(&self.bus_name, &self.path).await?;
            let proxies = proxy
                .proxies()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;
            let comp = proxies
                .component()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;
            let ok = comp
                .grab_focus()
                .await
                .map_err(|e| UiError::Platform(format!("grab_focus failed: {e}")))?;
            if !ok {
                return Err(UiError::Platform("grab_focus returned false".into()));
            }
            Ok(())
        })
    }

    pub fn scroll_into_view(&self) -> Result<(), UiError> {
        self.client.block_on(async {
            let proxy = self.client.accessible_proxy(&self.bus_name, &self.path).await?;
            let proxies = proxy
                .proxies()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;
            let comp = proxies
                .component()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;
            comp.scroll_to(ScrollType::Anywhere)
                .await
                .map_err(|e| UiError::Platform(format!("scroll_to failed: {e}")))?;
            Ok(())
        })
    }

    pub fn click(&self) -> Result<(), UiError> {
        self.click_at(0.5, 0.5, ClickType::Left)
    }

    pub fn double_click(&self) -> Result<(), UiError> {
        self.click_at(0.5, 0.5, ClickType::Double)
    }

    pub fn hover(&self) -> Result<(), UiError> {
        let (x, y, w, h) = self.bounds()?;
        if w <= 0 || h <= 0 {
            return Err(UiError::Platform(format!(
                "cannot hover element with invalid bounds ({x},{y},{w},{h})"
            )));
        }
        if x == 0 && y == 0 && crate::backend::resolve_display_backend().ok() == Some(crate::backend::DisplayBackend::Wayland) {
            log::warn!(
                "Wayland window/element reported origin (0,0) for bounds ({x},{y},{w},{h}); synthetic pointer hover may be offset if window is not at top-left"
            );
        }
        let cx = x + w / 2;
        let cy = y + h / 2;
        input::mouse_move(cx, cy).map_err(UiError::from)
    }

    pub fn click_at(&self, x_pct: f64, y_pct: f64, kind: ClickType) -> Result<(), UiError> {
        let (x, y, w, h) = self.bounds()?;
        if w <= 0 || h <= 0 {
            return Err(UiError::Platform(format!(
                "cannot click element with invalid bounds ({x},{y},{w},{h}); prefer `Invoke` if supported"
            )));
        }
        if x == 0 && y == 0 && crate::backend::resolve_display_backend().ok() == Some(crate::backend::DisplayBackend::Wayland) {
            log::warn!(
                "Wayland window/element reported origin (0,0) for bounds ({x},{y},{w},{h}); prefer `Invoke` over `Click` when window position is not exposed by Wayland"
            );
        }
        // Accept both 0.0..=1.0 fractions (ui-automata trait spec) and 0..=100 percentages.
        let (fx, fy) = if x_pct > 1.0 || y_pct > 1.0 {
            (x_pct / 100.0, y_pct / 100.0)
        } else {
            (x_pct, y_pct)
        };
        let px = x + ((w as f64) * fx).round() as i32;
        let py = y + ((h as f64) * fy).round() as i32;
        if let Ok(pid) = self.process_id() {
            window::check_click_point(px, py, pid).map_err(UiError::Platform)?;
        }
        let mapped = match kind {
            ClickType::Left => input::ClickType::Left,
            ClickType::Double => input::ClickType::Double,
            ClickType::Triple => input::ClickType::Triple,
            ClickType::Right => input::ClickType::Right,
            ClickType::Middle => input::ClickType::Middle,
        };
        input::mouse_click(px, py, mapped).map_err(UiError::from)
    }

    pub fn type_text(&self, text: &str) -> Result<(), UiError> {
        let _ = self.focus();
        let has_focus = self
            .states()
            .map(|s| s.contains(State::Focused))
            .unwrap_or(false);
        if !has_focus {
            if let Ok((_, _, w, h)) = self.bounds() {
                if w > 0 && h > 0 {
                    let _ = self.click_at(0.5, 0.5, ClickType::Left);
                }
            }
        }
        if let Ok(pid) = self.process_id() {
            window::check_keyboard_target(pid).map_err(UiError::Platform)?;
        }
        input::type_text(text).map_err(UiError::from)
    }

    pub fn press_key(&self, key: &str) -> Result<(), UiError> {
        let _ = self.focus();
        if let Ok(pid) = self.process_id() {
            window::check_keyboard_target(pid).map_err(UiError::Platform)?;
        }
        input::key_press(key).map_err(UiError::from)
    }

    pub fn locator(&self, selector: Selector) -> Result<Locator, UiError> {
        Ok(Locator::new(self.clone(), selector))
    }

    pub fn activate_window(&self) -> Result<(), UiError> {
        let pid = self.process_id().ok();
        let title = self.name().filter(|t| !t.trim().is_empty());

        if let Some(p) = pid {
            if activate_x11_window_by_pid(p, title.as_deref()) {
                let _ = self.focus();
                return Ok(());
            }
        }
        if activate_via_compositor(pid, title.as_deref()) {
            let _ = self.focus();
            return Ok(());
        }
        if self.focus().is_ok() {
            return Ok(());
        }
        if let Some(ref title) = title {
            if Command::new("wmctrl")
                .args(["-a", title])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                return Ok(());
            }
        }
        if let Some(p) = pid {
            if activate_via_status_notifier(&self.client, p) {
                return Ok(());
            }
        }
        Err(UiError::Platform(
            "activate_window failed via X11 wmctrl, Wayland compositor, AT-SPI grab_focus, and StatusNotifierItem".into(),
        ))
    }

    pub fn minimize_window(&self) -> Result<(), UiError> {
        let pid = self.process_id().ok();
        let title = self.name().filter(|t| !t.trim().is_empty());
        if let Some(p) = pid {
            let out = Command::new("xdotool")
                .args(["search", "--pid", &p.to_string(), "windowminimize"])
                .output();
            if let Ok(o) = out {
                if o.status.success() {
                    return Ok(());
                }
            }
        }
        if set_state_via_compositor(pid, title.as_deref(), crate::WindowAction::Minimize) {
            return Ok(());
        }
        Err(UiError::Platform(
            "minimize_window failed via xdotool and compositor backend".into(),
        ))
    }

    pub fn close(&self) -> Result<(), UiError> {
        // 1. Try AT-SPI Action named close
        let action_closed = self.client.block_on(async {
            let Ok(proxy) = self.client.accessible_proxy(&self.bus_name, &self.path).await else {
                return false;
            };
            let Ok(proxies) = proxy.proxies().await else {
                return false;
            };
            let Ok(action) = proxies.action().await else {
                return false;
            };
            if let Ok(actions) = action.get_actions().await {
                for (idx, act) in actions.iter().enumerate() {
                    if act.name.eq_ignore_ascii_case("close") {
                        if action.do_action(idx as i32).await.unwrap_or(false) {
                            return true;
                        }
                    }
                }
            }
            false
        });
        if action_closed {
            return Ok(());
        }

        let pid = self.process_id().ok();
        let title = self.name();

        // 2. Fallback: wmctrl -c <title>
        if let Some(ref title) = title {
            if Command::new("wmctrl")
                .args(["-c", title])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                return Ok(());
            }
        }

        // 3. Fallback: xdotool search --pid <pid> windowclose
        if let Some(p) = pid {
            if Command::new("xdotool")
                .args(["search", "--pid", &p.to_string(), "windowclose"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                return Ok(());
            }
        }

        // 4. Native compositor close (Hyprland / Sway / Niri / KWin)
        if set_state_via_compositor(pid, title.as_deref(), crate::WindowAction::Close) {
            return Ok(());
        }

        // 5. Wayland / GTK4 / CSD fallback: invoke the titlebar close button via AT-SPI2.
        if let Some(btn) = find_close_button_bfs(self, 32, 500) {
            if btn.invoke().is_ok() {
                return Ok(());
            }
        }

        Err(UiError::Platform(
            "close failed: no close action, titlebar Close button, compositor, wmctrl, or xdotool windowclose succeeded".into(),
        ))
    }
}

fn find_close_button_bfs(
    root: &LinuxElement,
    max_depth: usize,
    max_nodes: usize,
) -> Option<LinuxElement> {
    use std::collections::VecDeque;
    let mut queue = VecDeque::new();
    queue.push_back((root.clone(), 0usize));
    let mut visited = 0usize;

    while let Some((node, depth)) = queue.pop_front() {
        visited += 1;
        if visited > max_nodes {
            break;
        }
        if depth > 0 && ui_automata::Element::role(&node) == "button" {
            if let Some(name) = node.name() {
                let trimmed = name.trim();
                if trimmed.eq_ignore_ascii_case("close") || trimmed == "关闭" {
                    return Some(node);
                }
            }
        }
        if depth < max_depth {
            if let Ok(children) = node.children_elems() {
                for child in children {
                    queue.push_back((child, depth + 1));
                }
            }
        }
    }
    None
}

fn activate_via_compositor(pid: Option<u32>, title: Option<&str>) -> bool {
    let backend = crate::compositor::active_backend();
    if !backend.capabilities().can_focus {
        return false;
    }
    let native_id = crate::compositor::list_compositor_windows()
        .as_deref()
        .and_then(|wins| crate::compositor::find_matching_compositor_window(wins, pid, title))
        .map(|w| w.native_id)
        .unwrap_or(0);
    backend.focus_window(native_id, pid, title).is_ok()
}

fn set_state_via_compositor(
    pid: Option<u32>,
    title: Option<&str>,
    action: crate::WindowAction,
) -> bool {
    let backend = crate::compositor::active_backend();
    let native_id = crate::compositor::list_compositor_windows()
        .as_deref()
        .and_then(|wins| crate::compositor::find_matching_compositor_window(wins, pid, title))
        .map(|w| w.native_id)
        .unwrap_or(0);
    backend
        .set_window_state(native_id, pid, title, action)
        .is_ok()
}

fn select_wmctrl_window_xid<'a>(
    stdout: &'a str,
    pid: u32,
    title: Option<&str>,
) -> Option<&'a str> {
    let pid_str = pid.to_string();
    let matches: Vec<(&'a str, String)> = stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let xid = parts.next()?;
            let _desktop = parts.next()?;
            let row_pid = parts.next()?;
            if row_pid != pid_str {
                return None;
            }
            let _host = parts.next()?;
            let row_title = parts.collect::<Vec<_>>().join(" ");
            Some((xid, row_title))
        })
        .collect();

    if let Some(expected) = title.map(str::trim).filter(|s| !s.is_empty()) {
        let exp_lower = expected.to_ascii_lowercase();
        matches
            .iter()
            .find(|(_, t)| t == expected)
            .or_else(|| {
                matches
                    .iter()
                    .find(|(_, t)| t.to_ascii_lowercase().contains(&exp_lower))
            })
            .or_else(|| matches.first())
            .map(|(xid, _)| *xid)
    } else {
        matches.first().map(|(xid, _)| *xid)
    }
}

fn activate_x11_window_by_pid(pid: u32, title: Option<&str>) -> bool {
    let Ok(out) = Command::new("wmctrl").args(["-lp"]).output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some(xid) = select_wmctrl_window_xid(&stdout, pid, title) else {
        return false;
    };
    Command::new("wmctrl")
        .args(["-ia", xid])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn activate_via_status_notifier(client: &A11yClient, pid: u32) -> bool {
    let kde_prefix = format!("org.kde.StatusNotifierItem-{pid}-");
    let fdo_prefix = format!("org.freedesktop.StatusNotifierItem-{pid}-");
    client.block_on(async {
        let Ok(conn) = zbus::Connection::session().await else {
            return false;
        };
        let Ok(dbus) = zbus::Proxy::new(
            &conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        else {
            return false;
        };
        let Ok(names): std::result::Result<Vec<String>, _> = dbus.call("ListNames", &()).await
        else {
            return false;
        };
        let Some(svc) = names
            .into_iter()
            .find(|n| n.starts_with(&kde_prefix) || n.starts_with(&fdo_prefix))
        else {
            return false;
        };
        let Ok(sni) = zbus::Proxy::new(
            &conn,
            svc.as_str(),
            "/StatusNotifierItem",
            "org.kde.StatusNotifierItem",
        )
        .await
        else {
            return false;
        };
        sni.call_method("Activate", &(0i32, 0i32)).await.is_ok()
    })
}

impl ui_automata::Element for LinuxElement {
    fn name(&self) -> Option<String> {
        self.name()
    }

    fn role(&self) -> String {
        self.role()
    }

    fn text(&self) -> Result<String, AutomataError> {
        self.text().map_err(Into::into)
    }

    fn inner_text(&self) -> Result<String, AutomataError> {
        self.inner_text().map_err(Into::into)
    }

    fn is_enabled(&self) -> Result<bool, AutomataError> {
        self.is_enabled().map_err(Into::into)
    }

    fn is_visible(&self) -> Result<bool, AutomataError> {
        self.is_visible().map_err(Into::into)
    }

    fn process_id(&self) -> Result<u32, AutomataError> {
        self.process_id().map_err(Into::into)
    }

    fn process_name(&self) -> Option<String> {
        self.process_name()
    }

    fn hwnd(&self) -> Option<u64> {
        self.hwnd()
    }

    fn automation_id(&self) -> Option<String> {
        self.automation_id()
    }

    fn bounds(&self) -> Result<(i32, i32, i32, i32), AutomataError> {
        self.bounds().map_err(Into::into)
    }

    fn children(&self) -> Result<Vec<Self>, AutomataError> {
        self.children_elems().map_err(Into::into)
    }

    fn has_parent(&self) -> bool {
        self.has_parent_live()
    }

    fn parent(&self) -> Option<Self> {
        self.parent_elem()
    }

    fn click(&self) -> Result<(), AutomataError> {
        self.click().map_err(Into::into)
    }

    fn double_click(&self) -> Result<(), AutomataError> {
        self.double_click().map_err(Into::into)
    }

    fn hover(&self) -> Result<(), AutomataError> {
        self.hover().map_err(Into::into)
    }

    fn click_at(&self, x_pct: f64, y_pct: f64, kind: ClickType) -> Result<(), AutomataError> {
        self.click_at(x_pct, y_pct, kind).map_err(Into::into)
    }

    fn type_text(&self, text: &str) -> Result<(), AutomataError> {
        self.type_text(text).map_err(Into::into)
    }

    fn press_key(&self, key: &str) -> Result<(), AutomataError> {
        self.press_key(key).map_err(Into::into)
    }

    fn set_value(&self, value: &str) -> Result<(), AutomataError> {
        self.set_value(value).map_err(Into::into)
    }

    fn focus(&self) -> Result<(), AutomataError> {
        self.focus().map_err(Into::into)
    }

    fn invoke(&self) -> Result<(), AutomataError> {
        self.invoke().map_err(Into::into)
    }

    fn scroll_into_view(&self) -> Result<(), AutomataError> {
        self.scroll_into_view().map_err(Into::into)
    }

    fn activate_window(&self) -> Result<(), AutomataError> {
        self.activate_window().map_err(Into::into)
    }

    fn minimize_window(&self) -> Result<(), AutomataError> {
        self.minimize_window().map_err(Into::into)
    }

    fn close(&self) -> Result<(), AutomataError> {
        self.close().map_err(Into::into)
    }
}

/// Applies compositor window-origin correction on Wayland when AT-SPI2 reports
/// window-relative `(0, 0, fw, fh)` extents for the owning primary window while the
/// compositor places that window at `(cx, cy)`.
pub(crate) fn apply_wayland_frame_offset(
    rect: (i32, i32, i32, i32),
    frame_raw_rect: (i32, i32, i32, i32),
    compositor_rect: Option<(i32, i32, i32, i32)>,
    is_wayland: bool,
    is_frame: bool,
) -> (i32, i32, i32, i32) {
    if !is_wayland || rect.2 <= 0 || rect.3 <= 0 {
        return rect;
    }
    if frame_raw_rect.0 != 0
        || frame_raw_rect.1 != 0
        || frame_raw_rect.2 <= 0
        || frame_raw_rect.3 <= 0
    {
        return rect;
    }
    let Some((cx, cy, cw, ch)) = compositor_rect else {
        return rect;
    };
    if cx == 0 && cy == 0 {
        return rect;
    }
    if is_frame {
        let w = if cw > 0 && ch > 0 { cw } else { rect.2 };
        let h = if cw > 0 && ch > 0 { ch } else { rect.3 };
        return (cx, cy, w, h);
    }
    (rect.0.saturating_add(cx), rect.1.saturating_add(cy), rect.2, rect.3)
}

#[cfg(test)]
mod tests {
    use super::{apply_wayland_frame_offset, select_wmctrl_window_xid};

    #[test]
    fn selects_matching_x11_window_row_by_pid_and_title() {
        let wmctrl_out = "\
0x00a00012  0 267176 ivrty-X99 微信\n\
0x00a00033  0 267176 ivrty-X99 朋友圈\n\
0x00b00001  0 999999 ivrty-X99 Other App\n";
        assert_eq!(
            select_wmctrl_window_xid(wmctrl_out, 267176, Some("朋友圈")),
            Some("0x00a00033")
        );
        assert_eq!(
            select_wmctrl_window_xid(wmctrl_out, 267176, Some("微信")),
            Some("0x00a00012")
        );
        assert_eq!(
            select_wmctrl_window_xid(wmctrl_out, 267176, None),
            Some("0x00a00012")
        );
    }

    #[test]
    fn shifts_wayland_primary_and_descendant_rects_without_double_adding_xwayland() {
        let comp = Some((400, 250, 800, 600));
        // Native Wayland primary frame reporting (0, 0) gets compositor screen origin + size.
        assert_eq!(
            apply_wayland_frame_offset((0, 0, 790, 590), (0, 0, 790, 590), comp, true, true),
            (400, 250, 800, 600)
        );
        // Native Wayland child widget keeps its size and shifts by compositor window origin.
        assert_eq!(
            apply_wayland_frame_offset((80, 310, 64, 48), (0, 0, 790, 590), comp, true, false),
            (480, 560, 64, 48)
        );
        // Zero-size placeholder widget must never be shifted to non-zero screen coords.
        assert_eq!(
            apply_wayland_frame_offset((0, 0, 0, 0), (0, 0, 790, 590), comp, true, false),
            (0, 0, 0, 0)
        );
        // XWayland client already reports non-zero frame origin -> must NOT double-add.
        assert_eq!(
            apply_wayland_frame_offset((480, 560, 64, 48), (400, 250, 800, 600), comp, true, false),
            (480, 560, 64, 48)
        );
        // Pure X11 session ignores compositor offset helper completely.
        assert_eq!(
            apply_wayland_frame_offset((80, 310, 64, 48), (0, 0, 790, 590), comp, false, false),
            (80, 310, 64, 48)
        );
        // Missing compositor bounds leaves coords untouched.
        assert_eq!(
            apply_wayland_frame_offset((80, 310, 64, 48), (0, 0, 790, 590), None, true, false),
            (80, 310, 64, 48)
        );
    }
}
