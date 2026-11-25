# automata-linux

基于 AT-SPI2 + D-Bus + X11/Wayland + OCR 实现 `ui_automata::Desktop` 与 `ui_automata::Element` 的 Linux 桌面后端库，同时提供独立的诊断与工作流 CLI 二进制工具集。

## 核心模块（`src/`）

- **AT-SPI2 无障碍层（`a11y/`、`desktop.rs`、`locator.rs`、`selector.rs`）**：`atspi` / `zbus` 客户端（支持总线断连自动重试 `with_shared_retry`）、有界并发 wave-BFS 遍历与 `SnapshotArena`、32 KB 私有 JSON 溢出落盘、Qt/UTF-16 与 GTK/UTF-8 可编辑文本回读校验、密码保护以及归一化角色映射。
- **坐标与合成器适配（`coords.rs`、`compositor/`、`window.rs`）**：强类型坐标空间变换（`SpacePoint` / `SpaceRect`）、53-bit JS 安全 `WindowId`，以及 X11、GNOME Wayland、Hyprland、Sway/i3、KWin、Niri 窗口管理适配。
- **输入与剪贴板（`input/`、`clipboard.rs`、`mouse.rs`）**：基于纯 Rust `x11rb` XTEST 的输入（带 RAII 按键/鼠标释放守卫与 XKB 诊断）、`ydotool` / `wtype` / `xdotool` 后端，以及防阻塞的 `wl-clipboard` / `xclip` / `xsel` 剪贴板封装。
- **截屏与视觉 OCR（`capture.rs`、`vision.rs`、`vision_ort.rs`、`vision_omniparser.rs`）**：多通道截屏（X11/XWayland 自动走 `x11rb` 内存直采快速路径，支持 `screencast`、`grim`、`gnome-screenshot`、`import`、`scrot`、`portal`）与 OCR 识别（`ort` PP-OCRv4/v5 ONNX、`tesseract` TSV、可选 `OmniParser` HTTP sidecar）。
- **环境检测与诊断（`doctor.rs`、`diagnostics/`、`backend.rs`）**：后端自动检测与环境变量/CLI 覆写，以及 `ui-doctor` 依赖权限检查与修复建议。

## 专项 CLI 工具

```sh
list-windows [--diag]
ui-doctor [--profile auto|x11|wayland|xvfb|ocr|all] [--json] [--dry-run] [--fix] [-y]
element-tree <hwnd> [-i|--interactive]
ui-x-ray --hwnd <hwnd> | --title <title>
ui-inspector [--x <X> --y <Y>]
ui-vision [--screenshot-backend <CH>] <screen|window|image|click-text>
ui-workflow <script.yml> [--screenshot-backend <CH>] [-- <args>]
```
