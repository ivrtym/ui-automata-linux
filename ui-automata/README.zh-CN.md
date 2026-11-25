<div align="center">

  <img src="https://github.com/visioncortex/vtracer/raw/master/docs/images/visioncortex-banner.png">
  <h1>UI Automata</h1>

  <p>
    <strong>面向桌面自动化的 AI 工具包（上游原版 README 中文副本）</strong>
  </p>

  <h3>
    <a href="https://automata.visioncortex.org">官网</a>
    <span> | </span>
    <a href="https://github.com/visioncortex/ui-automata/releases/latest">下载</a>
    <span> | </span>
    <a href="https://automata.visioncortex.org/docs/introduction/what-is-ui-automata/">文档</a>
  </h3>
</div>

UI Automata 让 AI Agent 能够在同一份工作流文件中编写、运行和调试跨桌面应用（Windows 原版支持 Win32、WPF、WinForms、WinUI、UWP；本移植版由 `automata-linux` 通过 AT-SPI2 支持 Linux 桌面）与浏览器的自动化工作流。

Vision Cortex 最初打造该项目，是为了帮助工业设计客户自动化 CAD/CAM 任务：那些没有 API、对时序要求严格、且需要在不静默失败的前提下处理弹窗与错误对话框的多步桌面应用流程。如今将其开源，让更多团队能够以同样的方式实现自动化。

## 要解决的问题

桌面 UI 操作正是 AI Agent 最适合接手的工作，但每一步都可能以脚本察觉不到的方式失败：

- **时序问题**：按钮点击返回了成功，但应用尚未完成后台处理；UI 看似就绪，实则不然。
- **瞬态禁用状态**：元素存在且可见，但处于短暂禁用状态；点击发出了，却什么也没发生，也不报错。
- **弹窗打断**：模态对话框在步骤中途抢走焦点，脚本却在死等一个永远不会出现的结果。
- **句柄失效（Stale handles）**：页面导航后应用重建了 UI 树，缓存的引用指向了错误位置，点击静默落空或点到错误目标。
- **焦点丢失**：本该输入到 A 输入框的按键落到了 B 控件上。

这些并非边缘情况，而是任何真实桌面应用中的常态。纯视觉 Agent 每次运行都要撞上这些问题——每一步都是一次截图和一次大模型推理。UI Automata 给出的答案是 **高 Token 效率（token-efficient）** 的工作流：Agent 只需编写并调试一次，付出一次推理成本；随后的每一次运行都是确定性的、本地执行的，且零推理开销。

## 快速演示

<video src="https://github.com/user-attachments/assets/81f0cbb4-e3bf-461e-a373-74ee2cb3bedb" controls width="100%"></video>

> 从 Microsoft Store 安装 Python（不要用 python.org），选择最新 3.x 版本。

Agent 通过应用 URI 打开商店、搜索 Python、读取结果卡片识别 3.13、点击 Get 并轮询直到安装完成。

> 从官网获取最新安装包、静默安装，并打开 Git Bash 确认可用。

对于 Git，Agent 在 Edge 中打开 gitforwindows.org，使用 UIA 触发下载（CDP 合成点击会被浏览器拦截文件下载），通过下载面板等待安装器完成，在 UAC 确认下静默安装，最后在 UIA 覆盖不到的 Git Bash 终端界面回退使用视觉 OCR 读取终端输出。

## 安装（上游 Windows 版示例）

```powershell
PowerShell -ExecutionPolicy Bypass -Command "iwr https://raw.githubusercontent.com/visioncortex/ui-automata/refs/heads/main/install/install-windows.ps1 | iex"
```

## 典型用例

UI Automata 专为由 AI Agent 通过其 MCP Server 驱动而设计。以下是一些示例提示词：

### 自动化桌面应用

<video src="https://github.com/user-attachments/assets/59429bd6-fa34-4530-998f-b6628eacac54" controls width="100%"></video>

> 使用 ui-automata skill：打开 Mastercam，加载 `C:\Projects\part.mcam`，打开仿真窗口，并将结果 csv 导出到 `C:\output\`。

Agent 遍历元素树、实时测试选择器、编写工作流 YAML 并运行验证（全部在单次会话中完成）。你只需给出意图并验收结果。

### 修复损坏的工作流

> 我们用来向 AccountMate 录入发票的工作流在确认对话框关闭后会报句柄失效（stale handle）错误。这是错误追踪日志，请修复它。

Agent 回放工作流、在失败点暂停、检查实时元素树，并调整锚点（anchor）或选择器以适配 UI 重建。

### 跨桌面应用与浏览器协同自动化

> 在浏览器中从供应商门户下载本月供应商发票，然后打开我们的财务桌面应用逐条录入。门户地址是……

工作流可以在同一份文件中混合编写浏览器步骤（基于 CDP、结构化 DOM 访问）与桌面应用步骤，无需再把多个孤立工具拼接在一起。

## 工作原理

每个步骤都声明一个 **action（动作）**、一个 **expect（期望条件）**，以及可选的 **recovery（恢复处理器）**：

```yaml
- intent: click the Open button
  action:
    type: Click
    scope: main_window
    selector: ">> [role=button][name=Open]"
  expect:
    type: DialogPresent
    scope: main_window
  timeout: 10s
```

引擎对每一步执行统一的生命周期：

1. 执行动作（`action`）
2. 每 `100ms` 轮询一次 `expect`
3. 条件满足 → 进入下一步；超时 → 执行恢复处理器（recovery handler），随后按配置重试、跳过或失败中止。

没有 `sleep`，没有硬编码等待。恢复处理器只需声明一次，便可在任意位置触发条件满足时介入：工作流中途弹出的对话框、确认提示框、需要等待消失的进度条等。

<video src="https://github.com/user-attachments/assets/a68c115f-34fc-461e-b682-bc21840c6685" controls width="100%"></video>

完整示例（包含阶段、锚点、恢复处理器与流控）可参考 [notepad_demo.yml](https://github.com/visioncortex/ui-automata/blob/main/workflows/win11/notepad/notepad_demo.yml)。

## 选择器（Selectors）

基于无障碍属性的类 CSS 路径语法：

```
>> [role=edit][name='File name:']           # 后代编辑框
>  [role=button][name^=Don][name$=Save]     # 直接子控件："Don't Save"
>> [role=list item]:nth(0)                  # 第一个列表项
>> [role=list item][name~=Wing]:parent      # 匹配项的父节点
>> [id=SettingsPageAbout_New]               # 按 AutomationId（跨语言稳定）
>> [role=button|menu item]                  # 或匹配：button 或 menu item
```

完整语法参考：→ [Selectors 文档](https://automata.visioncortex.org/docs/core-concepts/selectors/)

## Shadow DOM

无论 Windows UIA 还是 Linux AT-SPI2，本质上都是跨进程 RPC 协议：每次元素查询都是一次到目标进程的往返调用。若逐层遍历嵌套控件，每一层都要付出跨进程通信成本；传统自动化工具在每一步都重复支付这份开销，20 步工作流就会付出 20 多次重复探索结构树的性能代价。

UI Automata 的解法是 **Shadow DOM**：实时元素树的缓存镜像。句柄只需解析一次，即可在后续步骤中复用（命中缓存几乎零成本）。可以把它看作 React Virtual DOM 的逆过程：React 维护虚拟树是为了高效写入自己控制的 UI；Shadow DOM 维护缓存树则是为了高效读取自己并不直接控制的外部 UI。

→ [Shadow DOM 工作原理](https://automata.visioncortex.org/docs/core-concepts/shadow-dom/)

## Agent 工具集

内置 MCP Server（`automata-agent`）赋予 AI Agent 直接操控桌面的能力：

- **desktop** — 列举窗口、遍历无障碍元素树、实时测试选择器
- **vision** — 面向无障碍树不完整应用的 OCR 与视觉版面采集
- **app** — 启动应用、列出已安装应用、通过任务栏/窗口系统管理窗口
- **window** — 按 HWND 最小化、最大化、还原、移动/调整大小或截取窗口
- **run_actions** — 无需编写工作流文件即可直接执行临时 UI 自动化步骤
- **start_workflow** — 运行命名工作流并流式推送各阶段进度
- **workflow** — 列出工作流、查询状态、取消运行、浏览历史记录、校验 YAML
- **input** — 原始鼠标键盘输入，适用于任何窗口
- **clipboard** — 读写系统剪贴板
- **browser** — 通过 CDP 控制浏览器：导航、执行 JavaScript、读取 DOM
- **file** — 读、写、复制、移动、删除、glob 匹配与 stat 查看
- **system** — Shell 命令执行、进程管理、系统诊断
- **resources** — 浏览内置工作流与指南资源库

## 与纯视觉 Agent 对比

纯视觉 Agent 通过截屏并询问大模型“下一步点哪里”来工作；UI Automata 则直接基于无障碍树驱动，并将视觉识别作为回退手段（而非主路径）：

| | UI Automata | 纯视觉 Agent |
|---|---|---|
| **技术路径** | 无障碍元素树 + DOM 查询 + 视觉回退 | 仅依赖截图 |
| **可靠性** | 确定性 —— 同一选择器在多次运行间行为一致 | 不同运行之间可能存在波动 |
| **速度** | 单步亚秒级 | 每一步都要往返推理 API |
| **成本** | 低 —— 本地运行，无逐步骤推理费用 | 高 —— 每一步都消耗大量 Token |
| **视觉能力** | 端侧本地运行，按需作为回退 | 云端推理，作为主要手段 |
| **模型依赖** | 任意 Agent、任意模型 | 常受限于特定视觉模型提供方 |
| **浏览器自动化** | CDP（结构化页面访问） | 仅能对浏览器截图 |
| **执行追踪** | 带完整动作与属性详情的结构化日志 | 一连串截图 |

两种方式相辅相成：需要确定性、可重复的工作流时使用 UI Automata；需要临时应对完全陌生且无障碍树缺失的应用或页面时结合视觉能力。

## 社区

有疑问或想分享你的实践？欢迎访问 [GitHub Discussions](https://github.com/visioncortex/ui-automata/discussions)。
发现 Bug 或希望新增功能？欢迎 [提交 Issue](https://github.com/visioncortex/ui-automata/issues/new)。
