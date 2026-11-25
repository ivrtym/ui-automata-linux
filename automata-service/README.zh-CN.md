# automata-service

面向调用方的统一会话感知自动化服务 crate，提供强类型 Rust API、`Command` / `Event` 协议、本地 `0700/0600` Unix Domain Socket 守护进程、动作录制器以及统一 `automata` CLI/REPL 二进制工具。

## 核心调用能力

- **Rust API（`AutomationService`）**：
  - 核心入口：`AutomationService::open(ServiceConfig)`、`call(Command)`、`call_async(Command)`、`submit(Command)`（返回可取消的 `CommandHandle`）、`events()`（`Event` 广播流）、`info()`。
  - 链式/分域子门面（`automata_service::api`）：
    - `svc.session()` — 获取服务信息与运行时能力、重置 `ShadowDom`、启停/查询录制（`record_start` / `record_status` / `record_stop`）
    - `svc.explore()` — 枚举窗口、查看焦点窗口、抓取受限子树、`find_elements`、离线快照包 `capture_bundle`、离线检索 `query_snapshot`、批量容错探针 `batch`
    - `svc.act()` — `launch`、`invoke`、`set_value`、`click`、`close_window`、`run_steps`
    - `svc.vision()` — `window_layout`、`capture`
    - `svc.workflow()` — `list`、`lint_yaml`、`run_file`、`run_inline`、`status`、`cancel`
    - `svc.input()` / `svc.clipboard()` / `svc.system()` / `svc.file()` — 原始键鼠输入、剪贴板、存活检查与文件操作
- **单三通道调度器**：
  - 独占的 `UiActor` OS 线程长期持有 `Desktop` 与 `ShadowDom` 状态；
  - 并发 IO 池负责非阻塞任务；
  - 可取消的工作流运行器支持实时中断与状态查询。
- **本地 UDS 守护进程（`automata daemon`）**：
  - 在多次独立的 CLI 调用之间保留 `Desktop` 连接、`ShadowDom` 缓存与已启动窗口会话；
  - 支持通过 `automata daemon systemd-unit --install --enable` 生成并启用用户级 `systemd --user` 服务。

## 命令行调用示例

```sh
automata daemon start|status|stop|systemd-unit
automata explore windows|focused|tree|find|capture-bundle|batch
automata act launch|focus|close|invoke|set-value|click|steps|move-cursor
automata vision window-layout|screen-layout|capture
automata workflow list|lint|run|status|cancel
automata record start|status|stop
automata system ping|processes|capabilities|doctor
automata repl
```
