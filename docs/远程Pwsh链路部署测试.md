# RemoteOps 远程 Pwsh 链路部署测试

- 适用版本：RemoteOps `0.2.0-preview.1`
- 测试链路：Codex → 本地 STDIO MCP → TLS Relay → Windows Agent → PowerShell 7
- 阶段定位：受控试点，不是生产无人值守运维

## 一、验收目标

本次只验证一台控制端、一台 Relay 和一台远端 Windows：

```text
本机 Codex
  ↓ 本地 STDIO
remoteops-controller-mcp.exe --command-mode approval
  ↓ TLS 7443
Linux Docker Relay
  ↓ Agent 主动出站 TLS
远端 Windows Agent
  ↓
pwsh.exe 持久 Shell
```

通过标准：

1. Codex 能准确识别唯一 `session_id` 和远端主机；
2. 远端上报 `power_shell` 能力；
3. `run_readonly_command` 可执行只读诊断；
4. 非只读命令在没有审批时被拒绝；
5. Human Controller 批准精确命令后，Codex 可调用 `run_command`；
6. 持久 `pwsh` 保留工作目录和变量状态；
7. 审批重复使用、命令变化和跨会话使用均被拒绝；
8. Agent、Relay 或网络短暂重启后可恢复连接；
9. 审计中包含请求、审批、执行和结果，但不包含 Controller Token。

## 二、环境前提

### Linux Relay

- `x86_64` Linux；
- Docker Engine 和 Docker Compose Plugin；
- 公网域名或固定 IP；
- TCP `7443` 仅允许控制端和远端 Windows 的来源 IP；
- 不对公网开放健康检查端口，Compose 固定绑定 `127.0.0.1`。

### 远端 Windows

- Windows 10/11 或 Windows Server x64；
- PowerShell 7，`pwsh.exe` 可从运行 Agent 的账户 PATH 中找到；
- 专用低权限测试账户；
- 明确的测试工作目录和可恢复快照；
- 允许主动连接 Relay 的 TCP `7443`。

### 本机控制端

- Codex 桌面端或 CLI；
- `remoteops-controller-mcp.exe` 和 `remoteops-controller-cli.exe`；
- Relay 公共证书；
- 相互独立的 AI/Human Controller Token；
- 可写的本地审计目录和传输目录。

## 三、构建发布文件

在项目目录执行：

```powershell
.\scripts\Build-Windows.ps1
.\scripts\Build-LinuxRelay.ps1
```

构建脚本会生成并核对：

```text
artifacts/release/0.2.0-preview.1/windows-x64/manifest.json
artifacts/linux-x64/manifest.json
```

部署前必须确认所有正式组件为 `0.2.0-preview.1`，并保留 Release 中的 SHA-256 用于传输后核对。串口 Demo 仍独立使用 `0.5.0`。

## 四、部署 Relay

Relay 部署目录至少包含：

```text
deploy/relay/docker-compose.yml
deploy/relay/Dockerfile
deploy/relay/.env.example
```

以 `.env.example` 为模板创建权限为 `600` 的 `.env`，填写：

- `REMOTEOPS_TLS_SANS`：必须包含 MCP 和 Agent 实际使用的公网域名或 IP；
- `REMOTEOPS_HUMAN_CONTROLLER_TOKEN`：至少 32 字节；
- `REMOTEOPS_AI_CONTROLLER_TOKEN`：至少 32 字节，且不能与 Human Token 相同；
- `REMOTEOPS_RELAY_PORT`：默认 `7443`。

从源码构建 Relay：

```bash
export REMOTEOPS_RELAY_DOCKERFILE=deploy/relay/Dockerfile
docker compose --env-file deploy/relay/.env \
  -f deploy/relay/docker-compose.yml \
  up -d --build
```

检查容器：

```bash
docker compose --env-file deploy/relay/.env \
  -f deploy/relay/docker-compose.yml \
  ps
docker logs --tail 100 remoteops-relay
curl --fail http://127.0.0.1:18080/health
```

默认 Compose 首次启动会在 `/data/tls` 生成自签名证书，此模式只用于实验环境，需要把 `cert.pem` 配发给 Agent 和 Controller；私钥 `key.pem` 必须留在 Relay 数据卷中。当前公网正式 Relay 使用受信任证书，Agent 和 MCP 通过系统可信根验证，不分发 `relay-cert.pem`。

如果域名或 IP 改变，不得继续复用 SAN 不匹配的旧证书。应在保留状态数据与回滚文件的前提下重新签发正确证书，再重新配发公共证书。

## 五、启动远端 Windows Agent

公网正式 Relay 使用受信任证书时，现场只需分发：

```text
remoteops-agent-gui.exe
```

以专用非管理员测试账户双击运行即可。GUI Agent 默认连接
`relay.example.com:7443`，使用操作系统可信根证书验证 TLS，并把状态和传输目录
放在 `%LOCALAPPDATA%\RemoteOps`。窗口会显示临时控制码、当前控制端数量、授权能力和工程师连接状态；“立即停止协助”或关闭窗口都会结束本次远程入口。

命令行自动化、无人值守实验和故障排查仍可使用：

```text
remoteops-agent.exe
```

需要连接其他 Relay 或自签名测试 Relay 时，仍可显式启动：

```powershell
.\remoteops-agent.exe `
  --relay relay.example.com:7443 `
  --server-name relay.example.com `
  --ca-cert .\relay-cert.pem `
  --state-file .\agent-state.json `
  --transfer-root .\transfers `
  --retry-seconds 3
```

记录窗口显示的临时控制码，但不要把控制码或 `agent-state.json` 写入聊天、仓库或长期文档。关闭 Agent 即可立即停止该远端入口。不要复制或删除 `%LOCALAPPDATA%\RemoteOps\agent-state.json`，否则会丢失当前 Agent 身份和恢复关系。

## 六、接入本机 Codex

按[Codex MCP 接入说明](CodexMCP接入说明.md)配置本地 STDIO MCP，并设置：

```text
REMOTEOPS_CONTROLLER_TOKEN=<AI Controller Token>
REMOTEOPS_COMMAND_MODE=approval
```

也可以在 MCP `args` 中显式使用：

```text
--command-mode approval
```

不要把 Controller Token 放入 `args`。完全退出并重新启动 Codex 后，使用 `/mcp` 确认 RemoteOps 已连接。

Human GUI/CLI 使用独立的 `REMOTEOPS_HUMAN_CONTROLLER_TOKEN`，并用同一控制码绑定到相同 `session_id`。

## 七、验收用例

### 7.1 目标与只读路径

1. `pair_connection`；
2. `list_connections`；
3. `get_target_info`，确认主机名和 `power_shell`；
4. 使用一次性 `power_shell` 的 `run_readonly_command` 执行 `Get-Host`、`Get-Location` 和 `Get-Process`；
5. `open_shell` 选择 `power_shell`，后续持久 Shell 命令使用 `run_command` 并完成逐项确认。

### 7.2 审批命令路径

在同一个持久 `shell_id` 上：

1. 为 `Set-Location '<测试工作目录>'` 申请 `run_command` 审批；
2. Human Controller 核对完整 operation JSON 后批准；
3. Codex 使用完全相同的命令和 `approval_id` 执行；
4. 通过 `Get-Location` 确认持久状态；
5. 为 `Set-Variable -Name RemoteOpsE2E -Value alpha` 重复逐条审批；
6. 通过 `Get-Variable RemoteOpsE2E` 确认变量状态；
7. 根据真实调试需求，对 `dotnet --info` 或测试目录中的诊断脚本执行同样流程。

### 7.3 拒绝边界

必须确认：

- `readonly` 模式调用 `run_command` 被拒绝；
- `approval` 模式没有 `approval_id` 时被拒绝；
- 同一审批第二次使用被拒绝；
- 命令文本增加空格、参数或管道后原审批失效；
- 更换 `session_id` 或 `shell_id` 后原审批失效；
- 审批过期或人工拒绝后不能执行；
- Agent 关闭后 Codex 无法继续执行远端命令。

### 7.4 自动验收

阿里云来源 IP 白名单生效后，直接验证公网 `7443`：

```powershell
.\scripts\Test-RemoteRelayE2E.ps1 -DirectRelay
```

白名单尚未生效时，也可以通过 SSH 隧道验证服务器回环入口：

```powershell
.\scripts\Test-RemoteRelayE2E.ps1
```

脚本从服务器安全读取测试 Token 到权限受限的临时目录，验证 MCP、持久
PowerShell 7、精确审批、重复消费拒绝和文件哈希，完成后恢复环境变量并删除
临时凭据。内网环境可通过 `-DirectRelayHost <Relay 内网地址>` 验证直连 TLS。

## 八、停止和回滚

停止测试时按以下顺序处理：

1. 关闭远端 Agent；
2. 在 Codex 配置中禁用 RemoteOps MCP；
3. 关闭 Human GUI/CLI；
4. 停止 Relay 容器；
5. 保留 Relay 数据卷、审计和现场快照，完成复盘后再决定是否删除。

停止 Relay 但保留数据：

```bash
docker compose --env-file deploy/relay/.env \
  -f deploy/relay/docker-compose.yml \
  down
```

不得在测试服务器执行全局 Docker prune，也不得在未确认备份前删除 Relay 数据卷。

## 九、当前非目标

- 代码签名、安装包和自动升级；
- Relay 高可用、多地域和高并发；
- 无人工审批的会话级完全控制；
- 远程桌面、图像识别和鼠标键盘操作；
- 生产客户环境的长期常驻 Agent。

