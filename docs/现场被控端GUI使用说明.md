# RemoteOps 现场被控端 GUI 使用说明

> 文档状态：当前现场客户机手册，适用于 Agent GUI `0.2.0-preview.1`。

- 适用版本：`0.2.0-preview.1`
- 正式程序：`artifacts/release/0.2.0-preview.1/windows-x64/remoteops-agent-gui.exe`
- Relay：由部署者配置，不内置公共默认值
- 使用方式：管理员完成一次配置后，现场人员双击启动
- 运行依赖：正式发布 EXE 静态链接 MSVC CRT，不需要另装 VC++ 运行库

## 一、现场使用

1. 将 `remoteops-agent-gui.exe` 复制到现场 Windows 客户机；
2. 使用专用低权限账户双击运行；
3. 首次运行只填写 Relay 地址，然后点击“保存并连接”；
4. 等待窗口显示“等待工程师连接”和九位临时控制码；
5. 通过可信渠道把本次控制码提供给工程师；
6. 工程师配对后，窗口状态切换为“工程师已连接”；
7. 工作完成后直接关闭窗口，并在确认框中结束本次协助。

GUI 默认将配置保存为与 EXE 同级的 `agent-config.json`；程序目录不可写时回退到 `%LOCALAPPDATA%\RemoteOps\agent-config.json`。旧版用户目录配置继续兼容，但同级配置优先。Agent 只主动出站连接配置的 Relay，不会在客户机监听公网端口。

Agent 首次连接不需要入网码、部署级 Agent Token 或 Controller Token。首次登记后，当前及后续进程使用受保护的身份状态文件和恢复令牌重连；重新生成或删除 Agent 身份状态时，再次启动也只需 Relay 地址。

## 二、TLS 证书确认

- 公网 CA：自动使用 Windows 系统可信根，不需要 PEM；
- 私有 CA：管理员可在配置文件、环境变量或调试参数中预置 `ca_cert` PEM；首次设置页仍只填写 Relay 地址；
- 未知自签名证书且没有 PEM：GUI 在发送 RemoteOps 凭据前显示 Relay 叶证书 SHA-256 指纹；
- “仅本次继续”：只在当前进程固定该证书，退出后不保留；
- “信任并保存”：把指纹写入 `agent-config.json`，后续证书变化时重新确认；
- “取消”：不建立 RemoteOps 连接。

必须通过部署管理员提供的独立可信渠道核对指纹。Relay 地址正确不等于证书可信，不能在未核对时直接选择继续。

## 三、窗口信息

- 主状态：启动、连接、等待工程师、工程师已连接、自动重连、停止或失败；
- 临时控制码：仅用于本次协助，可使用“复制控制码”按钮复制；
- 控制端状态：显示当前在线控制端数量；同一 Agent Session 只绑定一个 `ControllerOwnerId`，该 Owner 可以同时使用 Human 和 AI 角色；
- 能力授权：根据现场机器实际能力显示命令诊断、文件传输、SSH 和串口；
- Relay 与 Agent 标识：只用于现场故障排查，不包含恢复令牌或 Controller Token。

## 四、控制模式

- Agent GUI 不提供“逐项确认”或“完全控制”开关；现场端只负责启动连接、展示控制码和能力状态，以及结束本次协助；
- 普通 MCP 配对后默认逐项确认，修改操作由使用者在 MCP 客户端确认；
- 授权弹窗只显示在当前 Codex；用户点击一次允许即生效，禁止要求现场人员去 Agent 点击完全控制；
- 使用者可以在 MCP 侧仅为当前 `session_id` 临时开启完全控制；授权只保存在 MCP 内存，空闲一小时失效；
- Human 与 AI 使用同一个 Owner，属于同一控制者，Agent 界面不会把二者显示为两个控制者；
- Agent 仍校验 TLS 身份、Owner、会话、能力、路径和结构化操作，不因 MCP 完全控制而绕过这些边界；
- 现场人员需要立即终止当前任务和连接时，直接关闭 Agent 窗口并确认结束本次协助。

## 五、安全行为

- GUI 不展示或导出 Agent 恢复令牌；
- 密码 SSH 首次连接采用 TOFU：Agent 自动扫描并固定主机密钥，后续由 `StrictHostKeyChecking=yes` 严格校验；如需人工预先核对，仍可在 GUI 中扫描并确认指纹；
- SSH 主机信任保存在 Agent 专用本地目录，不接受控制端通过传输目录替换密码认证的 `known_hosts`；
- 密码通过当前 Windows 用户范围的 DPAPI 加密保存在 `%LOCALAPPDATA%\\RemoteOps\\ssh-credentials.dpapi`，解密后只进入 Agent 进程内存；明文不进入配置、RemoteOps 协议、Relay、MCP 参数或 SSH 命令行；
- 删除 GUI 中的 SSH 目标会同步删除对应内存凭据并更新 DPAPI 密文；删除全部目标后密文文件为空凭据，可手动删除该文件以清除本机全部凭据；
- 网络设备只读白名单命令免确认执行；其他 SSH 命令在只读模式下拒绝、默认模式下逐项确认、完全控制下按当前会话授权执行；现场首轮应从 `display version` 开始；
- 关闭窗口前会明确提示工程师将断开、当前任务将终止；
- GUI 不静默驻留托盘，窗口关闭后 Agent 进程结束；
- 修改操作由 MCP 控制模式或显式 Human Controller 兼容审批流程控制；
- 控制码、Token、恢复令牌和审批标识不得写入仓库或长期文档。

## 六、语言和后台服务

GUI 内置简体中文和 English。语言选择优先使用已保存的 GUI 设置，其次是 `--lang zh-CN` / `--lang en-US`、`REMOTEOPS_LANG` 和 Windows 系统语言；窗口右上角可以手动切换，退出时保存。设置 `REMOTEOPS_LANG_FILE` 可以覆盖文本语言包。

需要开机后台运行时，可使用可选 Windows Service：

```powershell
.\deploy\agent-service\windows\Install-RemoteOpsAgentService.ps1 `
  -ConfigPath .\agent-config.json `
  -StartService
```

服务默认运行在 `NT AUTHORITY\LocalService` 和 Session 0，不显示桌面；服务状态可通过 `Get-RemoteOpsAgentStatus.ps1` 查询。需要交互式桌面时仍使用 GUI，服务不会因运行在 Session 0 自动获得额外权限。

## 七、调试参数

正式 GUI 现场通常不需要以下参数。管理员为 CLI、Service 或无人值守环境预置私有 CA 时，可使用：

```powershell
.\remoteops-agent-gui.exe `
  --relay relay.example.com:7443 `
  --server-name relay.example.com `
  --ca-cert .\relay-cert.pem `
  --state-file .\agent-state.json `
  --transfer-root .\transfers `
  --retry-seconds 3
```

Agent 不支持也不需要注册 Token 参数。首次登记成功后，后续重连只使用恢复令牌。

视觉验收可使用：

```powershell
cargo run -p remoteops-agent-gui -- --demo
```

演示模式不连接真实 Relay，显示的控制码和能力均为虚构数据。

## 八、崩溃诊断

Agent GUI 在窗口初始化前安装本地 panic 记录器。Windows 事件查看器如果只显示 `0xc000041d` 和 `combase.dll`，优先检查：

```text
<remoteops-agent-gui.exe 所在目录>\logs
```

每次运行会生成 `agent-gui-runtime-*.log`，记录版本、UTC 时间、进程 ID 和不含凭据的初始化阶段。原生窗口初始化返回错误时，日志还会记录 `error_display` 和 `error_debug`；连接失败时记录 `network_error`，用于区分 TLS、网络和协议错误。Rust panic 另生成 `agent-gui-crash-*.log`，包含 panic 位置和回溯。程序目录不可写时依次回退到 `%LOCALAPPDATA%\RemoteOps\logs` 和 `%TEMP%\RemoteOps\logs`。这些日志不记录 Token、控制码或恢复令牌。

Windows 默认使用 WGPU 的 DirectX 12 后端和低延迟单帧交换链，避免基础显示驱动在悬停重绘时积压帧；复制按钮不再创建额外悬停浮层。日志会记录 `renderer`、`wgpu_backend`、`wgpu_device_type`、适配器和驱动。只有兼容性诊断时才使用 `--renderer glow`；它要求 OpenGL 2.0，不适用于仅提供系统 OpenGL 1.1 的 Windows Server、RDP 和云主机。窗口初始化失败时程序会显示系统错误对话框，并在日志中保留完整错误。

如果只有 Windows 事件 `0xc000041d` 而没有 `agent-gui-crash-*.log`，该异常可能发生在 Rust panic 捕获范围之外。请在管理员 PowerShell 中启用 Windows Error Reporting 完整转储：

```powershell
.\Enable-AgentGuiCrashDumps.ps1
```

复现后将同级 `logs` 中最新的 `.log` 和 `.dmp` 提供给维护者。完成诊断后撤销针对该程序名的转储配置：

```powershell
.\Enable-AgentGuiCrashDumps.ps1 -Disable
```

该脚本只配置 `remoteops-agent-gui.exe`，默认最多保留 3 个完整转储。完整转储可能包含进程内存中的短期敏感信息，只能通过可信渠道传递，分析完成后应删除。

首发前内部复测曾在目标 Windows Server Datacenter 26100 RDP 环境遇到 `tiptsf.dll / combase.dll` 退出崩溃，随后通过独立 UI 线程确认真实启动原因是目标系统只有 OpenGL 1.1。当前首发候选默认使用 DirectX 12，不再要求 OpenGL 2.0，且已修复固定公网叶证书指纹仍依赖本机 CA 根的问题。如果仍无法连接，请提供界面错误和最新 `agent-gui-runtime-*.log`；若出现 Windows 应用程序错误，再按上述步骤采集 `.dmp`。

## 九、验收命令

```powershell
cargo test -p remoteops-agent -p remoteops-agent-gui --locked
cargo clippy -p remoteops-agent -p remoteops-agent-gui --all-targets --locked -- -D warnings
.\scripts\Build-Windows.ps1
.\scripts\Test-AgentGuiCodexMcp.ps1
```

`Test-AgentGuiCodexMcp.ps1` 默认从剪贴板读取 GUI 复制的控制码，也可以通过 `-PairingCode` 显式提供。脚本仅在子进程内传递控制码，最终结果不会输出控制码、Token、`session_id` 或审批标识。
