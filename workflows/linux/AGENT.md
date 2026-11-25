# UI Automata Linux CLI Guide

You are controlling a live Linux desktop (X11, XWayland, or Wayland) via the `automata` CLI (`/home/ivrty/.ui-automata/automata --json`).

## Key Linux Practices

1. **AT-SPI2 instead of Win32 UIA**:
   - Inspect windows first with `automata --json explore windows`.
   - Use `automata --json explore capture-bundle --hwnd <HWND> --ocr --out /tmp/snap` and `automata --json explore find --from-snapshot /tmp/snap --query "<SEL>"` to iterate offline before acting.
   - Prefer `Invoke`, `SetValue`, and `Focus` when available — under native Wayland (e.g. GTK4), window-relative bounds can report `(0, 0)` origin, whereas AT-SPI2 semantic actions do not depend on global coordinates.

2. **X11 / XWayland vs Wayland Vision & Input**:
   - X11 / XWayland apps (such as Electron, WeChat `QT_QPA_PLATFORM=xcb`, Qt5/Qt6 on xcb) automatically use fast in-memory `x11rb` capture without triggering GNOME full-screen Portal flashes.
   - For self-drawn / Canvas / Chromium-embedded views (`WeChatAppEx`, terminals, custom UIs), switch to `automata --json vision window-layout --hwnd <HWND>` to obtain OCR text blocks and physical screen coordinates, then use `OcrClick` / `OcrExtract` or `Pointer*`.
   - On Wayland (`ydotool`), pointer actions automatically apply scale compensation and discrete wheel ticks (`PointerScroll`).

3. **Persistent Daemon Caching (`ShadowDom`)**:
   - `automata daemon start` maintains a persistent `Desktop` connection and session `ShadowDom` across consecutive CLI calls targeting the same window.
   - Prefer `automata --json workflow run <FILE>` once a sequence is validated and saved as a `.yml` workflow under `workflows/`.

4. **Browser Automation**:
   - On Linux, `automata-browser` drives Chrome or Chromium over CDP (`--remote-debugging-port`) cleanly via `automata-browser tabs|open|navigate|eval|dom|screenshot`.
