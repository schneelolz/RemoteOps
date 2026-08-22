# Visual Provider 调研

调研基线：2026-08-05。本文只作为未来接入参考，不代表 RemoteOps 已经集成或对这些项目当前状态作永久承诺。正式采用前必须重新检查上游仓库、完整依赖树、Release 状态、漏洞记录、商用限制和许可证版本。

## 结论

RemoteOps 不应自行开发屏幕编码、远程桌面传输、多显示器、剪贴板同步或基础鼠标键盘协议。第一阶段先保持命令行、文件、系统和设备能力稳定；未来按真实任务需要，以独立 Visual Provider 接入现成开源方案。

建议的验证顺序：

1. 先让 RemoteOps MCP 与现有图形 MCP 并列运行，验证 AI 是否真的需要画面和输入。
2. 再定义截图、观察、点击、键盘输入、停止、接管和审计的最小 Provider 接口。
3. 最后选择可自托管、许可证清晰、支持人工接管且不要求把 AI Key 下发到客户机的方案。

## 方案清单

| 项目 | 调研时许可证 | 主要定位 | 未来参考意见 |
|---|---|---|---|
| [QuickDesk](https://github.com/barry-ran/QuickDesk) | MIT | 远程桌面与 MCP 结合 | 优先验证是否能作为外部 Visual Provider 并列接入，不复制代码 |
| [Windows-MCP](https://github.com/CursorTouch/Windows-MCP) | MIT | Windows 本机 UI Automation | 可作为本机 GUI 自动化层，不负责远程画面传输 |
| [UI-TARS Desktop](https://github.com/bytedance/UI-TARS-desktop) | Apache-2.0 | 视觉 Computer Use Agent | 适合研究视觉决策和任务编排，不等同于远程传输层 |
| [windows-computer-use-mcp](https://github.com/sandraschi/windows-computer-use-mcp) | MIT | 截图、输入和 Windows GUI 自动化 MCP | 可用于比较 MCP 工具边界和安全确认设计 |
| [RustDesk](https://github.com/rustdesk/rustdesk) | AGPL-3.0 | 成熟的跨平台远程桌面 | 可作为远控底座候选，接入前重点评估进程边界和 AGPL 义务 |
| [Apache Guacamole](https://github.com/apache/guacamole-client) | Apache-2.0 | RDP/VNC/SSH Web 网关 | 适合浏览器网关和已有协议接入，不是便携 Windows Agent 的唯一答案 |

## Provider 准入条件

- 允许自托管，不能强制依赖闭源云端服务。
- 许可证和依赖许可证允许计划中的使用、分发和商业化方式。
- 能明确区分截图/观察与点击/键盘等输入操作。
- 支持停止、暂停、人工接管和操作结果回传。
- 不要求把 RemoteOps、Relay Token 或 AI API Key 写入客户机。
- 通过独立进程、MCP 或稳定协议集成，避免把大规模第三方代码并入核心。
- 能把会话、Owner、权限、审批、超时和审计映射到 RemoteOps 的安全模型。

## 风险提示

“开源”不等于可以无条件合并或再分发。许可证可能因仓库版本、依赖、附加组件或托管服务而变化；任何正式适配都必须保留许可证文本、第三方声明和独立安全评估。图形操作还会显著扩大风险面，默认应保持关闭，并要求醒目的人工接管和紧急停止能力。

