# 工作流编写指南（CLI + Skill 版）

> **核心概念**：工作流 YAML 包含顶层字段 `launch`、`params`、`outputs`、`anchors`、`phases` 与 `defaults`。

## 核心概念

- **`launch`**：可选；在各阶段执行前启动应用（`wait: new_pid` 或 `new_window`，Linux 下可配 `reuse_existing: true` 与 `env: { QT_QPA_PLATFORM: xcb }`）
- **`params`**：命名输入参数（支持默认值），通过 `{param.name}` 引用
- **`outputs`**：输出变量，通过 `Eval` / `Extract` / `OcrExtract` 写入并经 `{output.name}` 引用
- **`anchors`**：命名的 UI 元素句柄（`Root` 全局窗口、`Stable` 阶段内缓存、`Ephemeral` 每步重寻）
- **`phases`**：有序阶段列表，每个阶段含 `mount`/`unmount` 与 `steps`（或 `subflow` / `flow_control`）
- **`defaults`**：所有步骤共用的默认 `timeout` 与重试策略

## 探索与运行命令（CLI 方式）

- **查看工作流列表**：`automata --json workflow list`
- **探测控件树与选择器**（推荐离线快照）：
  1. `automata --json explore capture-bundle --hwnd <HWND> --ocr --out /tmp/snap`
  2. `automata --json explore find --from-snapshot /tmp/snap --query ">> [role=button][name~=Save]"`
  3. 视觉文本概览：`automata --json vision window-layout --hwnd <HWND>`
- **静态校验**：`automata --json workflow lint <FILE>`（或 `cargo run -p ui-automata --bin ui-workflow-check -- <FILE>`）
- **运行工作流**：`automata --json workflow run <FILE>`（或 `ui-workflow <FILE>`）

## 最佳实践要点

1. **禁止依赖 `Sleep`**：优先在 `expect:` 中声明可轮询后置状态（如 `ElementFound` / `ElementVisible` / `OcrTextFound`）配合 `timeout`。
2. **不要用 `expect: Always` 作为主要步骤断言**：每个关键动作应明确校验期望 UI 变化。
3. **优先直接子级选择器 `>` 与子容器 `Stable` 锚点**：缩小子树遍历深度。
4. **先探树后编写**：不要凭空猜测控件名称或角色，以 `explore find` / `explore capture-bundle` 实测为准。
5. **非激活界面注意可见性**：Qt `QStackedLayout` 隐藏控件可能残留 `0x0` 节点，判断显现请用 `ElementVisible` 而非仅用 `ElementFound`。
6. **语义触发优先**：能用 `Invoke` / `SetValue` 时优先于屏幕坐标 `Click`；若需坐标或自绘区操作，结合 `ClickAt` / `OcrClick`。
