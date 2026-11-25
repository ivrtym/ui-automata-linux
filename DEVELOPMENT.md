# UI-Automata Linux 二次开发指南

本文档面向需要在 `ui-automata-linux` 上扩展引擎原语（Action / Condition）、适配 Linux 桌面后端（Wayland / XWayland / X11 / AT-SPI2 / OCR）或集成自定义业务校验逻辑的开发者。

> 架构说明：`ui-automata-linux` 工作区包含四个核心 crate：
> - `ui-automata/`：平台无关的声明式工作流引擎、Shadow DOM、选择器、Linter 与 Schema 生成器；
> - `automata-linux/`：基于 AT-SPI2、D-Bus、X11/Wayland 与 OCR 的 Linux 桌面后端、`coords` 强类型坐标与 `WindowId`、`CapabilitiesSnapshot` 能力快照及独立 CLI 工具集；
> - `automata-browser/`：跨平台 Chrome DevTools Protocol (CDP) 浏览器后端；
> - `automata-service/`：以 Rust API 为一等公民的统一自动化服务层（`AutomationService::{call, call_async, submit, events, info}`、fluent API 门面、`Session` / `RecorderState`、三通道调度器 `UiActorHandle`）、`0700/0600` 本地 Unix Domain Socket 守护模式与统一 CLI/REPL 入口 `automata`。

---

## 1. 核心架构与关键模块索引

### 1.1 `ui-automata` 引擎执行链路

一次步骤执行遵循 **`Precondition → Action → Expect (100 ms 轮询) → Retry / Fallback → Recovery`** 闭环：

1. **平台抽象**（`ui-automata/src/platform.rs`）：
   - `Desktop` trait：窗口枚举（`application_windows`）、启动（`open_application`）、前台探测（`foreground_window`）、浏览器句柄（`browser`），以及 5 个细粒度视觉/输入原语（`ocr_recognize`、`pointer_move`、`pointer_click`、`pointer_type`、`capture_screenshot`）。
   - `Element` trait：节点属性（`name`、`role`、`text`、`inner_text`、`bounds`、`is_enabled`、`is_visible`、`has_parent`）、交互动作（`click`、`click_at`、`type_text`、`press_key`、`invoke`、`focus`、`activate_window`、`close` 等）。
2. **Shadow DOM 缓存与防失效**（`ui-automata/src/shadow_dom.rs`、`selector.rs`）：
   - 按 `Root / Session / Stable / Ephemeral / Browser / Tab` 生命周期缓存锚点；
   - `find_descendant` 会同时检查 `is_live(&cached)` 与 `selector.matches_leaf(&cached)`，防止 Qt/GTK 模型视图就地复用节点路径时误命中旧缓存；
   - `ocr_recognize_throttled` 在 `400 ms` TTL 内复用同一区域的 OCR 结果，并在 `Executor::exec` 执行新动作后清空。
3. **动作与条件**（`ui-automata/src/action.rs`、`condition.rs`、`executor.rs`）：
   - 交互型动作（`Click`、`TypeText`、`Focus` 等）通过 `find_required_interactive` 解析目标，遇到不可见或 `(0,0,0,0)` 占位控件时自动回退选取首个可见且非零尺寸的匹配节点；
   - `Executor::run_step` 执行 `action` 后按 `100 ms` 间隔轮询 `expect`，超时后依次走 `retry`、`step.fallback`（已支持 `{local.*}` / `{output.*}` 与 `apply_outputs`）和 `recovery_handlers`。
4. **参数、校验与 Schema**（`ui-automata/src/yaml.rs`、`lint.rs`、`schema.rs`、`bin/schema_gen.rs`）：
   - `{param.*}` 在加载期由 `substitute_params` 直接替换进 YAML AST；`{local.*}` / `{output.*}` 在执行期通过 `apply_output` 动态替换；
   - `lint.rs` 负责静态检查，`schema.rs` + `schema-gen` 维护工作区根目录的 `workflow-schema.json`。

### 1.2 `automata-linux` 后端与必须守住的工程不变量

二次开发改动 Linux 后端时，请注意以下经过实机验证的关键设计约束：

1. **后端探测与渠道/连接模式覆写**（`automata-linux/src/backend.rs`）：
   - 支持 `AUTOMATA_DISPLAY`、`AUTOMATA_INPUT_BACKEND`、`AUTOMATA_CLIPBOARD_BACKEND`、`AUTOMATA_OCR_BACKEND`、`AUTOMATA_SCREENSHOT_BACKEND`、`AUTOMATA_CONNECTION_MODE`、`AUTOMATA_PRESERVE_A11Y_ENV`、`AUTOMATA_OCR_MODEL_DIR`、`AUTOMATA_OCR_LANG`、`AUTOMATA_YDOTOOL_SCALE`、`ORT_DYLIB_PATH`。
   - 截屏渠道支持 `auto | x11rb | screencast | grim | gnome-screenshot | import | scrot | portal`，解析优先级为：**程序 API / CLI `--screenshot-backend`（`set_screenshot_backend_override` / `apply_screenshot_backend_arg`）> `AUTOMATA_SCREENSHOT_BACKEND` > 环境自动选择**。
   - 连接生命周期模式 `ConnectionMode` 支持 `OneShot` 与 `Persistent { screencast_idle_timeout }`（默认 `60s`），解析优先级为：**程序 API / CLI `--connection-mode`（`set_connection_mode_override` / `apply_connection_mode_arg`）> `AUTOMATA_CONNECTION_MODE` > 调用方默认策略**。
2. **Wayland / `ydotool` / 剪贴板**（`automata-linux/src/input.rs`、`clipboard.rs`）：
   - `ydotool 1.0.4` 在 GNOME Wayland 下 `mousemove --absolute` 存在 2× 坐标缩放，已由 `ydotool_abs_scale()` 修正；`scroll_axis_ticks` 对 `ydotool` 发射离散 `±1` 滚轮刻度（间隔 `25 ms`）以匹配物理滚轮；非 ASCII（如中文）输入通过 `clipboard_write` + `ctrl+v` 粘贴；
   - **剪贴板子进程禁止管道阻塞**：`wl-copy` 和 `xclip` 会派生后台守护子进程继承 `stdout`/`stderr`，调用方**必须**将 `stdout`/`stderr` 重定向到 `Stdio::null()` 并使用 `child.wait()`（绝不能对 `stderr(Stdio::piped())` 调用 `wait_with_output()`，否则会永久阻塞）。
3. **AT-SPI2 总线自愈、无障碍注入与窗口控制**（`automata-linux/src/a11y.rs`、`desktop.rs`、`element.rs`）：
   - `A11yClient::shared()` 使用可清空的 `Mutex<Option<Arc<A11yClient>>>`（`reset_shared()` + `with_shared_retry()`），首次总线未就绪不锁死，遇到 `Disconnected` / `Broken pipe` / `ServiceUnknown` / `NameHasNoOwner` 等连接级错误自动重连重试；
   - `Desktop::open_application()` 统一走 `spawn_launched_command()`，默认按需注入 `QT_ACCESSIBILITY=1`、`QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1` 并将缺失或上游壳进程泄露的 `NO_AT_BRIDGE=1` 置为 `0`（设 `AUTOMATA_PRESERVE_A11Y_ENV=1` 可跳过注入）；
   - `Desktop::hwnd_z_order()` 仅在纯 X11 会话（`DisplayBackend::X11`）读取 `_NET_CLIENT_LIST_STACKING` / `query_tree` 反转后，通过 `pid + title + bounds` 1-to-1 映射到 AT-SPI2 的 `synthetic_hwnd`；Wayland 下返回空 vec 避免原生窗口错序；
   - `Desktop::tooltip_windows()` 跨 X11/Wayland 扫描 AT-SPI2 应用顶层可见 `Role::ToolTip` 节点（`normalize_role` 已映射为 `"tool tip"`）；注意 GTK3 原生 `set_tooltip_text()` 不导出独立 AT-SPI2 节点、GTK4 导出为 `panel -> label`，此类原生 tooltip 仍需通过目标控件 `description` 或窗口内子树选择器读取；
   - `activate_window()` 优先通过 `wmctrl -lp` 按 PID 查找并执行 `wmctrl -ia <xid>` 拉起 X11/XWayland 窗口，再结合 `focus()` / `StatusNotifierItem.Activate(0,0)`；
   - Qt / GTK 的弹出菜单（`popup menu`）与模态对话框（`filler`/`dialog`）常作为主窗口的同级兄弟挂载在 `Application` 节点下，`children_elems()` 会自动追加同应用下可见的非窗口 overlay 兄弟节点；
   - `type_text()` 在 `focus()` 后若未进入 `Focused` 状态且边界非零，会自动居中点击一次再注入按键。
4. **高性能截屏与连接复用管线**（`automata-linux/src/capture.rs`、`vision.rs`、`vision_ort.rs`、`desktop.rs`）：
   - `x11rb` 通道通过纯 Rust X11 `GetImage` 内存直采屏幕/区域/X11 窗口，并在 `auto` 模式下为带 `hwnd` 的 X11/XWayland 窗口（即使运行于 GNOME Wayland）自动走 PID/几何/标题匹配直采，绕开 Portal 全屏闪屏与临时 PNG 落盘；纯 Wayland 原生窗口平滑回退到 `grim`（仅 wlroots 合成器检测通过时）或 `portal`；
   - 通过 `CaptureSessionGuard`（`desktop.x11_capture_session()` / `desktop.capture_session()`）按作用域控制复用：`OneShot` 下每次调用后断开；`Persistent` 或 `x11_capture_session` 作用域内复用 `CachedX11Conn` 及 5 个常用 Atom（断连自动重连一次）；
   - `screencast` 通道通过 `org.freedesktop.portal.ScreenCast` + `gst-launch-1.0 pipewiresrc` 采集单帧并持久化 `restore_token` 至 `~/.cache/ui-automata/screencast_restore_token`；`Persistent` 模式下复用已协商 Portal 会话并在空闲超过 `screencast_idle_timeout`（默认 `60s`）或 Guard Drop 时主动 `Session.Close`；
   - `recognize_target` 与 `recognize_dynamic_image_with_origin` 在使用 `OrtPpOcr` 时直接消费内存 `DynamicImage`（免 PNG 编解码落盘）；
   - GNOME 中文系统下 Portal 截图返回的 `%E5%9B%BE%E7%89%87` URI 需经 `decode_file_uri` 解码；`parse_tesseract_tsv` 使用 `join_ocr_line_words` 合并相邻 CJK 字符（去除中文单字间多余空格，保留 ASCII 词间空格）。

---

## 2. 常见二次开发扩展清单

### 2.1 新增 YAML `Action` 的标准步骤

1. 在 `ui-automata/src/action.rs`：
   - 向 `Action` 枚举添加新变体；
   - 更新 `TryFrom<serde_yaml::Value>` 反序列化分支；
   - 更新 `describe()`、`apply_output()`、`apply_outputs()` 与 `execute()`。
2. 在 `ui-automata/src/lint.rs`：
   - 将新类型名加入 `ALL_ACTION_TYPES`，补充字段校验逻辑（如 `scope`、`selector`、参数引用等）。
3. 在 `ui-automata/src/schema.rs`：
   - 为新 `Action` 添加 JSON Schema 描述对象，并执行 `cargo run -p ui-automata --bin schema-gen` 重新生成 `workflow-schema.json`。
4. 在 `ui-automata/tests/` 补充解析、执行与 Schema 测试（注意 `tests/schema.rs` 会校验枚举覆盖完整性）。

### 2.2 新增 YAML `Condition` 的标准步骤

1. 在 `ui-automata/src/condition.rs`：
   - 向 `Condition` 枚举添加新变体；
   - 更新 `TryFrom<serde_yaml::Value>`、`apply_output()`、`scope_name()`、`describe()` 与 `evaluate()`。
2. 若新条件涉及外部 IO / OCR / 进程调用：
   - 务必在 `ShadowDom`（`ui-automata/src/shadow_dom.rs`）中增加带 TTL 的结果节流缓存，并在 `Executor::exec` 执行新动作后清理缓存，避免 `100 ms` 轮询导致高频阻塞或过载。
3. 在 `ui-automata/src/lint.rs` 加入 `ALL_CONDITION_TYPES` 与字段校验。
4. 在 `ui-automata/src/schema.rs` 补充 Schema 并运行 `cargo run -p ui-automata --bin schema-gen`。
5. 在 `ui-automata/tests/` 补充单元测试与异步/超时场景测试。

### 2.3 新增 Linux 环境检测项（`ui-doctor`）

1. 在 `automata-linux/src/doctor.rs` 中补充检测项（区分 `CheckSeverity::Required` 与 `Optional`）及多发行版包名映射（`apt-get` / `dnf` / `pacman` / `zypper`）；
2. 若涉及系统权限（udev 规则、用户组、`systemd --user`），遵循最小权限脚本输出并兼容 `--dry-run`、`--json`、`--fix`；
3. 运行 `cargo test -p automata-linux` 与 `./target/debug/ui-doctor --profile all --dry-run` 验证。

---

## 3. 接入方式与使用场景指南（Rust API / CLI / Daemon）

针对 **Rust API** 与 **CLI / UDS 守护进程** 两种调用形态，核心区分原则是：**由谁持有 `ShadowDom` 与桌面后端连接生命周期**。

### 3.1 总体定位

| 调用方式 | 连接与状态默认模式 | 最适合承担的角色 |
| :--- | :--- | :--- |
| **Rust API（`automata_service::AutomationService` + fluent 门面）** | 进程内长期持有 `UiActorHandle`（`ConnectionLifecycle::OneShot` 或 `Persistent`）与 `ShadowDom` / `RecorderState` | 嵌入自有 Rust 服务、7×24 监听程序、自动化测试与自定义业务编排，统一走强类型 `Command` / `Event` 协议 |
| **统一 CLI / Daemon / REPL（`automata`）+ 专项工具（`automata-linux/src/bin/`）** | 无守护时 `OneShot`；启动 `automata daemon start`（`0700/0600` UDS）后跨命令共享 `Persistent` 会话与 `Recorder` | 人工探索排障、`record start/stop` 录制导出可 Lint YAML、CI 校验、单次/Cron 定时执行已固化工作流 |

### 3.2 分场景用法矩阵

#### 场景 1：人工探索与编写测试工作流
- **主力方案 — CLI / Daemon**：
  1. `ui-doctor --profile auto` 检查 AT-SPI2、`ydotool` 权限、OCR 与 `QT_ACCESSIBILITY` / `NO_AT_BRIDGE` 环境；
  2. `list-windows`（或 `automata explore windows`）获取目标窗口 `hwnd`、`pid`、`process`、`title`；
  3. `element-tree <hwnd> -i` 进入交互式 selector REPL 试写选择器，或通过 `automata explore capture-bundle --hwnd <hwnd> --ocr --out /tmp/snap` + `automata explore find --from-snapshot /tmp/snap` 离线调试选择器；
  4. 遇到自绘/Webview 区域改用 `ui-vision window <title>` / `automata vision window-layout --hwnd <hwnd>` 测 OCR 文本与裁剪 `region`；
  5. 编排 `.yml` 后运行 `ui-workflow-check <script.yml>` 静态校验，再运行 `ui-workflow <script.yml>` 或 `automata workflow run <script.yml>` 验证。
- **Rust API**：调试底层模块或编写集成测试时直接使用。

#### 场景 2：已探明工作流的自动化执行（生产跑批 / CI / 定时任务）
- **单次 / Cron / CI — CLI**：
  - 使用 `ui-workflow <script.yml>`（或 `automata workflow run <script.yml>`）：默认 `--connection-mode auto` 会在执行期通过 `desktop.x11_capture_session()` 复用 `x11rb` 连接并退出即清理；严格每步重建连接传 `--connection-mode oneshot`；被外部管道宿主消费 JSON 阶段事件时传 `--log-path <file>`。
- **7×24 常驻服务 / 高频轮询 / 自定义业务 — Rust API**：
  - 上层推荐直接调用 `AutomationService::open(ServiceConfig::default())` 并通过 `svc.workflow()` / `svc.explore()` / `svc.act()` / `svc.vision()` 调度；
  - 若直接调用底层 `automata-linux` / `ui-automata`，可初始化 `Desktop::with_connection_mode(ConnectionMode::persistent_default())`（`x11rb` 长连接 + `ScreenCast` 空闲 60s 自动回收），复用 `WorkflowFile::run(&mut executor, ...)` 或 `run_workflow_str(...)`；长循环共享同一 `ShadowDom` 时注意按轮次 `unmount()` / `cleanup_depth()` 清理动态锚点。

---

## 4. 规划中的扩展设计（待实现）

> 以下设计已完成评审，当前仓库代码**尚未改动**，后续可直接按此规格落地实现。

### 4.1 设计一：通用外部探针条件 `Condition::CommandMatches`（用于 `expect` 直接查库 / 调接口 / 跑脚本）

#### 设计动机
- `Executor::run_step` 默认每 `100 ms` 轮询一次 `expect`。当前 `Action::Exec` 只能作为独立动作步执行一次，无法作为 `expect`、`precondition` 或 `recovery` 的轮询判断条件；
- 通过新增通用外部探针条件 `CommandMatches`，即可在不为每种数据库（MySQL / Postgres / SQLite / Redis / HTTP）重编译 Rust 二进制的前提下，让工作流在 `expect` 中直接等待后台/数据库状态收敛。

#### YAML 语法与字段规范

```yaml
expect:
  type: CommandMatches
  command: python3                         # 必填：直接执行的命令名或路径（不走隐式 shell，防注入）
  args:                                    # 可选，默认 []；支持 {param.*} / {local.*} / {output.*}
    - "scripts/check_friend_deleted.py"
    - "{param.contact_name}"
  cwd: "/path/to/scripts"                  # 可选：子进程工作目录（支持变量替换）
  expected_exit: 0                         # 可选，默认 0：期望的进程退出码
  ignore_exit: false                       # 可选，默认 false：为 true 时忽略退出码，仅按 stdout 判定
  stdout:                                  # 可选：复用现有 TextMatch (exact / contains / starts_with / regex / non_empty)
    exact: "0"
  timeout: 2s                              # 可选，默认 2s：单次子进程执行超时上限
  throttle: 500ms                          # 可选，默认 500ms：两次真实执行之间的最小节流间隔
```

#### 判定与异常处理规则
1. **退出码校验**：当 `ignore_exit == false` 时，要求进程返回码等于 `expected_exit`（默认 `0`）；
2. **标准输出校验**：当设置了 `stdout` 时，先对子进程捕获的 UTF-8 输出去除末尾换行（`trim_end_matches(&['\r', '\n'])`），再调用 `TextMatch::test`；
3. **Spawn 失败（如命令不存在）**：返回 `Err(AutomataError::Internal(...))`，立即暴露配置错误；
4. **单次子进程超时（超过 `timeout`）**：向子进程发送 `kill()` 并 `wait()` 回收避免僵尸进程，记 `log::warn!`，本次判定返回 `Ok(false)`（让外层 Step 在总 `step.timeout` 耗尽前有机会继续下一轮或恢复）；
5. **非阻塞管道读取（纯 `std`，无新增 `tokio` 依赖）**：`stdout`/`stderr` 使用 `Stdio::piped()` 并分别派生轻量读取线程，主循环以约 `20 ms` 步长调用 `child.try_wait()`，防止大量输出填满 OS 管道缓冲区造成死锁。

#### 高频轮询节流缓存（防打爆数据库）
- 在 `ui-automata/src/shadow_dom.rs` 为 `ShadowDom` 增加：
  - 字段 `probe_cache: HashMap<String, (std::time::Instant, bool)>`；
  - 方法 `clear_probe_cache(&mut self)`（在 `Executor::exec` 执行任何 `action` 后与 `clear_ocr_cache()` 一并清理）；
  - 方法 `probe_command_throttled(...)`：以 `(command, args, cwd, expected_exit, ignore_exit, stdout)` 规范化签名为 key；若命中未过期的 `throttle` TTL（默认 `500ms`），直接返回缓存的 `bool` 值，否则真正运行一次探针并更新时间戳。

#### 受影响文件与改造点
- `ui-automata/src/condition.rs`：新增 `Condition::CommandMatches` 变体，完善 `TryFrom`（`timeout` / `throttle` 复用 `crate::duration::parse`）、`apply_output`、`describe` 与 `evaluate`。
- `ui-automata/src/shadow_dom.rs`、`executor.rs`：新增 `probe_cache` 节流与清理机制。
- `ui-automata/src/lint.rs`：把 `"CommandMatches"` 加入 `ALL_CONDITION_TYPES`，校验 `command` 非空、`args` 为字符串数组、`stdout` 走 `lint_text_match`、`timeout`/`throttle` 为合法时长。
- `ui-automata/src/schema.rs` & `workflow-schema.json`：补充 Schema 并执行 `cargo run -p ui-automata --bin schema-gen`。

#### 工作流编排最佳实践（防误重试 UI）
**不要**把数据库断言直接放在“点击 UI 删除按钮”同一步的 `expect` 里（否则弹窗已关但 DB 同步稍慢时触发 `retry` 会重新去点已经消失的按钮），而应拆为“UI 收口 + `NoOp` 异步查库等待”两步：

```yaml
      - intent: confirm deletion in confirmation modal
        action:
          type: Click
          scope: wechat
          selector: ">> [role=button][name=删除]"
        expect:
          type: Not
          condition:
            type: ElementFound
            scope: wechat
            selector: ">> [role=list][name=通讯录] >> [role='list item'][name~={param.contact_name}]"

      - intent: wait until friend record is removed in database
        action:
          type: NoOp
        timeout: 8s
        expect:
          type: CommandMatches
          command: python3
          args:
            - "scripts/assert_friend_deleted.py"
            - "{param.contact_name}"
          stdout:
            exact: "0"
          timeout: 1s
          throttle: 500ms
```

> 注：Linux PC 版微信自身本地 SQLite 通常为 SQLCipher 加密，外部脚本无法直接明文查询；此方案主要适用于校验业务自有后端数据库、未加密本地库、测试管理 API 或已预先配置解密访问的探针脚本。

#### 配套验证用例计划
1. 退出码 `0` / 非 `0`（`expected_exit` 与 `ignore_exit`）行为测试；
2. `stdout` `TextMatch` 匹配（含 `\n` / `\r\n` 裁剪、正则）测试；
3. `{param.*}` 与 `{local.*}` / `{output.*}` 变量插值测试；
4. 多次高频轮询命中 `throttle` 缓存、并在 `Action` 执行后清缓存的计数测试；
5. 慢命令超过单次 `timeout` 时准确 `kill` 并返回 `false` 的测试；
6. `lint_workflow` 与 `schema.rs` 完整性校验测试。

---

### 4.2 设计二（可选演进）：SDK 级自定义条件注册表 & 声明式显示环境启动

1. **`Executor` 代码级 `CustomCondition` 注册表**（仅在自建 Rust runner 需高频直连数据库连接池时叠加）：
   - 参照 `Executor.global_handlers` 模式增加 `custom_conditions: HashMap<String, Arc<dyn CustomCondition>>`，YAML 使用 `type: Custom` + `name` + `args`。
2. **`LaunchConfig` 与 `Action::Exec` 显示环境预设**（解决 Wayland / XWayland / Xvfb 启动切换需要手动挂环境变量的问题）：
   - 在 `LaunchConfig`（`ui-automata/src/yaml.rs`）和 `Action::Exec`（`ui-automata/src/action.rs`）补充 `args`、`env` 及 `display: auto | wayland | xwayland | xvfb` 预设（如 `xwayland` 自动注入 `QT_QPA_PLATFORM=xcb`、`GDK_BACKEND=x11`、`MOZ_ENABLE_WAYLAND=0`；`xvfb` 自动拉起独立虚拟显示并按进程生命周期清理）。

---

## 5. 本地开发与验证常用命令

在 `ui-automata_linux/` 目录下运行：

```sh
# 1. 运行全量单元、集成与文档测试
cargo test --workspace --no-fail-fast

# 2. 检查关闭默认特性（如无 ocr-ort）时的编译
cargo check -p automata-linux --no-default-features

# 3. 调整 Action / Condition 后重新生成 JSON Schema
cargo run -p ui-automata --bin schema-gen

# 4. 校验 Linux 工作流 YAML
cargo run -p ui-automata --bin ui-workflow-check -- workflows/linux/*.yml

# 5. 环境与无障碍变量全面体检
cargo run -p automata-linux --bin ui-doctor -- --profile all --dry-run

# 6. 运行无破坏性 E2E（图像查看器 loupe）
cargo run -p automata-linux --bin ui-workflow -- workflows/linux/loupe_e2e.yml
```

> 注：`automata-linux/tests/real_x11.rs` 为隔离纯 X11（`Xvfb + openbox + at-spi-bus-launcher + GTK3`）实机集成测试，默认标记 `#[ignore]`；在隔离 X11 会话内传 `--ignored --nocapture` 运行。
>
> 若编辑器工作区打开在父目录 `uia_research/`（而非 `ui-automata_linux/`），`codebase-memory-mcp` 默认不开启文件自动监听，改动 `.rs` 代码后请重新触发一次 `index_repository` 刷新知识图谱索引。
