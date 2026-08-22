# 安全策略

RemoteOps 可以在远程 Windows 主机上执行受控操作，安全问题可能直接影响客户环境。请不要通过公开 Issue 披露未修复漏洞。

## 支持范围

安全修复优先面向最新发布版本。旧版本可能要求先升级后再验证修复效果。

## 报告漏洞

请通过 GitHub Security Advisories 的“Report a vulnerability”私下提交，并包含：

- 受影响的组件和版本；
- 最小复现步骤；
- 预期影响和已知利用条件；
- 建议修复方向（如有）。

仓库维护者必须在首次公开发布前启用 GitHub Private Vulnerability Reporting。若仓库页面暂时没有“Report a vulnerability”，请等待私下通道启用，不要改用公开 Issue、Discussion、日志或演示视频披露 Token、控制码、客户地址和利用细节。

## 安全边界

- Agent 只主动出站连接 Relay，不应在客户公网开放监听端口。
- 普通 MCP 配对后默认逐项确认；本地 MCP 完成用户确认后使用 `ControllerApproved`，Relay 和 Agent 仍校验身份、会话、能力、路径、结构化参数和审计边界。
- 用户可以仅为一个 `session_id` 临时开启完全控制；授权只保存在 MCP 进程内存中，空闲一小时失效，MCP 重启、主动断开、会话变化或手动切回逐项确认后立即失效。
- 兼容模式下的一次性 Human Controller 审批仍必须绑定精确目标、精确参数和连接代次，且只能消费一次；MCP 不能批准自己的请求。
- Relay 的 Human/AI Controller Token 必须互不相同，并通过环境变量或专用凭据系统提供。Agent 首次登记不使用预共享 Token。
- 新 Agent 只凭 Relay 地址建立受限的待配对连接，只有持有 Controller Token 的已认证 Controller 才能使用九位控制码绑定会话；取得高熵恢复令牌后，重连只使用恢复令牌。Relay 同时限制连接数、握手时间、出站队列、Agent 总数和心跳频率，并回收断线后长期过期且从未配对的身份。
- TLS 私钥、Controller Token 和 Agent 恢复令牌不得进入源码仓库或发布包。

更完整的威胁与信任边界见 [安全模型](docs/安全模型.md)。

