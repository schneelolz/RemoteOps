# RemoteOps MCP 使用手册

> 文档状态：当前 Codex 操作手册。底层配置原理见[Codex MCP 接入说明](CodexMCP接入说明.md)，不要从历史验收报告复制安装参数。

- 更新日期：2026-08-16
- 发布状态：`0.2.0-preview.1` Technical Preview
- MCP 版本：`0.2.0-preview.1`
- 现场 Agent 版本：`0.2.0-preview.1`
- Relay：由部署者配置

## 一、推荐安装方式

Windows x64 安装包：

```text
artifacts\release\0.2.0-preview.1\mcp\RemoteOps-MCP-Windows-x64-0.2.0-preview.1.zip
```

解压后运行 `Install-RemoteOpsMcp.ps1`。安装后的默认程序和配置位置为：

```text
%USERPROFILE%\.codex\remoteops\remoteops-controller-mcp-0.2.0-preview.1.exe
%USERPROFILE%\.codex\remoteops\controller-config.json
%USERPROFILE%\.codex\config.toml
```

安装时必须提供自行部署或组织批准的自托管 Relay 地址和该 Relay 配置的统一 Owner UUID。MCP 不内置、不依赖共享中继服务；公网 CA 使用 Windows 系统可信根，不需要 `relay-cert.pem`。配置名称为 `remoteops`，AI Controller Token 与统一 Owner 分别由 Windows 当前用户环境变量 `REMOTEOPS_CONTROLLER_TOKEN`、`REMOTEOPS_CONTROLLER_OWNER_ID` 提供，不会明文写入 `config.toml` 或 JSON 配置。

安装脚本只替换 RemoteOps MCP 配置；已有 `config.toml` 会先备份为：

```text
%USERPROFILE%\.codex\config.toml.remoteops-backup-<时间戳>
```

Apple Silicon Mac 使用 `RemoteOps-MCP-macOS-arm64-0.2.0-preview.1.tar.gz`。安装器把 Token 保存到 macOS Keychain，不写入 Codex 配置；安装、检测和卸载命令见 [macOS MCP 接入说明](macOSMCP接入说明.md)。Windows 或 Mac 上运行的 MCP 都可以通过 Relay 控制现有 Windows Agent。

## 二、为什么当前任务还看不到 MCP

Codex 在应用启动时读取 MCP 配置，不会在已经运行的任务中自动热加载新 MCP。安装完成后需要：

1. 完全退出 Codex，包括托盘进程；
2. 重新打开 Codex；
3. 建议新建任务；
4. 输入 `/mcp`；
5. 确认存在并已连接 `remoteops`。

Codex 桌面端、CLI 和 IDE 扩展共享 MCP 配置。官方说明：[Model Context Protocol](https://learn.chatgpt.com/docs/extend/mcp.md)。

## 三、现场 Agent

现场客户机双击 `remoteops-agent-gui.exe`。没有配置时会自动显示首次设置页，默认保存到：

```text
<remoteops-agent-gui.exe 所在目录>\agent-config.json
```

程序目录不可写时回退到 `%LOCALAPPDATA%\RemoteOps\agent-config.json`，旧版路径继续兼容。

Agent 不内置共享中继服务，必须连接部署者提供的自托管 Relay。使用公网 CA 时不需要 `relay-cert.pem`、启动参数或入站端口；私有 CA 可设置 `ca_cert`。没有 PEM 的未知自签名证书会在 GUI 中显示 SHA-256 指纹，由现场用户取消、仅本次继续或核对后信任并保存。

GUI 窗口必须保持运行。窗口会显示连接状态、临时控制码、当前控制方和本机能力；关闭窗口并确认后会终止本次远程入口。控制码只用于配对，不要写入配置、文档或长期记录。

全新机器首次运行只需填写 Relay 地址，不需要先启动 Codex、申请入网码或获取部署级 Agent 注册 Token。Agent 显示九位控制码后，再由已安装并已认证的 RemoteOps MCP 完成配对。

MCP 配对后默认使用“逐项确认”，不弹出控制方式选择：只读检查直接执行，修改、终止进程、服务控制、重启、文件变更、可写串口等操作前由 MCP 向当前用户确认。用户明确要求时可调用一次 `set_control_mode` 为单个 `session_id` 开启“完全控制”；Codex 对该工具的授权是唯一确认，不再嵌套弹出第二次确认。Agent 端没有逐项确认或完全控制按钮。授权只保存在 MCP 内存，空闲一小时自动失效，成功操作后滑动续期。Agent 短暂掉线并以原会话恢复时保留，MCP/Codex 重启、Agent 重启生成新会话、主动断开或切回逐项确认后失效。

`remoteops-agent.exe` 继续保留，供命令行自动化和故障排查使用，不作为普通现场人员的默认入口。

Agent 状态和文件传输目录默认位于：

```text
%LOCALAPPDATA%\RemoteOps
```

不要公开或随意删除 `agent-state.json`，其中包含 Agent 身份和恢复信息。

Relay 临时中断或长时间停机时保持 Agent 运行即可。只要 `agent-state.json` 中的恢复令牌仍有效，Relay 恢复后 Agent 会自动重新认证并保留原控制码和 `session_id`；同一 MCP 进程会持续重试已知配对，不需要现场再次确认。断线前审批和在途写操作不会自动重放。

## 四、在 Codex 中配对

重启 Codex 后输入：

```text
使用 RemoteOps 配对现场客户机，控制码是 Agent 窗口当前显示的控制码，别名设置为“现场客户机”。
```

然后输入：

```text
使用 RemoteOps 列出远程连接，并告诉我主机名、操作系统和可用 Shell。
```

Codex 会依次调用：

```text
pair_connection
list_connections
get_target_info
```

后续操作使用 `list_connections` 返回的准确 `session_id`，不要根据主机名猜测目标。

## 五、查看 IP 地址

直接对 Codex 说：

```text
使用 RemoteOps 查看现场客户机的 IPv4 地址、默认网关和 DNS，只执行只读命令。
```

RemoteOps 会执行类似命令：

```powershell
Get-NetIPConfiguration
Get-NetIPAddress -AddressFamily IPv4
```

以下为使用 RFC 5737 文档网段整理的脱敏示例，字段格式与实际只读查询一致：

| 项目 | 结果 |
|---|---|
| 主机名 | `LAB-AGENT-A` |
| IPv4 | `192.0.2.106/24` |
| 默认网关 | `192.0.2.1` |
| DNS | `192.0.2.53`、`198.51.100.53` |
| 可用 Shell | Windows PowerShell |

历史验收中曾出现未上报 PowerShell 7 `power_shell` 能力的环境；后续自托管 Relay 验收已成功选择 `power_shell`，并通过 PowerShell 7 执行主机名和 IPv4 只读查询。公开文档不保留真实主机名、地址或 DNS 配置。

## 六、常用请求示例

### 系统信息

```text
使用 RemoteOps 查看现场客户机的系统版本、主机名、启动时间和当前登录用户，只执行只读命令。
```

### 进程和服务

```text
使用 RemoteOps 查看 CPU 或内存占用较高的进程，只读，不结束进程。
```

```text
使用 RemoteOps 查看状态异常或已经停止的自动启动服务，只读，不启动或停止服务。
```

### 网络和文件

```text
使用 RemoteOps 在现场客户机测试目标地址和 TCP 端口，只读，不修改网络配置。
```

```text
使用 RemoteOps 查看指定目录的文件列表、大小和修改时间，只读，不修改文件。
```

## 七、命令执行和审批

当前 MCP 默认使用：

```text
--command-mode agent-controlled
```

安装器设置 `default_tools_approval_mode = "approve"`、`set_control_mode.approval_mode = "prompt"` 和当前 Codex 支持的内联 `approval_policy = { granular = { ... mcp_elicitations = true ... } }`。普通写操作由 MCP 逐项确认；开启完全控制只使用 `set_control_mode` 的 Codex 工具确认，避免静态工具审批与嵌套 MCP 确认重复弹窗。

规则如下：

- 结构化只读命令通过 `run_readonly_command` 直接执行；
- 未知命令、写入命令和修改系统状态的命令不能伪装成只读；
- 持久 Shell 保留目录、变量、函数和模块状态，不能声明为免确认只读；其中所有命令都通过 `run_command`，并接受逐项确认或完全控制约束；
- 逐项确认模式下，MCP 通过 MCP elicitation 向当前用户确认本次完整操作；拒绝、关闭或超时后不执行；
- 完全控制模式下，MCP 写工具无需逐项询问，但仍受 TLS、单 Owner、目标绑定、结构化策略、审计和停止边界约束；
- 切回逐项确认或 TTL 过期后，新写操作立即再次询问；
- `request_action_approval` 仅用于显式 `--command-mode approval` 或独立 Human Controller 兼容流程，普通首版 MCP 不依赖它；
- MCP 不能通过自然语言提升 Agent 本地 FullAccess，也不能绕过 Relay/Agent 的结构化校验。

可以对 Codex 说：

```text
使用 RemoteOps 执行这条命令。如果它不是只读命令，先通过 MCP 向我确认本次操作，不要绕过确认。
```

如果 MCP 客户端不支持 elicitation，修改操作会被安全拒绝；不要改用本机 Shell、SSH 或其他远控工具绕过。

### 大文件传输

`upload_file` 和 `download_file` 只允许访问双方各自 `transfer-root` 内的受控路径。MCP 与 CLI 默认按 1 MiB 分块传输，单文件硬上限为 16 GiB。文件大于 1 GiB 时，MCP 会在读取本地文件、计算完整哈希或开始远程下载之前单独请求确认；CLI 上传使用 `--approve`，下载使用 `--approve-large`。

上传开始时会声明目标路径、大小、完整 SHA-256 和覆盖标志，每个分块再校验 SHA-256，并按连续偏移写入同目录临时文件。只有大小和完整哈希均匹配才原子提交；断线、取消、解绑或紧急停止会清理未完成临时上传。下载也先写同目录临时文件并验证完整哈希，覆盖失败时恢复或保留原文件，不会先删除目标文件。嵌套目标目录会在受控根目录内按需创建，绝对路径、`..`、符号链接逃逸和目录覆盖都会被拒绝。

## 八、主要 MCP 工具

- `pair_connection`：配对 Agent；
- `list_connections`：列出连接；
- `get_control_mode`、`set_control_mode`：查询或切换单个 Agent 的 MCP 控制模式；
- `get_target_info`：读取主机和能力；
- `run_readonly_command`：使用一次性 Shell 执行只读命令；
- `open_shell`：打开持久 Shell，后续命令必须使用 `run_command`；
- `run_command`：在逐项确认或完全控制模式下执行非只读命令；
- `close_shell`：显式关闭持久 Shell 并释放远端句柄；Shell 内执行 `exit` 后也会自动清理；
- `request_action_approval`：独立 Human Controller 的兼容审批入口；
- `read_output`：读取远端输出；
- `test_port`：从远端探测 TCP 端口；
- `tcp_exchange`：向调用者明确指定的单个目标执行有界 TCP 收发；
- `get_file_metadata`、`move_file`、`delete_file`：在 `transfer-root` 内查看或修改文件；
- `list_processes`、`terminate_process`：查看进程或终止指定进程树；
- `list_services`、`control_service`：查看或启动、停止、重启 Windows Service；
- `power_control`：重启或关机；
- `list_serial_ports`、`write_serial`、`run_serial_query`、`close_serial`：受控串口操作；
- `run_ssh`：从 Agent 发起明确目标的 SSH 命令；网络设备只读白名单免确认，其他命令在只读模式下拒绝、默认模式下逐项确认、完全控制下执行；
- `upload_file`、`download_file`：受控分块文件交换，支持 16 GiB 单文件上限、超过 1 GiB 单独确认和可恢复原子提交；
- `close_connection`：关闭连接。

上述工具按只读、修改和高风险分类执行统一策略。逐项确认模式下，修改及高风险工具必须先通过 MCP elicitation 获得当前用户确认；完全控制仍受 Agent、Relay 的结构化安全边界和 Owner 绑定约束。

## 九、Agent 环境和语言

Agent 首次连接后会上报脱敏环境画像，包括 Windows 版本摘要、架构、提升权限状态、CMD、Windows PowerShell、PowerShell 7、系统 Shell、SSH 和协议版本。不会上报用户名、环境变量内容、安装路径或完整可执行文件路径。

两个 GUI 内置 `zh-CN` 和 `en-US`，可以使用 GUI 右上角/侧栏切换并保存；也支持 `--lang`、`REMOTEOPS_LANG`、Windows 系统语言和 `REMOTEOPS_LANG_FILE` 外部 JSON 覆盖。语言包只定义文本和占位符，不定义命令或权限。

## 十、故障排查

### `/mcp` 没有 `remoteops`

完全退出并重启 Codex。也可以检查：

```powershell
codex mcp list
```

应看到 `remoteops` 状态为 `enabled`。

### MCP 启动提示配置不完整

MCP 不弹配置窗口。错误会列出实际读取的 `controller-config.json` 路径，以及缺失的 Relay、Token 或 Owner。Windows 检查用户环境变量；macOS 重新运行安装器并检查 Keychain。补齐后必须完全退出并重新打开 Codex。

公网 CA 自动使用系统可信根。Agent GUI 可以在首次连接未知自签名 Relay 时展示证书指纹并由现场用户确认；MCP 是无界面服务，仍必须在 `controller-config.json` 中配置 `ca_cert`，或使用安装器 `-TlsFingerprint` 写入已经独立核对的 `tls_fingerprint`，不会自动信任未知证书。

### MCP 提示缺少 Token

只检查用户环境变量是否存在，不要打印内容：

```powershell
[Environment]::GetEnvironmentVariable(
    'REMOTEOPS_CONTROLLER_TOKEN',
    'User'
) -ne $null
```

设置环境变量后必须重启 Codex。

### Agent 无法连接

在现场客户机检查：

```powershell
Test-NetConnection relay.example.com -Port 7443
```

现场只需允许主动出站 TCP `7443`，不需要开放入站端口。

### 配对失败

确认 Agent 窗口仍在运行并显示 `Relay 已连接`，使用窗口当前显示的完整控制码。Relay 或 Agent 正在恢复时可以等待自动重试，不要仅因 Relay 曾离线就删除 `agent-state.json`。完全退出 Codex、MCP 进程退出或已经调用 `close_connection` 后，自动恢复记录不再存在，需要重新调用 `pair_connection`。

### 不支持 `power_shell`

表示运行 Agent 的账户找不到 `pwsh.exe`。安装 PowerShell 7、加入 `PATH` 并重启 Agent；未安装时可使用 `windows_power_shell`。

## 十一、安全要求

- 不长期保存控制码；
- 不输出或复制 Controller Token；
- 不公开 `agent-state.json`；
- Agent 使用低权限账户运行；
- 只读诊断优先使用 `run_readonly_command`；
- 修改操作默认逐项确认；仅在使用者通过 MCP 明确为当前 `session_id` 开启完全控制后免除逐项确认；
- 完成协助后关闭 Agent即可停止远程入口；
- 试点稳定后收紧 Relay `7443` 的安全组来源范围。

## 十二、当前结论

截至 2026-08-06：

- RemoteOps MCP 已安装并被 Codex CLI 识别；
- Codex 已通过该 MCP 配对真实运行的 GUI Agent；
- Codex CLI → MCP → 公网可达的自托管 Relay → Agent 共享运行时 → PowerShell 7 只读查询通过；
- IP、网关和 DNS 已成功读取；
- 当前 Codex App 任务需要在应用重启后才会加载新 MCP；
- 未安装 PowerShell 7 的现场机器仍可使用 `windows_power_shell`；需要 `pwsh` 时，应先安装 PowerShell 7 并重启 Agent。

