//! Unified capability snapshot aggregating compositor, AT-SPI2, input,
//! screenshot, OCR, and browser capabilities for `AutomationService::info()`.

use serde::{Deserialize, Serialize};

use crate::a11y::A11yClient;
use crate::backend::{
    InputKind, command_exists, platform_diagnostics, resolve_input_backend,
    resolve_ocr_backend, resolve_screenshot_backend,
};
use crate::compositor::active_backend;

/// Explicit runtime capability matrix exposed to Rust consumers, CLI, and MCP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesSnapshot {
    pub display_backend: String,
    pub compositor: String,
    pub pointer_backend: Option<String>,
    pub keyboard_backend: Option<String>,
    pub clipboard_backend: Option<String>,
    pub screenshot_backend: Option<String>,
    pub ocr_backend: Option<String>,
    pub atspi_connected: bool,
    pub list_windows: bool,
    pub tree: bool,
    pub window_bounds: bool,
    pub window_stacking: bool,
    pub window_focus: bool,
    pub window_close: bool,
    pub window_move_resize: bool,
    pub input_pointer: bool,
    pub input_keyboard: bool,
    pub background_input: bool,
    pub capture: bool,
    pub ocr: bool,
    pub browser_cdp: bool,
    pub detail: String,
}

impl CapabilitiesSnapshot {
    /// Probes current session capabilities using cached compositor and platform checks.
    pub fn probe() -> Self {
        let diag = platform_diagnostics();
        let comp = active_backend().capabilities();
        let pointer_ok = resolve_input_backend(InputKind::Pointer).ok();
        let keyboard_ok = resolve_input_backend(InputKind::Keyboard).ok();
        let screenshot_ok = resolve_screenshot_backend().ok();
        let ocr_ok = resolve_ocr_backend().ok();
        let atspi_connected = A11yClient::shared().is_ok();
        let browser_cdp = command_exists("google-chrome")
            || command_exists("chromium")
            || command_exists("chromium-browser")
            || command_exists("microsoft-edge");

        Self {
            display_backend: diag.display,
            compositor: comp.kind.as_str().to_string(),
            pointer_backend: pointer_ok.map(|b| b.as_str().to_string()),
            keyboard_backend: keyboard_ok.map(|b| b.as_str().to_string()),
            clipboard_backend: (diag.clipboard_backend != "none").then_some(diag.clipboard_backend),
            screenshot_backend: screenshot_ok.map(|b| b.as_str().to_string()),
            ocr_backend: ocr_ok.map(|b| b.as_str().to_string()),
            atspi_connected,
            list_windows: atspi_connected || comp.can_list_windows,
            tree: atspi_connected,
            window_bounds: atspi_connected || comp.can_report_bounds,
            window_stacking: comp.can_report_stacking,
            window_focus: comp.can_focus || atspi_connected,
            window_close: comp.can_close || atspi_connected,
            window_move_resize: comp.can_move_resize || atspi_connected,
            input_pointer: pointer_ok.is_some(),
            input_keyboard: keyboard_ok.is_some(),
            background_input: atspi_connected,
            capture: screenshot_ok.is_some(),
            ocr: ocr_ok.is_some(),
            browser_cdp,
            detail: comp.detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_snapshot_roundtrips_json() {
        let snap = CapabilitiesSnapshot {
            display_backend: "x11".into(),
            compositor: "x11".into(),
            pointer_backend: Some("x11rb-xtest".into()),
            keyboard_backend: Some("xdotool".into()),
            clipboard_backend: Some("xclip".into()),
            screenshot_backend: Some("x11rb".into()),
            ocr_backend: Some("tesseract".into()),
            atspi_connected: true,
            list_windows: true,
            tree: true,
            window_bounds: true,
            window_stacking: true,
            window_focus: true,
            window_close: true,
            window_move_resize: true,
            input_pointer: true,
            input_keyboard: true,
            background_input: true,
            capture: true,
            ocr: true,
            browser_cdp: false,
            detail: "ok".into(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: CapabilitiesSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap);
    }
}
