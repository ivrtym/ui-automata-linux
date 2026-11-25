//! Cross-compositor Linux window listing, stacking order, geometry lookup,
//! and lifecycle control across X11/XWayland, GNOME Wayland, Hyprland,
//! Sway/i3, KWin, and Niri.

pub mod gnome;
pub mod hyprland;
pub mod kwin_niri;
pub mod sway_i3;
pub mod x11;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;

use crate::WindowAction;
use crate::a11y::MAX_SAFE_JS_HWND;
use crate::diagnostics::cache::{DEFAULT_PROBE_TTL, ProbeCache};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompositorKind {
    X11,
    GnomeWayland,
    KWinWayland,
    Hyprland,
    SwayI3,
    Niri,
    Cosmic,
    WlrForeignToplevel,
    Unknown,
}

impl CompositorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X11 => "x11",
            Self::GnomeWayland => "gnome-wayland",
            Self::KWinWayland => "kwin-wayland",
            Self::Hyprland => "hyprland",
            Self::SwayI3 => "sway-i3",
            Self::Niri => "niri",
            Self::Cosmic => "cosmic",
            Self::WlrForeignToplevel => "wlr-foreign-toplevel",
            Self::Unknown => "unknown",
        }
    }
}

/// Snapshot of a window as reported by the active desktop compositor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositorWindow {
    /// Compositor-native identifier clamped/mapped within IEEE-754 safe integer bounds.
    pub native_id: u64,
    pub title: String,
    pub app_id: String,
    pub pid: Option<u32>,
    pub bounds: Option<(i32, i32, i32, i32)>,
    pub is_focused: bool,
    pub is_minimized: bool,
    /// Zero-based topmost-first stacking rank when known (0 = topmost).
    pub z_order_rank: usize,
    pub backend: CompositorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositorCapabilities {
    pub kind: CompositorKind,
    pub available: bool,
    pub can_list_windows: bool,
    pub can_report_bounds: bool,
    pub can_report_stacking: bool,
    pub can_focus: bool,
    pub can_close: bool,
    pub can_move_resize: bool,
    pub detail: String,
}

/// Unified compositor interface for window inspection and management.
pub trait CompositorBackend: Send + Sync {
    fn kind(&self) -> CompositorKind;
    fn capabilities(&self) -> CompositorCapabilities;
    fn list_windows(&self) -> Result<Vec<CompositorWindow>>;

    fn focus_window(&self, native_id: u64, pid: Option<u32>, title: Option<&str>) -> Result<()> {
        let _ = (native_id, pid, title);
        anyhow::bail!(
            "focus_window is not supported by compositor backend '{}'",
            self.kind().as_str()
        )
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
        let _ = (native_id, title, x, y, width, height);
        anyhow::bail!(
            "set_window_bounds is not supported by compositor backend '{}'",
            self.kind().as_str()
        )
    }

    fn close_window(&self, native_id: u64, pid: Option<u32>, title: Option<&str>) -> Result<()> {
        let _ = (native_id, pid, title);
        anyhow::bail!(
            "close_window is not supported by compositor backend '{}'",
            self.kind().as_str()
        )
    }

    fn set_window_state(
        &self,
        native_id: u64,
        pid: Option<u32>,
        title: Option<&str>,
        action: WindowAction,
    ) -> Result<()> {
        let _ = (native_id, pid, title, action);
        anyhow::bail!(
            "set_window_state is not supported by compositor backend '{}'",
            self.kind().as_str()
        )
    }
}

/// Maps any compositor-specific identifier string into `[1, MAX_SAFE_JS_HWND]`.
pub fn hash_id_to_safe_u64(raw: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    raw.hash(&mut hasher);
    (hasher.finish() & MAX_SAFE_JS_HWND).max(1)
}

/// Executes an async D-Bus future on a dedicated compositor runtime.
pub(crate) fn block_on_compositor<F, T>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("automata-compositor")
            .build()
            .expect("compositor tokio runtime")
    });
    rt.block_on(fut)
}

static COMPOSITOR_CACHE: OnceLock<ProbeCache<&'static str, CompositorKind>> = OnceLock::new();
static WINDOWS_CACHE: OnceLock<ProbeCache<&'static str, Option<Vec<CompositorWindow>>>> =
    OnceLock::new();

fn kind_cache() -> &'static ProbeCache<&'static str, CompositorKind> {
    COMPOSITOR_CACHE.get_or_init(|| ProbeCache::new(DEFAULT_PROBE_TTL))
}

fn windows_cache() -> &'static ProbeCache<&'static str, Option<Vec<CompositorWindow>>> {
    WINDOWS_CACHE.get_or_init(|| ProbeCache::new(Duration::from_millis(600)))
}

/// Invalidates cached compositor detection and short-lived window list snapshots.
pub fn invalidate_compositor_cache() {
    kind_cache().clear();
    windows_cache().clear();
}

/// Detects the active compositor from session environment variables (pure, unit-testable).
pub fn detect_compositor_from_env(
    wayland_display: Option<&str>,
    display: Option<&str>,
    xdg_current_desktop: Option<&str>,
    xdg_session_type: Option<&str>,
    hyprland_sig: Option<&str>,
    swaysock: Option<&str>,
    i3sock: Option<&str>,
    niri_socket: Option<&str>,
) -> CompositorKind {
    let has_sig = |v: Option<&str>| v.map(|s| !s.trim().is_empty()).unwrap_or(false);
    if has_sig(hyprland_sig) {
        return CompositorKind::Hyprland;
    }
    if has_sig(niri_socket) {
        return CompositorKind::Niri;
    }
    if has_sig(swaysock) {
        return CompositorKind::SwayI3;
    }
    let is_wayland = has_sig(wayland_display)
        || xdg_session_type
            .map(|s| s.trim().eq_ignore_ascii_case("wayland"))
            .unwrap_or(false);

    if let Some(desktop) = xdg_current_desktop {
        let upper = desktop.to_ascii_uppercase();
        if upper.contains("HYPRLAND") {
            return CompositorKind::Hyprland;
        }
        if upper.contains("NIRI") {
            return CompositorKind::Niri;
        }
        if upper.contains("SWAY") || (!is_wayland && upper.contains("I3")) {
            return CompositorKind::SwayI3;
        }
        if is_wayland && upper.contains("COSMIC") {
            return CompositorKind::Cosmic;
        }
        if is_wayland && upper.contains("KDE") {
            return CompositorKind::KWinWayland;
        }
        if is_wayland && upper.contains("GNOME") {
            return CompositorKind::GnomeWayland;
        }
    }

    if has_sig(i3sock) {
        return CompositorKind::SwayI3;
    }
    if !is_wayland && has_sig(display) {
        return CompositorKind::X11;
    }
    if is_wayland {
        // Fallback when running Wayland without XDG_CURRENT_DESKTOP (e.g. headless niri socket).
        if has_sig(display) {
            return CompositorKind::X11;
        }
        return CompositorKind::WlrForeignToplevel;
    }
    CompositorKind::Unknown
}

/// Detects the active compositor for the current process environment (cached for 5s).
pub fn detect_compositor() -> CompositorKind {
    kind_cache().get_or_insert_with("kind", || {
        let env_opt = |k: &str| std::env::var(k).ok();
        let niri_env = env_opt("NIRI_SOCKET").or_else(discover_niri_socket_in_runtime_dir);
        detect_compositor_from_env(
            env_opt("WAYLAND_DISPLAY").as_deref(),
            env_opt("DISPLAY").as_deref(),
            env_opt("XDG_CURRENT_DESKTOP").as_deref(),
            env_opt("XDG_SESSION_TYPE").as_deref(),
            env_opt("HYPRLAND_INSTANCE_SIGNATURE").as_deref(),
            env_opt("SWAYSOCK").as_deref(),
            env_opt("I3SOCK").as_deref(),
            niri_env.as_deref(),
        )
    })
}

fn discover_niri_socket_in_runtime_dir() -> Option<String> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let entries = std::fs::read_dir(Path::new(&runtime)).ok()?;
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with("niri.") && name.ends_with(".sock") {
                return Some(entry.path().display().to_string());
            }
        }
    }
    None
}

/// Returns the best matching compositor backend implementation for the current session.
pub fn active_backend() -> Box<dyn CompositorBackend> {
    match detect_compositor() {
        CompositorKind::Hyprland => Box::new(hyprland::HyprlandBackend),
        CompositorKind::SwayI3 => Box::new(sway_i3::SwayI3Backend),
        CompositorKind::Niri => Box::new(kwin_niri::NiriBackend),
        CompositorKind::GnomeWayland => Box::new(gnome::GnomeBackend),
        CompositorKind::KWinWayland => Box::new(kwin_niri::KWinBackend),
        CompositorKind::X11 => Box::new(x11::X11Backend),
        CompositorKind::Cosmic | CompositorKind::WlrForeignToplevel | CompositorKind::Unknown => {
            if std::env::var("DISPLAY").map(|s| !s.is_empty()).unwrap_or(false) {
                Box::new(x11::X11Backend)
            } else {
                Box::new(UnsupportedWaylandBackend(detect_compositor()))
            }
        }
    }
}

struct UnsupportedWaylandBackend(CompositorKind);

impl CompositorBackend for UnsupportedWaylandBackend {
    fn kind(&self) -> CompositorKind {
        self.0
    }

    fn capabilities(&self) -> CompositorCapabilities {
        CompositorCapabilities {
            kind: self.0,
            available: false,
            can_list_windows: false,
            can_report_bounds: false,
            can_report_stacking: false,
            can_focus: false,
            can_close: false,
            can_move_resize: false,
            detail: format!(
                "no native compositor control backend active for '{}'; using AT-SPI2 fallback",
                self.0.as_str()
            ),
        }
    }

    fn list_windows(&self) -> Result<Vec<CompositorWindow>> {
        anyhow::bail!("{}", self.capabilities().detail)
    }
}

/// Best-effort cached compositor window listing (`None` when unsupported or unavailable).
pub fn list_compositor_windows() -> Option<Vec<CompositorWindow>> {
    windows_cache().get_or_insert_with("windows", || {
        let backend = active_backend();
        let mut wins = backend.list_windows().ok()?;
        // Fallback to XWayland client list if native Wayland compositor listing failed cleanly.
        if wins.is_empty()
            && backend.kind() != CompositorKind::X11
            && std::env::var("DISPLAY").map(|s| !s.is_empty()).unwrap_or(false)
        {
            if let Ok(x11_wins) = (x11::X11Backend).list_windows() {
                wins = x11_wins;
            }
        }
        Some(wins)
    })
}

/// Finds the best matching [`CompositorWindow`] for `(pid, title)` without
/// stealing unrelated windows from another helper process.
pub fn find_matching_compositor_window<'a>(
    windows: &'a [CompositorWindow],
    pid: Option<u32>,
    title: Option<&str>,
) -> Option<&'a CompositorWindow> {
    let norm_title = title.map(|t| t.trim()).filter(|t| !t.is_empty());

    if let Some(target_pid) = pid {
        let pid_matches: Vec<&CompositorWindow> =
            windows.iter().filter(|w| w.pid == Some(target_pid)).collect();
        if pid_matches.len() == 1 {
            return Some(pid_matches[0]);
        }
        if !pid_matches.is_empty() {
            if let Some(t) = norm_title {
                let tl = t.to_lowercase();
                if let Some(exact) = pid_matches.iter().find(|w| w.title == t) {
                    return Some(*exact);
                }
                if let Some(substr) = pid_matches
                    .iter()
                    .find(|w| w.title.to_lowercase().contains(&tl))
                {
                    return Some(*substr);
                }
            }
            return pid_matches
                .iter()
                .find(|w| w.is_focused)
                .copied()
                .or_else(|| Some(pid_matches[0]));
        }
    }

    if let Some(t) = norm_title {
        let tl = t.to_lowercase();
        if let Some(exact) = windows.iter().find(|w| w.title == t) {
            return Some(exact);
        }
        return windows.iter().find(|w| w.title.to_lowercase().contains(&tl));
    }

    None
}

/// Looks up screen bounds from the active compositor for `(pid, title)` when AT-SPI
/// on Wayland returns `(0, 0, w, h)` placeholder origins.
pub fn compositor_bounds_for_pid_title(
    pid: Option<u32>,
    title: Option<&str>,
) -> Option<(i32, i32, i32, i32)> {
    let windows = list_compositor_windows()?;
    find_matching_compositor_window(&windows, pid, title).and_then(|w| w.bounds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_compositor_priority_order() {
        assert_eq!(
            detect_compositor_from_env(
                Some("wayland-1"),
                Some(":0"),
                Some("GNOME"),
                Some("wayland"),
                Some("sig123"),
                None,
                None,
                None,
            ),
            CompositorKind::Hyprland
        );
        assert_eq!(
            detect_compositor_from_env(
                Some("wayland-1"),
                Some(":0"),
                Some("ubuntu:GNOME"),
                Some("wayland"),
                None,
                None,
                None,
                None,
            ),
            CompositorKind::GnomeWayland
        );
        assert_eq!(
            detect_compositor_from_env(
                None,
                Some(":99"),
                None,
                Some("x11"),
                None,
                None,
                None,
                None,
            ),
            CompositorKind::X11
        );
        assert_eq!(
            detect_compositor_from_env(
                Some("wayland-1"),
                None,
                Some("niri"),
                Some("wayland"),
                None,
                None,
                None,
                Some("/run/user/1000/niri.wayland-1.1.sock"),
            ),
            CompositorKind::Niri
        );
    }

    #[test]
    fn matches_compositor_window_by_pid_with_title_tiebreaker() {
        let windows = vec![
            CompositorWindow {
                native_id: 1,
                title: "Settings".into(),
                app_id: "app".into(),
                pid: Some(42),
                bounds: Some((0, 0, 400, 300)),
                is_focused: false,
                is_minimized: false,
                z_order_rank: 1,
                backend: CompositorKind::GnomeWayland,
            },
            CompositorWindow {
                native_id: 2,
                title: "Editor - main.rs".into(),
                app_id: "app".into(),
                pid: Some(42),
                bounds: Some((100, 100, 800, 600)),
                is_focused: true,
                is_minimized: false,
                z_order_rank: 0,
                backend: CompositorKind::GnomeWayland,
            },
        ];

        assert_eq!(
            find_matching_compositor_window(&windows, Some(42), Some("Settings"))
                .map(|w| w.native_id),
            Some(1)
        );
        assert_eq!(
            find_matching_compositor_window(&windows, Some(42), None).map(|w| w.native_id),
            Some(2)
        );
        assert_eq!(
            find_matching_compositor_window(&windows, Some(999), Some("Editor"))
                .map(|w| w.native_id),
            Some(2)
        );
    }
}
