# 工具中心运行时验收记录

日期：2026-11-05。环境：本机 Windows x64，系统内部版本 26200。此记录覆盖资源执行及 Agent 分块接收，不代替 Relay/MCP TLS 联调、后台验收或正式平台登记。

## 实现边界

- 集中管理器默认关闭，接收已认证目录；旧资源站配置不参与集中发现。
- 读取任务逐条同步发布策略及取消信号，异步等待清理。连续停用、重新启用不能保留旧实例。新连接清空旧目录修订，拒绝同连接旧修订及同修订不同内容。
- 中继包通过已认证回调分块接收；与 HTTPS 外链共用大小、SHA-256、安全 ZIP、启动前缓存完整性和平台门禁。普通工具列表删除外链查询参数。
- 通用 STDIO MCP 执行标准初始化、工具发现与调用；浏览器适配仍使用独立 profile。通用 CLI 使用登记 Schema、原生程序和 argv 数组，每次调用独立进程，返回退出码、stdout、stderr 和文本或 JSON 结果。
- 停用、断线、超时或取消会终止受管进程；已提交调用无法确认结果时返回 `result_unknown`，不自动重放。

## 自动化验证

| 命令 | 覆盖范围 | 结果 |
| --- | --- | --- |
| `cargo test -p remoteops-resources --lib` | ZIP 路径/链接/压缩限制、HTTPS 及缓存、真实原生 CLI 和通用 STDIO、Schema、会话隔离、取消、修订与快速开关 | 20 通过，1 个真实浏览器测试按默认忽略，另行显式执行通过 |
| `cargo test -p remoteops-agent tool_transport::tests --lib` | 256 KiB 逐块确认、拼接、偏移错误、超长/短块/截断、禁用拒绝、断线释放、同步目录门禁与重连 | 4 通过 |
| `cargo clippy -p remoteops-resources -p remoteops-agent --all-targets -- -D warnings` | 严格静态检查 | 通过 |

CLI 测试在本机用 `rustc` 编译原生夹具 EXE，实际经资源包解压、缓存和受管进程运行，验证包含空格及 Shell 元字符的输入保持为单个参数，拒绝错误类型及未知字段；非零退出码 7 和两个输出流被完整保留。通用 MCP 夹具拒绝任何额外启动参数，只提供 `fixture_echo`，验证通用协议不依赖浏览器参数或方法。

## 固定浏览器候选

命令：`cargo test -p remoteops-resources real_candidates_headless_navigation_state_isolation_and_unknown_results -- --ignored --nocapture`。

| 包 | SHA-256 |
| --- | --- |
| `agent-browser-0.41.2-win-x64-499683b78ec6dc3b.zip` | `499683b78ec6dc3b57e0ba096eed4e26924f67400399585da65378fb36ba7b0a` |
| `playwright-mcp-0.0.84-win-x64-6367bf1b69220db4.zip` | `6367bf1b69220db4663a11595dc4c710af7af9c80b72ddb1bfcd7558ea9946da` |

包从现有 `artifacts/resource-candidates/site/remoteops/v1` 读取，未改动 ZIP、许可证或清单平台记录。首轮真实测试 1 通过，49.81 秒：两者均完成 headless 页面导航、语义快照、填写与点击、跨会话拒绝、未知结果禁止重放、运行目录及 profile 回收，并按包路径核实所有真实进程退出。增加通用 MCP 参数 Schema 门禁后再次执行，1 通过、0 失败，99.47 秒；Playwright 退出核实约 1.719 秒，agent-browser 约 1.498 秒。

本批 headless 结果只记录本机测试事实，不转换为交互桌面平台 `passed`；未据此扩张 Windows 版本范围、其他系统或架构。未部署、替换运行组件或向生产中继上传工具包。

最终停用回归补充：同步发布停用时先 Drop 全部空闲实例以终止进程树，再等待后台目录清理；真实 3 个进程与首目录被占用测试通过。
