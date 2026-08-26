# 项目状态

更新时间：2026-08-25

## 当前基线

- 项目：RemoteOps
- 当前源码版本：`0.2.0-preview.5`
- 首个计划公开版本：`0.2.0-preview.5`
- 当前协议版本：`v14`
- 发布阶段：GitHub Technical Preview 准备中，尚未创建首个 GitHub Release
- 现场端：Windows x64 Agent
- Relay：Linux x64 + Docker
- Codex MCP：Windows x64、Apple Silicon macOS

公开版本以实际 GitHub Release 为准，当前源码候选版本为 `0.2.0-preview.5`，尚未创建首个公开 Release。迁移前的 `4.x` 只代表内部开发和验收历史，不属于公开版本序列。v9 完成 Controller 会话释放、同 Owner 新实例接管和 MCP 授权修复；v10 取消普通 Agent 首次连接所需的入网码和部署级注册 Token；v11 增加大文件分块传输、权限状态修复、持久 Shell 显式关闭及完整 Windows 隐藏进程回归；v13 增加 Agent 租约续期事件；v14 将 SSH 密码改为控制端本机安全窗口输入和 MCP 到 Agent 的 HPKE 单次加密载荷。Agent、Relay 和 Controller 必须使用同一协议版本。

## 已完成

- Rust Workspace 已按核心类库与表现层壳子组织：`crates`、`apps`、`tests`、`deploy`、`scripts` 和 `docs`。
- Agent、Relay、Controller CLI/GUI、MCP 和串口 Demo 的主要闭环已经实现。
- Agent 主动出站连接 Relay，不要求现场开放公网入站端口。
- Agent 首次连接不需要入网码或部署级注册 Token；GUI 普通流程只填写 Relay 地址并显示九位控制码。
- Agent 上报脱敏 Windows 环境画像，包括系统、版本、架构、Shell、PowerShell、SSH 和能力集合。
- MCP 按 Agent `session_id` 独立提供逐项确认和完全控制：默认逐项确认；完全控制仅保存在 MCP 内存，空闲一小时失效，成功操作滑动续期。
- Relay/Agent 使用 `ControllerApproved` 区分 MCP 已完成人机确认与 Agent 本地 `FullAccess`，Human Controller 不能申请该模式；底层身份、会话、能力、路径和结构化操作校验继续生效。
- Human 与 AI 使用同一 `ControllerOwnerId` 协作，同一个 Agent Session 不允许两个独立 Owner 同时控制。
- MCP、CLI 和 GUI 复用共享应用服务，协议消息、会话绑定、权限、审批和审计不在壳子中重复实现。
- 公网 CA、显式 CA PEM 和人工核对的 TLS 指纹信任路径已经形成；GUI 可首次确认未知自签名证书，无界面宿主继续要求预置 CA 或指纹。
- 中文/英文 GUI 和安全的外部语言包覆盖机制已经形成。
- Windows Agent GUI 已改用 WGPU/DirectX 12，解决多台 Windows Server、RDP、云主机和虚拟机只有 OpenGL 1.1 时无法启动的问题。
- Windows MCP 使用无控制台窗口发布；Agent 的一次性和持久 Shell 调用已增加无黑框回归测试。
- CMD、Windows PowerShell 5.1 和 PowerShell 7 已统一处理 UTF-8 中文输出；PowerShell 7 输出会移除 ANSI 控制序列，持久 Shell 支持 `close_shell`，显式 `exit` 会正常清理而不误报失败。
- MCP 与 CLI 使用 1 MiB 分块上传和下载，单文件上限为 16 GiB；超过 1 GiB 时必须在读取、哈希和发送前单独确认。上传和本地下载覆盖使用同目录临时文件、分块与完整 SHA-256 校验和可恢复原子提交，失败时保留原文件。
- Relay 分别保存 Human 与 AI 权限，Human 配对不再覆盖 AI 的 `ControllerApproved`；人工接管期间 AI 临时只读，释放后恢复。Agent GUI、Controller GUI、CLI 和 MCP 使用或显示同一有效权限来源。
- MCP 与随包 `remoteops` skill 支持自然语言路由，RemoteOps 控制码会优先进入配对和受控远程诊断流程。
- Agent GUI 已移除控制模式选择和 SSH 凭据管理入口；控制模式统一由使用者侧 MCP 管理。SSH 密码只在控制端本机安全窗口输入，可单次使用或在 MCP 内存固定缓存十分钟，并通过 HPKE 绑定精确请求发送；Agent 不建立密码仓库。运行页采用 `520 × 410` 固定紧凑窗口，突出会随在线心跳刷新的控制码租约倒计时、工程师连接状态和能力摘要，并保留停止确认和屏幕工作区居中。
- Apple Silicon macOS MCP 已加入源码适配、Keychain Token、标准 macOS 数据目录、安装/检测/卸载脚本和 GitHub Actions 构建链路。
- Relay 恢复后，Agent 可使用有效状态和恢复令牌重新认证；同一 MCP 进程会重试已知配对。审批和在途写操作不会自动重放。
- 本地 MCP 编译和单元测试已通过；Windows、Linux 和 macOS 的完整首发产物仍需由干净 CI 生成并复核。
- GitHub Release 工程门禁已加入精确标签/Cargo 版本校验、Rust 1.95 MSRV 检查、固定 Action 提交、prerelease 标记、许可证与依赖清单、最终资产白名单和 SHA-256 复算。

## 当前不包含

- 屏幕采集、画面编码、鼠标键盘远程控制和多显示器。
- macOS/Linux 被控端 Agent。
- Intel Mac 或 Universal Binary MCP。
- 维护者运营的共享 Relay。
- 任意端口转发、SOCKS、反向隧道和网段扫描。
- 跨 MCP/Codex 进程重启的已配对目标持久化。

## 公开发布前门禁

1. 在独立 Git 历史上完成秘密扫描，并复核 GitHub Actions、依赖和许可证。
2. 在干净提交和 GitHub Actions 中生成 Windows、Linux Relay、Windows MCP 和 Apple Silicon macOS MCP 产物。
3. 在真实 Apple Silicon Mac 验证安装、Keychain、Codex `/mcp`、自然语言路由和 Windows Agent 只读诊断。
4. 在全新低权限 Windows 账户验证 Agent GUI/CLI、Controller 和 MCP 的配置与错误提示。
5. 验证管理员环境下 Agent Service 的安装、停止、恢复、卸载和 `LocalService` 权限边界。
6. 使用真实串口和交换机复核正式远程串口路径、分页、审批写入、拔插和重连。
7. 决定 Windows Authenticode 和 macOS Developer ID/Notarization 策略，并在 Release 中披露未签名风险。
8. 在 Windows Server Datacenter 26100 RDP 环境复测首发候选：自然语言调用 RemoteOps 完成只读诊断，连续调用期间不得闪现 PowerShell/CMD 黑框。
9. 在 Windows Codex 和真实 Apple Silicon Mac 验证 MCP elicitation：默认逐项确认、拒绝不执行、每 Agent 隔离、完全控制一小时滑动过期、断线恢复和重启失效。
10. 在真实 Windows Agent 上验证大文件双向传输、覆盖失败恢复、嵌套目录、超过 1 GiB 的读取前确认，以及 CMD/Windows PowerShell/PowerShell 7 中文输出、`exit`、`close_shell` 和无可见控制台窗口。

## 本地保留物

`artifacts/release/0.2.0-preview.5` 是 Git 忽略的当前本地候选产物，不等于已公开 Release；该目录仅代表首个计划公开产物基线。自有 Relay 配置继续独立放在 `artifacts/local-test`，不混入可分发产物。`target` 是可随时重建的 Cargo 缓存。

## 判断

RemoteOps 已具备公开 Technical Preview 的产品形态。macOS MCP 可以明显降低 Mac 上 Codex 用户的试用门槛，但在 GitHub macOS arm64 CI 和真实 Apple Silicon Mac 安装验收完成前，仍应表述为“已实现、待实机验收”，不能宣称已经完成生产兼容。
