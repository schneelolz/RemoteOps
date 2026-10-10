# RemoteOps MCP macOS Apple Silicon 安装说明

- 版本：`0.2.0-preview.13`
- 适用系统：macOS 13 或更高版本，Apple Silicon（arm64）
- Codex MCP 名称：`remoteops`

本安装包只安装工程师本机使用的 STDIO MCP。它可以通过 Relay 控制现有 Windows Agent；不要求现场电脑是 Mac，也不代表 macOS Agent 已经完成。

## 推荐：一次性设置

请管理员在 Relay 管理界面签发短时、单次使用的 MCP 设置文件或设置码。文件包含
Relay 地址、登记 URL、TLS 服务名、必要的 CA 信任和一次性登记密钥；它不是通用
Controller Token，也不是现场 Agent 的九位控制码。请通过可信渠道核对登记目标，
不要把设置文件内容、设置码或长期 Token 发到 Codex 对话、截图、日志或命令行。

打开 Terminal，进入解压目录：

```bash
chmod +x install-remoteops-mcp.sh test-remoteops-mcp.sh uninstall-remoteops-mcp.sh
./install-remoteops-mcp.sh --setup-file "$HOME/Downloads/remoteops-setup.json"
```

安装器会显示不含密钥的 Relay、登记 URL、TLS 服务名、有效期和私有 CA 使用情况，
输入 `yes` 后才登记。只有设置码时，用隐藏粘贴提示，不要把码接在参数后：

```bash
./install-remoteops-mcp.sh --setup-code
```

由 AI 协助安装时，只给它本机文件路径和经过你确认的登记目标。先用以下命令查看
安全预览；确认该目标后才能使用 `--confirm-enrollment` 非交互安装：

```bash
./remoteops-controller-mcp --setup-file "$HOME/Downloads/remoteops-setup.json" --setup-preview
./install-remoteops-mcp.sh --setup-file "$HOME/Downloads/remoteops-setup.json" --confirm-enrollment
```

自动化也可从标准输入读取 JSON 或设置码。`--setup-stdin` 必须同时带
`--confirm-enrollment`，且输入只能来自已确认的安全来源，不能把码写到 shell 历史中。
此选项只跳过交互确认，不会关闭 HTTPS 或 TLS 校验，也不会跟随登记 HTTP 重定向。

每个安装生成独立 AI Controller 凭据，直接保存在当前用户的 macOS Keychain。
`controller-config.json` 只保存非秘密连接信息与 `credential_id`；运行时直接读取
Keychain，不使用旧 Token 启动脚本，也不改写旧 Token。登记成功后会验证 Relay
身份与认证响应，验证通过才写入 Codex MCP 注册并报告安装成功。安装器只替换
`mcp_servers.remoteops` 配置，保留其他 MCP、模型和全局审批设置。

如果网络中断或自检失败，保留 `~/.codex/remoteops/setup-state.json`、Keychain
条目和原设置文件，再运行相同安装命令。此非秘密检查点用于同一安装的幂等重试；
不要先删凭据或复制检查点到另一台机器。已由其他安装使用、过期或被撤销的设置
不能新登记，请管理员重新签发。成功后妥善删除下载的设置文件；不要把它放进安装包或 Git。

## 兼容：手工安装

打开 Terminal，进入解压目录：

```bash
chmod +x install-remoteops-mcp.sh test-remoteops-mcp.sh uninstall-remoteops-mcp.sh
./install-remoteops-mcp.sh \
  --relay relay.example.com:7443 \
  --owner-id '<controller-owner-uuid>'
```

安装器会以隐藏输入方式读取 AI Controller Token，并保存到当前用户的 macOS Keychain。Token 不写入 `config.toml`、RemoteOps JSON、日志或安装包。公网 CA 使用 macOS 系统可信根；私有 CA 使用 `--ca-cert`，已独立核对的证书指纹使用 `--tls-fingerprint`，两者只能选择一种。

安装位置：

```text
~/.codex/remoteops/remoteops-controller-mcp-0.2.0-preview.13
~/.codex/remoteops/remoteops-credential-prompt
~/.codex/remoteops/launch-remoteops-controller-mcp.sh  # 仅手工 Token 模式
~/.codex/remoteops/controller-config.json
~/.codex/remoteops/setup-state.json                 # 仅一次性设置模式
~/.codex/config.toml
~/.agents/skills/remoteops/SKILL.md
~/.codex/skills/remoteops/SKILL.md
```

## 验证

```bash
./test-remoteops-mcp.sh
```

`test-remoteops-mcp.sh` 会检查当前安装的 Keychain 凭据引用；网络检查只证明 TCP 可达，
不等同于重新认证。首次安装的认证自检由设置程序完成。安装器不会改写全局
`approval_policy`；如果你的 Codex 策略禁止 MCP elicitation，需按组织要求自行调整。

完全退出并重新打开 Codex，然后输入 `/mcp`，确认 `remoteops` 已启用。新建任务后可以直接说：

```text
这是 RemoteOps 控制码 123-456-789。请检查现场电脑的网络和指定进程，只进行只读诊断。
```

需要 SSH 密码认证时，只在对话中提供目标、用户名和命令，并让 `run_ssh` 设置 `use_password=true`。MCP 会打开 Mac 本机隐藏输入窗口；密码不会进入 Codex 对话或工具参数。窗口默认仅本次使用，也可由用户显式选择在 MCP 内存中固定保存 10 分钟，并通过 `clear_ssh_credential_cache` 提前清除。

## 卸载

```bash
./uninstall-remoteops-mcp.sh
```

卸载器会先删除当前独立 Keychain 凭据，再删除 MCP、Codex 配置段、RemoteOps skill
和设置检查点；手工模式只删除兼容的旧 Keychain Token。一次性设置模式不改动旧
Token。本地审计日志与文件交换目录保留。
若程序或凭据存储暂时不可用，卸载会停止并保留凭据引用，恢复后可重试。
