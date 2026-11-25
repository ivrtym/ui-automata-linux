use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ui_automata::{AutomataError, ClickType, OcrMatch, OcrRegion};

use crate::a11y::A11yClient;
use crate::backend::{
    ConnectionMode, DisplayBackend, has_x11_display, resolve_connection_mode,
    resolve_display_backend,
};
use crate::browser::LinuxBrowser;
use crate::capture::CaptureSessionGuard;
use crate::{LinuxElement, UiError, capture, input, vision};

/// Entry point for Linux UI automation: enumerate AT-SPI2 windows and launch applications.
pub struct Desktop {
    browser: LinuxBrowser,
    connection_mode: ConnectionMode,
    _session_guard: Option<CaptureSessionGuard>,
}

impl Desktop {
    /// Create a desktop handle defaulting to `OneShot` connection lifecycle unless
    /// overridden by CLI/API or `AUTOMATA_CONNECTION_MODE=persistent`.
    pub fn new() -> Self {
        Self::with_connection_mode(ConnectionMode::OneShot)
    }

    /// Create a desktop handle with a caller-specified default [`ConnectionMode`]
    /// (e.g., `ConnectionMode::persistent_default()` for long-running services/daemons).
    pub fn with_connection_mode(default_mode: ConnectionMode) -> Self {
        let effective = resolve_connection_mode(default_mode).unwrap_or(default_mode);
        let session_guard = match effective {
            ConnectionMode::OneShot => None,
            ConnectionMode::Persistent {
                screencast_idle_timeout,
            } => Some(capture::enter_capture_session(
                true,
                Some(screencast_idle_timeout),
            )),
        };
        Self {
            browser: LinuxBrowser::new(automata_browser::DEFAULT_PORT),
            connection_mode: effective,
            _session_guard: session_guard,
        }
    }

    /// Effective [`ConnectionMode`] active on this desktop instance.
    pub fn connection_mode(&self) -> ConnectionMode {
        self.connection_mode
    }

    /// Enter a scoped persistent capture session honoring this `Desktop`'s [`ConnectionMode`]
    /// (keeps X11 socket/atom caches alive, plus ScreenCast if `Persistent`).
    pub fn capture_session(&self) -> CaptureSessionGuard {
        let sc_timeout = match self.connection_mode {
            ConnectionMode::Persistent {
                screencast_idle_timeout,
            } => Some(screencast_idle_timeout),
            ConnectionMode::OneShot => None,
        };
        capture::enter_capture_session(true, sc_timeout)
    }

    /// Enter a scoped persistent X11 capture session (reuses X11 connection & atoms across steps,
    /// leaving Wayland ScreenCast one-shot unless already in `Persistent` mode).
    pub fn x11_capture_session(&self) -> CaptureSessionGuard {
        capture::enter_capture_session(true, None)
    }

    /// Explicitly close any cached persistent X11 / ScreenCast capture sessions immediately.
    pub fn close_persistent_connections(&self) {
        capture::close_persistent_captures();
    }

    /// Access the CDP browser client owned by this `Desktop`.
    pub fn linux_browser(&self) -> &LinuxBrowser {
        &self.browser
    }

    pub fn application_windows(&self) -> Result<Vec<LinuxElement>, UiError> {
        let (client, apps) = A11yClient::with_shared_retry(|client| {
            let apps = client.root_applications()?;
            Ok((client.clone(), apps))
        })
        .map_err(|e| UiError::Platform(e.to_string()))?;

        let mut windows = Vec::new();
        for (bus, path) in apps {
            let children = match client.children_refs(&bus, &path) {
                Ok(c) => c,
                Err(err) => {
                    client.warn_skipped_peer(&bus, &err);
                    continue;
                }
            };
            for (w_bus, w_path) in children {
                if client.is_top_level_window(&w_bus, &w_path) {
                    windows.push(LinuxElement::new(client.clone(), w_bus, w_path));
                }
            }
        }

        Ok(windows)
    }

    /// Launch a Linux application (`.desktop` entry, URI scheme via `xdg-open`, or executable)
    /// and return its PID.
    ///
    /// Automatically ensures Qt/GTK accessibility bridge env vars (`QT_ACCESSIBILITY=1`,
    /// `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1`, and clearing leaked `NO_AT_BRIDGE=1`) are set
    /// for launched targets unless `AUTOMATA_PRESERVE_A11Y_ENV=1`.
    pub fn open_application(&self, app_id: &str) -> Result<u32, UiError> {
        let empty = std::collections::HashMap::new();
        self.open_application_with_env(app_id, &empty)
    }

    /// Same as [`open_application`](Self::open_application) with explicit extra environment variables.
    pub fn open_application_with_env(
        &self,
        app_id: &str,
        extra_env: &std::collections::HashMap<String, String>,
    ) -> Result<u32, UiError> {
        // 1. URI scheme (e.g. https://..., file://..., mailto:...)
        let is_uri = app_id
            .find("://")
            .is_some()
            || app_id
                .find(':')
                .map(|i| i >= 2 && !app_id[..i].contains('/'))
                .unwrap_or(false);
        if is_uri {
            let mut cmd = Command::new("xdg-open");
            cmd.arg(app_id);
            return spawn_launched_command(cmd, &format!("xdg-open '{app_id}'"), extra_env);
        }

        // 2. Direct executable path or PATH command
        if app_id.contains('/') || command_in_path(app_id) {
            let cmd = Command::new(app_id);
            return spawn_launched_command(cmd, &format!("'{app_id}'"), extra_env);
        }

        // 3. `.desktop` launcher resolution (~/.local/share/applications, /usr/share/applications)
        if let Some(exec_tokens) = find_desktop_exec(app_id) {
            if let Some((bin, args)) = exec_tokens.split_first() {
                let mut cmd = Command::new(bin);
                cmd.args(args);
                return spawn_launched_command(cmd, &format!("'{bin}'"), extra_env);
            }
        }

        // 4. Final fallback: attempt spawning directly
        let cmd = Command::new(app_id);
        spawn_launched_command(cmd, &format!("'{app_id}'"), extra_env)
    }

    /// Launch `target` and wait up to `timeout` for a matching AT-SPI2 window using `wait`
    /// strategy (`NewPid`, `NewWindow`, `MatchAny`), returning `(pid, Option<hwnd>)`.
    pub fn launch_and_wait(
        &self,
        target: &str,
        wait: ui_automata::LaunchWait,
        timeout: std::time::Duration,
    ) -> Result<(u32, Option<u64>), UiError> {
        use std::collections::HashSet;
        use std::time::Instant;
        use ui_automata::LaunchWait;

        let pre_hwnds: HashSet<u64> = if wait == LaunchWait::NewWindow {
            self.application_windows()
                .unwrap_or_default()
                .iter()
                .filter_map(|w| w.hwnd())
                .collect()
        } else {
            HashSet::new()
        };

        let pid = self.open_application(target)?;
        let proc_needle = target
            .trim_end_matches(".desktop")
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(target)
            .to_ascii_lowercase();

        let deadline = Instant::now() + timeout;
        loop {
            let windows = self.application_windows().unwrap_or_default();
            let found = match wait {
                LaunchWait::NewPid => windows
                    .into_iter()
                    .find(|w| w.process_id().ok() == Some(pid)),
                LaunchWait::NewWindow => windows.into_iter().find(|w| {
                    w.hwnd().is_some_and(|h| !pre_hwnds.contains(&h))
                        && w.process_name()
                            .map(|n| n.to_ascii_lowercase().contains(&proc_needle))
                            .unwrap_or(true)
                }),
                LaunchWait::MatchAny => windows.into_iter().find(|w| {
                    w.process_id().ok() == Some(pid)
                        || w.process_name()
                            .map(|n| ui_automata::process_names_match(&n, &proc_needle))
                            .unwrap_or(false)
                }),
            };
            if let Some(win) = found {
                return Ok((win.process_id().unwrap_or(pid), win.hwnd()));
            }
            if Instant::now() >= deadline {
                let mut hint = String::new();
                if let Some(label) = crate::a11y::current_apparmor_label() {
                    if label != "unconfined" && !label.starts_with("unconfined ") {
                        hint = format!(
                            " (caller AppArmor label='{label}' may hide Snap AT-SPI2 windows; launch with `aa-exec -p unconfined` if '{target}' is a Snap)"
                        );
                    }
                }
                return Err(UiError::Platform(format!(
                    "timed out after {timeout:?} waiting for window of '{target}' (pid={pid}, wait={wait:?}){hint}"
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
    }

    pub fn foreground_window(&self) -> Option<LinuxElement> {
        let client = A11yClient::shared().ok()?;
        let windows = self.application_windows().ok()?;
        windows
            .into_iter()
            .find(|w| client.is_active_window(w.bus_name(), w.object_path()))
    }

    /// Currently visible top-level tooltip elements across AT-SPI2 applications.
    pub fn tooltip_windows(&self) -> Vec<LinuxElement> {
        let Ok((client, apps)) = A11yClient::with_shared_retry(|client| {
            let apps = client.root_applications()?;
            Ok((client.clone(), apps))
        }) else {
            return Vec::new();
        };
        let mut tooltips = Vec::new();
        for (bus, path) in apps {
            let children = match client.children_refs(&bus, &path) {
                Ok(c) => c,
                Err(err) => {
                    client.warn_skipped_peer(&bus, &err);
                    continue;
                }
            };
            for (c_bus, c_path) in children {
                if client.is_visible_tooltip(&c_bus, &c_path) {
                    tooltips.push(LinuxElement::new(client.clone(), c_bus, c_path));
                }
            }
        }
        tooltips
    }

    /// Top-level synthetic HWNDs ordered topmost-first under X11 or Wayland compositors
    /// that expose window ordering/focus state.
    pub fn hwnd_z_order(&self) -> Vec<u64> {
        let stacking_meta: Vec<capture::X11WindowMeta> =
            if matches!(resolve_display_backend(), Ok(DisplayBackend::X11)) && has_x11_display() {
                match capture::x11_topmost_windows() {
                    Ok(wins) => wins,
                    Err(_) => return Vec::new(),
                }
            } else if let Some(comp_wins) = crate::compositor::list_compositor_windows() {
                let mut ordered = comp_wins;
                ordered.sort_by_key(|w| w.z_order_rank);
                ordered
                    .into_iter()
                    .map(|w| capture::X11WindowMeta {
                        pid: w.pid,
                        title: (!w.title.is_empty()).then_some(w.title),
                        rect: w.bounds.unwrap_or((0, 0, 0, 0)),
                    })
                    .collect()
            } else {
                return Vec::new();
            };

        if stacking_meta.is_empty() {
            return Vec::new();
        }
        let Ok(app_wins) = self.application_windows() else {
            return Vec::new();
        };
        let atspi_meta: Vec<capture::AtspiWindowMeta> = app_wins
            .into_iter()
            .filter_map(|w| {
                let hwnd = w.hwnd()?;
                Some(capture::AtspiWindowMeta {
                    hwnd,
                    pid: w.process_id().ok(),
                    title: w.name(),
                    bounds: w.bounds().ok(),
                })
            })
            .collect();
        capture::match_z_order_to_hwnds(&stacking_meta, &atspi_meta)
    }
}

impl Default for Desktop {
    fn default() -> Self {
        Self::new()
    }
}

impl ui_automata::Desktop for Desktop {
    type Elem = LinuxElement;
    type Browser = LinuxBrowser;

    fn browser(&self) -> &Self::Browser {
        &self.browser
    }

    fn application_windows(&self) -> Result<Vec<Self::Elem>, AutomataError> {
        self.application_windows().map_err(Into::into)
    }

    fn open_application(&self, exe: &str) -> Result<u32, AutomataError> {
        self.open_application(exe).map_err(Into::into)
    }

    fn open_application_with_env(
        &self,
        exe: &str,
        env: &std::collections::HashMap<String, String>,
    ) -> Result<u32, AutomataError> {
        self.open_application_with_env(exe, env).map_err(Into::into)
    }

    fn foreground_window(&self) -> Option<Self::Elem> {
        self.foreground_window()
    }

    fn foreground_hwnd(&self) -> Option<u64> {
        self.foreground_window().and_then(|w| w.hwnd())
    }

    fn tooltip_windows(&self) -> Vec<Self::Elem> {
        self.tooltip_windows()
    }

    fn hwnd_z_order(&self) -> Vec<u64> {
        self.hwnd_z_order()
    }

    fn find_descendant_with_parent(
        &self,
        root: &Self::Elem,
        selector: &ui_automata::SelectorPath,
    ) -> Option<(Self::Elem, Option<Self::Elem>)> {
        if !selector.has_descendant_step() {
            return selector.find_one_with_parent(root);
        }
        crate::a11y::tree::find_descendant_with_parent_wave(root, selector)
    }

    fn find_all_descendants(
        &self,
        root: &Self::Elem,
        selector: &ui_automata::SelectorPath,
    ) -> Vec<Self::Elem> {
        if !selector.has_descendant_step() {
            return selector.find_all(root);
        }
        crate::a11y::tree::find_all_descendants_wave(root, selector)
    }

    fn ocr_recognize(
        &self,
        hwnd: Option<u64>,
        region: Option<OcrRegion>,
    ) -> Result<Vec<OcrMatch>, AutomataError> {
        let rect = region.map(|r| (r.x, r.y, r.width, r.height));
        let layout = vision::recognize_target(hwnd, rect)
            .map_err(|e| AutomataError::Platform(e.to_string()))?;
        Ok(layout
            .blocks
            .into_iter()
            .map(|b| OcrMatch {
                text: b.text,
                confidence: b.confidence,
                x: b.x,
                y: b.y,
                width: b.width,
                height: b.height,
            })
            .collect())
    }

    fn pointer_move(&self, x: i32, y: i32) -> Result<(), AutomataError> {
        input::mouse_move(x, y).map_err(|e| AutomataError::Platform(e.to_string()))
    }

    fn pointer_click(&self, x: i32, y: i32, kind: ClickType) -> Result<(), AutomataError> {
        let mapped = match kind {
            ClickType::Left => input::ClickType::Left,
            ClickType::Double => input::ClickType::Double,
            ClickType::Triple => input::ClickType::Triple,
            ClickType::Right => input::ClickType::Right,
            ClickType::Middle => input::ClickType::Middle,
        };
        input::mouse_click(x, y, mapped).map_err(|e| AutomataError::Platform(e.to_string()))
    }

    fn pointer_type(&self, text: &str) -> Result<(), AutomataError> {
        input::type_text(text).map_err(|e| AutomataError::Platform(e.to_string()))
    }

    fn pointer_scroll(
        &self,
        x: i32,
        y: i32,
        delta_x: i32,
        delta_y: i32,
    ) -> Result<(), AutomataError> {
        input::mouse_scroll(x, y, delta_x, delta_y)
            .map_err(|e| AutomataError::Platform(e.to_string()))
    }

    fn pointer_drag(&self, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<(), AutomataError> {
        input::mouse_drag(x1, y1, x2, y2).map_err(|e| AutomataError::Platform(e.to_string()))
    }

    fn pointer_key(&self, key: &str) -> Result<(), AutomataError> {
        input::key_press(key).map_err(|e| AutomataError::Platform(e.to_string()))
    }

    fn capture_screenshot(
        &self,
        hwnd: Option<u64>,
        region: Option<OcrRegion>,
        path: &str,
    ) -> Result<(), AutomataError> {
        let out = std::path::Path::new(path);
        match (hwnd, region) {
            (Some(h), Some(r)) => {
                vision::capture_window_region(h, r.x, r.y, r.width, r.height, out)
                    .map(|_| ())
                    .map_err(|e| AutomataError::Platform(e.to_string()))
            }
            (None, Some(r)) => vision::capture_region(r.x, r.y, r.width, r.height, out)
                .map_err(|e| AutomataError::Platform(e.to_string())),
            (Some(h), None) => vision::capture_window(h, out)
                .map(|_| ())
                .map_err(|e| AutomataError::Platform(e.to_string())),
            (None, None) => vision::capture_screen(out)
                .map_err(|e| AutomataError::Platform(e.to_string())),
        }
    }
}

fn command_in_path(cmd: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| {
        let candidate = dir.join(cmd);
        candidate.is_file()
    })
}

fn desktop_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    dirs.push(PathBuf::from("/usr/local/share/applications"));
    dirs.push(PathBuf::from("/usr/share/applications"));
    dirs
}

fn find_desktop_exec(app_id: &str) -> Option<Vec<String>> {
    let needle = app_id.trim_end_matches(".desktop").to_ascii_lowercase();
    for dir in desktop_dirs() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(tokens) = match_desktop_entry(&content, &stem, &needle, &path) {
                return Some(tokens);
            }
        }
    }
    None
}

/// Installed desktop application entry discovered from `.desktop` files.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub desktop_file: String,
}

struct ParsedDesktopEntry {
    name: Option<String>,
    exec_tokens: Vec<String>,
    hidden: bool,
}

fn parse_desktop_entry_fields(content: &str) -> ParsedDesktopEntry {
    let mut in_entry = false;
    let mut name_val: Option<String> = None;
    let mut exec_val: Option<String> = None;
    let mut hidden = false;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "Name" if name_val.is_none() => name_val = Some(v.trim().to_string()),
                "Exec" if exec_val.is_none() => exec_val = Some(v.trim().to_string()),
                "Hidden" | "NoDisplay" if v.trim().eq_ignore_ascii_case("true") => hidden = true,
                _ => {}
            }
        }
    }

    let exec_tokens = exec_val
        .map(|raw| {
            parse_exec_tokens(&raw)
                .into_iter()
                .filter(|t| !matches!(t.as_str(), "%u" | "%U" | "%f" | "%F" | "%i" | "%c" | "%k"))
                .collect()
        })
        .unwrap_or_default();

    ParsedDesktopEntry {
        name: name_val,
        exec_tokens,
        hidden,
    }
}

/// Enumerate visible installed Linux desktop applications from standard `.desktop` directories.
pub fn list_installed_apps() -> Vec<InstalledApp> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in desktop_dirs() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !seen.insert(stem.to_string()) {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let parsed = parse_desktop_entry_fields(&content);
            if parsed.hidden || parsed.exec_tokens.is_empty() {
                continue;
            }
            out.push(InstalledApp {
                id: stem.to_string(),
                name: parsed.name.unwrap_or_else(|| stem.to_string()),
                exec: parsed.exec_tokens.join(" "),
                desktop_file: path.display().to_string(),
            });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn match_desktop_entry(
    content: &str,
    stem: &str,
    needle: &str,
    _path: &Path,
) -> Option<Vec<String>> {
    let parsed = parse_desktop_entry_fields(content);
    if parsed.hidden || parsed.exec_tokens.is_empty() {
        return None;
    }

    let stem_matches = stem == needle || stem.ends_with(&format!(".{needle}"));
    let name_matches = parsed
        .name
        .as_deref()
        .map(|n| n.eq_ignore_ascii_case(needle))
        .unwrap_or(false);

    if !stem_matches && !name_matches {
        return None;
    }
    Some(parsed.exec_tokens)
}

fn parse_exec_tokens(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in raw.chars() {
        match (quote, ch) {
            (None, '"' | '\'') => quote = Some(ch),
            (Some(q), c) if q == c => quote = None,
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (_, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn resolve_a11y_launch_env<F>(get_env: F) -> Vec<(&'static str, &'static str)>
where
    F: Fn(&str) -> Option<String>,
{
    if matches!(
        get_env("AUTOMATA_PRESERVE_A11Y_ENV").as_deref(),
        Some("1" | "true" | "TRUE")
    ) {
        return Vec::new();
    }
    let mut vars = Vec::new();
    if get_env("QT_ACCESSIBILITY").is_none() {
        vars.push(("QT_ACCESSIBILITY", "1"));
    }
    if get_env("QT_LINUX_ACCESSIBILITY_ALWAYS_ON").is_none() {
        vars.push(("QT_LINUX_ACCESSIBILITY_ALWAYS_ON", "1"));
    }
    // Many IDE/Electron shells leak NO_AT_BRIDGE=1, which silently disables GTK AT-SPI2.
    if !matches!(get_env("NO_AT_BRIDGE").as_deref(), Some("0")) {
        vars.push(("NO_AT_BRIDGE", "0"));
    }
    vars
}

fn spawn_launched_command(
    mut cmd: Command,
    desc: &str,
    extra_env: &std::collections::HashMap<String, String>,
) -> Result<u32, UiError> {
    for (k, v) in resolve_a11y_launch_env(|key| std::env::var(key).ok()) {
        cmd.env(k, v);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    crate::runtime::command::spawn_detached(cmd, desc)
        .map_err(|e| UiError::Platform(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::resolve_a11y_launch_env;
    use std::collections::HashMap;

    #[test]
    fn injects_qt_and_clears_inherited_no_at_bridge() {
        let env: HashMap<&str, &str> = [("NO_AT_BRIDGE", "1")].into_iter().collect();
        let vars = resolve_a11y_launch_env(|k| env.get(k).map(|v| (*v).to_string()));
        assert_eq!(
            vars,
            vec![
                ("QT_ACCESSIBILITY", "1"),
                ("QT_LINUX_ACCESSIBILITY_ALWAYS_ON", "1"),
                ("NO_AT_BRIDGE", "0"),
            ]
        );
    }

    #[test]
    fn respects_explicit_overrides_and_preserve_flag() {
        let explicit: HashMap<&str, &str> = [
            ("QT_ACCESSIBILITY", "0"),
            ("QT_LINUX_ACCESSIBILITY_ALWAYS_ON", "1"),
            ("NO_AT_BRIDGE", "0"),
        ]
        .into_iter()
        .collect();
        assert!(resolve_a11y_launch_env(|k| explicit.get(k).map(|v| (*v).to_string())).is_empty());

        let preserve: HashMap<&str, &str> = [("AUTOMATA_PRESERVE_A11Y_ENV", "1")]
            .into_iter()
            .collect();
        assert!(resolve_a11y_launch_env(|k| preserve.get(k).map(|v| (*v).to_string())).is_empty());
    }

    #[test]
    fn reaps_spawned_child_without_leaving_zombie() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "exit 0"]);
        let empty = std::collections::HashMap::new();
        let pid = super::spawn_launched_command(cmd, "test exit 0", &empty).unwrap();
        let proc_dir = std::path::PathBuf::from(format!("/proc/{pid}"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while proc_dir.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !proc_dir.exists(),
            "launched child pid={pid} remained in /proc (possible <defunct> zombie)"
        );
    }
}
