# RemoteOps MCP Windows x64 安装说明

- 版本：`0.2.0-preview.13`
- 适用系统：Windows x64
- Codex MCP 名称：`remoteops`
- 许可证：`AGPL-3.0-only`

本安装包把 Codex 接入你自行部署或获准使用的 RemoteOps Relay。安装包不包含默认 Relay、Token、服务器密码、控制码、私钥或证书。

## 推荐：一次性设置

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

## 兼容：手工安装前准备

你需要：

1. Relay 地址，例如 `relay.example.com:7443`；
2. Relay TLS 服务名，通常与地址中的主机名相同；
3. AI Controller Token，长度至少 32 个字符；
4. Relay 配置的统一 Controller Owner UUID，Human 与 AI 必须使用同一个值；
5. 私有 CA 可准备 CA PEM；没有 PEM 时，必须由管理员通过独立可信渠道核对 Relay 叶证书 SHA-256 指纹。

公网 CA 颁发的证书使用 Windows 系统可信根，不需要 `relay-cert.pem`。

## 兼容：手工安装

在解压目录打开 PowerShell：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-RemoteOpsMcp.ps1 `
  -RelayAddress 'relay.example.com:7443' `
  -OwnerId '<controller-owner-uuid>'
```

如果证书服务名与 Relay 主机名不同：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-RemoteOpsMcp.ps1 `
  -RelayAddress '203.0.113.10:7443' `
  -ServerName 'relay.example.com' `
  -OwnerId '<controller-owner-uuid>'
```

私有 CA 环境额外提供：

```powershell
-CaCert '.\my-relay-ca.pem'
```

没有 PEM、但已独立核对自签名证书指纹时，可以改用：

```powershell
-TlsFingerprint '<64 位 SHA-256 证书指纹>'
```

`-CaCert` 和 `-TlsFingerprint` 只能选择一种。MCP 不显示确认窗口，也不会自动信任未知证书。

脚本会隐藏提示输入 Token，并把 Token 和统一 Owner 分别保存为当前 Windows 用户环境变量 `REMOTEOPS_CONTROLLER_TOKEN`、`REMOTEOPS_CONTROLLER_OWNER_ID`。Token 和 Owner 不会写入 Codex `config.toml` 或 RemoteOps JSON 配置。

已由管理员安全设置 Token 时，可使用：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-RemoteOpsMcp.ps1 `
  -RelayAddress 'relay.example.com:7443' `
  -OwnerId '<controller-owner-uuid>' `
  -SkipTokenPrompt
```

安装内容：

```text
%USERPROFILE%\.codex\remoteops\remoteops-controller-mcp-0.2.0-preview.13.exe
%USERPROFILE%\.codex\remoteops\remoteops-credential-prompt.exe
%USERPROFILE%\.codex\remoteops\controller-config.json
%USERPROFILE%\.codex\remoteops\setup-state.json  # 仅一次性设置模式
%USERPROFILE%\.codex\config.toml
%USERPROFILE%\.agents\skills\remoteops\SKILL.md
%USERPROFILE%\.codex\skills\remoteops\SKILL.md
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
请在 60 秒内点击 Cancel：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Test-RemoteOpsMcp.ps1 -CredentialPromptSmokeTest
```

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

全新 Agent 首次运行只需填写 Relay 地址并等待九位控制码，不需要让 Codex 生成入网码，也不需要部署级 Agent 注册 Token。MCP 只在工程师本机使用 AI Controller Token 完成自身认证和后续配对。

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
