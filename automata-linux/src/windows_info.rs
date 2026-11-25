use crate::a11y::{A11yClient, synthetic_hwnd};
use crate::{LinuxElement, Result, process};

/// Basic info about a top-level Linux AT-SPI2 window.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowInfo {
    pub pid: u32,
    pub process_name: String,
    pub title: String,
    pub control_type: String,
    pub automation_id: String,
    pub hwnd: u64,
    pub bus_name: String,
    pub object_path: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub is_minimized: bool,
    pub is_active: bool,
}

/// Inspect a `LinuxElement` window into serializable [`WindowInfo`].
pub fn inspect_window_info(el: &LinuxElement, client: &A11yClient) -> WindowInfo {
    let pid = el.process_id().unwrap_or(0);
    let process_name = if pid > 0 {
        process::get_process_name(pid as i32).unwrap_or_else(|_| format!("unknown-{pid}"))
    } else {
        "unknown".to_string()
    };
    let bus_name = el.bus_name().to_string();
    let object_path = el.object_path().to_string();
    let (x, y, width, height) = el.bounds().unwrap_or((0, 0, 0, 0));
    WindowInfo {
        pid,
        process_name,
        title: el.name_or_empty(),
        control_type: el.role(),
        automation_id: el.automation_id().unwrap_or_default(),
        hwnd: synthetic_hwnd(&bus_name, &object_path),
        is_active: client.is_active_window(&bus_name, &object_path),
        bus_name,
        object_path,
        x,
        y,
        width,
        height,
        is_minimized: el.is_minimized(),
    }
}

/// Return all top-level windows visible via AT-SPI2 using a shared [`A11yClient`].
pub fn application_windows() -> Result<Vec<WindowInfo>> {
    let (client, apps) = A11yClient::with_shared_retry(|client| {
        let apps = client.root_applications()?;
        Ok((client.clone(), apps))
    })?;
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
            if !client.is_top_level_window(&w_bus, &w_path) {
                continue;
            }
            let el = LinuxElement::new(client.clone(), w_bus, w_path);
            windows.push(inspect_window_info(&el, &client));
        }
    }

    Ok(windows)
}

/// Return windows belonging to the given process name (case-insensitive).
pub fn find_windows(process_name: &str) -> Result<Vec<WindowInfo>> {
    let all = application_windows()?;
    Ok(all
        .into_iter()
        .filter(|w| ui_automata::process_names_match(&w.process_name, process_name))
        .collect())
}
