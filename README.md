# UI Automata Linux

Linux desktop & browser automation engine, service SDK, and CLI toolkit (**Rust API**, **local Unix Domain Socket daemon**, **declarative YAML workflow engine**, and **CLI tools**) built on Linux **AT-SPI2 + D-Bus + X11/Wayland + OCR** and **CDP**.

## Workspace Crates

- **`ui-automata`** (MIT): Platform-agnostic declarative YAML workflow engine, Shadow DOM handle cache, CSS-like selector engine, expression evaluator, and workflow linter (`ui-workflow-check`, `schema-gen`).
- **`automata-browser`** (MIT): Cross-platform Chrome DevTools Protocol (CDP) client supporting Chrome/Chromium on Linux.
- **`automata-linux`** (MIT): Linux desktop backend implementing `ui_automata::Desktop` and `ui_automata::Element` over AT-SPI2 (`atspi` / `zbus`, bounded concurrent wave-BFS traversal, 32 KB private JSON spill, Qt/UTF-16 + GTK/UTF-8 editable readback verification, password protection), plus strongly-typed coordinate transforms & 53-bit-safe `WindowId` (`coords`), runtime capability probe (`CapabilitiesSnapshot`), compositor adapters (`X11`, `GNOME Wayland`, `Hyprland`, `Sway/i3`, `KWin`, `Niri`), `/proc` process inspection & bounded reaper runtime, pure-Rust `x11rb` XTEST input with RAII key/button release guards & XKB diagnostics (`+ xdotool` / `ydotool` / `wtype`), `find_elements`, `window` state/bounds & foreground safety guards, `Locator`/`Selector`, **OCR & visual detection** (`ort` + PP-OCRv4/v5 ONNX / `tesseract` TSV / optional `OmniParser` HTTP sidecar + `x11rb` multi-monitor safe capture & ScreenCast Portal), and standalone CLI tools.
- **`automata-service`** (MIT): Rust-first unified `AutomationService` (`call`, `call_async`, `submit`, `events`, `info`) with fluent Rust APIs (`svc.session()`, `svc.explore()`, `svc.act()`, `svc.vision()`, `svc.workflow()`, `svc.input()`, `svc.clipboard()`, `svc.system()`, `svc.file()`), single three-lane scheduler (`UiActor` OS thread holding persistent `Desktop` & `ShadowDom`, concurrent IO pool, cancellable workflow runner), workflow `Recorder` exporting lint-validated YAML, local `0700/0600` Unix Domain Socket daemon (`automata daemon`), and unified CLI/REPL binary **`automata`**.

## Using as a Cargo Dependency

Reference the crates you need via Cargo `path` (or `git`) dependencies:

```toml
[dependencies]
ui-automata = { path = "../ui-automata-linux/ui-automata" }
automata-linux = { path = "../ui-automata-linux/automata-linux" }
automata-service = { path = "../ui-automata-linux/automata-service" }
automata-browser = { path = "../ui-automata-linux/automata-browser" }
```

### Rust API Quickstart (`automata-service`)

```rust
use std::collections::HashMap;
use automata_service::{AutomationService, ServiceConfig, Target, WindowFilter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let svc = AutomationService::open(ServiceConfig::default())?;

    // 1. Enumerate windows
    let windows = svc.explore().list_windows(WindowFilter::default())?;
    println!("{windows:?}");

    // 2. Find elements or invoke semantic actions
    let target = Target {
        process: Some("gnome-calculator".into()),
        ..Default::default()
    };
    svc.act().invoke(target, ">> [role=button][name=Clear]")?;

    // 3. Run a declarative YAML workflow file
    svc.workflow().run_file("workflows/linux/loupe_e2e.yml", HashMap::new())?;
    Ok(())
}
```

## Unified CLI & Local UDS Daemon (`automata`)

- `automata daemon start|status|stop|systemd-unit` — Start/inspect/stop a per-user (`0700` dir / `0600` socket) Unix Domain Socket daemon (or generate/install `~/.config/systemd/user/automata-service.service` via `systemd-unit [--install] [--enable]`) so repeated CLI calls and launched GUI apps persist cleanly across turns.
- `automata explore windows|focused|tree|find|capture-bundle|batch` — Explore windows and AT-SPI2 trees live, dump offline `SnapshotBundle` directories (`capture-bundle` -> `tree.json` + PNG + optional `ocr.json`), run offline selector queries (`find --from-snapshot <DIR>`), or execute fault-tolerant multi-step probe scripts (`batch --file <JSON>`).
- `automata act launch|focus|close|invoke|set-value|click|steps|move-cursor` — Execute app, window, semantic, or batch step actions (captured by active recording if running).
- `automata vision window-layout|screen-layout|capture` — Capture screenshots and run OCR layout inspection.
- `automata workflow list|lint|run|status|cancel` — Manage and execute YAML workflows.
- `automata record start|status|stop` — Record executed actions across CLI/daemon calls and export a `WorkflowFile`-validated + linted `.yml` file (`--out <path>`).
- `automata system ping|processes|capabilities|doctor` — Probe runtime capabilities, processes, and run `ui-doctor` diagnostics.
- `automata repl` — Interactive in-process or daemon-backed session shell.

## CLI Tools (`automata-linux`)

- `list-windows [--diag]` — Enumerate all top-level AT-SPI2 windows (or inspect backend diagnostics with `--diag`).
- `ui-doctor [--profile auto|x11|wayland|xvfb|ocr|all] [--json] [--dry-run] [--fix] [-y]` — Inspect dependencies/permissions (AT-SPI2, `xdotool`/`wmctrl`/`Xvfb`, `ydotool` + `/dev/uinput` + `udev` rule, OCR langs/models) and optionally install packages & configure `/dev/uinput` via `sudo` or Polkit `pkexec`.
- `element-tree <hwnd> [-i|--interactive]` — Dump a window's AT-SPI2 tree as YAML or enter an interactive selector REPL.
- `ui-x-ray --hwnd <hwnd> | --title <title>` — Export an interactive HTML wireframe of a window's element tree.
- `ui-inspector [--x <X> --y <Y>]` — Inspect the AT-SPI2 element chain under the cursor or at a screen coordinate.
- `ui-vision [--screenshot-backend <CH>] <screen|window|image|click-text>` — Capture screen/window and run OCR fallback (`ort-ppocr` or `tesseract`), or click text by OCR coordinates.
- `ui-workflow <script.yml> [--screenshot-backend <CH>] [-- <args>]` — Run a declarative YAML workflow (interactive logs or pipe-mode JSON events).

## Wayland vs X11 Auto-Detection & Overrides

Backends are **auto-detected** from `WAYLAND_DISPLAY` / `XDG_SESSION_TYPE` / `DISPLAY` and installed tools, and can be explicitly overridden via environment variables:

- `AUTOMATA_DISPLAY=auto|wayland|x11`
- `AUTOMATA_INPUT_BACKEND=auto|xtest|ydotool|wtype|xdotool`
- `AUTOMATA_CLIPBOARD_BACKEND=auto|wl-clipboard|xclip|xsel`
- `AUTOMATA_OCR_BACKEND=auto|ort|tesseract|omniparser`
- `AUTOMATA_OCR_MODEL_DIR=/path/to/ppocr-onnx` (contains `det.onnx`, `rec.onnx`, `keys.txt`, optional `cls.onnx`)
- `AUTOMATA_OCR_LANG=chi_sim+eng` (for `tesseract` fallback)
- `AUTOMATA_SCREENSHOT_BACKEND=auto|x11rb|screencast|grim|gnome-screenshot|import|scrot|portal` (or pass `--screenshot-backend <CHANNEL>` to `ui-workflow` / `ui-vision`; in `auto` mode, X11 and XWayland windows fast-path in-memory via `x11rb`)
- `ORT_DYLIB_PATH=/path/to/libonnxruntime.so` (auto-discovers `/usr/lib/x86_64-linux-gnu/libonnxruntime.so*`)

Notes:
- **AT-SPI2 tree inspection & semantic actions (`Invoke`, `SetValue`, `Focus`, `ScrollIntoView`, `Extract`)**: Work natively on both **Wayland** (GNOME/KDE) and **X11** without needing global screen coordinates.
- **Native Wayland windows & `ydotool`**: `ydotool` injects kernel `/dev/uinput` events and works on native Wayland windows (requires `ydotoold` running and read/write permission on `/dev/uinput`, e.g., `input` group). Note that Wayland clients (e.g., GTK4) often report window-relative `(0, 0)` origin in AT-SPI `bounds()`, so prefer `Invoke` / `SetValue` over coordinate-based `Click`.
- **X11 / XWayland windows**: `xdotool` and `x11rb` XTEST work directly for pointer/keyboard injection.
- **Clipboard**: Auto-selects `wl-copy`/`wl-paste` on Wayland (`wl-clipboard`) and `xclip`/`xsel` on X11.
- **Diagnostics**: Run `cargo run -p automata-linux --bin list-windows -- --diag` to inspect active backends, overrides, `/dev/uinput`, and installed tools.

## Build & Test

```bash
# Run workspace checks & tests
cargo test --workspace

# List live Linux desktop windows via AT-SPI2
cargo run -p automata-linux --bin list-windows

# Inspect a window's element tree interactively
cargo run -p automata-linux --bin element-tree -- 0x<hwnd> --interactive

# Lint workflows
cargo run -p ui-automata --bin ui-workflow-check -- workflows/linux/loupe_e2e.yml

# Run unified Linux CLI / local daemon / REPL
cargo run -p automata-service --bin automata -- --help
cargo run -p automata-service --bin automata -- daemon start
```
