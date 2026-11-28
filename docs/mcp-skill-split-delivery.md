# MCP 与 Skill 拆分交付记录

日期：2026-11-05。状态：实现、Windows 验证、本机配置切换和 Y 盘发布完成；当前宿主未重开。长期规则见 [独立发布规范](mcp-skill-release.md)，后续发布入口已登记到仓库根 `AGENTS.md`。

## 交付范围

- MCP `0.2.0-preview.20`，helper `0.2.0-preview.6`，协议 19。精简重复公共指引，工具参数和关键约束保留；`initialize.serverInfo.version` 改为从 Cargo 实际版本生成，已验证为 `.20`。
- Skill 首个独立版本 `0.1.0`，最终交付补丁 `0.1.1`，最低 MCP `0.2.0-preview.19`，协议 19。唯一源码 `deploy/skills/remoteops`；包含现场已验证的交换机 SSH 指纹核对与排障流程，不写入实际现场地址、账号或指纹。
- MCP 安装、升级、自检和卸载不再强制复制、覆盖或删除 Skill。Skill 可独立安装、更新、卸载，拒绝降级、同版异内容及本地修改，未版本化旧内容显式迁移并完整备份。
- 发布入口 `scripts/Publish-WindowsMcpDelivery.ps1` 同时生成 MCP、Skill、全套包及根 README／组件清单。`-SkillOnly` 校验复用原 MCP；发布在目标盘完整准备后再移动切换，持有发布锁，校验 manifest、内部摘要清单及 ZIP 逐文件内容，保留旧发布。
- Agent `0.4.0-jlai.9` 与 Relay `0.2.0-preview.12` 沿用既有产物；本次未构建或部署 Agent／Relay，也未改变协议、权限或凭据格式。没有执行 Git 写操作。

## 验证与状态

| 项目 | 实际结果 |
| --- | --- |
| `cargo check -p remoteops-controller-mcp` | 通过；仅更新 MCP 自身 Cargo 版本及锁文件中的对应记录 |
| `cargo test --locked -p remoteops-controller-mcp` | 74 通过、2 个既有原生凭据／Relay 集成用例默认忽略 |
| `cargo clippy --locked -p remoteops-controller-mcp --all-targets -- -D warnings` | 通过 |
| `cargo fmt --all -- --check` | 通过 |
| `Test-McpSetupInstallers.py --platform windows --powershell powershell.exe/pwsh.exe --helper <debug .20>` | 两种 PowerShell 各 48 项，OK；各 3 个环境／平台跳过；版本声明 2 项通过 |
| `Test-RemoteOpsSkill.py --powershell powershell.exe/pwsh.exe --package <Skill包>` | 最终 `0.1.1` 在两种 PowerShell 各 19 项场景通过，各套同时覆盖公共 Python 安装逻辑；摘要篡改、链接、升级、降级、本地修改、独立卸载、外部备份及失败回滚均覆盖 |
| Skill `quick_validate.py`、PowerShell AST、Python 编译、macOS `bash -n` | 通过；原生 macOS 安装未据此宣称通过 |
| `Test-McpSkillDelivery.py --mcp <隔离包> --skill <最终Skill包>` | 13 项通过：完整／仅 Skill 发布、备份、复制／移动失败、回滚及恢复失败保留证据、摘要损坏与降级拒绝 |
| `Build-WindowsMcpPackage.ps1 -OutputRoot <新目录> -ArtifactDirectory <新目录>` | `release-small` 静态 CRT 构建通过；版本、来源输入稳定性、许可证、路径脱敏和完整 ZIP 内容校验通过 |
| 最终 MCP Release／本机已安装 EXE 的独立 STDIO 进程 | `initialize` 实际版本 `.20`，44 工具、两项工具管理工具存在；原凭据有效、获授权只读目录读取通过；工具中心仍为空且全局关闭 |
| 已安装 MCP 自检 | 两 EXE 外部摘要、包清单、实际版本和系统凭据引用检查通过 |
| Y 盘发布及备份 | 52 个文件递归摘要通过，3 个 ZIP 摘要通过，升级前 `.19` ZIP 与旧摘要清单复核通过 |
| 配置已更新 | Codex TOML 与 pi JSON 仅 `remoteops.command` 改变，认证配置字节不变，其他 MCP、模式及超时保持 |
| Skill 已安装 | 标准、Codex 兼容目录及 pi 独立目录均为 `0.1.1`，受管文件摘要一致；旧未版本化目录完整备份 |
| 宿主重新加载 | 当前进程未中断、未重启；Codex CLI 完全退出重开后生效，pi 重开或 `/reload`。独立进程验证不能代替当前宿主重载 |

第一次 Release 编译成功后，来源稳定性门禁发现构建期间的安装脚本修正，拒绝生成发布包；保留 `release-build.log`。源码稳定后以新输出目录重新构建，最终日志为 `release-build-final.log`。这属于构建来源检查生效，不将被拒绝批次作为已交付产物。

## 产物与摘要

Y 盘入口：`Y:\MCP\RemoteOps\README.md`；根 `delivery-manifest.json` 和 `SHA256SUMS.txt` 分别记录独立组件与递归摘要。

- MCP 包：`MCP/0.2.0-preview.20/RemoteOps-MCP-Windows-x64-0.2.0-preview.20.zip`，SHA-256 `5a6a012381f89f646b5d75c3e4d6fc70a41a32d6f74640095c8e51a85f9884e6`。
- Skill 包：`Skill/0.1.1/RemoteOps-Skill-0.1.1.zip`，SHA-256 `9689265ae65e64a3b98e0ce9ac08df80867240ff763c3f707980be1f9a2f25dc`。
- 全套包：`Bundle/mcp-0.2.0-preview.20_skill-0.1.1/RemoteOps-Setup-Windows-x64-mcp-0.2.0-preview.20_skill-0.1.1.zip`，SHA-256 `6924cb1aedf6908d12981f39b331ff3d61b7f76f92955dfa61eab36f49899b95`。
- MCP EXE：`d68b8ba70b468b06fcd0db53d6c51e9626bd682b6b0674df5977cd1b52eb1b5b`；helper：`b6ee59a74b38ff84130f8ab27374b0d8d4931916a4d5d0a4de7faecdaafb77aa`。
- 编译输入清单：`release-final2/artifacts/source-inputs.json`，SHA-256 `448fc0c699d7c147c360a281fdacc6d10bd38cbc6f1e165a926114b262185e0f`；实际编译器、profile、锁文件摘要在相邻 `build-info.json`。本轮锁文件及所有输入逐字节归档到 `source-snapshot.zip`，摘要 `15279c62970813b787dc9783704b36a18341214dccb639686ac90c0093b885ef`，共 321 个文件。

本机程序：`D:\App\AppData\CodexData\.codex\remoteops\candidates\protocol19-preview20-d68b8ba70b46-3bda6cba097b\remoteops-controller-mcp.exe`。配置备份：`D:\App\AppData\CodexData\.codex\remoteops\backups\mcp-skill-split-20261105-1547`；仅保存本地配置，没有导出系统凭据。Skill 标准、兼容和 pi 目录的备份均位于其技能搜索目录之外的 `remoteops-skill-backups`。早期 `0.1.0` 的标准与兼容迁移备份也已完整移出搜索目录，避免旧 `SKILL.md` 重复发现。

Y 盘原 `.19` 发布完整备份：`Y:\MCP\RemoteOps-backups\20261105-154708-14fb78e1`；第一次拆分发布（Skill `0.1.0`）完整备份：`Y:\MCP\RemoteOps-backups\20261105-155714-c0217abb`。本轮准备、程序、测试及脱敏结果在 `C:\Temp\RemoteOps-split-20261105`，持久归档复制到工作区 `artifacts/mcp-skill-split/20261105`，以归档自身清单为准。

## Skill 补丁的独立更新实证

末轮发现 `0.1.0` 将旧目录备份放在 Skill 搜索目录内，可能重复发现旧指引。按独立版本规则发布 `0.1.1`，没有改写同版内容。备份、暂存和失败证据改到目标目录祖父目录下固定 `remoteops-skill-backups`，预检确保位于所有目标搜索目录之外；新增完整备份可恢复、多目标升级、提交失败回滚及外部存储链接拒绝测试。

实际运行 `Publish-WindowsMcpDelivery.ps1 -OutputRoot 'Y:\MCP\RemoteOps' -SkillOnly -SkillPackageDirectory <0.1.1包目录>` 完成生产发布。更新前后 MCP 包内全部文件摘要一致；Codex TOML、pi JSON 及身份配置字节一致；本机三个独立 Skill 安装均为 `0.1.1`，受管摘要通过。没有调用 Cargo、安装 MCP 或重启任何宿主。

最终补丁源码单独归档到 `skill-0.1.1-source.zip`，不会冒记为旧 MCP `.20` 的编译输入。`evidence/skill-only-final.json` 保存脱敏结果；Skill 最终日志位于 `skill-agent/final5/tests-powershell51.log` 和 `tests-pwsh7.log`。归档中的 `delivery-current` 为最终 Y 盘完整副本，早期 `delivery` 保留历史发布。

## 平台边界与回退

此前已完成一台现场 H3C 交换机的主机指纹核对及 MCP 密码 SSH `display version` 验收；本轮没有重放现场认证，不能把旧现场结果写成新 `.20` 现场实测。Linux SSH、新 macOS 原生安装、交互 PTY 和其他厂商交换机仍待验。没有上传、启用或执行工具中心工具。

回退时分别处理组件：本机恢复备份配置中的旧 MCP 路径并重开宿主，沿用原凭据；Skill 恢复已核对的完整备份；Y 盘恢复完整旧发布目录。默认安装／发布拒绝意外降级，回退必须明确操作目标与备份。已用隔离故障注入验证发布失败恢复；没有在生产环境制造故障或执行实际回退。
