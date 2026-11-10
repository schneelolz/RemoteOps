# 全局指引与 TCP 转发交付记录

日期：2026-11-08。公共 RemoteOps 工作区完成全局指引增强和持续 TCP 转发实现，构建并验证 MCP、Skill、Relay 和管理前端交付材料。两个工作区的既有未提交改动和未跟踪文件均保留，未执行 Git 提交、分支、推送或清理。

## 版本与产物

| 组件 | 版本 | 结果 |
|---|---|---|
| Controller MCP | `0.2.0-preview.26` | Windows 包生成并发布到 `Y:\MCP\RemoteOps` |
| Relay | `0.2.0-preview.18` | Linux x64 产物生成，未切换生产服务 |
| Agent | `0.2.0-preview.37` | 公共工作区构建通过 |
| Relay Admin | `0.1.2` | 前端构建产物生成，未切换生产服务 |
| Skill | `0.1.7` | 独立包生成并发布到 `Y:\MCP\RemoteOps` |
| JLAI Win32 | `0.4.0-jlai.14` | 候选生成并复制到桌面和 Z 盘 |

MCP ZIP SHA-256：`07db9b9bb30335c4de48799e0ac4a2c567b30aed1efd0ff0e39195ec29492291`。

Skill ZIP SHA-256：`b8f4aef18c307056aca1c89964145d9beab5c779f15c135e22eb8963431bd4ab`。

Relay Linux 二进制 SHA-256：`cf711c61a9b60c97e3bd4e2bd30ca74a75f7846ad8a41a0054f902c2645acc84`。

MCP/Skill 交付备份：`Y:\MCP\RemoteOps-backups\20261108-134456-869b7690`。

本批临时交付目录：`C:\Temp\RemoteOps-delivery-20261108`。

## 公共验证

- `cargo test --workspace --locked`：工作区测试通过。关键结果包括 Agent 156 项、Relay 128 项、Protocol 23 项、Application 52 项，其他包和文档测试均通过。存在既有真实桌面/真实资源候选等 ignored 用例，未将其计为现场通过。
- `cargo check`：Agent、Relay、Controller MCP、Application 通过。
- `npm test`：Relay Admin 16 项通过。
- `npm run build`：Relay Admin TypeScript 和 Vite 构建通过。
- `Build-RemoteOpsSkill.ps1`：Skill 包和摘要生成通过。
- `Build-WindowsMcpPackage.ps1`：MCP 包、版本、许可证、路径清理和 ZIP 校验通过。
- `Build-LinuxRelay.ps1`：Relay Linux x64 Release 构建、版本、许可证和路径校验通过。

## JLAI 候选

- 构建目录：`C:\Temp\RemoteOps-JLAI-j14-20261108-133131`。
- 公共输入摘要：`118c4f3d40fffeb55a677db87d3735b6ebca5a9bc5c11cc20bcf5f659b60c395`。
- 公共 HEAD：`2df104e07f155c0b189be31bf139e2c4a43ee65b`，`base_dirty=true`。
- JLAI HEAD：`431987ce574051e30769932362f256ede70cb463`，`product_dirty=true`。
- 候选 SHA-256：`5e154d86c1f18812553e6b0402fe5d7898327720bf48715f2317c2b0fb1c47f5`。
- 无窗口检查 181 项通过，构建脚本 47 项和 Win32 隔离检查 10 项通过。
- 详细记录见 JLAI 仓库 `docs/JLAI-Win32更新与测试产物-0.4.0-jlai.14.md`。

## 生产 Relay 状态

当前环境没有 Docker、生产主机 SSH 入口或已配对的 RemoteOps Controller 连接，无法执行生产 Relay 停服、备份、staging、切换或健康验收。生产更新脚本已审查，但没有把“产物已生成”写成“生产已上线”。

生产切换仍需使用现有 `scripts/relay-admin-rollout.py` 类似流程，先备份 Compose、环境文件、Relay JSON、门户 SQLite/WAL、工具中心数据库/WAL 和镜像信息，再在隔离副本检查完整性，最后执行 staging 和生产切换。当前 Relay 白名单配置需要增加 `REMOTEOPS_TCP_FORWARD_ALLOW=host:port,...`，未配置时 TCP 转发默认拒绝。

## 待回归

宿主重启或 `/reload`、JLAI `.14` 现场配对、全局指引刷新、TCP 浏览器 HTTP/HTTPS、SSH 主机密钥、RDP/NLA、真实 Relay 白名单和生产健康检查需要在生产连接可用后继续执行。当前未签名 JLAI 候选不能替代现场和签名验收。
