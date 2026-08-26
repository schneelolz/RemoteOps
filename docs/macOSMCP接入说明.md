# macOS MCP 接入说明

RemoteOps 首个公开预览支持在 Apple Silicon Mac 上运行 Codex STDIO MCP，并通过自托管 Relay 控制现有 Windows Agent。这里的 macOS 支持是控制端 MCP，不是 macOS 被控端 Agent。

## 支持范围

- 系统：macOS 13 或更高版本；
- 架构：Apple Silicon（arm64）；
- AI 客户端：使用 `~/.codex/config.toml` 的 Codex；
- 现场端：当前仍为 Windows x64 Agent；
- Relay：当前首发部署仍为 Linux x64 + Docker。

Intel Mac、Universal Binary、Developer ID 签名和 Apple Notarization 尚未包含在首个预览。发布包未签名时，macOS 可能要求用户在“系统设置 → 隐私与安全性”中确认来源；不要通过长期关闭 Gatekeeper 绕过保护。

## 安装前准备

你需要：

1. `RemoteOps-MCP-macOS-arm64-0.2.0-preview.4.tar.gz`；
2. Relay 地址，例如 `relay.example.com:7443`；
3. AI Controller Token，长度至少 32 个字符；
4. Relay 配置的统一 Controller Owner UUID；
5. 私有 CA 环境中的 CA PEM，或已经通过独立可信渠道核对的 Relay 叶证书 SHA-256 指纹。

## 安装

```bash
tar -xzf RemoteOps-MCP-macOS-arm64-0.2.0-preview.4.tar.gz
cd RemoteOps-MCP-macOS-arm64-0.2.0-preview.4
chmod +x install-remoteops-mcp.sh test-remoteops-mcp.sh uninstall-remoteops-mcp.sh
./install-remoteops-mcp.sh \
  --relay relay.example.com:7443 \
  --owner-id '<controller-owner-uuid>'
```

公网 CA 证书使用 macOS 系统可信根。私有 CA 增加：

```bash
--ca-cert ./relay-ca.pem
```

没有 CA PEM、但已经独立核对叶证书指纹时使用：

```bash
--tls-fingerprint '<64 位 SHA-256 指纹>'
```

两种信任方式只能选择一种。安装器会隐藏读取 Controller Token，并保存到当前用户的 macOS Keychain，服务名为 `RemoteOps Controller Token`。Codex 配置只指向本地启动脚本，不包含 Token。

默认安装位置：

```text
~/.codex/remoteops/remoteops-controller-mcp-0.2.0-preview.4
~/.codex/remoteops/launch-remoteops-controller-mcp.sh
~/.codex/remoteops/controller-config.json
~/.codex/config.toml
~/.agents/skills/remoteops/SKILL.md
~/.codex/skills/remoteops/SKILL.md
```

审计日志和文件交换目录位于：

```text
~/Library/Application Support/RemoteOps/audit.jsonl
~/Library/Application Support/RemoteOps/transfers
```

## 验证

```bash
./test-remoteops-mcp.sh
```

没有网络条件时先运行：

```bash
./test-remoteops-mcp.sh --skip-network
```

随后完全退出 Codex，再重新打开并输入 `/mcp`。应看到已启用的 `remoteops`。新建任务后直接使用自然语言：

```text
这是 RemoteOps 控制码 123-456-789。请检查现场电脑的网络和指定进程，只进行只读诊断。
```

## 更新和卸载

同一个公开版本的首发候选反复安装时，安装器会覆盖当前二进制并保留审计和文件交换目录。公开版本号只在实际 GitHub Release 时递增，内部修复不制造新的公开版本序列。

卸载：

```bash
./uninstall-remoteops-mcp.sh
```

卸载器会备份并删除 Codex 中的 RemoteOps MCP 配置段，删除 MCP、skill 和 Keychain Token，但保留审计日志与文件交换目录。

## 故障排查

- `/mcp` 看不到 `remoteops`：完全退出 Codex，包括仍在运行的后台进程，再重新打开并新建任务。
- 提示无法读取 Token：重新运行安装器，不使用 `--skip-token-prompt`；不要把 Token 写到 shell 历史或 `config.toml`。
- Relay 不可达：使用 `nc -G 5 -vz relay.example.com 7443` 检查网络、防火墙和端口。
- 证书不受信任：公网证书检查域名与系统时间；私有证书使用 CA PEM 或独立核对后的固定指纹。
- macOS 阻止未签名二进制：在确认下载来源和 SHA-256 后，通过系统设置对该文件进行一次性允许；不要全局关闭 Gatekeeper。

