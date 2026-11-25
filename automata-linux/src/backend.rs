use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::RwLock;
use std::time::Duration;

use crate::UiError;

/// Default idle timeout before closing an unused ScreenCast Portal session in persistent mode.
pub const DEFAULT_SCREENCAST_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Lifecycle strategy for desktop connections (`AUTOMATA_CONNECTION_MODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionMode {
    /// Short-lived per-operation connections (safe default for CLI / one-shot invocations).
    #[default]
    OneShot,
    /// Reuse X11 socket/atom cache across calls and hold ScreenCast Portal session until idle.
    Persistent {
        screencast_idle_timeout: Duration,
    },
}

impl ConnectionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OneShot => "oneshot",
            Self::Persistent { .. } => "persistent",
        }
    }

    pub fn persistent_default() -> Self {
        Self::Persistent {
            screencast_idle_timeout: DEFAULT_SCREENCAST_IDLE_TIMEOUT,
        }
    }

    /// Parse a mode string (`auto` maps to `Ok(None)`).
    pub fn parse_override(raw: &str) -> Result<Option<Self>, UiError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(None),
            "oneshot" | "one-shot" | "single" => Ok(Some(Self::OneShot)),
            "persistent" | "long" | "keepalive" => Ok(Some(Self::persistent_default())),
            other => Err(UiError::Internal(format!(
                "invalid connection mode {other:?}; expected 'auto', 'oneshot', or 'persistent'"
            ))),
        }
    }
}

static CONNECTION_MODE_OVERRIDE: RwLock<Option<Option<ConnectionMode>>> = RwLock::new(None);

/// Programmatically override the connection lifecycle mode (`None` = auto/caller default).
pub fn set_connection_mode_override(mode: Option<ConnectionMode>) {
    if let Ok(mut guard) = CONNECTION_MODE_OVERRIDE.write() {
        *guard = Some(mode);
    }
}

/// Clear any programmatic connection mode override so env/caller defaults apply.
pub fn clear_connection_mode_override() {
    if let Ok(mut guard) = CONNECTION_MODE_OVERRIDE.write() {
        *guard = None;
    }
}

/// Apply an optional CLI/config connection mode string (`auto|oneshot|persistent`).
pub fn apply_connection_mode_arg(raw: Option<&str>) -> Result<(), UiError> {
    let Some(val) = raw else {
        return Ok(());
    };
    let parsed = ConnectionMode::parse_override(val)?;
    set_connection_mode_override(parsed);
    Ok(())
}

/// Inspect the current programmatic connection mode override, if explicitly set.
pub fn connection_mode_override() -> Option<ConnectionMode> {
    CONNECTION_MODE_OVERRIDE
        .read()
        .ok()
        .and_then(|g| (*g).flatten())
}

/// Whether the user explicitly pinned `oneshot` or `persistent` via API/CLI/env rather than `auto`.
pub fn is_connection_mode_explicit() -> bool {
    if let Ok(guard) = CONNECTION_MODE_OVERRIDE.read() {
        match *guard {
            Some(Some(_)) => return true,
            Some(None) => return false,
            None => {}
        }
    }
    std::env::var("AUTOMATA_CONNECTION_MODE")
        .ok()
        .is_some_and(|raw| !matches!(raw.trim().to_ascii_lowercase().as_str(), "" | "auto"))
}

/// Resolve the effective connection mode given a caller-supplied `auto` default.
///
/// Precedence: explicit programmatic/CLI override > `AUTOMATA_CONNECTION_MODE` > `default_for_caller`.
pub fn resolve_connection_mode(default_for_caller: ConnectionMode) -> Result<ConnectionMode, UiError> {
    if let Ok(guard) = CONNECTION_MODE_OVERRIDE.read() {
        match *guard {
            Some(Some(m)) => return Ok(m),
            Some(None) => return Ok(default_for_caller),
            None => {}
        }
    }
    if let Ok(raw) = std::env::var("AUTOMATA_CONNECTION_MODE") {
        if let Some(m) = ConnectionMode::parse_override(&raw)? {
            return Ok(m);
        }
    }
    Ok(default_for_caller)
}

/// Active display server mode (auto-detected or overridden via `AUTOMATA_DISPLAY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayBackend {
    Wayland,
    X11,
}

impl DisplayBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wayland => "wayland",
            Self::X11 => "x11",
        }
    }
}

/// Synthetic pointer/keyboard backend (`AUTOMATA_INPUT_BACKEND`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBackend {
    /// Pure-Rust X11 (`x11rb` XTEST extension + core keymap).
    X11XTest,
    Xdotool,
    Ydotool,
    Wtype,
}

impl InputBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X11XTest => "x11rb-xtest",
            Self::Xdotool => "xdotool",
            Self::Ydotool => "ydotool",
            Self::Wtype => "wtype",
        }
    }
}

/// Clipboard backend (`AUTOMATA_CLIPBOARD_BACKEND`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardBackend {
    WlClipboard,
    Xclip,
    Xsel,
}

impl ClipboardBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WlClipboard => "wl-clipboard",
            Self::Xclip => "xclip",
            Self::Xsel => "xsel",
        }
    }
}

/// OCR inference backend (`AUTOMATA_OCR_BACKEND`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrBackend {
    OrtPpOcr,
    Tesseract,
    OmniParser,
}

impl OcrBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OrtPpOcr => "ort-ppocr",
            Self::Tesseract => "tesseract",
            Self::OmniParser => "omniparser",
        }
    }
}

/// Screen/window capture backend (`AUTOMATA_SCREENSHOT_BACKEND`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotBackend {
    /// Pure-Rust X11 (`x11rb`) direct pixel capture (works on X11 and XWayland windows).
    X11rb,
    /// XDG Desktop Portal ScreenCast + PipeWire (`gst-launch-1.0 pipewiresrc`).
    ScreenCast,
    Grim,
    GnomeScreenshot,
    Import,
    Scrot,
    Portal,
}

impl ScreenshotBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X11rb => "x11rb",
            Self::ScreenCast => "screencast",
            Self::Grim => "grim",
            Self::GnomeScreenshot => "gnome-screenshot",
            Self::Import => "import",
            Self::Scrot => "scrot",
            Self::Portal => "portal",
        }
    }

    /// Parse a channel name (`auto` maps to `Ok(None)`).
    pub fn parse_override(raw: &str) -> Result<Option<Self>, UiError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(None),
            "x11rb" | "x11" => Ok(Some(Self::X11rb)),
            "screencast" | "pipewire" => Ok(Some(Self::ScreenCast)),
            "grim" => Ok(Some(Self::Grim)),
            "gnome-screenshot" => Ok(Some(Self::GnomeScreenshot)),
            "import" => Ok(Some(Self::Import)),
            "scrot" => Ok(Some(Self::Scrot)),
            "portal" => Ok(Some(Self::Portal)),
            other => Err(UiError::Internal(format!(
                "invalid screenshot backend {other:?}; expected 'auto', 'x11rb', 'screencast', 'grim', 'gnome-screenshot', 'import', 'scrot', or 'portal'"
            ))),
        }
    }
}

static SCREENSHOT_BACKEND_OVERRIDE: RwLock<Option<Option<ScreenshotBackend>>> = RwLock::new(None);

/// Programmatically override the active screenshot capture channel (`None` = auto).
pub fn set_screenshot_backend_override(backend: Option<ScreenshotBackend>) {
    if let Ok(mut guard) = SCREENSHOT_BACKEND_OVERRIDE.write() {
        *guard = Some(backend);
    }
}

/// Clear any programmatic screenshot override so environment/auto detection applies.
pub fn clear_screenshot_backend_override() {
    if let Ok(mut guard) = SCREENSHOT_BACKEND_OVERRIDE.write() {
        *guard = None;
    }
}

/// Apply an optional CLI/config channel string (`auto|x11rb|screencast|grim|gnome-screenshot|import|scrot|portal`).
pub fn apply_screenshot_backend_arg(raw: Option<&str>) -> Result<(), UiError> {
    let Some(val) = raw else {
        return Ok(());
    };
    let parsed = ScreenshotBackend::parse_override(val)?;
    set_screenshot_backend_override(parsed);
    if parsed.is_some() {
        resolve_screenshot_backend()?;
    }
    Ok(())
}

/// Inspect the current programmatic screenshot backend override, if explicitly fixed.
pub fn screenshot_backend_override() -> Option<ScreenshotBackend> {
    SCREENSHOT_BACKEND_OVERRIDE
        .read()
        .ok()
        .and_then(|g| (*g).flatten())
}

/// Whether the user explicitly pinned a concrete screenshot channel (via API or env)
/// rather than leaving selection in `auto` mode.
pub fn is_screenshot_backend_explicit() -> bool {
    if let Ok(guard) = SCREENSHOT_BACKEND_OVERRIDE.read() {
        match *guard {
            Some(Some(_)) => return true,
            Some(None) => return false,
            None => {}
        }
    }
    std::env::var("AUTOMATA_SCREENSHOT_BACKEND")
        .ok()
        .is_some_and(|raw| !matches!(raw.trim().to_ascii_lowercase().as_str(), "" | "auto"))
}

fn is_wlroots_wayland() -> bool {
    if std::env::var_os("SWAYSOCK").is_some()
        || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        || std::env::var_os("WAYFIRE_SOCKET").is_some()
        || std::env::var_os("LABWC_PID").is_some()
    {
        return true;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase();
    ["sway", "hyprland", "wlroots", "wayfire", "river", "labwc", "niri"]
        .iter()
        .any(|k| desktop.contains(k))
}

pub fn command_exists(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| dir.join(name).is_file())
}

pub fn has_x11_display() -> bool {
    std::env::var("DISPLAY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

pub fn has_x11_xtest() -> bool {
    if !has_x11_display() {
        return false;
    }
    use x11rb::protocol::xtest::ConnectionExt as XTestExt;
    x11rb::connect(None)
        .ok()
        .and_then(|(conn, _)| conn.xtest_get_version(2, 1).ok()?.reply().ok())
        .is_some()
}

pub fn has_wayland_display() -> bool {
    std::env::var("WAYLAND_DISPLAY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// Resolve display server mode from `AUTOMATA_DISPLAY` (`wayland` | `x11` | `auto`),
/// falling back to `WAYLAND_DISPLAY` / `XDG_SESSION_TYPE` / `DISPLAY`.
pub fn resolve_display_backend() -> Result<DisplayBackend, UiError> {
    if let Ok(raw) = std::env::var("AUTOMATA_DISPLAY") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => {}
            "wayland" => return Ok(DisplayBackend::Wayland),
            "x11" | "xorg" => return Ok(DisplayBackend::X11),
            other => {
                return Err(UiError::Internal(format!(
                    "invalid AUTOMATA_DISPLAY={other:?}; expected 'auto', 'wayland', or 'x11'"
                )));
            }
        }
    }

    if has_wayland_display()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
    {
        Ok(DisplayBackend::Wayland)
    } else {
        Ok(DisplayBackend::X11)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Pointer,
    Keyboard,
    CursorQuery,
}

/// Resolve input backend from `AUTOMATA_INPUT_BACKEND`
/// (`auto` | `xtest` | `xdotool` | `ydotool` | `wtype`)
/// or auto-detect based on display session and available tools.
pub fn resolve_input_backend(kind: InputKind) -> Result<InputBackend, UiError> {
    if let Ok(raw) = std::env::var("AUTOMATA_INPUT_BACKEND") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => {}
            "xtest" | "x11rb-xtest" => {
                if !has_x11_display() {
                    return Err(UiError::Platform(
                        "AUTOMATA_INPUT_BACKEND=xtest requested, but DISPLAY is not set".into(),
                    ));
                }
                if kind != InputKind::CursorQuery && !has_x11_xtest() {
                    return Err(UiError::Platform(
                        "AUTOMATA_INPUT_BACKEND=xtest requested, but XTEST extension is unavailable on DISPLAY"
                            .into(),
                    ));
                }
                return Ok(InputBackend::X11XTest);
            }
            "xdotool" => {
                if !command_exists("xdotool") {
                    return Err(UiError::Platform(
                        "AUTOMATA_INPUT_BACKEND=xdotool requested, but `xdotool` is not in PATH"
                            .into(),
                    ));
                }
                if !has_x11_display() {
                    return Err(UiError::Platform(
                        "AUTOMATA_INPUT_BACKEND=xdotool requested, but DISPLAY is not set".into(),
                    ));
                }
                return Ok(InputBackend::Xdotool);
            }
            "ydotool" => {
                if !command_exists("ydotool") {
                    return Err(UiError::Platform(
                        "AUTOMATA_INPUT_BACKEND=ydotool requested, but `ydotool` is not in PATH"
                            .into(),
                    ));
                }
                if kind == InputKind::CursorQuery {
                    return Err(UiError::Platform(
                        "ydotool does not support querying cursor coordinates".into(),
                    ));
                }
                return Ok(InputBackend::Ydotool);
            }
            "wtype" => {
                if !command_exists("wtype") {
                    return Err(UiError::Platform(
                        "AUTOMATA_INPUT_BACKEND=wtype requested, but `wtype` is not in PATH".into(),
                    ));
                }
                if kind != InputKind::Keyboard {
                    return Err(UiError::Platform(
                        "wtype only supports keyboard input (not mouse pointer actions)".into(),
                    ));
                }
                return Ok(InputBackend::Wtype);
            }
            other => {
                return Err(UiError::Internal(format!(
                    "invalid AUTOMATA_INPUT_BACKEND={other:?}; expected 'auto', 'xtest', 'xdotool', 'ydotool', or 'wtype'"
                )));
            }
        }
    }

    let display = resolve_display_backend()?;
    let xtest_ready = has_x11_xtest();
    let xdotool_ready = command_exists("xdotool") && has_x11_display();
    let ydotool_ready = command_exists("ydotool") && is_ydotool_usable();
    let wtype_ready = command_exists("wtype");

    let chosen = match (display, kind) {
        (_, InputKind::CursorQuery) => {
            if has_x11_display() {
                Some(InputBackend::X11XTest)
            } else if xdotool_ready {
                Some(InputBackend::Xdotool)
            } else {
                None
            }
        }
        (DisplayBackend::X11, InputKind::Pointer) => {
            if xtest_ready {
                Some(InputBackend::X11XTest)
            } else if xdotool_ready {
                Some(InputBackend::Xdotool)
            } else if ydotool_ready {
                Some(InputBackend::Ydotool)
            } else {
                None
            }
        }
        (DisplayBackend::X11, InputKind::Keyboard) => {
            if xdotool_ready {
                Some(InputBackend::Xdotool)
            } else if xtest_ready {
                Some(InputBackend::X11XTest)
            } else if ydotool_ready {
                Some(InputBackend::Ydotool)
            } else {
                None
            }
        }
        (DisplayBackend::Wayland, InputKind::Pointer) => {
            if ydotool_ready {
                Some(InputBackend::Ydotool)
            } else if xtest_ready {
                Some(InputBackend::X11XTest)
            } else if xdotool_ready {
                Some(InputBackend::Xdotool)
            } else {
                None
            }
        }
        (DisplayBackend::Wayland, InputKind::Keyboard) => {
            if ydotool_ready {
                Some(InputBackend::Ydotool)
            } else if wtype_ready {
                Some(InputBackend::Wtype)
            } else if xdotool_ready {
                Some(InputBackend::Xdotool)
            } else if xtest_ready {
                Some(InputBackend::X11XTest)
            } else {
                None
            }
        }
    };

    chosen.ok_or_else(|| {
        UiError::Platform(format!(
            "no usable input backend found (display={}); install `ydotool` (+ ydotoold) for native Wayland or use X11 XTEST / `xdotool`, or set AUTOMATA_INPUT_BACKEND explicitly. For Wayland apps, prefer AT-SPI2 `Invoke`/`SetValue`.",
            display.as_str()
        ))
    })
}

/// Resolve clipboard backend from `AUTOMATA_CLIPBOARD_BACKEND`
/// (`auto` | `wl-clipboard` | `xclip` | `xsel`) or auto-detect.
pub fn resolve_clipboard_backend() -> Result<ClipboardBackend, UiError> {
    if let Ok(raw) = std::env::var("AUTOMATA_CLIPBOARD_BACKEND") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => {}
            "wl-clipboard" | "wl" | "wayland" => {
                if !command_exists("wl-copy") || !command_exists("wl-paste") {
                    return Err(UiError::Platform(
                        "AUTOMATA_CLIPBOARD_BACKEND=wl-clipboard requested, but wl-copy/wl-paste not found"
                            .into(),
                    ));
                }
                return Ok(ClipboardBackend::WlClipboard);
            }
            "xclip" => {
                if !command_exists("xclip") {
                    return Err(UiError::Platform(
                        "AUTOMATA_CLIPBOARD_BACKEND=xclip requested, but `xclip` not found".into(),
                    ));
                }
                return Ok(ClipboardBackend::Xclip);
            }
            "xsel" => {
                if !command_exists("xsel") {
                    return Err(UiError::Platform(
                        "AUTOMATA_CLIPBOARD_BACKEND=xsel requested, but `xsel` not found".into(),
                    ));
                }
                return Ok(ClipboardBackend::Xsel);
            }
            other => {
                return Err(UiError::Internal(format!(
                    "invalid AUTOMATA_CLIPBOARD_BACKEND={other:?}; expected 'auto', 'wl-clipboard', 'xclip', or 'xsel'"
                )));
            }
        }
    }

    let display = resolve_display_backend()?;
    let wl = command_exists("wl-copy") && command_exists("wl-paste");
    let xclip = command_exists("xclip");
    let xsel = command_exists("xsel");

    let chosen = match display {
        DisplayBackend::Wayland => {
            if wl {
                Some(ClipboardBackend::WlClipboard)
            } else if xclip {
                Some(ClipboardBackend::Xclip)
            } else if xsel {
                Some(ClipboardBackend::Xsel)
            } else {
                None
            }
        }
        DisplayBackend::X11 => {
            if xclip {
                Some(ClipboardBackend::Xclip)
            } else if xsel {
                Some(ClipboardBackend::Xsel)
            } else if wl {
                Some(ClipboardBackend::WlClipboard)
            } else {
                None
            }
        }
    };

    chosen.ok_or_else(|| {
        UiError::Platform(
            "no clipboard tool found; install `wl-clipboard` (Wayland) or `xclip`/`xsel` (X11)"
                .into(),
        )
    })
}

/// Best-effort check whether `ydotool` daemon/uinput is likely usable without hanging.
pub fn is_ydotool_usable() -> bool {
    if !command_exists("ydotool") {
        return false;
    }
    if ydotoold_running() {
        return true;
    }
    // Older ydotool (v0.1.x) opens /dev/uinput directly without ydotoold.
    uinput_writable()
}

pub fn ydotoold_running() -> bool {
    if let Ok(uid) = std::env::var("UID") {
        if Path::new(&format!("/run/user/{uid}/.ydotool_socket")).exists() {
            return true;
        }
    }
    if Path::new("/tmp/.ydotool_socket").exists() {
        return true;
    }
    Command::new("pgrep")
        .arg("-x")
        .arg("ydotoold")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn uinput_writable() -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_ok()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlatformDiagnostics {
    pub display: String,
    pub display_override: Option<String>,
    pub wayland_display: Option<String>,
    pub x11_display: Option<String>,
    pub pointer_backend: String,
    pub keyboard_backend: String,
    pub clipboard_backend: String,
    pub ocr_backend: String,
    pub screenshot_backend: String,
    pub ort_dylib: Option<String>,
    pub ocr_model_dir: Option<String>,
    pub input_override: Option<String>,
    pub clipboard_override: Option<String>,
    pub ocr_override: Option<String>,
    pub screenshot_override: Option<String>,
    pub connection_mode: String,
    pub connection_override: Option<String>,
    pub ydotoold_running: bool,
    pub uinput_writable: bool,
    pub installed_tools: Vec<&'static str>,
}

/// Locate the ONNX Runtime shared library for `ort` dynamic loading.
pub fn find_ort_dylib() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
        let pb = PathBuf::from(p.trim());
        if pb.is_file() {
            return Some(pb);
        }
    }
    const CANDIDATES: &[&str] = &[
        "/usr/lib/x86_64-linux-gnu/libonnxruntime.so",
        "/usr/lib/x86_64-linux-gnu/libonnxruntime.so.1.23",
        "/usr/lib/aarch64-linux-gnu/libonnxruntime.so",
        "/usr/lib/libonnxruntime.so",
        "/usr/local/lib/libonnxruntime.so",
    ];
    for c in CANDIDATES {
        let pb = PathBuf::from(c);
        if pb.exists() {
            return Some(pb);
        }
    }
    None
}

/// Locate a directory containing PP-OCRv4/v5 ONNX models (`det.onnx`, `rec.onnx`, `keys.txt`).
pub fn find_ocr_model_dir() -> Option<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(p) = std::env::var("AUTOMATA_OCR_MODEL_DIR") {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            dirs.push(PathBuf::from(trimmed));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(&home).join(".local/share/ui-automata/ocr-models"));
        dirs.push(PathBuf::from(&home).join(".ui-automata/models"));
    }
    dirs.push(PathBuf::from("/usr/share/ui-automata/ocr-models"));

    dirs.into_iter().find(|d| has_ppocr_files(d))
}

pub fn has_ppocr_files(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    resolve_ppocr_file(dir, &["det.onnx", "ch_PP-OCRv5_mobile_det.onnx", "ch_PP-OCRv4_det_infer.onnx"], "det").is_some()
        && resolve_ppocr_file(dir, &["rec.onnx", "ch_PP-OCRv5_mobile_rec.onnx", "ch_PP-OCRv4_rec_infer.onnx"], "rec").is_some()
        && resolve_ppocr_keys(dir).is_some()
}

pub fn resolve_ppocr_file(dir: &Path, preferred: &[&str], keyword: &str) -> Option<PathBuf> {
    for name in preferred {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("onnx") {
            let fname = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if fname.contains(keyword) {
                return Some(path);
            }
        }
    }
    None
}

pub fn resolve_ppocr_keys(dir: &Path) -> Option<PathBuf> {
    for name in ["keys.txt", "ppocr_keys_v1.txt", "ppocrv5_dict.txt", "dict.txt"] {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Resolve OCR backend from `AUTOMATA_OCR_BACKEND` (`auto` | `ort` | `tesseract`).
pub fn resolve_ocr_backend() -> Result<OcrBackend, UiError> {
    let ort_compiled = cfg!(feature = "ocr-ort");
    let ort_ready = ort_compiled && find_ort_dylib().is_some() && find_ocr_model_dir().is_some();
    let tesseract_ready = command_exists("tesseract");

    if let Ok(raw) = std::env::var("AUTOMATA_OCR_BACKEND") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => {}
            "ort" | "ort-ppocr" | "ppocr" => {
                if !ort_compiled {
                    return Err(UiError::Platform(
                        "AUTOMATA_OCR_BACKEND=ort requested, but crate was built without `ocr-ort` feature"
                            .into(),
                    ));
                }
                if find_ort_dylib().is_none() {
                    return Err(UiError::Platform(
                        "AUTOMATA_OCR_BACKEND=ort requested, but libonnxruntime.so was not found (set ORT_DYLIB_PATH)"
                            .into(),
                    ));
                }
                if find_ocr_model_dir().is_none() {
                    return Err(UiError::Platform(
                        "AUTOMATA_OCR_BACKEND=ort requested, but PP-OCR det/rec ONNX + keys.txt were not found (set AUTOMATA_OCR_MODEL_DIR)"
                            .into(),
                    ));
                }
                return Ok(OcrBackend::OrtPpOcr);
            }
            "tesseract" => {
                if !tesseract_ready {
                    return Err(UiError::Platform(
                        "AUTOMATA_OCR_BACKEND=tesseract requested, but `tesseract` is not in PATH"
                            .into(),
                    ));
                }
                return Ok(OcrBackend::Tesseract);
            }
            "omniparser" | "omni" => {
                return Ok(OcrBackend::OmniParser);
            }
            other => {
                return Err(UiError::Internal(format!(
                    "invalid AUTOMATA_OCR_BACKEND={other:?}; expected 'auto', 'ort', 'tesseract', or 'omniparser'"
                )));
            }
        }
    }

    if crate::vision_omniparser::resolve_omniparser_url(false).is_some() {
        Ok(OcrBackend::OmniParser)
    } else if ort_ready {
        Ok(OcrBackend::OrtPpOcr)
    } else if tesseract_ready {
        Ok(OcrBackend::Tesseract)
    } else {
        Err(UiError::Platform(
            "no OCR backend available: install `tesseract` (e.g. tesseract-ocr-chi-sim), configure `ort` + PP-OCRv4/v5 ONNX models via AUTOMATA_OCR_MODEL_DIR, or set AUTOMATA_OMNIPARSER_URL"
                .into(),
        ))
    }
}

fn validate_screenshot_backend(backend: ScreenshotBackend) -> Result<ScreenshotBackend, UiError> {
    match backend {
        ScreenshotBackend::X11rb => {
            if !has_x11_display() {
                return Err(UiError::Platform(
                    "screenshot backend 'x11rb' requested, but DISPLAY is unavailable".into(),
                ));
            }
            Ok(ScreenshotBackend::X11rb)
        }
        ScreenshotBackend::ScreenCast => {
            if !has_wayland_display() {
                return Err(UiError::Platform(
                    "screenshot backend 'screencast' requested, but WAYLAND_DISPLAY is unavailable"
                        .into(),
                ));
            }
            if !command_exists("gst-launch-1.0") {
                return Err(UiError::Platform(
                    "screenshot backend 'screencast' requested, but `gst-launch-1.0` not in PATH"
                        .into(),
                ));
            }
            Ok(ScreenshotBackend::ScreenCast)
        }
        ScreenshotBackend::Grim => {
            if !command_exists("grim") {
                return Err(UiError::Platform(
                    "AUTOMATA_SCREENSHOT_BACKEND=grim requested, but `grim` not in PATH".into(),
                ));
            }
            Ok(ScreenshotBackend::Grim)
        }
        ScreenshotBackend::GnomeScreenshot => {
            if !command_exists("gnome-screenshot") {
                return Err(UiError::Platform(
                    "AUTOMATA_SCREENSHOT_BACKEND=gnome-screenshot requested, but `gnome-screenshot` not in PATH"
                        .into(),
                ));
            }
            Ok(ScreenshotBackend::GnomeScreenshot)
        }
        ScreenshotBackend::Import => {
            if !command_exists("import") || !has_x11_display() {
                return Err(UiError::Platform(
                    "AUTOMATA_SCREENSHOT_BACKEND=import requested, but `import` or DISPLAY unavailable"
                        .into(),
                ));
            }
            Ok(ScreenshotBackend::Import)
        }
        ScreenshotBackend::Scrot => {
            if !command_exists("scrot") || !has_x11_display() {
                return Err(UiError::Platform(
                    "AUTOMATA_SCREENSHOT_BACKEND=scrot requested, but `scrot` or DISPLAY unavailable"
                        .into(),
                ));
            }
            Ok(ScreenshotBackend::Scrot)
        }
        ScreenshotBackend::Portal => Ok(ScreenshotBackend::Portal),
    }
}

/// Resolve screenshot backend from programmatic override or `AUTOMATA_SCREENSHOT_BACKEND`
/// (`auto` | `x11rb` | `screencast` | `grim` | `gnome-screenshot` | `import` | `scrot` | `portal`).
pub fn resolve_screenshot_backend() -> Result<ScreenshotBackend, UiError> {
    if let Ok(guard) = SCREENSHOT_BACKEND_OVERRIDE.read() {
        match *guard {
            Some(Some(backend)) => return validate_screenshot_backend(backend),
            Some(None) => {}
            None => {
                if let Ok(raw) = std::env::var("AUTOMATA_SCREENSHOT_BACKEND") {
                    if let Some(backend) = ScreenshotBackend::parse_override(&raw)? {
                        return validate_screenshot_backend(backend);
                    }
                }
            }
        }
    }

    let display = resolve_display_backend()?;
    match display {
        DisplayBackend::X11 => {
            if has_x11_display() {
                Ok(ScreenshotBackend::X11rb)
            } else {
                Ok(ScreenshotBackend::Portal)
            }
        }
        DisplayBackend::Wayland => {
            if is_wlroots_wayland() && command_exists("grim") {
                Ok(ScreenshotBackend::Grim)
            } else if command_exists("gnome-screenshot") {
                Ok(ScreenshotBackend::GnomeScreenshot)
            } else {
                Ok(ScreenshotBackend::Portal)
            }
        }
    }
}

/// Collect a summary of active backend detection, overrides, and system capabilities.
pub fn platform_diagnostics() -> PlatformDiagnostics {
    let display = resolve_display_backend()
        .map(|d| d.as_str().to_string())
        .unwrap_or_else(|e| format!("error({e})"));
    let pointer_backend = resolve_input_backend(InputKind::Pointer)
        .map(|b| b.as_str().to_string())
        .unwrap_or_else(|_| "none".to_string());
    let keyboard_backend = resolve_input_backend(InputKind::Keyboard)
        .map(|b| b.as_str().to_string())
        .unwrap_or_else(|_| "none".to_string());
    let clipboard_backend = resolve_clipboard_backend()
        .map(|b| b.as_str().to_string())
        .unwrap_or_else(|_| "none".to_string());
    let ocr_backend = resolve_ocr_backend()
        .map(|b| b.as_str().to_string())
        .unwrap_or_else(|_| "none".to_string());
    let screenshot_backend = resolve_screenshot_backend()
        .map(|b| b.as_str().to_string())
        .unwrap_or_else(|_| "none".to_string());

    let candidates = [
        "xdotool",
        "ydotool",
        "ydotoold",
        "wtype",
        "wmctrl",
        "wl-copy",
        "wl-paste",
        "xclip",
        "xsel",
        "tesseract",
        "grim",
        "gnome-screenshot",
        "import",
        "scrot",
        "gst-launch-1.0",
    ];
    let installed_tools = candidates
        .into_iter()
        .filter(|t| command_exists(t))
        .collect();

    PlatformDiagnostics {
        display,
        display_override: std::env::var("AUTOMATA_DISPLAY").ok().filter(|s| !s.is_empty()),
        wayland_display: std::env::var("WAYLAND_DISPLAY").ok().filter(|s| !s.is_empty()),
        x11_display: std::env::var("DISPLAY").ok().filter(|s| !s.is_empty()),
        pointer_backend,
        keyboard_backend,
        clipboard_backend,
        ocr_backend,
        screenshot_backend,
        ort_dylib: find_ort_dylib().map(|p| p.display().to_string()),
        ocr_model_dir: find_ocr_model_dir().map(|p| p.display().to_string()),
        input_override: std::env::var("AUTOMATA_INPUT_BACKEND")
            .ok()
            .filter(|s| !s.is_empty()),
        clipboard_override: std::env::var("AUTOMATA_CLIPBOARD_BACKEND")
            .ok()
            .filter(|s| !s.is_empty()),
        ocr_override: std::env::var("AUTOMATA_OCR_BACKEND")
            .ok()
            .filter(|s| !s.is_empty()),
        screenshot_override: screenshot_backend_override()
            .map(|b| b.as_str().to_string())
            .or_else(|| {
                std::env::var("AUTOMATA_SCREENSHOT_BACKEND")
                    .ok()
                    .filter(|s| !s.is_empty())
            }),
        connection_mode: resolve_connection_mode(ConnectionMode::OneShot)
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|e| format!("error({e})")),
        connection_override: connection_mode_override()
            .map(|m| m.as_str().to_string())
            .or_else(|| {
                std::env::var("AUTOMATA_CONNECTION_MODE")
                    .ok()
                    .filter(|s| !s.is_empty())
            }),
        ydotoold_running: ydotoold_running(),
        uinput_writable: uinput_writable(),
        installed_tools,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn display_env_override_and_validation() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("AUTOMATA_DISPLAY");

        unsafe { std::env::set_var("AUTOMATA_DISPLAY", "x11") };
        assert_eq!(resolve_display_backend().unwrap(), DisplayBackend::X11);

        unsafe { std::env::set_var("AUTOMATA_DISPLAY", "wayland") };
        assert_eq!(resolve_display_backend().unwrap(), DisplayBackend::Wayland);

        unsafe { std::env::set_var("AUTOMATA_DISPLAY", "bogus") };
        assert!(resolve_display_backend().is_err());

        match prev {
            Some(v) => unsafe { std::env::set_var("AUTOMATA_DISPLAY", v) },
            None => unsafe { std::env::remove_var("AUTOMATA_DISPLAY") },
        }
    }

    #[test]
    fn invalid_input_and_clipboard_overrides_fail_clearly() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev_in = std::env::var_os("AUTOMATA_INPUT_BACKEND");
        let prev_cb = std::env::var_os("AUTOMATA_CLIPBOARD_BACKEND");

        unsafe { std::env::set_var("AUTOMATA_INPUT_BACKEND", "invalid_tool") };
        assert!(resolve_input_backend(InputKind::Pointer).is_err());

        unsafe { std::env::set_var("AUTOMATA_CLIPBOARD_BACKEND", "invalid_cb") };
        assert!(resolve_clipboard_backend().is_err());

        match prev_in {
            Some(v) => unsafe { std::env::set_var("AUTOMATA_INPUT_BACKEND", v) },
            None => unsafe { std::env::remove_var("AUTOMATA_INPUT_BACKEND") },
        }
        match prev_cb {
            Some(v) => unsafe { std::env::set_var("AUTOMATA_CLIPBOARD_BACKEND", v) },
            None => unsafe { std::env::remove_var("AUTOMATA_CLIPBOARD_BACKEND") },
        }
    }

    #[test]
    fn screenshot_backend_auto_and_override() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev_disp = std::env::var_os("AUTOMATA_DISPLAY");
        let prev_shot = std::env::var_os("AUTOMATA_SCREENSHOT_BACKEND");
        let prev_x11 = std::env::var_os("DISPLAY");
        let prev_override = *SCREENSHOT_BACKEND_OVERRIDE.read().unwrap();

        clear_screenshot_backend_override();
        unsafe {
            std::env::remove_var("AUTOMATA_SCREENSHOT_BACKEND");
            std::env::set_var("AUTOMATA_DISPLAY", "x11");
            std::env::set_var("DISPLAY", ":0");
        }
        assert_eq!(resolve_screenshot_backend().unwrap(), ScreenshotBackend::X11rb);
        assert!(!is_screenshot_backend_explicit());

        unsafe { std::env::set_var("AUTOMATA_SCREENSHOT_BACKEND", "portal") };
        assert_eq!(resolve_screenshot_backend().unwrap(), ScreenshotBackend::Portal);
        assert!(is_screenshot_backend_explicit());

        set_screenshot_backend_override(Some(ScreenshotBackend::X11rb));
        assert_eq!(resolve_screenshot_backend().unwrap(), ScreenshotBackend::X11rb);

        // Explicit `auto` override supersedes exported env var
        set_screenshot_backend_override(None);
        assert_eq!(resolve_screenshot_backend().unwrap(), ScreenshotBackend::X11rb);
        assert!(!is_screenshot_backend_explicit());

        clear_screenshot_backend_override();
        unsafe { std::env::set_var("AUTOMATA_SCREENSHOT_BACKEND", "bad_channel") };
        assert!(resolve_screenshot_backend().is_err());

        *SCREENSHOT_BACKEND_OVERRIDE.write().unwrap() = prev_override;
        match prev_disp {
            Some(v) => unsafe { std::env::set_var("AUTOMATA_DISPLAY", v) },
            None => unsafe { std::env::remove_var("AUTOMATA_DISPLAY") },
        }
        match prev_shot {
            Some(v) => unsafe { std::env::set_var("AUTOMATA_SCREENSHOT_BACKEND", v) },
            None => unsafe { std::env::remove_var("AUTOMATA_SCREENSHOT_BACKEND") },
        }
        match prev_x11 {
            Some(v) => unsafe { std::env::set_var("DISPLAY", v) },
            None => unsafe { std::env::remove_var("DISPLAY") },
        }
    }

    #[test]
    fn connection_mode_auto_env_and_override() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev_env = std::env::var_os("AUTOMATA_CONNECTION_MODE");
        let prev_override = *CONNECTION_MODE_OVERRIDE.read().unwrap();

        clear_connection_mode_override();
        unsafe { std::env::remove_var("AUTOMATA_CONNECTION_MODE") };

        assert_eq!(
            resolve_connection_mode(ConnectionMode::OneShot).unwrap(),
            ConnectionMode::OneShot
        );
        assert_eq!(
            resolve_connection_mode(ConnectionMode::persistent_default()).unwrap(),
            ConnectionMode::persistent_default()
        );
        assert!(!is_connection_mode_explicit());

        unsafe { std::env::set_var("AUTOMATA_CONNECTION_MODE", "persistent") };
        assert_eq!(
            resolve_connection_mode(ConnectionMode::OneShot).unwrap(),
            ConnectionMode::persistent_default()
        );
        assert!(is_connection_mode_explicit());

        set_connection_mode_override(Some(ConnectionMode::OneShot));
        assert_eq!(
            resolve_connection_mode(ConnectionMode::persistent_default()).unwrap(),
            ConnectionMode::OneShot
        );

        // Explicit `auto` override resets to caller's default even with env set
        set_connection_mode_override(None);
        assert_eq!(
            resolve_connection_mode(ConnectionMode::OneShot).unwrap(),
            ConnectionMode::OneShot
        );
        assert!(!is_connection_mode_explicit());

        clear_connection_mode_override();
        unsafe { std::env::set_var("AUTOMATA_CONNECTION_MODE", "invalid_mode") };
        assert!(resolve_connection_mode(ConnectionMode::OneShot).is_err());

        *CONNECTION_MODE_OVERRIDE.write().unwrap() = prev_override;
        match prev_env {
            Some(v) => unsafe { std::env::set_var("AUTOMATA_CONNECTION_MODE", v) },
            None => unsafe { std::env::remove_var("AUTOMATA_CONNECTION_MODE") },
        }
    }
}
