# RemoteOps 模块说明

可以把 RemoteOps 简单理解成两层：

1. **核心逻辑**：决定能不能做、应该怎么做、如何连接和记录，放在 `crates`。
2. **使用入口**：把按钮、命令行、MCP 工具或 Windows Service 转成核心请求，放在 `apps`。

目标是同一条规则只写一次。比如“重启必须确认”应该由核心逻辑决定，GUI 显示确认框、CLI 打印提示、MCP 发起确认，都是不同的外壳表现。

## 当前各部分做什么

| 位置 | 通俗职责 |
|---|---|
| `remoteops-domain` | 定义连接、操作、权限、事件这些基本数据 |
| `remoteops-policy` | 判断命令是否只读、是否需要审批 |
| `remoteops-protocol` | 定义 Agent、Relay、Controller 如何通信 |
| `remoteops-session` | 管理连接、别名、Owner 和人工接管 |
| `remoteops-device` | 真正调用 Windows Shell、文件、SSH、TCP 和设备 |
| `remoteops-serial` | 串口终端、分页、串口查询和脱敏 |
| `remoteops-audit` | 审计记录、哈希和敏感信息脱敏 |
| `remoteops-application` | 把连接、策略、审批和事件串成 Controller 用例 |
| `remoteops-ai` | 调用 OpenAI 兼容接口和执行工具循环 |
| `remoteops-agent` | 当前 Agent 的运行逻辑和 CLI 入口，后续会拆出 runtime 类库 |
| `remoteops-controller-gui` | Controller 窗口、按钮、终端和状态展示 |
| `remoteops-controller-cli` | Controller 命令行参数和终端输出 |
| `remoteops-controller-mcp` | 把 MCP 工具和确认请求接到核心服务 |
| `remoteops-agent-gui` | Agent 窗口、配对码和运行状态展示 |
| `remoteops-agent-service` | Windows Service 的启动、停止和恢复 |
| `remoteops-relay` | Relay 网络服务、配对、转发和持久状态 |

## 一次操作怎么走

使用者从 Codex、CLI 或 GUI 发起请求；入口把请求交给共享的应用服务；应用服务检查 Session、权限和审批；Relay 把通过检查的请求转给 Agent；Agent 再做一次现场安全校验，最后调用 Shell、文件或设备能力。

因此将来增加 TUI 时，TUI 只需要新增“终端输入和输出”，不应该复制权限判断、审批规则或文件安全逻辑。

## 后续整理顺序

1. 先保持当前接口和行为不变，记录每个模块的职责。
2. 从 Controller GUI 后端提取可共享的 AI 会话和操作编排。
3. 从 `apps/remoteops-agent` 提取 `remoteops-agent-runtime` 类库，让 CLI、GUI 和 Service 共用。
4. 补齐公共接口说明和依赖边界检查。

这是一项维护性整理，不要求为了拆文件而改变用户操作方式、协议或配置格式。
