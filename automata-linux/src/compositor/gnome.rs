//! GNOME Shell Wayland compositor backend using `org.gnome.Shell.Introspect`
//! over the session D-Bus connection.

use std::collections::HashMap;

use anyhow::{Context, Result};
use zbus::zvariant::OwnedValue;

use crate::compositor::{
    CompositorBackend, CompositorCapabilities, CompositorKind, CompositorWindow, block_on_compositor,
};
use crate::a11y::MAX_SAFE_JS_HWND;

pub struct GnomeBackend;

impl CompositorBackend for GnomeBackend {
    fn kind(&self) -> CompositorKind {
        CompositorKind::GnomeWayland
    }

    fn capabilities(&self) -> CompositorCapabilities {
        let windows = self.list_windows();
        let ok = windows.is_ok();
        CompositorCapabilities {
            kind: CompositorKind::GnomeWayland,
            available: ok,
            can_list_windows: ok,
            can_report_bounds: ok,
            can_report_stacking: false,
            can_focus: false,
            can_close: false,
            can_move_resize: false,
            detail: match windows {
                Ok(w) => format!(
                    "org.gnome.Shell.Introspect returned {} window(s); activation uses AT-SPI/XWayland fallback",
                    w.len()
                ),
                Err(e) => format!("org.gnome.Shell.Introspect unavailable: {e}"),
            },
        }
    }

    fn list_windows(&self) -> Result<Vec<CompositorWindow>> {
        block_on_compositor(async {
            let conn = zbus::Connection::session()
                .await
                .context("connect session D-Bus for GNOME Introspect")?;
            let proxy = zbus::Proxy::new(
                &conn,
                "org.gnome.Shell",
                "/org/gnome/Shell/Introspect",
                "org.gnome.Shell.Introspect",
            )
            .await
            .context("create org.gnome.Shell.Introspect proxy")?;

            let raw: HashMap<u64, HashMap<String, OwnedValue>> = proxy
                .call("GetWindows", &())
                .await
                .context("GNOME Shell GetWindows failed (Introspect disabled in non-GNOME or locked screen)")?;

            let mut windows: Vec<CompositorWindow> = raw
                .into_iter()
                .map(|(id, props)| parse_introspect_entry(id, &props))
                .collect();

            // Place focused windows first, preserving stable native ID ordering.
            windows.sort_by_key(|w| (!w.is_focused, w.native_id));
            for (rank, w) in windows.iter_mut().enumerate() {
                w.z_order_rank = rank;
            }
            Ok(windows)
        })
    }
}

fn prop_str(props: &HashMap<String, OwnedValue>, key: &str) -> String {
    props
        .get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default()
}

fn prop_i32(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    let val = props.get(key)?;
    i32::try_from(val.clone())
        .ok()
        .or_else(|| u32::try_from(val.clone()).ok().and_then(|u| i32::try_from(u).ok()))
        .or_else(|| i64::try_from(val.clone()).ok().and_then(|u| i32::try_from(u).ok()))
}

fn prop_bool(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false)
}

fn parse_introspect_entry(id: u64, props: &HashMap<String, OwnedValue>) -> CompositorWindow {
    let title = prop_str(props, "title");
    let app_id = {
        let a = prop_str(props, "app-id");
        if a.is_empty() {
            prop_str(props, "wm-class")
        } else {
            a
        }
    };
    let pid = props
        .get("pid")
        .and_then(|v| u32::try_from(v.clone()).ok().or_else(|| i32::try_from(v.clone()).ok().and_then(|p| u32::try_from(p).ok())))
        .filter(|&p| p > 0);

    let bounds = match (
        prop_i32(props, "x"),
        prop_i32(props, "y"),
        prop_i32(props, "width"),
        prop_i32(props, "height"),
    ) {
        (Some(x), Some(y), Some(w), Some(h)) if w > 0 && h > 0 => Some((x, y, w, h)),
        _ => None,
    };

    CompositorWindow {
        native_id: id & MAX_SAFE_JS_HWND,
        title,
        app_id,
        pid,
        bounds,
        is_focused: prop_bool(props, "has-focus"),
        is_minimized: prop_bool(props, "is-hidden"),
        z_order_rank: 0,
        backend: CompositorKind::GnomeWayland,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    #[test]
    fn parses_gnome_introspect_properties() {
        let mut props = HashMap::new();
        props.insert("title".to_string(), OwnedValue::try_from(Value::from("Firefox")).unwrap());
        props.insert("app-id".to_string(), OwnedValue::try_from(Value::from("firefox")).unwrap());
        props.insert("pid".to_string(), OwnedValue::try_from(Value::from(2468i32)).unwrap());
        props.insert("x".to_string(), OwnedValue::try_from(Value::from(50i32)).unwrap());
        props.insert("y".to_string(), OwnedValue::try_from(Value::from(60i32)).unwrap());
        props.insert("width".to_string(), OwnedValue::try_from(Value::from(1280i32)).unwrap());
        props.insert("height".to_string(), OwnedValue::try_from(Value::from(720i32)).unwrap());
        props.insert("has-focus".to_string(), OwnedValue::try_from(Value::from(true)).unwrap());

        let win = parse_introspect_entry(987654, &props);
        assert_eq!(win.native_id, 987654);
        assert_eq!(win.title, "Firefox");
        assert_eq!(win.app_id, "firefox");
        assert_eq!(win.pid, Some(2468));
        assert_eq!(win.bounds, Some((50, 60, 1280, 720)));
        assert!(win.is_focused);
    }
}
