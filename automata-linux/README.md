# automata-linux

Linux desktop backend implementing `ui_automata::Desktop` and `ui_automata::Element` over AT-SPI2 + D-Bus + X11/Wayland + OCR, together with standalone diagnostic and workflow CLI binaries.

## Core Modules (`src/`)

- **AT-SPI2 Accessibility (`a11y/`, `desktop.rs`, `locator.rs`, `selector.rs`)**: `atspi` / `zbus` client with automatic bus reconnect (`with_shared_retry`), concurrent wave-BFS `SnapshotArena`, 32 KB private JSON spill, Qt/UTF-16 & GTK/UTF-8 editable text readback verification, password protection, and normalized roles.
- **Coordinates & Compositors (`coords.rs`, `compositor/`, `window.rs`)**: Strongly-typed coordinate transforms (`SpacePoint` / `SpaceRect`), 53-bit-safe `WindowId`, and adapters for X11, GNOME Wayland, Hyprland, Sway/i3, KWin, and Niri.
- **Input & Clipboard (`input/`, `clipboard.rs`, `mouse.rs`)**: Pure-Rust `x11rb` XTEST input with RAII key/button release guards & XKB diagnostics, `ydotool` / `wtype` / `xdotool` support, and non-blocking `wl-clipboard` / `xclip` / `xsel` clipboard handling.
- **Capture & Vision OCR (`capture.rs`, `vision.rs`, `vision_ort.rs`, `vision_omniparser.rs`)**: Multi-channel screenshot capture (`x11rb` in-memory fast path for X11/XWayland, `screencast`, `grim`, `gnome-screenshot`, `import`, `scrot`, `portal`) and OCR (`ort` PP-OCRv4/v5 ONNX, `tesseract` TSV, optional `OmniParser` HTTP sidecar).
- **Environment Diagnostics (`doctor.rs`, `diagnostics/`, `backend.rs`)**: Auto-detection + env/CLI backend overrides and `ui-doctor` readiness checks.

## CLI Binaries

```sh
list-windows [--diag]
ui-doctor [--profile auto|x11|wayland|xvfb|ocr|all] [--json] [--dry-run] [--fix] [-y]
element-tree <hwnd> [-i|--interactive]
ui-x-ray --hwnd <hwnd> | --title <title>
ui-inspector [--x <X> --y <Y>]
ui-vision [--screenshot-backend <CH>] <screen|window|image|click-text>
ui-workflow <script.yml> [--screenshot-backend <CH>] [-- <args>]
```
