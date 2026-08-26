# Agent GUI 设计 QA

- 支持范围：Windows Agent GUI；macOS Agent GUI 不在当前版本范围内。
- 目标视口：Windows 原生窗口内部 `520 × 410`。
- 最终界面：主状态、临时控制码倒计时、工程师连接状态、能力摘要、高级设置和停止协助。
- 高级设置只显示 Relay、进程提升状态和传输目录；SSH 密码不在 Agent GUI 输入或展示。
- SSH 密码由 Windows/macOS Controller MCP 的本机安全窗口获取，并使用 Agent 当前进程 HPKE 公钥加密。

## 证据状态

此前记录引用了 Windows 用户目录中的绝对路径，并声称高级设置含 SSH 凭据入口；那是界面调整过程中的中间状态，不再作为最终 QA 证据。

最终 Windows 截图应在 Windows 发布验收时保存到以下仓库相对路径：

- `docs/assets/agent-gui/runtime-zh.png`
- `docs/assets/agent-gui/runtime-en.png`
- `docs/assets/agent-gui/advanced-settings-zh.png`
- `docs/assets/agent-gui/stop-confirmation-zh.png`

在这些截图由 Windows 验收重新生成并提交前，视觉 QA 状态为 `pending-windows-recapture`；不得继续引用旧绝对路径或宣称 SSH 凭据入口已验证通过。
