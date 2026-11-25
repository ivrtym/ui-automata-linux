use schemars::JsonSchema;
use serde::Deserialize;

use crate::AutomataError;

// ── Browser / TabInfo ─────────────────────────────────────────────────────────

/// Basic info about a browser tab.
#[derive(Debug, Clone)]
pub struct TabInfo {
    pub title: String,
    pub url: String,
}

/// CDP browser: operations for controlling browser sessions and tabs.
pub trait Browser: Send + Sync + 'static {
    /// Ensure the browser is running with CDP enabled.
    fn ensure(&self) -> Result<(), AutomataError>;
    /// Open a new tab at `url` (or about:blank). Returns the CDP tab ID.
    fn open_tab(&self, url: Option<&str>) -> Result<String, AutomataError>;
    /// Close a tab by CDP target ID.
    fn close_tab(&self, tab_id: &str) -> Result<(), AutomataError>;
    /// Bring a tab to the foreground (switch to it).
    fn activate_tab(&self, tab_id: &str) -> Result<(), AutomataError>;
    /// Navigate a tab to a URL.
    fn navigate(&self, tab_id: &str, url: &str) -> Result<(), AutomataError>;
    /// Evaluate a JS expression in a tab. Returns the string result.
    fn eval(&self, tab_id: &str, expr: &str) -> Result<String, AutomataError>;
    /// Title + URL of a specific tab.
    fn tab_info(&self, tab_id: &str) -> Result<TabInfo, AutomataError>;
    /// All open tabs: (tab_id, TabInfo).
    fn tabs(&self) -> Result<Vec<(String, TabInfo)>, AutomataError>;
}

/// Type of mouse click to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClickType {
    #[default]
    Left,
    Double,
    Triple,
    Right,
    Middle,
}

/// Rectangular region in screen coordinates (or relative to an element/window).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct OcrRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// A single recognized OCR text block in screen coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrMatch {
    pub text: String,
    pub confidence: f32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl OcrMatch {
    /// Return the screen-space center `(cx, cy)` of this OCR block.
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }
}

/// A UI element handle. Implementations wrap platform-specific COM/UIA objects.
///
/// All property methods return `None` / empty on a stale handle; interaction
/// methods return `Err(AutomataError::Platform(...))` on staleness or failure.
pub trait Element: Clone + 'static {
    // ── Properties ───────────────────────────────────────────────────────────

    /// Localized name / label of the element. `None` if empty or unavailable.
    fn name(&self) -> Option<String>;

    /// Localized role string from `GetLocalizedControlType` (e.g. "button", "pane", "tool bar").
    fn role(&self) -> String;

    /// Value text (ValuePattern) or name fallback.
    fn text(&self) -> Result<String, AutomataError>;

    /// Direct children's names joined by newlines, excluding the element's own name.
    fn inner_text(&self) -> Result<String, AutomataError>;

    fn is_enabled(&self) -> Result<bool, AutomataError>;
    fn is_visible(&self) -> Result<bool, AutomataError>;
    fn process_id(&self) -> Result<u32, AutomataError>;
    /// Process name (without .exe) for this element's owning process.
    /// Returns `None` on non-Windows or if the lookup fails.
    fn process_name(&self) -> Option<String> {
        None
    }
    /// Native window handle (HWND as u64) for this element's owning window.
    /// Returns `None` on non-Windows or if the lookup fails.
    fn hwnd(&self) -> Option<u64> {
        None
    }

    /// UIA AutomationId property. `None` if empty or unavailable.
    fn automation_id(&self) -> Option<String> {
        None
    }

    /// Bounding box as `(x, y, width, height)`.
    fn bounds(&self) -> Result<(i32, i32, i32, i32), AutomataError>;

    /// Direct children. Returns `Err` on a stale handle.
    fn children(&self) -> Result<Vec<Self>, AutomataError>;

    /// Returns `false` when the element has been detached from the accessibility
    /// tree (e.g. a dismissed dialog). Root windows have the desktop as parent so
    /// they return `true`. Default returns `true` for platforms that don't implement it.
    fn has_parent(&self) -> bool {
        true
    }

    /// Navigate to this element's parent. Returns `None` at the root or on error.
    fn parent(&self) -> Option<Self> {
        None
    }

    // ── Interactions ─────────────────────────────────────────────────────────

    fn click(&self) -> Result<(), AutomataError>;
    fn double_click(&self) -> Result<(), AutomataError>;

    /// Move the mouse cursor to the centre of the element without clicking.
    fn hover(&self) -> Result<(), AutomataError>;

    /// Click at a position expressed as fractions of the element's bounding box.
    fn click_at(&self, x_pct: f64, y_pct: f64, kind: ClickType) -> Result<(), AutomataError>;

    fn type_text(&self, text: &str) -> Result<(), AutomataError>;
    fn press_key(&self, key: &str) -> Result<(), AutomataError>;

    /// Set a field's value directly via IValuePattern (avoids needing to
    /// select-all + type). Preferred over `type_text` for pre-filled fields.
    fn set_value(&self, value: &str) -> Result<(), AutomataError>;

    fn focus(&self) -> Result<(), AutomataError>;

    /// Activate this element via UIA's `IInvokePattern::Invoke()`.
    ///
    /// Unlike `click()`, `invoke()` does not require a valid bounding rect —
    /// it works on off-screen elements whose bounds are `(0,0,1,1)` because they
    /// are scrolled out of view.  Prefer this over `Click` + `ScrollIntoView`
    /// for items in virtualised or scrollable lists (e.g. Settings nav items,
    /// WinUI ListView rows) where mouse-wheel scrolling causes elastic snap-back.
    ///
    /// Also tries `SelectionItemPattern` when `InvokePattern` is unavailable.
    /// Returns an error if neither pattern is supported — does not fall back to `click()`.
    fn invoke(&self) -> Result<(), AutomataError> {
        // Default: fall back to click for platforms that don't override this.
        self.click()
    }

    /// Scroll ancestor containers until this element is within their visible
    /// viewport. Uses `ScrollItemPattern` when supported; falls back to a
    /// geometric ancestor walk with `ScrollPattern`.
    fn scroll_into_view(&self) -> Result<(), AutomataError>;

    fn activate_window(&self) -> Result<(), AutomataError>;
    fn minimize_window(&self) -> Result<(), AutomataError>;
    fn close(&self) -> Result<(), AutomataError>;
}

/// Platform desktop: discovers windows and provides foreground state.
pub trait Desktop: Send + 'static {
    type Elem: Element;
    type Browser: Browser;

    /// CDP browser handle. Used by the workflow engine for browser automation.
    fn browser(&self) -> &Self::Browser;

    /// All top-level application windows currently visible.
    fn application_windows(&self) -> Result<Vec<Self::Elem>, AutomataError>;

    /// Launch an executable by name or full path. Returns the process ID.
    fn open_application(&self, exe: &str) -> Result<u32, AutomataError>;

    /// Launch an executable with explicit environment overrides.
    /// Platforms that do not customize process environment fall back to [`open_application`](Self::open_application).
    fn open_application_with_env(
        &self,
        exe: &str,
        _env: &std::collections::HashMap<String, String>,
    ) -> Result<u32, AutomataError> {
        self.open_application(exe)
    }

    /// The element for the current OS foreground window (`GetForegroundWindow`).
    /// Returns `None` if there is no foreground window or the lookup fails.
    fn foreground_window(&self) -> Option<Self::Elem>;

    /// Raw HWND as `u64` for process-ownership checks without a full element
    /// query. Returns `None` on non-Windows or when nothing is focused.
    fn foreground_hwnd(&self) -> Option<u64>;

    /// All currently visible tooltip windows on the desktop.
    /// Returns an empty vec on non-Windows or if none are present.
    fn tooltip_windows(&self) -> Vec<Self::Elem> {
        vec![]
    }

    /// Top-level window handles in Z-order (topmost first).
    ///
    /// Used by the anchor resolver to prefer the topmost window of a process
    /// when multiple windows match the same filter. Returns an empty vec on
    /// non-Windows or if the enumeration fails.
    fn hwnd_z_order(&self) -> Vec<u64> {
        vec![]
    }

    /// Resolve the first descendant matching `selector` along with its step-parent.
    /// Platforms with fast batch/wave-BFS tree snapshots (e.g. Linux AT-SPI2) can
    /// override this to evaluate deep selectors in memory rather than crawling live RPCs.
    fn find_descendant_with_parent(
        &self,
        root: &Self::Elem,
        selector: &crate::SelectorPath,
    ) -> Option<(Self::Elem, Option<Self::Elem>)> {
        selector.find_one_with_parent(root)
    }

    /// Resolve all descendants matching `selector` from `root`.
    /// Defaults to [`SelectorPath::find_all`].
    fn find_all_descendants(
        &self,
        root: &Self::Elem,
        selector: &crate::SelectorPath,
    ) -> Vec<Self::Elem> {
        selector.find_all(root)
    }

    // ── Fine-grained vision / OCR & pointer primitives ───────────────────────

    /// Capture a window (`hwnd`), sub-region (`region`), or full screen and run OCR.
    /// Returned bounding boxes must be in global screen coordinates.
    fn ocr_recognize(
        &self,
        _hwnd: Option<u64>,
        _region: Option<OcrRegion>,
    ) -> Result<Vec<OcrMatch>, AutomataError> {
        Err(AutomataError::Platform(
            "OCR vision is not implemented for this platform".into(),
        ))
    }

    /// Move the OS pointer to absolute screen coordinates `(x, y)`.
    fn pointer_move(&self, _x: i32, _y: i32) -> Result<(), AutomataError> {
        Err(AutomataError::Platform(
            "pointer_move is not implemented for this platform".into(),
        ))
    }

    /// Click at absolute screen coordinates `(x, y)`.
    fn pointer_click(&self, _x: i32, _y: i32, _kind: ClickType) -> Result<(), AutomataError> {
        Err(AutomataError::Platform(
            "pointer_click is not implemented for this platform".into(),
        ))
    }

    /// Type text into the currently focused target (e.g. after `pointer_click`).
    fn pointer_type(&self, _text: &str) -> Result<(), AutomataError> {
        Err(AutomataError::Platform(
            "pointer_type is not implemented for this platform".into(),
        ))
    }

    /// Scroll the mouse wheel at absolute screen coordinates `(x, y)` by `(delta_x, delta_y)` ticks
    /// (positive `delta_y` scrolls up, negative scrolls down; positive `delta_x` scrolls right).
    fn pointer_scroll(
        &self,
        _x: i32,
        _y: i32,
        _delta_x: i32,
        _delta_y: i32,
    ) -> Result<(), AutomataError> {
        Err(AutomataError::Platform(
            "pointer_scroll is not implemented for this platform".into(),
        ))
    }

    /// Press left mouse button at `(x1, y1)`, drag to `(x2, y2)`, and release.
    fn pointer_drag(&self, _x1: i32, _y1: i32, _x2: i32, _y2: i32) -> Result<(), AutomataError> {
        Err(AutomataError::Platform(
            "pointer_drag is not implemented for this platform".into(),
        ))
    }

    /// Send a synthetic key or key chord (e.g. `"ctrl+f"`, `"{ESCAPE}"`) without targeting an element.
    fn pointer_key(&self, _key: &str) -> Result<(), AutomataError> {
        Err(AutomataError::Platform(
            "pointer_key is not implemented for this platform".into(),
        ))
    }

    /// Capture a screenshot of `hwnd` / `region` / full screen to `path` (PNG).
    fn capture_screenshot(
        &self,
        _hwnd: Option<u64>,
        _region: Option<OcrRegion>,
        _path: &str,
    ) -> Result<(), AutomataError> {
        Err(AutomataError::Platform(
            "capture_screenshot is not implemented for this platform".into(),
        ))
    }
}

/// Compare an observed platform process name against an expected filter or target.
///
/// Matches case-insensitively on exact equality and tolerates Linux's 15-byte
/// `/proc/<pid>/comm` truncation when `observed` is a 15-byte prefix of `expected`
/// (e.g. `"gnome-calculato"` vs `"gnome-calculator"`).
pub fn process_names_match(observed: &str, expected: &str) -> bool {
    let obs = observed.trim();
    let exp = expected.trim();
    if obs.eq_ignore_ascii_case(exp) {
        return true;
    }
    obs.len() == 15
        && exp.len() > 15
        && exp.is_char_boundary(15)
        && exp[..15].eq_ignore_ascii_case(obs)
}

#[cfg(test)]
mod tests {
    use super::process_names_match;

    #[test]
    fn matches_exact_and_15_byte_linux_comm_truncation() {
        assert!(process_names_match("gnome-calculator", "gnome-calculator"));
        assert!(process_names_match("gnome-calculato", "gnome-calculator"));
        assert!(process_names_match("GNOME-CALCULATO", "gnome-calculator"));
        assert!(!process_names_match("app", "apple"));
        assert!(!process_names_match("gnome-text-edit", "gnome-calculator"));
    }
}
