# 工作流编写指南（Workflow Authoring Guide）

> **引擎源码仓库**：https://github.com/ivrtym/ui-automata-linux
> 当需要进一步了解动作语义、条件求值或恢复处理器（recovery handler）在本指南之外的精确行为时，请直接阅读源码。

## 核心概念

工作流是一个包含以下顶层字段的 YAML 文件：

- **`launch`** —— 可选；在各阶段执行前启动一个可执行程序（`wait: new_pid` 或 `new_window`，Linux 下支持 `reuse_existing: true` 与 `env:`）
- **`params`** —— 命名输入参数（支持可选默认值），通过 `{param.name}` 引用
- **`outputs`** —— 工作流对外发布的命名输出值，通过 `Eval … output: name` / `Extract` / `OcrExtract` 写入
- **`anchors`** —— 命名的 UI 元素句柄（见下文）
- **`phases`** —— 有序工作单元列表；每个阶段包含 `steps`（或 `subflow` / `flow_control`）
- **`defaults`** —— 所有步骤共用的默认 `timeout` 与 `action_snapshot`

**Anchors（锚点）** 是指向 UI 元素的句柄：
- `Root` —— 顶层窗口；挂载一次后在整个工作流生命周期内保持存活
- `Stable` —— 阶段 `mount` 时解析一次，在 `unmount` 前持续缓存复用
- `Ephemeral` —— 每一步都会重新求值（适用于可能会被重建替换的元素）

**Steps（步骤）** 包含：
- `intent` —— 人类可读的意图说明（写入日志，便于调试）
- `action` —— 要执行的动作（`Click`、`TypeText`、`SetValue`、`Extract`、`Eval`、`Exec`、`PressKey`、`Invoke`、`ActivateWindow`、`MoveFile`、`WriteOutput` 等）
- `expect` —— 动作后必须满足的条件（`ElementFound`、`ElementVisible`、`ElementHasText`、`DialogPresent`、`DialogAbsent`、`WindowClosed`、`ExecSucceeded` 等）
- `timeout` —— 覆盖阶段或工作流级默认超时

**变量替换**可用于大多数字符串字段：
- `{param.name}` —— 输入参数
- `{output.name}` —— 前序 `Eval`/`Extract` 写入的输出值
- `{env.VAR}` —— 环境变量
- `{workflow.dir}` —— 当前工作流文件所在目录（适合引用同级脚本或资源路径）

**Flow control（流控）** 阶段使用 `flow_control: { condition: …, go_to: phase_name }` 替代 `steps` —— 用于循环与条件跳转。`condition` 表达式支持 `==`、`!=`、`<`、`>`、`<=`、`>=`、`&&`、`||`、`!`、字符串字面量与变量替换（如 `{output.count} != "0"`）。

**Subflows（子工作流）** 将另一份工作流内联为一个阶段：`subflow: other.yml`，支持可选 `params:` 传参。

## 快速开始

打开记事本并输入文本的最小工作流示例：

```yaml
name: hello_notepad
defaults:
  timeout: 5s
launch:
  exe: notepad.exe
  wait: new_pid
anchors:
  notepad:
    type: Root
    selector: "[name~=Notepad]"
  editor:
    type: Stable
    parent: notepad
    selector: ">> [role=edit][name='Text Editor']"
phases:
  - name: type_text
    mount: [notepad, editor]
    steps:
      - intent: type hello
        action:
          type: TypeText
          scope: editor
          selector: "[role=edit][name='Text Editor']"
          text: "Hello World"
        expect:
          type: ElementHasText
          scope: editor
          selector: "[role=edit][name='Text Editor']"
          pattern:
            contains: "Hello World"
```

**运行方法**：使用 `automata --json workflow run workflows/linux/loupe_e2e.yml`（或 `cargo run -p automata-linux --bin ui-workflow -- workflows/linux/loupe_e2e.yml`）。

**探测控件 ID**：先用 `automata --json explore capture-bundle --hwnd <HWND> --ocr --out /tmp/snap` 采集离线快照包，再用 `automata --json explore find --from-snapshot /tmp/snap --query "<SEL>"`（或实时调用 `automata --json explore find --hwnd <HWND> --query "<SEL>"`）查看真实控件属性。切勿凭空猜测控件名称或 ID。也可调用 `automata --json vision window-layout --hwnd <HWND>` 快速获得 OCR 概览。

**浏览工作流库**：运行 `automata --json workflow list` 或直接查看 `workflows/` 下的文件。

**参考示例**：
- `workflows/linux/loupe_e2e.yml` / `workflows/linux/calculator_add.yml` —— Linux 端到端完整示例
- `workflows/linux/wechat_comment_and_delete.yml` —— 混合 AT-SPI2 + 剪贴板 + OCR + 主界面收尾示例
- `win10/notepad/notepad_extract_value.yml` —— `Extract` 与 `{output.x}` 变量替换
- `win10/notepad/notepad_loop_counter.yml` —— `Eval`、`flow_control`、循环与输出
- `mastercam/process_file.yml` —— 子工作流、`Exec`、`MoveFile`、多阶段流水线

## 最佳实践

## 1. 永远不要依赖 `Sleep` —— 始终使用带超时的 `expect:`

`Sleep` 是固定延时，会让工作流既脆弱又缓慢。每个步骤本身都自带可配置 `timeout`。用 `expect:` 描述动作应当产出的状态，让引擎以 100 ms 间隔自动轮询，直到满足条件或超时。

```yaml
# 糟糕做法
- intent: wait for results
  action: { type: Sleep, duration: 4s }
  expect: { type: Always }

# 推荐做法
- intent: wait for results to appear
  action: { type: NoOp }
  expect:
    type: ElementFound
    scope: store
    selector: "> [role=card]"
```

为阶段或步骤设置合理的 `timeout:` 上限（如 `10s`）。若等待失败可能需要纠偏动作，可添加 `recovery:` 处理器。

## 2. 永远不要用 `expect: Always` —— 哪怕很简单，也要明确写出期望结果

`expect: Always` 等同于“不管成没成功我都放行”，这会把失败伪装成静默通过，掩盖真实 Bug。每个动作都会产生可检测的后置状态——明确写出它：

| 动作 | 最小有意义的 `expect` |
|---|---|
| `Click` 按钮 | `ElementFound` / `ElementVisible` 检查随后出现的元素 |
| `TypeText` | `ElementHasText` 校验已输入的内容 |
| `SetValue ""` | `ElementHasText` 匹配 `exact: ""` |
| `PressKey Return` | `ElementFound` 检查跳转后的页面或元素 |
| `ActivateWindow` | `ElementFound` 检查窗口中的标志性控件 |
| `CloseWindow` | `ProcessRunning: false` 或 `WindowClosed` / 其他界面元素出现 |

## 3. 优先使用直接子级选择器 `>`，而非全局后代 `>>`

`>>` 会遍历整棵子树——复杂度为 O(n)，在大型 UI 上开销较高。当目标就在作用域根节点的直接子节点时使用 `>`，仅在层级深度确实未知时才用 `>>`。

```yaml
# 较慢 —— 遍历整棵子树
selector: ">> [id=TextBox]"

# 更快 —— 仅检查直接子节点
selector: ">> [id=SearchBox] > [id=TextBox]"
```

如果元素在固定几层深度下且路径已知，可串联 `>`：

```yaml
selector: "> [role=pane] > [id=TextBox]"
```

## 4. 利用额外锚点缩小搜索范围

Root 锚点指向整个窗口。如果目标控件位于某个已知子容器内，请为该容器再声明一个锚点。之后绑定到该锚点的所有选择器都只搜索极小子树，即便从窗口根出发的深度未知，容器内部也完全可以用 `>` 快速定位。

```yaml
# 糟糕 —— 每一步都从整个窗口树重新深搜
anchors:
  app:
    type: Root
    process: notepad
    selector: "*"

steps:
  - action: { type: Click, scope: app, selector: ">> [id=SearchBox]" }
  - action: { type: TypeText, scope: app, selector: ">> [id=TextBox]", text: "hello" }

# 推荐 —— 在搜索框容器锁定一个 Stable 锚点，内部直接用 >
anchors:
  app:
    type: Root
    process: notepad
    selector: "*"
  search_box:
    type: Stable
    parent: app
    selector: ">> [id=SearchBox]"

steps:
  - action: { type: Click, scope: search_box, selector: "> [id=TextBox]" }
  - action: { type: TypeText, scope: search_box, selector: "> [id=TextBox]", text: "hello" }
```

锚点类型说明：
- `Root` —— 解析为顶层窗口；全局注册一次并在整个工作流期间持有（不可 unmount）。
- `Stable` —— 解析一次并在所属阶段生命周期内缓存元素句柄。
- `Ephemeral` —— 每一步都重新解析（适合动态重建替换的控件）。

## 5. 写步骤前先检查元素树

在为陌生页面编写步骤之前，先运行 `automata --json explore capture-bundle` 或 `automata --json explore find` 查看真实的自动化 ID、角色与边界坐标。不要靠猜写选择器——一次检查可以省去大量试错往返。

优先使用 `[id=AutomationId]` 而非 `[name=...]`：开发者指定的 ID 在多语言与微调文案时依然稳定，而显示文本很容易变化。需要更强区分度时可组合 `[role=...]`（如 `[role=button][id=OkButton]`）。

**选择器匹配操作符：**

| 操作符 | 含义 | 大小写 |
|---|---|---|
| `=` | 精确匹配 | 区分大小写 |
| `~=` | 包含子串 | **不区分** |
| `^=` | 前缀匹配 | **不区分** |
| `$=` | 后缀匹配 | **不区分** |

对于标题会轻微变化的窗口可使用 `~=`（如 `[name~=Notepad]`）。对于含单引号等难转义特殊字符的名称，可联合使用 `^=`/`$=`（如 `[name^=Don][name$=ave]` 可匹配 `"Don't Save"`）。

## 6. Root 锚点在整个工作流中持久存在 —— 为不同窗口分别声明

`Root` 锚点是**全局共享**的：第一个列出该 Root 锚点的 `mount:` 会完成注册并解析；后续阶段再列出同一名称时为空操作。同时 Root 锚点**无法被卸载（unmount）**——它们在整个工作流中保持存活，仅在缓存 HWND 失效（窗口关闭/销毁）时重新查询。

正因为 Root 锚点解析后会锁定到一个特定句柄，即使同一窗口在导航中标题变化也能继续使用；但若导航打开了一个**新窗口（独立句柄）**，则必须声明第二个 Root 锚点：

```yaml
anchors:
  panel:
    type: Root
    process: explorer
    selector: "[name=Control Panel]"             # 原资源管理器窗口

  firewall:
    type: Root
    process: explorer
    selector: "[name=Windows Defender Firewall]" # 同一句柄或新打开窗口的句柄

phases:
  - name: search
    mount: [panel]
    steps: [...]          # 在 panel 内导航；panel 保持有效

  - name: read_status
    mount: [firewall]     # 若尚未注册则重新解析
    steps: [...]
```

## 7. 托管子窗口（`role=window`）对 `FindAll` 不可见（Windows UIA 注意事项）

Windows UIA 的 `FindAll(TreeScope::Descendants)` 会穿透进入子 HWND（嵌入式窗口）并**能**找到其中的叶子节点，但不会把子窗口节点本身（`role=window`）作为匹配项返回。这意味着：

- 即使内部的叶子节点 `>> [id=hubProfileText]` 能找到，`>> [id=Hub]` 也可能返回空；
- 以 `selector: ">> [id=Hub]"` 定义的 `Stable` 锚点将始终无法解析；
- **不要**把锚点作用域绑定到嵌入式子窗口元素，请将作用域保持在 Root 锚点并用 `>>` 直达叶子控件。

判断特征：如果 `automata --json explore find` 传入 `include_ancestors: true` 后显示某个祖先是 `role=window` 且带有非空 `automation_id`，该节点就是托管子窗口，不能作为锚点或选择器最终目标。

## 8. 离屏元素边界退化 —— 使用 `Invoke` 而非 `Click`

已存在于无障碍树中、但当前被滚动出可视区的元素常报告退化边界 `(0, 0, 1, 1)`。对其执行 `Click` 会失败，因为点击安全守卫会拒绝落在窗口外的 `(0, 0)` 坐标。

对于 WinUI / UWP 可滚动列表项也**不要**使用 `ScrollIntoView` —— 其基于鼠标滚轮，在带弹性惯性的列表中经常滚过去后又弹回。

请直接**使用 `Invoke`**。`Invoke` 直接调用底层语义触发接口（Windows `IInvokePattern` / Linux AT-SPI2 `Action::DoAction`），不依赖边界矩形、无需鼠标参与、也无滚动副作用。

```yaml
# 糟糕 —— 若 About 在屏幕外（边界 0,0,1,1）会失败
- intent: click About
  action: { type: Click, scope: settings, selector: ">> [id=SettingsPageAbout_New]" }

# 糟糕 —— WinUI 列表中的弹性滚动会回弹
- intent: scroll About into view
  action: { type: ScrollIntoView, scope: settings, selector: ">> [id=SettingsPageAbout_New]" }

# 推荐 —— 无论滚动位置如何，均通过语义 Invoke 直接激活
- intent: navigate to About
  action: { type: Invoke, scope: settings, selector: ">> [id=SettingsPageAbout_New]" }
  expect:
    type: ElementFound
    scope: settings
    selector: ">> [name=Device Specifications]"
```
