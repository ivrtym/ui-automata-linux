# UI Automata Linux

面向 Linux 桌面与浏览器的自动化引擎、服务 SDK 与命令行工具集（提供 **Rust SDK API**、**本地 Unix Domain Socket 守护进程**、**声明式 YAML 工作流引擎**与 **CLI 工具**），底层基于 Linux **AT-SPI2 + D-Bus + X11/Wayland + OCR** 与 **CDP** 构建。

## Workspace Crate 说明

- **`ui-automata`**（MIT）：平台无关的声明式 YAML 工作流引擎、Shadow DOM 句柄缓存、类 CSS 选择器引擎、表达式求值器与工作流静态校验工具（`ui-workflow-check`、`schema-gen`）。
- **`automata-browser`**（MIT）：跨平台 Chrome DevTools Protocol（CDP）客户端库与命令行工具，支持控制 Linux Chrome/Chromium。
- **`automata-linux`**（MIT）：基于 AT-SPI2、D-Bus、X11/Wayland 与 OCR 的 Linux 桌面后端（实现 `ui_automata::Desktop` 与 `ui_automata::Element`、有界并发 wave-BFS 遍历、强类型 `coords` 坐标变换与 53-bit 安全 `WindowId`、`CapabilitiesSnapshot` 能力探测、多合成器适配、`x11rb` XTEST 输入与 `ydotool`/`xdotool`/`wtype`、高能效 `x11rb`/ScreenCast/Portal 截屏、PP-OCRv4/v5 ONNX / `tesseract` OCR 识别）以及专项诊断/执行 CLI 工具。
- **`automata-service`**（MIT）：以 Rust API 为一等公民的统一自动化服务层 `AutomationService`（`call`、`call_async`、`submit`、`events`、`info`），提供流畅的子 API 门面（`svc.session()`、`svc.explore()`、`svc.act()`、`svc.vision()`、`svc.workflow()`、`svc.input()`、`svc.clipboard()`、`svc.system()`、`svc.file()`）、单三通道调度器（持有持久 `Desktop` 与 `ShadowDom` 的 `UiActor` 线程、并发 IO 池、可取消工作流运行器）、工作流录制器 `Recorder`、本地 `0700/0600` UDS 守护进程（`automata daemon`）以及统一 CLI/REPL 二进制 **`automata`**。

## 作为 Cargo 依赖引入

在你的项目 `Cargo.toml` 中通过 `path` 或 `git` 依赖引入所需 crate：

```toml
[dependencies]
ui-automata = { path = "../ui-automata-linux/ui-automata" }
automata-linux = { path = "../ui-automata-linux/automata-linux" }
automata-service = { path = "../ui-automata-linux/automata-service" }
automata-browser = { path = "../ui-automata-linux/automata-browser" }
```

### Rust API 快速调用示例（`automata-service`）

```rust
use std::collections::HashMap;
use automata_service::{AutomationService, ServiceConfig, Target, WindowFilter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 打开服务实例（默认采用 Persistent 连接复用模式）
    let svc = AutomationService::open(ServiceConfig::default())?;

    // 1. 枚举当前桌面窗口
    let windows = svc.explore().list_windows(WindowFilter::default())?;
    println!("{windows:?}");

    // 2. 定位窗口并触发 AT-SPI2 语义动作
    let target = Target {
        process: Some("gnome-calculator".into()),
        ..Default::default()
    };
    svc.act().invoke(target, ">> [role=button][name=Clear]")?;

    // 3. 运行声明式 YAML 工作流文件
    svc.workflow().run_file("workflows/linux/loupe_e2e.yml", HashMap::new())?;
    Ok(())
}
```

## 统一 CLI 与本地 UDS 守护进程（`automata`）

- `automata daemon start|status|stop|systemd-unit` — 启动/查看/停止每用户独立（`0700` 目录 / `0600` 套接字）的 Unix Domain Socket 守护进程（或通过 `systemd-unit [--install] [--enable]` 安装 `systemd --user` 服务），跨命令行调用保持 `Desktop` 与 `ShadowDom` 会话持久化。
- `automata explore windows|focused|tree|find|capture-bundle|batch` — 枚举窗口与 AT-SPI2 元素树、导出离线 `SnapshotBundle`（`tree.json` + PNG + 可选 `ocr.json`）、离线选择器检索（`find --from-snapshot <DIR>`），或运行多步探针脚本（`batch --file <JSON>`）。
- `automata act launch|focus|close|invoke|set-value|click|steps|move-cursor` — 执行启动应用、聚焦/关闭窗口、控件语义调用、设值、点击或批量 JSON 步骤。
- `automata vision window-layout|screen-layout|capture` — 捕获窗口或屏幕截图并执行 OCR 版面文本识别。
- `automata workflow list|lint|run|status|cancel` — 列出、校验、执行、查询或取消 YAML 工作流。
- `automata record start|status|stop` — 录制通过 CLI/守护进程执行的动作并导出经校验的 `.yml` 工作流。
- `automata system ping|processes|capabilities|doctor` — 检查后端能力与系统环境状态。
- `automata repl` — 进入交互式本地或守护进程模式控制台。

## 专项 CLI 工具（`automata-linux`）

- `list-windows [--diag]` — 枚举全部顶层 AT-SPI2 窗口（`--diag` 查看后端检测详情）。
- `ui-doctor [--profile auto|x11|wayland|xvfb|ocr|all] [--json] [--dry-run] [--fix] [-y]` — 检查并修复 AT-SPI2、输入后端（`xdotool` / `ydotool` + `/dev/uinput`）与 OCR 依赖。
- `element-tree <hwnd> [-i|--interactive]` — 导出窗口元素树 YAML 或进入交互式选择器调试 REPL。
- `ui-x-ray --hwnd <hwnd> | --title <title>` — 导出窗口元素树交互式 HTML 线框图。
- `ui-inspector [--x <X> --y <Y>]` — 检查光标位置或指定坐标处的 AT-SPI2 控件链。
- `ui-vision [--screenshot-backend <CH>] <screen|window|image|click-text>` — 截图、OCR 识别及按文本坐标点击。
- `ui-workflow <script.yml> [--screenshot-backend <CH>] [-- <args>]` — 直接执行声明式 YAML 工作流文件。

## Wayland 与 X11 自动检测及环境变量覆写

- `AUTOMATA_DISPLAY=auto|wayland|x11`
- `AUTOMATA_INPUT_BACKEND=auto|xtest|ydotool|wtype|xdotool`
- `AUTOMATA_CLIPBOARD_BACKEND=auto|wl-clipboard|xclip|xsel`
- `AUTOMATA_OCR_BACKEND=auto|ort|tesseract|omniparser`
- `AUTOMATA_OCR_MODEL_DIR=/path/to/ppocr-onnx`（包含 `det.onnx`、`rec.onnx`、`keys.txt` 及可选 `cls.onnx`）
- `AUTOMATA_OCR_LANG=chi_sim+eng`（用于 `tesseract` 回退）
- `AUTOMATA_SCREENSHOT_BACKEND=auto|x11rb|screencast|grim|gnome-screenshot|import|scrot|portal`
- `ORT_DYLIB_PATH=/path/to/libonnxruntime.so`

关键注意事项：
- **AT-SPI2 语义动作（`Invoke`、`SetValue`、`Focus`、`Extract`）**：在 **Wayland** 与 **X11** 下均可原生工作，不依赖全局屏幕坐标；GTK4 等原生 Wayland 客户端在 `bounds()` 中常报告相对窗口 `(0, 0)` 原点的坐标，因此尽量优先使用语义动作而非坐标点击。
- **X11 / XWayland 窗口**：`x11rb` 内存直采截屏与 XTEST/`xdotool` 键鼠注入可直接工作，无需全屏 Portal 弹窗。
- **Wayland 下的 `ydotool`**：需启动 `ydotoold` 且当前用户拥有 `/dev/uinput` 读写权限。

## 构建与测试

```bash
# 运行核心引擎与各库测试
cargo test --workspace

# 查看当前桌面窗口（AT-SPI2）
cargo run -p automata-linux --bin list-windows

# 校验 YAML 工作流
cargo run -p ui-automata --bin ui-workflow-check -- workflows/linux/loupe_e2e.yml

# 运行统一服务 CLI / 守护进程
cargo run -p automata-service --bin automata -- --help
cargo run -p automata-service --bin automata -- daemon start
```
