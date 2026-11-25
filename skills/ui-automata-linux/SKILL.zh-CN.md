---
name: ui-automata-linux
description: 仅通过 CLI 优先的 `/home/ivrty/.ui-automata/automata`（配合持久化的 `0600` UDS/`systemd --user` 守护进程、离线 `SnapshotBundle` 探索、`explore batch` 与 `record` 录制）操控运行中的 Linux 桌面应用（X11、XWayland、Wayland）与 Chromium/Edge 浏览器，并编写、校验和执行声明式 YAML UI 工作流。在需要检查/控制 Linux GUI 应用、自动化桌面/浏览器 UI 任务、截屏/OCR 识别或编写 `.yml` UI 工作流时启用。
---

# UI Automata Linux Skill（纯 CLI 模式，无 MCP）

通过统一的 `AutomationService` CLI 工具（`/home/ivrty/.ui-automata/automata --json`）操控运行中的 Linux 桌面：
- 连接到共享持久 Unix Domain Socket 守护进程（`automata daemon start` / `systemd --user`），跨轮次保留 `Desktop`、`ShadowDom`、`Recorder` 与已登录应用，且无需逐次承载庞大的工具 JSON Schema 开销。
- 支持**单次离线 `SnapshotBundle` 采集**（`explore capture-bundle` + `explore find --from-snapshot`）、多步容错探针（`explore batch`）、交互式录制（`record start/stop`）与确定性 YAML 工作流执行（`workflow run`）。

始终优先选择可靠性最高的自动化层级，先探测检查再动手操作，操作后务必校验 UI 状态变化。

---

## 1. CLI 初始化与标准操作闭环

> 全局 CLI 参数（对所有子命令生效）：`--json`、`--socket <PATH>`、`--no-daemon`、`--workflows-dir <DIR>`。  
> 窗口定位参数（`explore` / `act` / `record start`）：`--hwnd <0x...>`（最快）、`--process <NAME>`、`--pid <PID>`、`--title <TEXT>`、`--selector <SEL>`。

1. **确保持久守护进程会话就绪**：
   - 检查 `/home/ivrty/.ui-automata/automata --json daemon status`。
   - 若尚未运行，启动一次（`automata daemon start`，或通过 `automata daemon systemd-unit --install --enable` 安装 `systemd --user` 持久化服务），使 `ShadowDom`、`Recorder` 与已启动窗口跨轮次保持存活。
   - 通过 `automata --json system capabilities` 检查运行时就绪度与当前激活后端（若显示/输入配置异常，可运行 `automata system doctor --profile auto`）。
2. **第 1 步 — 获取窗口（`act launch` / `explore windows`）**：
   - 干净附着或启动目标：`automata --json explore windows`（或 `automata --json act launch <TARGET> --wait new_window --timeout-ms 30000`），获取 `hwnd`（`"0x..."`）、`pid`、`process` 与 `bounds`。
   - 若已有通过校验的工作流（`automata --json workflow list`），优先直接运行工作流。
3. **第 2 步 — 单次快照与离线迭代（首选）**：
   - 不要反复轮询实时 AT-SPI2 树，而应一次性采集完整快照包：
     `automata --json explore capture-bundle --hwnd <HWND> --ocr --out /tmp/ui_snap`
     （写入窗口元数据、`tree.json`（上限 `--max-nodes 5000` / `--max-depth 32`）、PNG 截图及 `ocr.json`）。
   - 针对该离线快照零成本反复调试选择器：
     `automata --json explore find --from-snapshot /tmp/ui_snap --query ">> [role=button][name~=Save]"`
   - 若需实时筛选检查，使用 `automata --json explore find --hwnd <HWND> --query "<SEL>"` 或限定范围的 `automata --json explore tree --hwnd <HWND> --selector "<SEL>" --max-results 10`。**严禁**不加限制地转储整棵实时大树。
4. **第 3 步 — 多步批量探针**：
   - 使用 `automata --json explore batch --hwnd <HWND> --file probes.json --bundle-dir /tmp/ui_probes` 探测多分支对话框/菜单，期间无需关闭应用：
     ```json
     [
       { "name": "open_menu", "step": { "intent": "open menu", "action": { "type": "Invoke", "scope": "window", "selector": ">> [role=button][name=Menu]" }, "expect": { "type": "Always" } }, "settle_ms": 300, "capture_tree": true, "capture_screenshot": true, "capture_ocr": false, "query": ">> [role=dialog]" }
     ]
     ```
     （除非传入 `--stop-on-error`，否则 `BatchProbeStep` 在单步失败时仍会继续执行后续探针。）
5. **第 4 步 — 原型动作验证或录制（`act` / `record`）**：
   - 执行单步动作（`act invoke`、`act set-value`、`act click`）或 JSON 步骤数组（`act steps --hwnd <HWND> --file steps.json`），并用 `expect` 断言验证（100 ms 轮询——禁止硬编码 sleep）。
   - 或将实时探索过程录制导出为经校验的 YAML：`automata record start <NAME> --hwnd <HWND>` -> 执行 `act ...` 步骤 -> `automata record stop --out workflows/linux/<name>.yml`。
6. **第 5 步 — 校验视觉/元素树状态**：
   - 使用 `explore find`、`explore windows` 或 `vision window-layout --hwnd <HWND>` 复核结果——退出码 `0` / `"ok": true` 并不代表 UI 已经成功切换。
7. **第 6 步 — 沉淀为 YAML 工作流**：
   - 使用 `automata --json workflow lint <FILE>` 进行静态校验，并用 `automata --json workflow run <FILE>` 执行验证。

---

## 2. 分层策略与 CLI 能力对照表

**优先级顺序**：**AT-SPI2 UI 元素树（`explore` + `act`）** → **浏览器 CDP（`automata-browser` / `Browser*` 步骤）** → **OCR 视觉（`vision` / `Ocr*` 步骤）** → **原始输入（`Pointer*` / `PressKey` / `act move-cursor`）**。

| 能力领域 | CLI 命令（`automata --json ...`） | 说明 |
| --- | --- | --- |
| 守护进程 / 环境就绪 | `daemon start\|status\|stop\|systemd-unit`、`system ping\|processes\|capabilities\|doctor` | 持久守护进程绑定每用户 `0700/0600` UDS；可传 `--no-daemon` 退化为单次进程内直连。 |
| 窗口 / 元素树 / 快照 | `explore windows\|focused\|tree\|find\|capture-bundle\|batch` | `capture-bundle` 写入完整离线 `SnapshotBundle`；`find --from-snapshot` 本地零开销查询。 |
| 视觉 / OCR / 截图 | `vision window-layout --hwnd <H>`、`vision screen-layout`、`vision capture [--hwnd <H>] [--path <PNG>]` | 支持 `ort-ppocr`、`tesseract` 或可选 `omniparser`（`AUTOMATA_OMNIPARSER_URL`）；优先 `window-layout`。 |
| 应用启动与窗口控制 | `act launch <TARGET> [--wait new_pid\|new_window\|match_any] [--timeout-ms 30000]`、`act focus\|close` | 自动注入 `QT_ACCESSIBILITY=1`、`QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1`、`NO_AT_BRIDGE=0`。 |
| 语义与步骤动作 | `act invoke --elem <S>`、`act set-value --elem <S> --value <V>`、`act click --elem <S>`、`act steps --file <JSON>`、`act move-cursor --x <X> --y <Y>` | 自动挂载 Root 锚点 `"window"`；匹配同一窗口时跨调用复用 `ShadowDom`。 |
| 工作流与录制 | `workflow list\|lint <F>\|run <F>\|status\|cancel`、`record start\|status\|stop` | `lint` 返回结构化诊断信息（`line`、`col`、`path`、`message`）；`record stop` 导出已校验 YAML。 |
| 浏览器 CDP / 独立命令 | `automata-browser tabs\|open\|navigate\|eval\|dom\|screenshot`，或在 `act steps` / `.yml` 中使用 `BrowserNavigate` / `BrowserEval` / `Pointer*` | 浏览器外壳 UI（地址栏、下载框）使用 `explore`/`act`，网页 DOM 使用 CDP。 |

---

## 3. Linux 平台特定行为规则

1. **Wayland 与 X11/XWayland 差异**：
   - **原生 Wayland 客户端（如 GTK4）**：AT-SPI2 `bounds()` 常上报相对于窗口自身 `(0, 0)` 原点的坐标，导致基于全局屏幕坐标的 `Click` 偏移落空。**强烈优先使用 AT-SPI2 语义动作（`Invoke`、`SetValue`、`Focus`、`PressKey`）**，而非 `Click` / `ClickAt`。
   - **X11 / XWayland 客户端**（如 Electron、Chromium、Qt-XCB / `QT_QPA_PLATFORM=xcb`）：支持完整屏幕坐标以及无闪屏、零落盘的 `x11rb` 内存直采截屏。
   - **Wayland 下的 `ydotool`**：`PointerScroll` / `PointerDrag` 与指针输入会自动处理 GNOME 2× 绝对坐标缩放补偿及离散 `±1` 滚轮刻度；需确保 `ydotoold` 正在运行且具备 `/dev/uinput` 访问权限。
2. **Qt & GTK 浮层节点 / 自绘控件**：
   - 弹出菜单、下拉框与模态框常作为主窗口的同级兄弟挂载在 `Application` 节点下，而非主窗口子节点。`automata-linux` 会在窗口子树下暴露可见的非窗口浮层兄弟节点——可用 `>> [role=menu] >> [role='menu item']` 或 `>> [role=dialog]` 查询。
   - 隐藏或离屏的 Qt `QStackedLayout` 页面可能残留 `0x0` 占位节点（此时 `ElementFound` 为真但不可见）：切换标签页/页面时请优先使用 `ElementVisible` 或检查 `visibility`/`bounds`，不要只用 `ElementFound`。
   - 部分自绘 Qt 控件虽然带有 `[role=button]`，却未实现 AT-SPI2 `Action` 接口（`Invoke` 会报无 action 接口），或只能通过 `Click`、`ClickAt`、`PressKey`（`Focus` 后按 `{ENTER}`）稳定触发。优先按 `Invoke` -> 回退 `Click` / `PressKey` / `OcrClick` 的顺序尝试。
3. **HWND 失效与复用**：
   - 合成 HWND（`"0x..."`）由 AT-SPI2 `(bus_name, object_path)` 派生。窗口关闭/重建后旧句柄即失效——请重新运行 `automata explore windows`。
   - 若应用在 YAML 工作流运行前已打开并登录（如微信 WeChat），请配置 `launch.reuse_existing: true` 与 `launch.env`（如 `QT_QPA_PLATFORM: xcb`），以便直接附着而不重复拉起新实例。
4. **AppArmor 与 Snap 应用（Ubuntu）**：
   - 在工具运行于具名 AppArmor 配置（如 `ai.opencode (unconfined)` 等）的 Ubuntu 宿主上，Snap 软件包通常仅允许来自字面量 `peer=(label=unconfined)` 的 AT-SPI2 D-Bus 通信。
   - `automata` 在枚举时会自动跳过返回 `AccessDenied` 的 Snap 对端；若确实需要自动化 Snap 应用自身 UI，请在执行命令前加前缀 `aa-exec -p unconfined -- automata ...`。
5. **与宿主 Agent 工具的分工共存**：
   - 读写工作区代码文件请使用 Agent 原生编码工具（`read_file`、`write_file`、`run_command`），将 `automata` CLI 保留给桌面 UI 检查与自动化操作。

---

## 4. 选择器语法与 Linux 归一化角色名

- `>>` ：后代匹配（从 `scope` 根节点向下任意深度搜索）
- `>` ：直接子控件匹配
- `[attr=val]` / `[attr^=val]` / `[attr$=val]` / `[attr~=val]` / `[attr=a|b]`
- `:nth(N)` / `:parent`
- 归一化角色：`window`、`dialog`、`pane`、`menu`、`tab`、`tool tip`、`button`、`edit`、`text`、`menu bar`、`menu item`、`list item`、`tree item`、`check box`、`radio button`、`combo box`、`tab item`、`document`。

---

## 5. 步骤与工作流规范（`act steps` / `.yml`）

执行任何新编写的 YAML 前，请务必先用 `automata --json workflow lint <FILE>` 完成静态校验。详情参考 [SKILL.md](SKILL.md) 与仓库根目录 [DEVELOPMENT.md](../../DEVELOPMENT.md)。

---

## 6. 安全与用户沟通规范

- 在触发可见的 GUI 操作、外部下载或进程终止之前，用清晰平实的语言说明你的操作意图。
- 当操作具有破坏性（删除数据/联系人/文件、发送消息、修改系统设置）或匹配到多个窗口存在歧义时，先向用户确认再继续。
- 简明准确地汇报实际观测到的结果（窗口状态、提取出的输出、或保存的截图/快照路径），切勿未经核实就假定成功。
