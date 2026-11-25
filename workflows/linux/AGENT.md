# UI Automata Linux Agent Guide

You are controlling a live Linux desktop (X11, XWayland, or Wayland) via `automata-agent` MCP tools.

## Key Linux Differences from Windows UIA

1. **AT-SPI2 instead of Win32 UIA**:
   - Inspect windows first with `desktop({ action: "list_windows" })`.
   - Use `desktop({ action: "find_elements", ... })` or `desktop({ action: "element_tree", hwnd, selector, max_results: 10 })` to inspect controls instead of guessing selectors.
   - Prefer `Invoke`, `SetValue`, and `Focus` when available — under native Wayland (e.g. GTK4), window-relative bounds can report `(0, 0)` origin, whereas AT-SPI2 semantic actions do not depend on global coordinates.

2. **X11 / XWayland vs Wayland Vision & Input**:
   - X11 / XWayland apps (such as Electron, WeChat `QT_QPA_PLATFORM=xcb`, Qt5/Qt6 on xcb) automatically use fast in-memory `x11rb` capture without triggering GNOME full-screen Portal flashes.
   - For self-drawn / Canvas / Chromium-embedded views (`WeChatAppEx`, terminals, custom UIs), switch to `vision({ action: "window_layout", hwnd })` to obtain OCR text blocks and physical screen coordinates, then use `input` or `OcrClick` / `OcrExtract`.
   - On Wayland (`ydotool`), `input` automatically applies scale compensation and discrete wheel ticks (`PointerScroll`).

3. **Persistent Session Caching (`ShadowDom`)**:
   - `automata-agent` maintains a persistent `Desktop` connection and session `ShadowDom` across consecutive `run_actions` calls targeting the same window.
   - Prefer `start_workflow` once a sequence is validated and saved as a `.yml` workflow under `workflows/`.

4. **Browser Automation**:
   - Call `browser({ action: "ensure" })` before `tabs` / `open` / `navigate` / `eval` / `dom` / `screenshot` / `close`.
   - On Linux, `automata-browser` drives Chrome or Chromium over CDP (`--remote-debugging-port`).
