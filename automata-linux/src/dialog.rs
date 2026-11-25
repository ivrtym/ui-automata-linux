use std::time::{Duration, Instant};

use crate::{Desktop, LinuxElement};

fn find_named_descendant(root: &LinuxElement, name: &str, depth: usize) -> Option<LinuxElement> {
    if depth > 24 {
        return None;
    }
    for child in root.children_elems().ok()? {
        if child.name().as_deref() == Some(name) {
            return Some(child);
        }
        if let Some(found) = find_named_descendant(&child, name, depth + 1) {
            return Some(found);
        }
    }
    None
}

/// Find a top-level window named `dialog_title`, wait up to `timeout` for it to appear,
/// then find a descendant named `button_name` and invoke/click it.
///
/// Returns `true` if the button was successfully actioned; `false` if the dialog never appears.
pub fn dismiss_dialog_button(dialog_title: &str, button_name: &str, timeout: Duration) -> bool {
    let desktop = Desktop::new();
    let deadline = Instant::now() + timeout;

    let dialog = loop {
        if let Ok(windows) = desktop.application_windows() {
            if let Some(win) = windows
                .into_iter()
                .find(|w| w.name().as_deref() == Some(dialog_title))
            {
                break win;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let Some(button) = find_named_descendant(&dialog, button_name, 0) else {
        return false;
    };

    button.invoke().is_ok() || button.click().is_ok()
}
