# UI Automata Linux CLI 指南

你正在通过 `automata` CLI（`/home/ivrty/.ui-automata/automata --json`）控制运行中的 Linux 桌面（X11、XWayland 或 Wayland）。

## Linux 实践要点

1. **基于 AT-SPI2 元素树操作**：
   - 先运行 `automata --json explore windows` 列出窗口与 `hwnd`。
   - 优先通过 `automata --json explore capture-bundle --hwnd <HWND> --ocr --out /tmp/snap` 与 `automata --json explore find --from-snapshot /tmp/snap --query "<SEL>"` 在离线快照上调试选择器。
   - 能用 `Invoke`、`SetValue`、`Focus` 时优先使用——原生 Wayland（如 GTK4）下窗口边界常报告 `(0, 0)` 原点，而 AT-SPI2 语义动作不依赖全局坐标。

2. **X11 / XWayland 与 Wayland 的视觉及输入差异**：
   - X11 / XWayland 应用（Electron、`QT_QPA_PLATFORM=xcb` 下的微信、XCB 模式 Qt）自动走快速的 `x11rb` 内存直采截屏，无 Portal 闪屏。
   - 遇到自绘 / Canvas / Chromium 内嵌视图（`WeChatAppEx`、终端、自绘控件）时，使用 `automata --json vision window-layout --hwnd <HWND>` 获取 OCR 文本与屏幕坐标，再配合 `OcrClick` / `OcrExtract` 或 `Pointer*`。
   - Wayland（`ydotool`）下指针操作会自动完成 2× 缩放补偿与离散滚轮刻度（`PointerScroll`）。

3. **守护进程会话缓存（`ShadowDom`）**：
   - 启动 `automata daemon start` 后，跨命令行调用自动共享持久 `Desktop` 连接与 `ShadowDom` 缓存。
   - 操作序列验证稳定后，保存为 `workflows/` 下的 `.yml` 工作流并用 `automata --json workflow run <FILE>` 固化执行。

4. **浏览器自动化**：
   - Linux 上可通过 `automata-browser tabs|open|navigate|eval|dom|screenshot` 经 CDP（`--remote-debugging-port`）控制 Chrome/Chromium。
