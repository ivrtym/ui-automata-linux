//! XKB keyboard layout inspection and active pointer/keyboard grab detection.

use std::process::Command;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, GrabMode, GrabStatus};

use crate::runtime::command::run_bounded;

/// Snapshot of active keyboard layout state useful for diagnosing keysym
/// mismatches across multi-layout / IME setups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardLayoutInfo {
    pub layouts: Vec<String>,
    pub active_variant: Option<String>,
    pub has_non_us_primary: bool,
}

/// Active pointer/keyboard grab state reported by the X11 server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrabProbe {
    pub pointer_grabbed: bool,
    pub keyboard_grabbed: bool,
}

impl GrabProbe {
    pub fn any_grabbed(self) -> bool {
        self.pointer_grabbed || self.keyboard_grabbed
    }
}

/// Queries active XKB layout via `setxkbmap -query` when available.
pub fn inspect_xkb_layout() -> Option<KeyboardLayoutInfo> {
    let mut cmd = Command::new("setxkbmap");
    cmd.arg("-query");
    let out = run_bounded(cmd, "setxkbmap -query", Duration::from_secs(2), 16 * 1024).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_setxkbmap_query(&String::from_utf8_lossy(&out.stdout)))
}

/// Parses `setxkbmap -query` stdout cleanly.
pub fn parse_setxkbmap_query(stdout: &str) -> KeyboardLayoutInfo {
    let mut layouts = Vec::new();
    let mut variant = None;

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("layout:") {
            layouts = rest
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        } else if let Some(rest) = line.strip_prefix("variant:") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                variant = Some(trimmed.to_string());
            }
        }
    }

    let has_non_us_primary = layouts
        .first()
        .is_some_and(|l| !l.eq_ignore_ascii_case("us") && !l.eq_ignore_ascii_case("gb"));

    KeyboardLayoutInfo {
        layouts,
        active_variant: variant,
        has_non_us_primary,
    }
}

/// Checks whether another X11 client currently holds an active pointer or
/// keyboard grab (e.g., an open popup menu or locked drag operation).
pub fn probe_x11_grabs() -> Option<GrabProbe> {
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;

    let ptr_grabbed = match conn
        .grab_pointer(
            false,
            root,
            Default::default(),
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            x11rb::CURRENT_TIME,
        )
        .ok()
        .and_then(|c| c.reply().ok())
    {
        Some(r) if r.status == GrabStatus::SUCCESS => {
            let _ = conn.ungrab_pointer(x11rb::CURRENT_TIME);
            let _ = conn.flush();
            false
        }
        Some(r) if r.status == GrabStatus::ALREADY_GRABBED => true,
        _ => false,
    };

    let kbd_grabbed = match conn
        .grab_keyboard(
            false,
            root,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .ok()
        .and_then(|c| c.reply().ok())
    {
        Some(r) if r.status == GrabStatus::SUCCESS => {
            let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
            let _ = conn.flush();
            false
        }
        Some(r) if r.status == GrabStatus::ALREADY_GRABBED => true,
        _ => false,
    };

    Some(GrabProbe {
        pointer_grabbed: ptr_grabbed,
        keyboard_grabbed: kbd_grabbed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_setxkbmap_single_and_multi_layouts() {
        let sample = "rules:      evdev\nmodel:      pc105\nlayout:     ru,us\nvariant:    ,qwerty\n";
        let info = parse_setxkbmap_query(sample);
        assert_eq!(info.layouts, vec!["ru", "us"]);
        assert_eq!(info.active_variant.as_deref(), Some(",qwerty"));
        assert!(info.has_non_us_primary);

        let us_only = parse_setxkbmap_query("layout:     us\n");
        assert!(!us_only.has_non_us_primary);
    }
}
