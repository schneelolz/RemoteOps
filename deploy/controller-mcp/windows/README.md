# RemoteOps MCP Windows x64 安装说明

> 当前安装规则：新安装只支持设置码/设置文件，不再支持 Token 安装；已有安装使用 `-UpgradeExisting` 保留身份和凭据。Relay 后台「允许旧 Token 客户端接入」默认开启，已安装的旧 MCP 可继续使用原 Token；关闭时断开旧连接并拒绝重连，独立凭据及 Human 不受影响。旧 Token 配置说明仅供已有安装排查，不能用于新版安装；新安装见[一次性安装配置](../../../docs/mcp-one-time-setup.md)。

- 版本：`0.2.0-preview.30`
- 适用系统：Windows x64
- Codex MCP 名称：`remoteops`
- 许可证：`AGPL-3.0-only`

本安装包把 Codex 接入你自行部署或获准使用的 RemoteOps Relay。安装包不包含默认 Relay、Token、服务器密码、控制码、私钥或证书。

## MCP 与 Skill 分开安装和更新

自本版起，MCP 与 Skill 使用独立版本和独立压缩包：

| 组件 | 当前版本 | 内容与更新范围 |
| --- | --- | --- |
| MCP | `0.2.0-preview.30` | MCP EXE、密码辅助程序 `.7`、安装/自检脚本及许可证；不含 Skill |
| Skill | `0.1.9` | 操作流程、交换机 SSH 排障和工具中心指引；最低 MCP 为 `.28`，协议 20 |

首次部署建议下载完整安装包，或分别解压 `RemoteOps-MCP-Windows-x64-0.2.0-preview.30.zip`
与 `RemoteOps-Skill-0.1.9.zip`。先按本文安装 MCP，再进入 Skill 包执行其独立安装器。
完整包中的两个目录为 `MCP` 与 `Skill`；在 `MCP` 目录完成安装后可执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File ..\Skill\Install-RemoteOpsSkill.ps1
```

使用自定义 `-CodexHome` 时，两个安装器必须使用同一目标目录。pi 的 Skill 加载路径按
独立 Skill 包文档配置，MCP 安装器不会改写 pi 配置。组件兼容范围见 Skill 的
`skills/remoteops/skill.json`；升级 Skill 前核对最低 MCP 版本及所需工具。完整工具中心 Schema 和字段级本地诊断需要 MCP `.22`。

仅更新使用指引时，只下载安装 Skill 包，无需更新 EXE、连接配置、凭据、Agent 或
Relay；宿主何时重新读取 Skill 取决于其加载机制，新会话或重载后核对已加载内容。
MCP 单独安装、自检或卸载不要求存在 Skill，也不会覆盖更新版本或本地修改的 Skill。
卸载 Skill 使用其独立卸载器。MCP 程序升级仍需重开 Codex，pi 重开或 `/reload`。

## 升级已有安装

已有可用安装不需要重新签发设置码。使用同一 Windows 用户运行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-RemoteOpsMcp.ps1 -UpgradeExisting
```

升级使用 MCP `0.2.0-preview.30` 和密码安全输入程序 `0.2.0-preview.7`，安装到
`<CodexHome>/remoteops/candidates/protocol20-preview30-<程序摘要>-<包内容摘要>`。
两个摘要均取 SHA-256 前 12 位，准确路径由安装器输出；同一 EXE 的配套脚本或文档变化
也会生成独立目录。两个程序、安装/自检脚本和随包许可并排保留；独立 Skill 不复制、
不覆盖，现有 Codex/pi 进程及旧文件不受影响。
升级不兑换设置、不登记新身份、不更改 `controller-config.json` 或 Windows Credential
Manager，仅通过 TOML 解析器更新现有 `mcp_servers.remoteops.command`，原参数、
认证配置、命令模式、审批、超时、其他 MCP 和全局设置保持。变更前由程序自动备份
Codex 配置。`-UpgradeExisting` 不能与设置输入、`-ConfirmEnrollment` 或 `-CommandMode`
混用；凭据缺失时应先排查原安装，不能用重新登记作为默认修复。

升级后按“旁路安装检查”提供此目录及构建记录中的两个 SHA-256。完全重开 Codex 后
生效；pi 共用此程序时，需要维护者另外结构化修改 pi 的 `remoteops` 程序路径，保留
认证、命令模式与超时，然后重开 pi 或执行 `/reload`。单独 reconnect 不会重载 pi 配置。

## 使用一次性设置安装

请管理员在 Relay 管理界面签发短时、单次使用的 MCP 设置文件或设置码。文件包含
Relay 地址、登记 URL、TLS 服务名、必要的 CA 信任和一次性登记密钥；它不是通用
Controller Token，也不是现场 Agent 的九位控制码。请通过可信渠道核对登记目标，
不要把设置文件内容、设置码或长期 Token 发到 Codex 对话、截图、日志或命令行。

在解压目录打开 PowerShell：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-RemoteOpsMcp.ps1 `
  -SetupFile "$env:USERPROFILE\Downloads\remoteops-setup.json"
```

安装器会显示不含密钥的 Relay、登记 URL、TLS 服务名、有效期和私有 CA 使用情况，
输入 `yes` 后才登记。只有设置码时，使用隐藏输入提示，不要把码接在参数后：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-RemoteOpsMcp.ps1 -SetupCode
```

由 AI 协助安装时，只给它本机文件路径和经过你确认的登记目标。先查看安全预览，
确认目标后才能使用 `-ConfirmEnrollment` 非交互安装：

```powershell
.\remoteops-controller-mcp.exe --setup-file "$env:USERPROFILE\Downloads\remoteops-setup.json" --setup-preview
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-RemoteOpsMcp.ps1 `
  -SetupFile "$env:USERPROFILE\Downloads\remoteops-setup.json" -ConfirmEnrollment
```

自动化也可使用 `-SetupStdin -ConfirmEnrollment`，通过进程标准输入传入 JSON
或设置码，不要把码写进 PowerShell 命令行或历史。`-ConfirmEnrollment` 只跳过交互
确认，不会关闭 HTTPS 或 TLS 校验，也不会跟随登记 HTTP 重定向。

每个安装生成独立 AI Controller 凭据，直接保存在当前用户的 Windows Credential
Manager。`controller-config.json` 只保存非秘密连接信息、Owner 和 `credential_id`；
运行时直接读取凭据存储，不需要转发或改写旧 Token/Owner 环境变量。登记成功后
会验证 Relay 身份与认证响应，验证通过才写入 Codex MCP 注册并报告安装成功。
安装器只替换 `mcp_servers.remoteops` 配置，保留其他 MCP、模型和全局审批设置。

如果网络中断或自检失败，保留 `%USERPROFILE%\.codex\remoteops\setup-state.json`、
Credential Manager 条目和原设置文件，再运行相同安装命令。此非秘密检查点用于
同一安装的幂等重试；不要先删凭据或复制检查点到另一台机器。已由其他安装使用、
过期或被撤销的设置不能新登记，请管理员重新签发。成功后妥善删除下载的设置文件；
不要把它放进安装包或 Git。`-CodexHome` 和 `CODEX_HOME` 可指定其他安装目录。

## 安装位置

安装内容：

```text
%USERPROFILE%\.codex\remoteops\remoteops-controller-mcp-0.2.0-preview.30.exe
%USERPROFILE%\.codex\remoteops\remoteops-credential-prompt.exe
%USERPROFILE%\.codex\remoteops\controller-config.json
%USERPROFILE%\.codex\remoteops\setup-state.json
%USERPROFILE%\.codex\config.toml
```

覆盖安装成功后会删除 `%USERPROFILE%\.codex\remoteops` 中旧版本的
`remoteops-controller-mcp-*.exe`，并清理已被新配置替代的旧 `relay-cert.pem` 副本；
不会删除审计日志、文件交换目录或当前仍在使用的 CA 文件。旧 MCP 正被 Codex
占用时，安装器会按新程序 SHA-256 的前 12 位生成旁路文件名并更新 `config.toml`；
安装仍会成功，完全退出 Codex 后再次运行安装器即可清理旧文件。

以下优先级适用于旧手工 Token 模式。一次性设置的 `credential_id` 将连接与本机凭据绑定，
不会被遗留 Token/Owner 环境变量替代。手工模式配置优先级为：

```text
命令行参数 > 环境变量 > controller-config.json
```

## 验证

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Test-RemoteOpsMcp.ps1
codex mcp list
```

在有交互桌面的 Windows 会话中，可额外运行可取消的凭据窗口烟测；脚本会启动窗口，
请在 60 秒内点击“取消／Cancel”：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Test-RemoteOpsMcp.ps1 -CredentialPromptSmokeTest
```

### 旁路安装检查

标准安装仍使用上面的默认命令。若维护者将完整 MCP 包放在
`<CodexHome>/remoteops/candidates/<构建目录>`，可显式选择该目录，并提供维护者已核验的
MCP 和凭据输入程序 SHA-256。两项哈希必须来自可信构建记录；不能仅对当前待检查文件
临时计算哈希，就把它当作可信来源。

```powershell
$codexHome = Join-Path $env:USERPROFILE '.codex'
$candidate = Join-Path $codexHome 'remoteops\candidates\approved-build'
# 将目录和下面两项占位值替换为维护者提供的实际值。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Test-RemoteOpsMcp.ps1 `
  -CodexHome $codexHome -CandidateDirectory $candidate `
  -ExpectedMcpSha256 '<可信构建记录中的 MCP SHA-256>' `
  -ExpectedCredentialPromptSha256 '<可信构建记录中的凭据输入程序 SHA-256>'
```

目录必须是 `candidates` 的直接子目录，不能通过符号链接或目录联接跳转；Codex 配置
也必须准确指向该目录的 `remoteops-controller-mcp.exe`。两个程序的哈希验证成功后才会
执行检查，并从同目录检查 `remoteops-credential-prompt.exe`。连接配置仍使用
`<CodexHome>/remoteops/controller-config.json`，不会复制或修改凭据、配置或运行中的进程。
`CodexHome` 和候选目录应使用一致的路径别名；系统自有的 Codex 用户目录联接不受影响。

若同目录存在 `manifest.json`，自检还会验证其组件、版本、协议、文件大小及摘要，
且清单必须包含两个 EXE；清单本身不能代替外部可信构建摘要。

该功能只检查本地安装及固定包版本，不查询或下载最新发布版本；缺少候选目录或任一
可信哈希时不能通过旁路检查。`-SkipNetwork` 只跳过 TCP 探测，不跳过路径、哈希和版本校验。

安装检查会根据 `credential_id` 读取安全存储中的凭据，不把凭据返回到脚本；旧模式仍
检查用户环境变量。网络检查只证明 TCP 可达，不等同于重新认证；首次安装认证自检
由设置程序完成。安装器不会改写全局 `approval_policy`；如果你的 Codex 策略禁止
MCP elicitation，需按组织要求自行调整。

安装或更新 MCP 后必须完全退出并重新打开 Codex。随后输入 `/mcp`，应看到已启用的 `remoteops`。

新建任务后无需记忆工具名，可以直接说：

```text
这是 RemoteOps 控制码 123-456-789。请检查现场电脑的网络和指定进程，只进行只读诊断。
```

Codex 应自动通过 RemoteOps 配对并使用远程工具。若当前任务没有加载 RemoteOps MCP，skill 会要求刷新 Codex，不会改用远程桌面或本机命令假装完成远程检查。

`-CommandMode agent-controlled` 是推荐默认模式：未获有效授权时逐项确认。MCP 可通过 `set_control_mode` 授权，也可由现场在 Agent“控制权限”中授权；Relay 确认后无需重复 RemoteOps 审批。MCP 授权空闲一小时到期，现场授权到连接结束或撤销失效。Agent 默认 `default_full_control=false`；默认完全控制为新绑定自动授权，现场切回逐项确认会抑制本次运行的自动授权。`readonly`、`approval` 仍是权限上限，协议 16 的 `full-access` 不会自动授权。详见[控制权限说明](../../../docs/control-permissions.md)。

全新 Agent 首次运行只需填写 Relay 地址并等待九位控制码，不需要让 Codex 生成入网码，也不需要部署级 Agent 注册 Token。新版 MCP 使用设置兑换的独立凭据完成自身认证，再使用 Agent 控制码配对。

## 工具中心管理

工具中心需要协议 20 Relay、Agent 和本 MCP。Relay 管理员须单独为当前安装授予
`tool_management`；完全控制或旧共享 Token 不能代替这项权限。生产升级只授予当前
共用身份管理权时，不应自动为其他安装授权、上传现有工具包或开启工具运行。

- `manage_relay_tools` 的 `snapshot` 用于读取目录、策略和修订；每次管理请求使用稳定
  `request_id` 和当前 `expected_revision`。修订冲突后重新读取，未知结果不自动重放。
- `upload_relay_tool_package` 只接受本机 transfer-root 内的 ZIP 相对路径；先实际测试并
  整理用途、说明、示例及执行定义，再上传或导入并登记真实环境与结果。
- 上传回执仅证明 ZIP 安全检查和摘要一致。上传、登记、平台测试、单工具启用及全局
  运行开关彼此独立；全局及新工具默认关闭，必须按用户明确指令启用。
- Agent 工具由已连接中继统一管理；旧本地资源站配置不再是运行入口。目录同步只能
  按实际确认状态报告，失联、停用或无法确认的在途结果按 `result_unknown` 处理。

## 第一次只读测试

现场 Agent 显示控制码后，让 Codex：

```text
使用 RemoteOps 配对现场客户机，然后列出连接并查看 IPv4 地址、默认网关和 DNS。只执行只读命令。
```

后续操作必须使用 `list_connections` 返回的准确 `session_id`，不能根据主机名或别名猜测目标。

## 安全边界

- `run_readonly_command` 只使用一次性 Shell 执行只读诊断；持久 Shell 的所有命令必须走 `run_command`；
- 逐项确认时，`run_command` 由当前 Codex 的 MCP 授权弹窗确认；
- 完全控制可由 MCP 用户或 Agent 现场人员明确授予，Relay 确认后生效；仅免除 RemoteOps 内部逐项审批，不覆盖 Codex 宿主工具授权；
- SSH 密码不得写入 Codex 对话或 MCP 参数；`run_ssh` 设置 `use_password=true` 后，由本机安全窗口直接输入并用 Agent HPKE 公钥加密；
- 安全窗口可由用户显式选择在 MCP 内存中记住 10 分钟，默认仅本次使用；缓存不写磁盘，可用 `clear_ssh_credential_cache` 清除；
- 不要把 Token、Owner UUID、证书私钥、客户地址或控制码写入聊天、日志、截图和 Git 仓库；
- 不要通过关闭 Defender、SmartScreen 或企业安全策略绕过来源检查。

源码、问题反馈和许可证信息以项目 GitHub 仓库为准。
