# Codex MCP 一次性安装配置

本功能面向 Windows 和 Apple Silicon macOS 的现有 Codex 安装器。Relay 管理页的「MCP 接入」可以创建安装码或下载 `.remoteops-setup` 文件。二者包含相同的安装配置，安装器负责登记独立凭据、写入连接配置、安装 MCP 与 skill、保留其他 Codex 配置，并验证 Relay TLS 连接。

仅源码已加入此功能，不代表生产 Relay 已升级、发布包已发布或任何真实客户端已登记。

## 管理员首次配置

1. 将 Relay 管理 HTTP 服务放在可信 HTTPS 反向代理后；保持管理服务仅在受保护网络内可访问。公网仅暴露所需页面/API，禁止代理记录请求体、响应体或敏感 Cookie。
2. 在「MCP 接入」中保存真实对外 Relay TLS 地址（明确的 `host:port`）、TLS 服务器名称，以及完整的 `https://.../api/mcp/enroll` 登记地址。不能把监听地址 `0.0.0.0` 当作对外地址，也不会根据管理页面域名猜测 Relay 端口。
3. 公共 CA 通常无需填写额外信任配置。使用私有 CA 时，分别填写 Relay 与 HTTPS 登记服务的公开 PEM 证书。禁止填写私钥。客户端会检查证书链、有效期和服务器名称，不支持忽略证书错误。
4. 创建安装配置，可填写客户端名称。默认首次领取有效期为 24 小时，也可选 1 小时或 7 天。

安装码/文件只在创建成功时提供，后台之后只保留状态和元数据，不能重新查看原始安装秘密。把它当作短期凭据，通过可信私密渠道传给安装者，不要贴入 AI 对话、工单、Git 或截图。创建结果关闭后，重新取码需要创建新配置；不再使用的配置可撤销。

## 两种安装入口

Windows 安装包：

```powershell
.\Install-RemoteOpsMcp.ps1 -SetupFile 'C:\Private\client.remoteops-setup'
```

macOS 安装包：

```bash
./install-remoteops-mcp.sh --setup-file "$HOME/Private/client.remoteops-setup"
```

粘贴安装码和 stdin 的具体参数、非交互确认方式见 [Windows 安装器](../deploy/controller-mcp/windows/README.md) 与 [macOS 安装器](../deploy/controller-mcp/macos/README.md)。不要把安装码作为命令行参数；让 AI 使用文件路径即可，AI 不必读取或回显文件内容。

安装器会先显示 Relay、HTTPS 登记地址、期限及是否使用私有 CA；确认这是管理员提供的目标之后才登记。文件内容在预览与登记间固定，不会因文件被修改而悄悄换目标。导入管理员配发的私有 CA 意味着信任该配置中的 CA，务必验证来源。

安装成功需要 HTTPS 登记、系统凭据保存、连接配置写入、Relay AI 身份自检和 Codex 配置步骤均成功。请完全退出并重启 Codex，然后检查 `/mcp`。安装器不会自动配对 Agent，也不会授予完全控制。

## 24 小时到期与失败恢复

- 有效期只控制首次成功领取。服务端时钟是最终依据，客户端不会因本地时间或旧的展示时间拒绝恢复。
- 首次领取会将安装配置原子地绑定到一个安装实例。复制相同文件给另一台电脑不能登记第二个客户端。
- 安装器在发出网络请求之前，把 256 位随机安装秘密保存并验证到当前用户的 Windows Credential Manager（仅本机持久保存，不启用企业漫游）或 macOS Keychain，并将非秘密的恢复索引写入 `setup-state.json`。
- 如果网络响应丢失、写配置失败、自检失败或后续安装步骤中断，使用相同用户、相同恢复索引和原安装文件/码重试；即使首次领取期限已经过去，也会恢复同一安装，不会创建第二条登记。
- 不要在失败后删除恢复索引或系统凭据。若已丢失其一，应由管理员撤销旧客户端，再创建新的安装配置。一个新安装码可在同一索引中建立新记录。
- 显式撤销安装配置会阻止领取和领取重试。撤销客户端会阻止后续登录和该客户端的领取重试，并断开已连接的该客户端。已开始执行的远端操作不保证被强制终止，应另用现有停止能力。

正常连接不再使用安装码；它使用每次安装独立的持久凭据。到期的安装码不会让已经安装的 MCP 断线。管理页显示待领取、已领取、已过期、已撤销的安装配置，以及独立客户端、最近活动和撤销入口。离线/重启后的最近时间保留最后持久化的连接时间，在线时显示当前连接活动时间。

## 安全和兼容边界

- 服务端从自身配置决定唯一 Owner，安装凭据固定为 AI Controller，不能用于 Human 身份、管理 API 或 Agent 登记。请求不能指定角色、权限范围、配对或 FullAccess。
- SQLite `IMMEDIATE` 事务、唯一绑定与持久提交保证并发领取最多产生一个客户端；数据库仅保存域分离的 SHA-256 凭据哈希。元数据和响应中没有全局 AI/Human Token、管理 Token、密码、私钥或脚本。
- 恢复要求同时拥有安装实例标识及其随机秘密，并仍持有原安装配置；公开机器名称或请求 ID 不足以恢复。
- HTTPS 请求中的秘密只在 POST body，不在 URL；拒绝重定向，错误不回显服务端响应内容；发码/登记响应禁止缓存，接口有请求大小和速率限制。
- 新安装的 JSON/TOML 仅保存凭据引用，不保存秘密；运行时直接访问操作系统凭据库。该配置优先于遗留 Token、Owner 和连接地址环境变量，避免旧环境变量覆盖新身份/目标。Linux 只可预览，不支持此安装凭据存储模式。
- 旧的共享 AI/Human Token 和手动安装方式继续兼容。单客户端撤销不撤销旧共享 Token，应按旧部署流程独立轮换。安装器不会改写 Codex 全局审批策略，也不会删除其他 MCP 配置。
- 设置更新只影响新创建的安装配置。已创建配置保留原地址/信任快照；如需迁移，撤销旧配置并重新配发。
- 本阶段为单 Relay 进程部署；不提供跨多个 Relay 实例的在线连接撤销广播。多个独立 SQLite 连接的领取事务安全不等于支持高可用多实例 Relay。

## 运维和备份

登记数据库位于 Relay `--state-file` 同目录、原文件名追加 `.mcp.sqlite3` 的文件，例如 `/data/relay-state.json.mcp.sqlite3`。运行期间可能同时有 `-wal`、`-shm` 文件。数据库有独立版本和 Owner 校验，无法读取、损坏或 Owner 不一致时拒绝启动，不会清空后自动重新开放登记。

不要在服务运行时只复制 SQLite 主文件。使用 SQLite 在线备份，或停止 Relay 后一致备份其状态 JSON、登记 SQLite 数据及 TLS 材料；按已有部署流程保护目录权限。旧 JSON 身份/会话数据保持原格式，不做一次性身份迁移。恢复旧数据库快照可能恢复已撤销凭据，因此恢复后必须按实际撤销记录核对客户端。

## 验证入口

- `cargo test -p remoteops-enrollment -p remoteops-enrollment-store -p remoteops-relay -p remoteops-controller-mcp --locked`
- `node scripts/test-relay-admin.mjs`
- `python3 scripts/Test-McpSetupInstallers.py`
- 全工作区 fmt、check、clippy、test，以及 Windows/macOS CI 的安装器测试。

测试使用隔离临时目录、虚构身份、模拟系统凭据或本地临时证书；不使用生产管理秘密。Native CI 与脚本模拟通过不能替代实际签名发布包、企业 Keychain/系统策略和生产反向代理的现场验收。
