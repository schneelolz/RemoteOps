# Codex MCP 接入说明

> 文档状态：当前安装和配置说明。普通使用者优先阅读[RemoteOps MCP 使用手册](RemoteOpsMCP使用手册.md)。

本机已安装环境的日常使用、自然语言示例和现场实测结果见
[RemoteOps MCP 使用手册](RemoteOpsMCP使用手册.md)。

- 核对日期：2026-08-13
- 当前版本：`0.2.0-preview.4`
- MCP 传输：本地 STDIO
- MCP SDK：官方 `modelcontextprotocol/rust-sdk` 的 `rmcp`
- 官方依据：[Model Context Protocol](https://learn.chatgpt.com/docs/extend/mcp.md)

## 一、接入结论

Codex 桌面端、Codex CLI 和 IDE 扩展在同一 Codex 主机上共享 MCP 配置。默认全局配置文件是：

```text
~/.codex/config.toml
```

受信任项目也可以使用：

```text
<项目目录>/.codex/config.toml
```

RemoteOps 只启动本地 STDIO MCP，不开放公网 MCP HTTP。公网只部署 TLS Relay，Codex 的登录信息和 API Key 不会传到 Windows Agent 或 Linux Relay。

## 二、控制模式

普通使用推荐保持安装器默认的 `agent-controlled`。配对完成后直接进入逐项确认，不弹出控制方式选择。只有用户明确要求完全控制时才调用 `set_control_mode`；Codex 对该工具的授权是唯一一次确认，MCP 不再嵌套弹出第二次确认。Agent GUI 没有逐项确认或完全控制按钮，不应要求现场人员去 Agent 点击。`set_control_mode` 返回 `full_access` 后应立即继续任务，不得再次索要授权。

| 选择 | 行为 |
|---|---|
| 逐项确认（默认、推荐） | 只读检查直接执行；写入、终止进程、服务控制、重启、文件变更、可写串口等修改操作前由 MCP 询问当前用户 |
| 完全控制 | 当前 `session_id` 的修改操作不再逐项询问；授权只保存在 MCP 内存，空闲一小时失效，成功远程操作后重新计时 |

每台 Agent 按不可变 `session_id` 独立保存控制模式，连接两个 Agent 时不会互相继承完全控制。Agent 短暂掉线并以原 `session_id` 恢复时授权保留；Agent 重启产生新 `session_id`、MCP/Codex 重启、主动断开、用户切回逐项确认后立即失效。

`readonly`、`approval` 和 `full-access` 启动参数保留给兼容或受控部署：`readonly` 禁止修改；`approval` 继续使用独立 Human Controller 的一次性 `approval_id`；`full-access` 是显式的无人值守部署选项。普通首版 MCP 不依赖 Human Controller。

## 三、推荐安装和配置

Windows x64 优先使用：

```text
artifacts\release\0.2.0-preview.4\mcp\RemoteOps-MCP-Windows-x64-0.2.0-preview.4.zip
```

安装包内的 `README.md` 可直接交给安装者或安装者的 Codex 阅读。`Install-RemoteOpsMcp.ps1` 会保留其他 Codex 配置、创建备份，并安全配置 Token 与统一 Owner 环境变量转发。

Apple Silicon Mac 使用：

```text
RemoteOps-MCP-macOS-arm64-0.2.0-preview.4.tar.gz
```

macOS 安装器把 Token 保存到当前用户 Keychain，由 `~/.codex/remoteops/launch-remoteops-controller-mcp.sh` 在启动 MCP 时读取。非敏感的 Owner UUID 写入 `controller-config.json`，因此从 Finder 启动 Codex 时不依赖 shell 环境变量继承。完整步骤见 [macOS MCP 接入说明](macOSMCP接入说明.md)。

安装器会生成不含 Token 的 `controller-config.json`。MCP 不内置共享中继服务，安装时必须提供自行部署或组织批准的自托管 Relay 地址：

```toml
approval_policy = { granular = { sandbox_approval = true, rules = true, mcp_elicitations = true, request_permissions = false, skill_approval = false } }

[mcp_servers.remoteops]
command = 'C:\Users\<用户名>\.codex\remoteops\remoteops-controller-mcp-0.2.0-preview.4.exe'
args = ['--config', 'C:\Users\<用户名>\.codex\remoteops\controller-config.json', '--command-mode', 'agent-controlled']
env_vars = ['REMOTEOPS_CONTROLLER_TOKEN', 'REMOTEOPS_CONTROLLER_OWNER_ID']
startup_timeout_sec = 15
tool_timeout_sec = 180
enabled = true
required = false
default_tools_approval_mode = 'approve'

[mcp_servers.remoteops.tools.set_control_mode]
approval_mode = 'prompt'
```

连接参数的优先级是命令行、环境变量、JSON 配置。公网环境默认使用 Windows 系统可信根。自签名或私有 CA Relay 可以设置 `ca_cert`；没有 PEM 时，只能填写已通过独立可信渠道核对的 `tls_fingerprint`。MCP 是无界面 STDIO 服务，不显示证书确认窗口，也不会自动信任未知证书。

Windows 安装包把 AI Controller Token 和统一 Owner UUID 分别写入当前用户环境变量 `REMOTEOPS_CONTROLLER_TOKEN`、`REMOTEOPS_CONTROLLER_OWNER_ID`。macOS 安装包使用 Keychain 保存 Token，并把非敏感 Owner 写入本地 JSON。Human 与 AI Controller 必须使用同一个 Owner。不要把 Token 写入 `args`、仓库、文档或聊天。安装后需要完全退出并重新启动 Codex。

Windows 配置使用 STDIO MCP 的 `env_vars` 转发已有用户环境变量，避免把 Token 明文复制到 `config.toml`。macOS 不依赖 Finder 是否继承终端环境变量，而是通过 Keychain 启动脚本只向 MCP 子进程提供 Token。

不建议把临时配对码长期写在 `args` 中。MCP 启动后调用 `pair_connection` 即可新增连接，不需要修改配置并重启 Codex。

全新 Agent 首次运行只需填写 Relay 地址并等待九位控制码，不需要在 MCP 中申请入网码，也不需要部署级 Agent 注册 Token。MCP 仍必须使用已认证的 AI Controller Token；Agent 控制码只用于把当前 Agent 会话绑定到该 Controller。

安装包同时将 `remoteops` skill 安装到当前 Codex 使用的标准目录 `~/.agents/skills/remoteops`，并同步写入兼容目录 `~/.codex/skills/remoteops`。它负责把“RemoteOps 控制码 + 诊断目标”这样的自然语言请求路由到 MCP，并在当前任务未加载 RemoteOps 工具时明确要求刷新，而不是误用远程桌面或本机命令。安装或更新后必须完全退出并重新打开 Codex，再新建任务使 MCP 工具和 skill 同时生效。

同一 MCP 进程已经成功配对后，即使 Relay 长时间停机，连接恢复时也会自动重试原配对；Controller 先恢复而 Agent 后恢复时，重试会持续到 Agent 在线。Agent 使用有效恢复令牌重新认证后保留原控制码和 `session_id`，不需要另一端再次确认。Codex 思考或本地开发期间没有工具调用，不会因为普通业务空闲而永久失去该配对。

自动恢复记录当前只保存在 MCP 进程内存中。完全退出 Codex、MCP 进程异常退出或显式调用 `close_connection` 后，需要重新调用 `pair_connection`。`close_connection` 只有在 Agent 完成资源清理且 Relay 确认释放当前 Controller 绑定后才返回成功。同一 Owner、同一角色的新 MCP 使用有效控制码配对时会接管旧绑定；旧 MCP 会删除本地连接和自动恢复记录，不会反向抢回。Relay 断开时未完成请求会失败，审批和在途写操作不会自动重放；恢复后应先调用 `list_connections` 和 `get_target_info` 核对目标状态，再重新发起必要操作。

同一预览版本反复更新时，Windows 可能仍锁定正在运行的旧 MCP。安装器会改用带 12 位构建哈希的旁路文件名并更新 Codex 配置，不会强制结束当前 Codex 或 MCP 进程；完全重启 Codex 后新任务使用新版程序，再次运行安装器可清理旧文件。

`upload_file` 和 `download_file` 只能使用 `transfer-root` 内的相对路径。绝对路径、`..` 跳转以及通过符号链接逃逸目录都会被拒绝。MCP 与 CLI 使用 1 MiB 分块，单文件硬上限为 16 GiB；超过 1 GiB 时必须在读取、哈希或发送前单独确认。上传和下载覆盖均先写同目录临时文件，校验每块及完整 SHA-256 后再可恢复地原子提交，失败时保留原文件。

## 四、独立 Human Controller（预留兼容）

普通 MCP 首版的逐项确认由 MCP 直接向当前用户发起，不要求另外启动 Human Controller。原有独立 Human Controller、`request_action_approval` 和一次性 `approval_id` 不删除，供未来 Controller GUI、多观察者、人工接管和明确使用 `--command-mode approval` 的部署继续使用。

```powershell
$env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN = '<仅保存在本机安全环境中的 Human Token>'
$env:REMOTEOPS_CONTROLLER_OWNER_ID = '<与 AI MCP 相同的 Owner UUID>'
```

人工可以使用 GUI 查看审批，也可以使用 CLI：

```powershell
.\remoteops-controller-cli.exe `
  --relay relay.example.com:7443 `
  --server-name relay.example.com `
  --ca-cert D:\RemoteOps\relay-cert.pem `
  --pair 123-456-789=测试环境 `
  events '<session_id>'

.\remoteops-controller-cli.exe `
  --relay relay.example.com:7443 `
  --server-name relay.example.com `
  --ca-cert D:\RemoteOps\relay-cert.pem `
  approval-decide '<session_id>' '<approval_id>' '<MCP 返回的完整 operation JSON>'
```

配对码、Token、恢复令牌和真实审批标识不得写入文档或聊天。

## 五、当前工具

- `pair_connection`
- `list_connections`
- `get_control_mode`、`set_control_mode`
- `get_target_info`
- `open_shell`
- `run_readonly_command`
- `run_command`
- `close_shell`
- `test_port`
- `tcp_exchange`
- `get_file_metadata`、`move_file`、`delete_file`
- `list_processes`、`terminate_process`
- `list_services`、`control_service`
- `power_control`
- `list_serial_ports`、`write_serial`、`run_serial_query`、`close_serial`
- `run_ssh`
- `open_serial`
- `read_output`
- `upload_file`
- `download_file`
- `request_action_approval`
- `close_connection`

## 六、Codex 操作顺序

```text
先调用 list_connections。
每个远程工具必须使用它返回的不可变 session_id。
调用 get_target_info，确认目标主机和 power_shell 能力。
只读诊断使用一次性 Shell 的 run_readonly_command。
需要保持目录、变量或模块状态时，以 power_shell 打开持久 Shell；持久 Shell 的所有命令都通过 run_command，并接受逐项确认或完全控制约束。
配对后保持默认逐项确认；仅在用户明确要求时开启完全控制。
逐项确认下调用修改工具时，MCP 会向当前用户确认本次操作；拒绝、关闭或超时后停止。
完全控制下无需逐项确认，但只对当前 session_id 生效，空闲一小时后恢复逐项确认。
用户在聊天中要求切换时调用 set_control_mode；Codex 对该工具的授权是唯一确认，不再嵌套 MCP 交互确认。
通过 read_output 查看人工、AI、系统和远端输出。
```

同一 Agent Session 只允许一个 `ControllerOwnerId`。Human Controller 和 AI Controller 可以共享这个 Owner 并分工协作，但 Human 和 AI 不能被配置成两个不同 Owner 后同时控制同一会话。

底层 `ControllerApproved` 表示“人机确认由已认证本地 MCP 完成”，只允许 AI Controller 使用。Relay 和 Agent 不判断自然语言意图，但仍验证 TLS 身份、Owner、`session_id`、Controller 绑定、结构化操作、能力、路径边界、负载限制和审计声明。它不是跳过底层安全校验的通用 `FullAccess`。

PowerShell 7 的 MCP 枚举值为 `power_shell`；Windows PowerShell 5.1 为 `windows_power_shell`。远端没有安装 `pwsh.exe` 时不会上报 `power_shell` 能力。

## 七、验证步骤

1. 调用 `pair_connection`，输入 Agent 窗口显示的临时控制码和别名；
2. 调用 `list_connections`，记录准确 `session_id`；
3. 调用 `get_target_info`，核对主机名和 `power_shell` 能力；
4. 使用一次性 `power_shell` 调用 `run_readonly_command` 验证只读诊断；
5. 调用 `open_shell` 后，使用返回的 `shell_id` 调用 `run_command`，并确认持久 Shell 命令进入逐项确认；
6. 保持逐项确认，调用一条修改命令，确认 Codex 显示 MCP 交互并在拒绝后不执行；
7. 再次调用并批准本次操作，确认命令成功；
8. 调用 `set_control_mode(full_access)` 并明确确认，核对 `get_control_mode`；
9. 对 Agent A 开启完全控制，确认 Agent B 仍为逐项确认；
10. 成功执行操作后确认 TTL 续期，模拟空闲过期后确认恢复逐项确认；
11. 切回逐项确认，确认新写操作再次询问；
12. 调用 `read_output` 检查流式输出和审计事件；
13. 调用 `close_connection`。

自动化验证入口：

```powershell
.\scripts\Test-LocalE2E.ps1
.\scripts\Invoke-LabE2E.ps1 -SkipBuild
```

本机 STDIO MCP、官方 SDK Smoke 和三机链路的历史结果见[第一阶段验收报告](第一阶段验收报告.md)。旧版本 Linux Docker Relay 记录仅作为历史证据；当前 Technical Preview 使用 `0.2.0-preview.4`，公网来源 IP 白名单和真实远端 PowerShell 7 仍需按[远程 Pwsh 链路部署测试](远程Pwsh链路部署测试.md)完成现场验收。

