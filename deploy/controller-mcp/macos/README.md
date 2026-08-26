# RemoteOps MCP macOS Apple Silicon 安装说明

- 版本：`0.2.0-preview.4`
- 适用系统：macOS 13 或更高版本，Apple Silicon（arm64）
- Codex MCP 名称：`remoteops`

本安装包只安装工程师本机使用的 STDIO MCP。它可以通过 Relay 控制现有 Windows Agent；不要求现场电脑是 Mac，也不代表 macOS Agent 已经完成。

## 安装

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
~/.codex/remoteops/remoteops-controller-mcp-0.2.0-preview.4
~/.codex/remoteops/launch-remoteops-controller-mcp.sh
~/.codex/remoteops/controller-config.json
~/.codex/config.toml
~/.agents/skills/remoteops/SKILL.md
~/.codex/skills/remoteops/SKILL.md
```

## 验证

```bash
./test-remoteops-mcp.sh
```

完全退出并重新打开 Codex，然后输入 `/mcp`，确认 `remoteops` 已启用。新建任务后可以直接说：

```text
这是 RemoteOps 控制码 123-456-789。请检查现场电脑的网络和指定进程，只进行只读诊断。
```

## 卸载

```bash
./uninstall-remoteops-mcp.sh
```

卸载器会删除 MCP、Codex 配置段、RemoteOps skill 和 Keychain Token，但保留本地审计日志与文件交换目录。

