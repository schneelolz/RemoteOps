---
name: remoteops
description: 使用 RemoteOps MCP 连接和诊断远程 Agent。用户提到 RemoteOps、RemoteOps 控制码/配对码、九位控制码（如 123-456-789）、现场 Agent、Relay，或要求通过 RemoteOps 检查远程电脑的进程、证书、网络、服务、文件、Shell、SSH、串口时使用。当前任务看不到 RemoteOps MCP 工具时，立即明确报告未加载并停止；禁止改用 Computer Use、屏幕操作、本机 Shell、SSH 直连或其他远控工具，也禁止声称已操作远端。
---

# RemoteOps 远程诊断

## 路由规则

- 用户明确称九位数字为 RemoteOps 控制码或配对码时，必须优先使用 RemoteOps MCP。
- 不要把 RemoteOps 当成远程桌面。除非用户另行明确要求，否则不要改用 Computer Use、屏幕操作、本机 Shell、SSH 直连或其他远控工具。
- 当前任务看不到 RemoteOps MCP 工具时，立即说明“当前任务未加载 RemoteOps MCP”，请用户确认安装后完全退出并重新打开 Codex，再新建任务。不要声称已经连接、检查、修改或重启远程电脑。

## 工作流

1. 全新 Agent 只需填写 Relay 地址并等待显示九位控制码；不得要求用户获取入网码或部署级注册 Token。
2. 收到新控制码时调用 `pair_connection`。已有目标且没有新控制码时先调用 `list_connections`。
3. 使用工具返回的准确 `session_id`；别名和主机名只用于核对，不能代替它。
4. 配对后默认保持“逐项确认”，不额外弹出控制方式确认。用户明确要求完全控制时调用一次 `set_control_mode`；Codex 对该工具的授权就是唯一确认，不得再触发嵌套确认。Agent 端没有逐项确认或完全控制按钮，禁止引导用户去 Agent 点击。
5. 调用 `get_target_info` 核对目标、系统和能力；调用 `get_control_mode` 核对该 `session_id` 当前控制方式。返回 `full_access` 后立即继续任务，不得再次要求用户授权。
6. 优先使用结构化只读工具；一次性 Shell 的只读诊断使用 `run_readonly_command`。需要保持目录、变量或模块状态时调用 `open_shell`，持久 Shell 的所有命令都使用 `run_command` 并接受逐项确认或完全控制约束。
7. 结合用户目标分析结果。信息不足时继续只读检查，不要提前声称结论。
8. 逐项确认模式下，写入、终止进程、服务控制、重启、文件变更、可写串口等操作由 MCP 向当前用户确认。完全控制按 `session_id` 独立生效，空闲一小时自动失效，成功操作后重新计时。

## 使用约束

- 默认只读。用户只要求检查、分析或判断时，不执行修改、重启或终止操作。
- 用户明确要求切换时调用 `set_control_mode`，不要在配对后主动调用。Agent 重启产生新 `session_id`、MCP 重启、主动断开或用户切回逐项确认后，完全控制立即失效。
- `request_action_approval` 只保留给独立 Human Controller 的后续/兼容流程；普通 MCP 首版不要调用它。
- 不在最终回复中暴露控制码、Token、`session_id`、`approval_id` 或恢复令牌。
- 连接或工具失败时报告实际阶段和脱敏错误，不伪造远端结果。
- 若用户要求检查某个进程绑定证书，先确认进程及路径，再检查签名证书、证书有效期和相关服务或监听配置；不要仅凭进程名猜测证书来源。

