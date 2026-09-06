# Linux Headless Agent 验收报告（2026-09-06）

## 结论

Ubuntu 24.04.4 LTS x86_64 测试机已安装并运行 `remoteops-agent` `0.2.0-preview.6`，使用专用 `remoteops` 用户和 systemd。生产 Relay 连接在线，MCP 已在本机安装同源构建。

## 证据

- Linux 工作区：`cargo fmt --check`、`cargo check --workspace --locked`、Clippy `-D warnings`、Workspace tests 全部通过；测试机日志为 0 failed。
- MCP/Relay/Agent 隔离全链路：50 项通过。覆盖配对、审批拒绝、完全控制、POSIX `/bin/sh`、UTF-8/ANSI、实时输出、持久 Shell、进程与 systemd 服务、journald、2 MiB 文件哈希往返、覆盖保护、目录穿越和 symlink 防护、TCP、PTY 串口、SSH 目标注入拒绝、权限失败、Agent/Relay 重启恢复、短暂断网、在途写操作不重放、SIGTERM/SIGINT、审计脱敏。
- systemd 生命周期：安装、enable/active、运行用户、私有状态文件、SIGKILL 自动重启、优雅停止、卸载、配置与身份保留、重装恢复均通过。
- 发布包：两个 x86_64 ELF、systemd unit、安装/卸载/状态脚本、配置模板、第三方许可证、依赖清单、manifest 和 SHA-256 清单均通过。
- 现有 Windows/Mac 回归：本机 Workspace tests 262 passed；Windows 交叉编译未在 macOS 完成（缺少 MSVC SDK），由 CI Windows job 继续负责。

## 交付范围

首版只承诺 Ubuntu 24.04 x86_64、glibc、systemd；不承诺 RHEL/Rocky/Alma、Fedora、SUSE、Alpine、ARM、桌面 GUI 或屏幕输入。Linux 默认低权限运行；systemd、电源和系统日志按实际权限返回明确失败，完全控制不会变成 root。

测试证据保存在测试机 `artifacts/acceptance/`，发布归档位于测试机 `artifacts/release/0.2.0-preview.6/linux-x64/`。测试机配置基线以 root-only `/var/backups/remoteops/delivery-baseline.tar.gz` 保存；没有可用的 VMware 主机快照 API，因此未伪造虚拟机快照。
