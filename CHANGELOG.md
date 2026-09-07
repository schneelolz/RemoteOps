# 变更日志

本变更日志只记录实际公开发布的版本。GitHub Release 发布前的内部构建、现场复测和修复不会递增公开版本号；必要的技术过程记录在项目状态和测试资料中。

## [0.2.0-preview.6] - 待发布候选

- 重做 Windows Agent GUI 品牌与运行页：新增 RemoteOps Shield + Terminal 标识、Windows 应用图标、连接详情入口、可用能力状态和按连接状态变化的退出/停止文案。
- 增加 Ubuntu 24.04 x86_64 Headless Agent、systemd 宿主、安装/卸载/状态脚本和带许可证、哈希清单的发布包。
- Linux 进程与服务查询使用结构化输出，支持 POSIX Shell、低权限运行、SIGTERM/SIGINT 清理和安全的短期配对状态文件。
- Controller 事件历史游标在 Agent 重启后保持单调，避免增量输出丢失；线协议保持 v14。
- macOS MCP 安装器修复 JSON/plutil 写入，并保留现有全局审批配置。
- 此版本为本地验收候选；未创建公开 Release。详见 Linux 验收报告。

## [未发布]

- 准备首个 GitHub Technical Preview。
- 当前源码候选版本为 `0.2.0-preview.6`，尚未创建公开 Release；此前内部构建和现场复测不构成公开版本历史。
- 重做 Windows Agent GUI 日常运行页：采用 `520 × 410` 紧凑窗口，突出服务状态、临时控制码倒计时、工程师连接状态和能力摘要；技术运行信息移入高级设置，SSH 密码入口从 Agent GUI 移除。
- 线协议先升级到 v9，增加短时单次 Agent 入网码、可确认的 Controller 会话释放和同 Owner 新实例接管通知；随后升级到 v10，取消普通 Agent 首次连接所需的入网码和部署级注册 Token；v11 增加有状态分块文件传输、下载覆盖授权和完整文件校验；v13 增加 Agent 租约续期事件；当前 v14 将 SSH 密码改为控制端本机安全窗口输入和端到端加密。
- 公网 CA Agent 继续零证书配置；未知自签名证书由 GUI 在发送 RemoteOps 凭据前展示 SHA-256 指纹，经独立渠道核对后可仅本次继续或固定保存，无界面 CLI/Service 仍需预置 CA 或已核对指纹。
- 修复 Codex 接受 MCP 授权后完全控制仍未生效的问题：配对默认逐项确认，完全控制只通过 `set_control_mode` 的 Codex 工具授权开启，不再嵌套确认；Agent 端没有授权按钮。
- 修复 Agent 永久拒绝 SSH 修改命令的问题：网络设备只读命令继续免确认，其他 SSH 命令在只读模式下拒绝、默认逐项确认，完全控制下按 Relay 已验证授权执行。
- 修复旧 MCP 进程残留绑定导致无法断开或重新配对的问题；`close_connection` 仅在 Relay 确认解绑后成功，接管后的旧 MCP 会清理本地连接和自动恢复记录。
- 将受约束的 `appcmd list` 纳入只读查询，同时继续拒绝管理动作、管道、重定向和命令拼接；发布门禁新增 Windows、MCP 包目录和 ZIP 内 MCP 哈希一致性校验。
- Windows Agent GUI 移除会触发基础显示驱动反复重绘的悬停浮层，并把 WGPU 交换链调整为低延迟单帧队列；高频系统命令改为直接隐藏启动，减少 PowerShell/CMD 窗口闪现。
- MCP 与 CLI 文件传输改用 1 MiB 分块，单文件上限提升到 16 GiB；超过 1 GiB 时在读取、哈希和发送前单独确认，上传和下载覆盖通过同目录临时文件、分块及完整 SHA-256 校验和可恢复原子提交保护原文件。
- 修复 Human/AI 权限状态互相覆盖和 Agent GUI 本地 FullAccess 失效问题；MCP、CLI、GUI 与 Relay 现在显示并执行一致的有效权限，人工接管只在接管期间把 AI 降为只读。
- 新增 MCP `close_shell`；持久 Shell 执行 `exit` 后不再误报失败。CMD、Windows PowerShell 5.1 和 PowerShell 7 的一次性与持久子进程统一隐藏窗口，并规范 UTF-8 中文输出及 ANSI 清理。
- 修复持久 Windows PowerShell 5.1 和 PowerShell 7 每条命令落入子作用域、导致普通变量与函数不能跨命令保留的问题，并补充双版本回归测试。
- 修复 Windows PowerShell 5.1 下正式构建、发布门禁和 MCP 安装脚本的编码与旧 .NET API 兼容问题；旧式 granular 审批配置可迁移为当前 Codex 内联语法，并保留空行及其他配置段。
- 修复 Relay 紧急停止授权顺序、管理台动态 HTML 转义、管理密码 Argon2id 迁移、管理登录限速及跨平台 Clippy 门禁。

## [0.2.0-preview.5] - 未发布

### 安全与质量修复

- 线协议升级到 v14；Agent 每个进程生成临时 HPKE 密钥，SSH 密码使用 X25519-HKDF-SHA256 与 ChaCha20-Poly1305 绑定精确会话、目标和命令后加密。
- MCP `run_ssh` 通过 Windows/macOS 本机安全窗口直接获取密码，工具参数、对话和审计不再接收密码；可显式在 MCP 内存中固定缓存十分钟。
- 移除独立凭据注入操作和 Agent 进程级密码仓库；Agent 解密密码后仅用于当前 SSH 请求，并拒绝加密载荷重放。

## [0.2.0-preview.4] - 未发布

### 安全与质量修复

- 线协议升级到 v13，新增 Agent 专用租约续期事件；现有管理 API 路径保持不变。
- SSH 密码改为 MCP 一次性注入，仅保存在 Agent 进程内存，不再由 GUI 管理或写入磁盘。

## [0.2.0-preview.1] - 历史候选，未发布

### 首个公开预览

- Windows x64 Agent CLI/GUI、可选 Agent Service、Controller CLI/GUI 和 Codex STDIO MCP。
- Linux x64 Docker Relay，Agent 通过主动出站 TLS 连接，不要求现场开放公网入站端口。
- Apple Silicon macOS MCP 安装包，可从 Mac 上的 Codex 通过 Relay 控制现有 Windows Agent。
- 受控 Shell、文件、系统、串口、SSH 和指定 TCP 能力。
- Agent 环境画像、三档权限、同 Owner 的 Human/AI 协作、TLS 配置和双语 GUI。
- 自托管部署、配置示例、测试脚本和安全边界说明。

### 首发前安全与兼容性收口

- 线协议升级到 v7；Agent 首次登记增加独立部署级注册 Token，Relay 不在内存快照或状态文件中留存该 Token，后续重连只使用恢复令牌。
- 持久 Shell 绑定实际 Shell 类型并保留可变状态，不能伪装成免确认只读；只读工具改为一次性 Shell，持久 Shell 命令统一走审批或完全控制路径。
- Agent 增加在途请求、持久 Shell、串口会话的全局和单 session 上限，并拒绝重复或近期完成的 `request_id` 重放。
- 下载默认创建新文件，覆盖需要显式 `--overwrite`；串口非 UTF-8 输出和敏感行在远端事件前脱敏。
- Relay 增加连接/握手/首帧/出站队列/Agent 登记和心跳限流，发布脚本补齐许可证、依赖清单、最终资产白名单和哈希门禁。

### 首发前内部修复

- Windows Agent GUI 保留 WGPU/DirectX 12 作为显式诊断选项；首发前根据基础显示驱动兼容性复测调整默认渲染器。
- 修复固定 Relay 叶证书指纹仍依赖本机 CA 根的问题，并增强 GUI 网络错误日志。
- MCP 与随包 `remoteops` skill 增加自然语言路由，用户提供 RemoteOps 控制码时优先配对并执行受控远程诊断。
- Windows STDIO MCP 使用无控制台窗口发布；Agent 的一次性和持久 Shell 调用避免弹出 PowerShell/CMD 黑框。
- Agent GUI 收紧主窗口和首次配置页布局，修复底部黑区，增加输入框高度，并在屏幕工作区居中启动。
- MCP 在 macOS 支持 `HOME` 下的 Codex 配置路径，审计与文件交换使用 `~/Library/Application Support/RemoteOps`。
- macOS 安装器使用 Keychain 保存 Controller Token，不把 Token 写入 Codex 配置、RemoteOps JSON、日志或安装包。
- Windows 正式发布产物静态链接 MSVC CRT，不要求现场机器另行安装 VC++ 运行库。
- 发布工作流强制校验 Git 标签与 Cargo 版本一致，将 Technical Preview 标记为 prerelease，并拒绝缺少许可证、依赖清单、哈希不一致或包含额外文件的发布资产。
