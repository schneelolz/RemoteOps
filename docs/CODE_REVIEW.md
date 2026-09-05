# RemoteOps 代码审查结果

审查日期：2026-09-05

## 先说结论

你现在使用的主流程是通的。本次检查没有发现“配对后不能操作”“权限绕过”“文件传输损坏”这类已经被测试证明的故障，也没有因此修改运行逻辑。

我发现的是一些不会马上影响使用、但以后会增加维护风险的地方：有些业务代码还和入口程序放在一起，部分公共函数的错误说明不够完整，Linux 检查没有覆盖所有 Windows GUI/Service 代码。这些属于整理和发布质量问题，不代表当前功能不能用。

## 检查过什么

- Agent、Relay、Controller、MCP 的连接、配对、断线恢复和权限流程。
- 高风险操作的人工确认、Session 绑定和 Owner 隔离。
- 文件上传下载的目录限制、分块、哈希和覆盖失败恢复。
- SSH、串口、Shell 和只读命令判断。
- Token、密码、证书和审计日志的脱敏。
- GUI、CLI、MCP 和 Service 是否重复实现同一套业务规则。
- 注释、公共接口说明、lint 豁免、CI 和发布检查。

## 需要整理但不影响当前使用的地方

### 1. Agent 的核心运行逻辑还放在入口项目里

现在 GUI 和 Windows Service 已经复用 Agent 代码，所以现有功能没有问题。只是以后如果增加 TUI 或新的 Agent 入口，容易继续复制代码。

建议：以后把“连接 Relay、接收请求、执行设备操作、保存运行状态”提取为独立的 Agent runtime 类库，CLI、GUI 和 Service 都调用它。

### 2. Controller GUI 后端承担的事情太多

GUI 后端同时处理界面消息、AI 请求、对话历史、串口工具和 Relay 调用。现在可以工作，但以后增加 CLI/TUI 时，容易把同样的逻辑再写一遍。

建议：把 AI 对话历史、工具定义和通用操作编排放入共享类库，GUI 只负责按钮、窗口和事件显示。

### 3. 部分错误说明和 lint 规则过于宽松

几个核心类库关闭了“必须说明错误”的检查。现有重要接口大多有注释，但超时、取消、资源清理和可能的 panic 没有全部写清楚。

建议：以后新增或修改公共接口时补充错误和副作用说明，逐步取消整文件豁免，保留必要的局部豁免。

### 4. Linux CI 没有检查部分 GUI/Service 项目

Linux job 排除了 Windows GUI、Windows Service 和 Controller GUI；Windows job 会检查它们，但 Linux 侧不能提前发现条件编译或依赖问题。

建议：保留平台专属测试，同时增加明确的跨平台编译检查，或者在 CI 文档中说明哪些项目只能在对应系统验证。

## 分阶段整理方案

### 第一阶段：画清边界

列出每个模块负责什么，给现有 lint 豁免写明原因，并保留当前行为测试作为基线。

验收：核心类库不依赖 GUI、MCP SDK 或 CLI 参数；现有测试全部通过。

### 第二阶段：提取共用逻辑

先从 Controller GUI 后端提取 AI 会话、工具和通用操作，再让 CLI、MCP、GUI 共用同一套接口。

验收：增加一个新入口时不需要重新编写权限、审批和 AI 会话逻辑。

### 第三阶段：提取 Agent runtime

把 Agent 的连接、请求执行和状态管理移入 `remoteops-agent-runtime` 类库，现有 CLI、GUI、Service 作为不同宿主调用它。

验收：协议、配置和现有命令保持兼容，runtime 可以不依赖 GUI 单独测试。

### 第四阶段：补齐说明和门禁

完善公共接口的错误、取消、资源和安全约束说明，增加 rustdoc、依赖边界和文档检查。

验收：fmt、check、clippy、test、rustdoc 和文档链接检查全部通过。

## 当前验证结果

- `cargo fmt --all -- --check`：通过。
- `cargo check --workspace --locked`：通过。
- `cargo test --workspace --locked`：通过。
- Markdown 相对链接检查：通过，0 个断链。
- `git diff --check`：通过。
- PowerShell 文档检查：当前 macOS 没有 `pwsh`，未执行。
