# automata-browser

用于 [ui-automata](https://github.com/visioncortex/ui-automata/) 的基于 CDP 的浏览器控制模块。

## 适用场景

`automata-browser` 让 ui-automata 工作流能够将浏览器控制无缝嵌入更大的桌面自动化流程中——就像点击 Win32 对话框或调用 Ribbon 按钮一样。上游主要面向 **Windows 上的 Microsoft Edge**，因其预装、常驻且可通过 CDP 稳定自动化。

典型用例：

- 作为工作流步骤打开某个 URL、提取数据并交由后续步骤处理
- 在操作原生桌面应用之后，在工作流中途填写网页表单
- 在已知的工作流检查点截取网页截图
- 执行 JavaScript 读取页面状态，再决定下一步动作

浏览器被视为又一类可控界面——标签页通过其 CDP target ID 标识，所有操作（navigate、eval、dom、screenshot）均通过该 ID 寻址，无需调用方维护持久连接状态。

## 非适用场景

- **通用跨浏览器测试** —— 不支持 Safari 或 Firefox，也暂无支持计划。若需跨浏览器覆盖测试，请使用 Playwright。
- **仅以 macOS 或 Linux 为核心的原设计定位** —— 启动器会尽力在 Linux/macOS 上探测 Chrome/Chromium 作为兼容回退（注：本 Linux 移植仓库已通过 `automata-linux` 适配 Linux Chrome/Chromium），但上游设计主目标为 Windows Edge。
- **高频抓取或无头流水线** —— 当前“按次调用无状态 CDP”模型每次操作都会新建 WebSocket 连接。该方式简洁正确，但未针对高吞吐优化。详见下方路线图。

## 设计说明

### 以 Edge 作为稳定目标

现代 Windows 均预装 Edge，并可在无需用户交互的情况下通过 `--remote-debugging-port` 启动，这使其在 Windows 优先的工具中具备极高的可靠性：无需下载步骤、无需锁定版本、无额外外部依赖。

在 Linux 和 macOS 上可便捷探测并使用 Chrome/Chromium。

### 使用 Tab ID，而非 WebSocket URL

公开 API 使用 CDP **target ID**（十六进制字符串）来标识标签页——即 `GET /json` 返回的同一标识符。WebSocket 调试地址属于内部实现细节，由已知端口与 target ID 在内部拼接：

```
ws://127.0.0.1:{port}/devtools/page/{tab_id}
```

调用方无需构造或存储 WebSocket URL。这使得 API 不受 CDP 内部路由变更影响，也让标签页句柄可以安全地在 ui-automata 协议中传递。

### 每次调用无状态（当前设计）

每次操作都会新建一次 WebSocket 连接、发送一条 CDP 命令并关闭。这意味着：

- 无需管理或恢复会话状态
- 多线程并发调用天然安全，无需协调锁
- 非常适合操作间隔以秒计的工作流场景

其代价是每次调用的连接开销，这在工作流节拍下可以忽略不计。

### 持久会话（规划路线图）

对于需要连续性的操作——如监听下载、流式接收 DOM 变更事件，或驱动较长的多步网页表单——规划提供持久会话模型：会话持有打开的 WebSocket 并暴露强类型事件流，同时保留无状态 API 用于单次操作。

## 路线图

- **持久 CDP 会话** —— 面向事件驱动工作流的长生命周期 WebSocket 连接；作为现有无状态 API 的可选扩展
- **下载管理器** —— 基于会话跟踪 `Browser.downloadWillBegin` / `Page.downloadProgress` 事件；将下载状态暴露为工作流条件，从而无需轮询文件系统即可等待或响应文件下载

## CLI 命令

```
automata-browser launch      [--port 9222]
automata-browser tabs        [--port 9222]
automata-browser open        [<url>]          [--port 9222]
automata-browser navigate    <tab_id> <url>   [--port 9222]
automata-browser eval        <tab_id> <expr>  [--port 9222]
automata-browser dom         <tab_id>         [--port 9222]
automata-browser screenshot  <tab_id>         [--port 9222]
automata-browser activate    <tab_id>         [--port 9222]
automata-browser close       <tab_id>         [--port 9222]
```

不传 URL 调用 `open` 将打开空白页（`about:blank`）。
未指定 `--port` 时，所有命令默认使用端口 `9222`。

## 许可证

MIT
