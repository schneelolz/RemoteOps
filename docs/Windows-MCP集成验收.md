# Windows-MCP stdio 集成验收

此目录中的 `Test-WindowsMcpProtocol.py` 只读验证锁定的 Windows-MCP 版本是否真正完成 MCP 初始化。脚本要求当前 Python 环境中的 `windows-mcp` 版本为 0.8.5，以无控制台子进程启动 `python -m windows_mcp serve --transport stdio`，发送 `initialize`、`notifications/initialized` 和 `tools/list`，限制单帧 4 MiB、总响应超时 60 秒，并在结束时回收子进程。

2026-09-10 在提交 `08ddee78c26182b103d62c1c84c1fbec82a280b2` 的上游工作区中使用 Python 3.14.7 和锁定依赖运行通过：协商协议为 `2025-11-25`，服务端版本为 `4.0.3`，发现 21 个工具（包括 `Snapshot`、`Screenshot`、`Click`、`Type`），子进程退出码为 0。该结果只证明上游 MCP 生命周期和工具发现成功，不代表 RemoteOps 已完成内容块到 `VisualObservation` 的转换或 Named Pipe 适配；Provider 仍必须在初始化失败、超时和转换失败时报告错误并禁止宣称 ready。

同日将相同源码、锁定依赖和 Python 3.14 运行时部署到 Windows 11 目标机的交互式 RDP Session，验证结果同样为初始化成功、协议 `2025-11-25`、21 个工具、子进程退出码 0；只读 `Snapshot` 还返回了显示器、Focused Window 和 UI Tree。该结果证明上游在目标交互式 Session 可运行，但仍不等于 RemoteOps 已接管它的输入权限。
