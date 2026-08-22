# 远程 AI 运维协助项目交接文档

> 文档状态：`3.0.0` 以前的历史维护快照。日常安装、版本和操作参数必须使用[文档中心](README.md)中的当前手册。

- 文档用途：新 Codex 会话继续工作的上下文入口
- 编写时间：2026-07-31
- 历史快照版本：现场 Agent CLI/GUI `2.3.1`，Controller MCP `2.2.0`，Relay、Controller CLI/GUI `2.1.0`（协议 v3），本地串口 Demo `0.5.0`、控制码查看器 `1.4.1`；当前公开版本统一见 `PROJECT_STATUS.md`。
- 当前阶段：Linux Docker Relay 独立公网 `7443` 部署、公网 PowerShell 7 持久 Shell E2E、逐条审批和 Agent 零参数连接均已完成；等待真实远端 Windows 验收
- 交接结论：不要重新实现第一阶段或串口核心，不要重复运行破坏性三机实验；下一步按本文档分别完成真实 COM 验收和 Codex → MCP → Relay → 远端 Pwsh 受控部署测试

## 一、项目目标和当前结论

本项目用于客户现场远程运维协助：

```text
客户机器运行轻量 Windows Agent
        ↓ 主动出站 TLS 连接
Linux Docker Relay
        ↓
控制端 CLI / 本地 STDIO MCP / 后续 GUI
        ↓
人工和 AI 在同一个 session_id 目标上协作
```

第一阶段已经完成：

```text
Rust 核心业务类库
+ Windows Agent
+ Linux Docker Relay
+ Controller CLI
+ 本地 STDIO MCP
+ Linux、`LAB-AGENT-A`、`LAB-AGENT-B` 三机端到端验收
```

第一阶段明确不包含：

- GUI；
- 远程桌面；
- 图像识别；
- 鼠标键盘自动化；
- 默认无人值守；
- 多租户 SaaS；
- 自动刷固件或自动提交交换机配置。

第一阶段验收结论为“通过”。真实交换机 SSH 和真实 USB/COM 串口物理硬件仍待现场验证，不能表述为已经通过。

## 二、最重要的文档入口

新会话建议按以下顺序阅读：

1. [项目 README](../README.md)；
2. [项目状态](PROJECT_STATUS.md)；
3. [路线图](ROADMAP.md)；
4. [安全模型](安全模型.md)；
5. [本交接文档](远程AI运维协助交接文档.md)；
6. [第一阶段验收报告](第一阶段验收报告.md)；
7. [部署与三机验收手册](部署与三机验收手册.md)；
8. [Codex MCP 接入说明](CodexMCP接入说明.md)；
9. [功能索引](功能索引.md)。

第一阶段结构化结果：

- 文件：[docs/history/evidence/lab-e2e-result-2026-07-31.json](history/evidence/lab-e2e-result-2026-07-31.json)
- 最终运行标识：`20260731-201611-552a2c90`
- SHA-256：`8B578AD5D0CDBE27AE50306C27C9EB8F444D6E235239D0620FB0914EE0E5056E`

## 三、源码结构

项目根目录：

```text
<仓库根目录>
```

Workspace：

```text
remoteops
├─ Cargo.toml
├─ Cargo.lock
├─ crates
│  ├─ remoteops-domain
│  ├─ remoteops-protocol
│  ├─ remoteops-session
│  ├─ remoteops-serial
│  ├─ remoteops-policy
│  ├─ remoteops-audit
│  ├─ remoteops-device
│  ├─ remoteops-application
│  └─ remoteops-ai
├─ apps
│  ├─ remoteops-agent
│  ├─ remoteops-relay
│  ├─ remoteops-controller-cli
│  ├─ remoteops-controller-mcp
│  ├─ remoteops-controller-gui
│  └─ remoteops-serial-demo
├─ tests
│  └─ remoteops-mcp-smoke
├─ deploy
│  ├─ relay
│  └─ lab-ssh
├─ scripts
└─ docs
```

### 3.1 核心类库职责

| 类库 | 职责 |
|---|---|
| `remoteops-domain` | Agent/Controller/Session 标识、连接状态、租约、能力、远程操作、事件和错误 |
| `remoteops-protocol` | TLS、协议消息、授权消息、帧编解码 |
| `remoteops-session` | 多连接注册表、默认编号、别名、目标解析、人工接管 |
| `remoteops-serial` | 有界串口记录、华为/ANSI 终端、按键映射、结构化查询、分页、提示符、只读授权和脱敏 |
| `remoteops-policy` | 只读白名单、风险分类、审批记录、一次性审批消费 |
| `remoteops-audit` | JSONL 审计、SHA-256、敏感信息脱敏和导出 |
| `remoteops-device` | CMD、Windows PowerShell 5.1、可选 `pwsh.exe`、持久 Shell、SSH、串口、端口和文件 |
| `remoteops-application` | Controller 应用服务、请求生命周期、事件流、Relay 客户端和结果处理 |
| `remoteops-ai` | OpenAI 兼容 Responses/Chat Completions 客户端和受控工具循环 |

### 3.2 壳子职责

| 壳子 | 职责 |
|---|---|
| `remoteops-agent` | CLI 与 GUI 共用的 Windows Agent 生命周期、状态文件、主动出站、心跳、重连和设备操作适配；CLI 只保留命令行壳 |
| `remoteops-agent-gui` | 现场被控端 GUI、控制码与控制端状态、能力状态、复制控制码和停止协助入口 |
| `remoteops-relay` | TLS Relay、注册、配对、转发、Controller 角色认证、审批和状态持久化 |
| `remoteops-controller-cli` | 人工控制端、参数解析、终端输出、CLI E2E 入口 |
| `remoteops-controller-mcp` | 本地 STDIO MCP Server；只把工具输入转给核心应用服务 |
| `remoteops-controller-gui` | 人工控制端 GUI、串口工作台和任务级 AI 只读查询入口 |
| `remoteops-serial-demo` | 本地真实 COM 人工与 AI 查询验收壳 |
| `remoteops-mcp-smoke` | 使用官方 `rmcp` SDK 验证工具发现、调用、审批和文件边界 |

业务规则只能放在核心类库中。后续 GUI 必须直接复用这些类库，不得复制 CLI 或 MCP 中的业务流程。

## 四、三台测试机器

### 4.1 Linux Relay

- 地址：`192.0.2.10`
- 主机名：已从公开文档移除
- 系统：Ubuntu 22.04.4 LTS
- Docker：`29.1.5`
- SSH 用户：已从公开文档移除；复现时使用专用低权限实验账户
- SSH 主机 ED25519 指纹：

```text
SHA256:<历史实验室指纹已从公开文档移除>
```

- 开发机专用密钥路径：`%USERPROFILE%\.ssh\remoteops_lab_runner_ed25519`
- 当前用户可以直接使用 Docker；
- `sudo` 需要交互密码；
- Linux 根分区在验收期间为满载状态，因此三机实验默认使用预编译 Relay；
- 实验工作区：`/dev/shm/remoteops-lab/<运行标识>`；
- 实验 Relay 宿主端口：`17443`；
- 自托管 Relay 默认监听端口：`7443`。

三机验收后已确认：

- RemoteOps 容器：`0`；
- RemoteOps 网络：`0`；
- RemoteOps Volume：`0`；
- RemoteOps 镜像：`0`；
- `/dev/shm/remoteops-lab` 实验目录：`0`；
- 实验账户 home 下的 `remoteops-lab` 目录：`0`；
- RabbitMQ 和 EMQX 仍在运行；
- 未执行全局 Docker prune。

### 4.2 LAB-AGENT-A / 测试目标 A

- 地址：`192.0.2.117`
- 主机名：`LAB-AGENT-A`
- Windows 11 企业版 LTSC
- Windows PowerShell：`5.1`
- 架构：`AMD64`
- WinRM：已配置受控访问；
- 最新验收 Agent GUID：已从公开文档移除
- 最新验收 `session_id`：已从公开文档移除

已验证：

- Agent 注册和主动出站；
- CMD；
- Windows PowerShell 5.1；
- 持久 Shell；
- 端口探测；
- 文件上传、下载和 SHA-256；
- Agent 侧 OpenSSH 密钥认证；
- 单独断线、重连和进程重启恢复；
- 串口枚举，当前枚举数量为 `0`。

### 4.3 LAB-AGENT-B / 测试目标 B

- 地址：`192.0.2.118`
- 主机名：`LAB-AGENT-B`
- Windows 11 企业版 LTSC
- Windows PowerShell：`5.1`
- 架构：`AMD64`
- WinRM：已配置受控访问；
- 最新验收 Agent GUID：已从公开文档移除
- 初始 `session_id`：已从公开文档移除
- 租约到期后 `session_id`：已从公开文档移除

已验证：

- 第二个 Agent 独立注册；
- 与 Agent A 的会话、别名、命令和文件隔离；
- CMD；
- Windows PowerShell 5.1；
- 端口探测；
- 文件上传、下载和 SHA-256；
- Agent 侧 OpenSSH 密钥认证；
- Agent A 离线期间 Agent B 独立工作；
- Agent 进程重启；
- Relay 重启；
- 租约过期后旧控制码失效并生成新 `session_id`；
- 串口枚举，当前发现 `COM1`。

不要把上述临时 GUID、`session_id` 或运行标识当作长期认证凭据；它们只用于验收追踪。

## 五、第一阶段能力和证据

三机结果中的必需布尔检查共 `46` 项，当前全部为真。串口硬件字段明确为假：

```json
{
  "Serial": {
    "EnumerationCompleted": true,
    "RealHardwareValidated": false,
    "OpenReadWriteValidated": false
  }
}
```

已通过的主要能力：

- 双 Agent 同时连接；
- 默认“连接 1、连接 2”；
- 修改为“客户 A、客户 B”；
- 每个工具按不可变 `session_id` 绑定；
- 防止 Agent A/Agent B 串线；
- CMD 和 Windows PowerShell 5.1；
- 持久 Shell、实时输出和取消；
- Agent OpenSSH 密钥认证；
- 端口探测；
- 文件上传、下载和哈希；
- Human/AI 同一事件流；
- 高风险操作人工审批；
- 独立 Human Controller 审批 MCP 操作；
- 人工接管和中断 AI；
- Relay 状态文件 `/data/relay-state.json`；
- Relay 容器重启和网络重建恢复；
- 本地 STDIO MCP；
- 官方 Rust MCP SDK Smoke；
- `transfer-root` 路径越界拒绝；
- 审计脱敏和导出。

## 六、测试门禁

已经通过：

```powershell
cd <仓库根目录>
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
.\scripts\Test-LocalE2E.ps1
.\scripts\Build-Windows.ps1
.\scripts\Build-LinuxRelay.ps1
.\scripts\Invoke-LabE2E.ps1 -SkipBuild
.\scripts\Invoke-LabE2E.ps1 -CleanupRunToken '<运行标识>'
```

结果：

- 当前 Workspace Rust 测试：`129` 项通过，`0` 失败；
- Clippy：`0` warning；
- PowerShell AST：`Invoke-LabE2E.ps1` 和 `Test-LocalE2E.ps1` 均通过；
- 本机 TLS E2E：通过；
- Windows Release：通过；
- Linux Relay 交叉编译：通过；
- 安全加固后的三机 E2E：通过；
- 文档和验收产物秘密模式扫描：通过；
- 最新三机结果 SHA-256 与文档一致。

## 七、发布产物

Windows Release 目录：

```text
<仓库根目录>\artifacts\windows-x64
```

| 文件 | 版本 | SHA-256 |
|---|---:|---|
| `remoteops-agent.exe` | `2.3.1` | `8ba089fbd0c78ed388fc4d2fa504b6e52ec06acc13a1777cb286264cba94ceca` |
| `remoteops-agent-gui.exe` | `2.3.1` | `3bcce1dfc1e93a3b4c2345f82ccb94a60dbe594489ba902dd1febe3c30d4c308` |
| `remoteops-controller-cli.exe` | `2.1.0` | `e7a7a796d1dcc8313e0cf298ab47bcbf3d2bdf748720b208c65460e4a71477f0` |
| `remoteops-controller-mcp.exe` | `2.2.0` | `5ff0c4781edaf932786c30705cd93790c8a22d98e6755d08911165879db1f3f0` |
| `remoteops-controller-gui.exe` | `2.1.0` | `0ad9be9554f335410977ade3d4caf29098d2dcd28404379cd6794970cfada84e` |
| `remoteops-serial-demo.exe` | `0.5.0` | `72eccfa5b68d83598c06694bffa72baf52d1750c75d3228e50c6ffc2f446307f` |
| `remoteops-mcp-smoke.exe` | `2.1.0` | `d8ccda3758adc54e1302fa8110c308bf629f3ca4a7ea79d4f811b240503c6c15` |

Linux Relay：

```text
<仓库根目录>\artifacts\linux-x64\remoteops-relay
```

- 版本：`2.1.0`
- 目标：`x86_64-unknown-linux-gnu.2.36`
- 文件长度：`8714176` 字节
- SHA-256：`cb5c28eb603692ccb579288cbfe2e41a97f9545cc757497744cb4bf2bc101c06`

Relay 已在自托管 Linux Docker 验收环境中运行，宿主机监听独立 `7443`，健康检查仅绑定回环地址。Agent CLI/GUI 使用 Windows 系统信任链连接公网可达的自托管 Relay；Codex CLI → MCP → Relay → PowerShell 7 只读回归、完整 MCP、持久 PowerShell 7 和逐条审批 Smoke 均应在发布前重新验证。

Controller MCP 可按示例配置连接自托管的 `relay.example.com:7443` 并使用操作系统可信根，不再要求 `relay-cert.pem`。发布包必须重新生成 SHA-256 校验文件，公开文档不得沿用历史构建产物的固定哈希。

`target`、`artifacts`、状态文件、审计文件和运行日志已加入根 `.gitignore`。构建产物和 E2E JSON 只作为内部材料，不应无选择提交或公开。

## 八、安全边界

- 不记录服务器密码、API Key、恢复令牌或 Controller Token；
- Linux 使用固定主机指纹和专用 SSH 密钥；
- Windows 管理凭据由 Credential Manager 读取；
- Agent 只主动出站连接 Relay；
- Controller Token 不通过 SSH 或 MCP Smoke 命令行传递；
- 实验 Token 通过内存和权限为 `0600` 的远程临时环境文件传递；
- Relay 日志不包含密码、API Key 或 Bearer；
- MCP 只能申请审批，不能自我批准；
- MCP 非只读命令默认关闭；显式 `command-mode=approval` 后仍须对精确会话、Shell 和命令逐条人工审批；
- AI 修改操作、高风险命令和可写串口必须人工审批；
- 只有显式任务/会话级授权内的完整华为 `display ...` 结构化查询可免逐条审批；授权绑定串口、期限和次数；
- 串口查询审计不记录命令明文，交给 AI 的查询结果先遮盖常见凭据行；
- 不执行全局 Docker prune；
- 不修改 RemoteOps 之外的既有服务；
- 不为了测试关闭 Windows 安全策略。

Linux 根分区目前为 `100%`，下一次三机实验必须继续优先使用：

```powershell
.\scripts\Invoke-LabE2E.ps1 -SkipBuild
```

## 九、当前阶段：串口核心与远程 AI 主动查询

已完成：

- 新增 `remoteops-serial`，集中实现有界收发记录、华为/ANSI 终端、按键映射、安全控制序列、结构化查询、自动分页、提示符识别、授权和脱敏；
- `remoteops-device` 的串口写入改为 `write_all + flush`，避免底层短写导致命令残缺；
- Demo `0.5.0`、正式 Agent 和 GUI 复用同一个 `SerialQueryRunner`，不再复制串口业务流程；
- 协议 v3 新增 `RunSerialQuery`；Relay、Agent 和策略层重新计算风险，不信任调用者的 `readonly` 声明；
- Demo 提供会话级 `/ai-access readonly`；GUI 提供本次 AI 任务级的 10 分钟、最多 8 条完整华为 `display ...` 授权；
- 原始 `WriteSerial` 仍属于修改操作；分页空格是已授权查询计划的一部分；
- 查询命令审计只记录长度和 SHA-256，交给 LLM 的查询结果只包含脱敏文本；
- `cargo fmt`、Clippy、`134` 项 Workspace 测试和 Windows Release 构建全部通过。
- Linux Relay `2.1.0` 已完成 `x86_64-unknown-linux-gnu.2.36` Release 构建并部署到真实 Docker 环境，内网直连 E2E 通过。
- Agent CLI/GUI 通过部署配置连接自托管的 `relay.example.com:7443`，使用操作系统可信根证书；现场默认分发 `remoteops-agent-gui.exe`，CLI 保留给自动化和排障；显式自签名证书参数保持兼容。
- 公网直连 MCP Smoke 已验证持久 PowerShell 7、精确审批、一次性消费、命令篡改拒绝和文件哈希。

真实环境已确认：

- 本地 `COM7 9600 8N1` 的华为 CE5850 人工 RX/TX、分页、终端/管理切换和行编辑通过；
- 旧版逐次批准路径已由 AI 主动执行 `display version` 并基于真实响应回答。

仍待验收：

1. Demo `0.5.0` 执行 `/ai-access readonly 10 20` 后主动查询并自动分页；
2. 确认不再出现“工具调用超过最大轮数”和管理提示符插入设备命令；
3. 确认未知、修改型、过期、次数耗尽和跨串口请求均被拒绝；
4. 正式 GUI 勾选任务级授权，经 Relay、Agent 和真实 `COM7` 完成同一查询闭环；
5. 长时间运行、拔插、断开重连和 AI 写入拒绝的真实设备行为。

### 9.1 MCP 远程 PowerShell 7 命令模式

已完成：

- `remoteops-controller-mcp` 新增 `--command-mode readonly|approval`，默认保持只读；
- 验收配置使用公网可达的自托管 Relay 和系统可信根；`--ca-cert` 保留给自签名或私有 CA 环境；
- Windows MCP 安装脚本默认配置 `approval` 模式，并同时保留 Codex `writes` 审批和独立 Human Controller 精确审批两层安全边界；
- 新增 `run_command`，只在 `approval` 模式接受独立 Human Controller 的一次性 `approval_id`；
- 命令审批精确绑定 `session_id`、Shell 类型或持久 `shell_id` 和完整命令；
- 本机 E2E 已实际打开 PowerShell 7 持久 Shell，完成命令申请、人工批准、执行和变量状态读取；
- 篡改命令、缺少审批、重复消费和 MCP 自我审批均已验证拒绝；
- Relay Compose 已参数化镜像、TLS SAN 和两个角色 Token，新增无真实凭据的 `.env.example`；
- 详细步骤见[远程 Pwsh 链路部署测试](远程Pwsh链路部署测试.md)。

仍待验收：

1. 在真实远端 Windows 以专用低权限账户双击启动 GUI Agent `2.3.1`；
2. 从本机 Codex 完成 MCP 配对、只读诊断和逐条审批命令；
3. 验证公网断线、Relay 重启、Agent 重启和停止入口；
4. 检查公网日志、审计和运行目录不包含 Token 或恢复令牌。

## 十、给新 Codex 会话的启动指令

新会话读取本文档后，应该执行以下动作：

```text
你正在继续 RemoteOps 项目。请先阅读：

1. README.md
2. docs/README.md
3. docs/安全模型.md
4. docs/功能索引.md
5. docs/远程AI运维协助交接文档.md
6. docs/第一阶段验收报告.md

第一阶段、GUI 和串口核心接入已经完成，不要重新实现核心类库、Agent、Relay、
CLI、MCP、GUI 或 Demo。当前优先目标是按 docs/远程Pwsh链路部署测试.md 将
GUI Agent 部署到真实远端 Windows，验证 Codex、MCP、自有 Relay、Agent
到持久 PowerShell 7 的逐条审批链路；真实 COM 验收继续按
本文档第九节执行。不要粘贴完整配置或任何凭据。
```

## 十一、交接时不要做的事

- 不要把第一阶段验收报告中的临时 GUID 或 `session_id` 当作认证凭据；
- 不要把密码、Token、私钥、恢复令牌写入文档或源码；
- 不要在 Linux 根分区满载时执行全局 Docker prune；
- 不要使用未固定指纹的 SSH 连接；
- 不要直接在 Agent A/Agent B 安装虚拟串口内核驱动；
- 不要把虚拟串口结果表述为真实交换机 Console 结果；
- 不要在 GUI 中复制 CLI/MCP 的业务逻辑；
- 不要在 GUI 阶段提前加入远程桌面和图像操作。
