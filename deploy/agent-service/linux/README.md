# Linux Headless Agent

首版基线：Ubuntu 24.04 x86_64、systemd、glibc。Agent 与 Service 为 `0.2.0-preview.6`，沿用线协议 v14，兼容 `0.2.0-preview.5` Relay/MCP。其他发行版和 ARM 尚未验收。

## 安装

运行时需要 `ca-certificates`、`libudev1`、`procps` 和 systemd；SSH 功能需要 `openssh-client`。安装和状态脚本使用 Ubuntu 自带的 Python 3；Agent 本身不依赖 Python。

1. 解压后执行 `sha256sum -c SHA256SUMS`。
2. 复制 `agent-config.example.json`，填写真实 Relay 地址；公网 CA 保留 `ca_cert: null`，私有 CA 配置已核验的绝对路径并确保 remoteops 用户可读。Owner/Controller Token 只配置在 MCP，不写进 Agent。
3. 执行 `sudo ./install-remoteops-agent.sh /absolute/path/agent-config.json`。
4. 执行 `sudo ./status-remoteops-agent.sh` 查看 Relay 连接状态。服务 active 只表示进程存在；状态应为 `waiting` 或 `controlled` 才表示已连上 Relay。
5. 执行 `sudo ./status-remoteops-agent.sh --pairing` 在本机读取有效配对码。配对码只保存在权限 0600 的短期状态文件，不写入 journald。

升级时执行 `sudo ./install-remoteops-agent.sh`，保留现有配置和身份。卸载使用 `sudo ./uninstall-remoteops-agent.sh`，删除程序和服务，保留用户、配置和数据，便于重装恢复。

## 路径与运行方式

- 服务配置：`/etc/remoteops/agent-config.json`
- 身份与传输目录：`/var/lib/remoteops/agent-state.json`、`/var/lib/remoteops/transfers`
- 临时状态：`/run/remoteops-agent/runtime-status.json`，systemd 自动创建/清理目录。
- 日志：`journalctl -u remoteops-agent --no-pager`，记录状态变化和错误，不记录配对码。
- 用户前台运行无需 sudo：`./remoteops-agent --relay HOST:PORT --state-file "$HOME/.local/state/remoteops/agent-state.json" --transfer-root "$HOME/.local/share/remoteops/transfers"`。Ctrl+C 和 SIGTERM 都会清理会话。

## 能力与权限

支持 `/bin/sh` 一次性/持久 Shell、实时输出、文件分块与哈希、进程与 systemd 服务结构化列表、TCP/SSH、串口枚举以及身份恢复。协议中的 `cmd` capability 是既有“平台 Shell”标识；Linux 环境画像实际 Shell 为 `system`，不会启动 Windows CMD。

发行版和工具画像报告 systemctl、journalctl、apt-get/dnf 的可用性。日志和额外诊断使用受现有 MCP 审批保护的 Shell；未额外增加新协议工具。GUI、PTY 全屏应用、桌面输入控制和截图不在本版范围。

默认专用 remoteops 用户没有 root 权限。完全控制仅影响应用内审批，不会赋予 Linux root 权限；系统服务修改、电源控制和读取系统日志可能返回 permission denied。需要这些权限时，由管理员用 polkit 为指定 unit/操作配置规则；安装器不会赋予通用免密 sudo。journalctl 只能读取运行用户获准访问的日志。

## 构建与验收

在 Ubuntu 24.04 安装 Rust（不低于仓库 MSRV）、build-essential、pkg-config、libudev-dev，然后执行 `bash scripts/Build-LinuxAgent.sh`。产物包括两个 ELF、安装/卸载/状态脚本、unit、配置模板、许可证和 SHA-256 清单。

先执行 `cargo build --locked -p remoteops-relay -p remoteops-controller-mcp`，再以普通用户运行 `python3 scripts/Test-LinuxAgentE2E.py --output artifacts/acceptance/linux-e2e.json`。它创建本机隔离 Relay，不会停止正式 Relay。`sudo bash scripts/Test-LinuxService.sh /path/to/package` 仅供专用测试机使用，会卸载重装正式 Agent，并创建待清理的服务控制测试 fixture。发布验收报告单独记录原生测试、MCP 调用、服务恢复与权限结果。
