# 第三方声明

本仓库（`ui-automata-linux`）包含来自 `visioncortex/ui-automata` 的 MIT 许可组件（详见各 crate `LICENSE` 文件），并在遵循各自开源许可证的前提下，借鉴和选择性适配了以下开源项目的架构模式与模块设计：

1. **agent-sh/computer-use-linux**（MIT 许可证）
   - 仓库地址：https://github.com/agent-sh/computer-use-linux
   - 版权所有 (c) agent-sh 贡献者
   - 在 `automata-linux` 与 `automata-agent` 中参考/适配的模式：有界 `CommandRunner`（`ETXTBSY` 重试/超时处理）、53-bit JS 安全窗口 ID 约束、MCP HTTP 安全中间件、并发/有界 AT-SPI2 树遍历与紧凑 `U+FFFC` 节点过滤、Qt/GTK `EditableText` 回读校验、多合成器窗口管理后端以及 `RemoteDesktop + EIS` 输入设计。

2. **enigo-rs/enigo**（MIT 许可证）
   - 仓库地址：https://github.com/enigo-rs/enigo
   - 参考用于 Linux XKB / `x11rb` / `libei` 输入集成模式。

3. **bilelmoussaoui/ashpd**（MIT 许可证）
   - 仓库地址：https://github.com/bilelmoussaoui/ashpd
   - 参考用于 XDG Desktop Portal D-Bus 交互模式。
