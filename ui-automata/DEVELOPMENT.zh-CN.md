# 开发指南

## 重新生成工作流 JSON Schema

```sh
cargo run -p ui-automata --bin schema-gen
```

该命令会在 workspace 根目录写出 `workflow-schema.json`。在任意 `.yml` 工作流文件顶部添加 `# yaml-language-server: $schema=../../workflow-schema.json`，即可在 VS Code 中获得自动补全与实时校验（需安装 [YAML 扩展](https://marketplace.visualstudio.com/items?itemName=redhat.vscode-yaml)）。

## 工作流文件格式

请参阅 [`src/yaml.rs`](src/yaml.rs) 中的字段级文档注释——它们会被直接编译生成至 `workflow-schema.json`，并在支持 JSON Schema 的编辑器或 Agent 中作为提示与补全信息展示。

核心概念：

- **Anchors（锚点）** — 指向实时 UI 元素的命名缓存句柄。在 `anchors:` 中统一声明，通过各阶段的 `mount:` 激活，并在动作与条件中作为 `scope` 引用。
- **Selectors（选择器）** — 在锚点子树内定位元素的类 CSS 路径（例如 `>> [role=button][name=Close]`）。
- **Conditions（条件）** — 每 100 ms 轮询一次的布尔断言：`ElementFound`、`WindowWithAttribute`、`DialogPresent`、`AnyOf`、`Not` 等。
- **Steps（步骤）** — “动作（action）+ 期望条件（expect）”对。可选的 `precondition` 在不满足时会跳过该步骤（不会报错）。
- **Phases（阶段）** — 带 `mount`/`unmount` 生命周期的有序步骤分组，支持可选 `precondition`（为 false 时跳过该阶段，不视为错误）。
- **Recovery handlers（恢复处理器）** — 当步骤超时且处于已知异常状态时触发；执行纠正动作后可执行 `retry_step`、`skip_step` 或 `fail`。

## 运行测试

```sh
# 单元测试 + mock 测试（无需真实桌面后端即可运行）
cargo test -p ui-automata
```

## 静态校验全部工作流

```sh
cargo run --bin ui-workflow-check -- $(find workflows -name '*.yml' | tr '\n' ' ')
```
