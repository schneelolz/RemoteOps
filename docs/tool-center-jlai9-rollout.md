# 定制 Agent 与工具中心配套上线记录

日期：2026-11-05。已完成定制 Win32 候选、Windows MCP 和生产 Relay 配套更新。保留原有未提交改动，未执行 Git 写操作，正式薄产品的固定公共 Git rev 未变。

## 实际结果

| 项目 | 实际版本与状态 |
| --- | --- |
| 定制 Agent | `0.4.0-jlai.9`，单 EXE，未签名候选；desktop-host `0.3.1`、原生界面 `0.5.1` |
| Windows MCP | `0.2.0-preview.19`，密码输入辅助程序 `0.2.0-preview.6`，协议 19 |
| 生产 Relay | `0.2.0-preview.12`，健康，最终复核重启次数 0；公网 7443 系统可信 TLS 1.3 验证通过 |
| 工具管理权 | 仅为当前 Codex/pi 共用安装身份授予；其他五个安装身份的权限保持，数据库中授权总数 1 |
| 生产运行策略 | 全局关闭、目录修订 0、已登记工具 0；没有上传或运行工具 |
| 宿主状态 | 配置已更新，独立新版进程验证通过；当前 Codex/pi 未由本任务重启，重开后生效 |

两条 Win32 设置路径均移除旧资源站地址、白名单及资源保存/停止/重连入口，显示中英文只读说明。旧配置字段仍兼容保留；`SaveResourceTools` 在真实和演示宿主均明确拒绝，不改变配置、权限和快照状态。

Windows 安装器新增 `-UpgradeExisting`，复用原连接配置和系统凭据，完整程序并排安装。MCP 的 `--update-codex-command` 用 TOML 解析结果定位现有字符串，仅替换 `remoteops.command` 的字节范围，保留参数、超时、审批及其他 MCP。pi JSON 经结构化修改后验证仅该节点的 command 变化。没有重新登记或导出系统凭据。

## 来源与生成物

公共源码内容摘要：`43773c74096a63875f05f2319c169519e7bfda86732aa1df19e32623b3f4388f`，213 个输入文件。产品源码摘要：`0cc41c59d0a24e9fad6f6ccdab146d3b8bfab9624a63f0f7277dd5ffdbd869fc`，6 个输入文件。公共 HEAD 只作为来源参考，不能代表未提交内容；完整逐文件清单、两个 Cargo.lock、编译器信息及许可证均在归档中。

Agent、Windows MCP/辅助程序、Windows 配套及 Linux Relay 均从同一份 `build-final/base` 快照构建。快照包括后台、MCP 模板、skill、构建脚本和测试资源样例。修正 Linux 构建脚本，使产物路径遵循 Cargo 实际 target_directory，避免设置 `CARGO_TARGET_DIR` 时取错旧文件。Windows 使用静态 CRT 与 `release-small`；Linux 使用既有 Zig 0.13.0，目标 `x86_64-unknown-linux-gnu.2.36`。生产容器实际 glibc 2.36，宿主机为 2.32。

| 文件 | 字节数 | SHA-256 |
| --- | ---: | --- |
| `.9` 单 EXE | 8449536 | `97eab0bf5c6389ecf58b9fefea9e4c4a8e4b99532ff553fd64abb61356424a1d` |
| MCP EXE | 9814528 | `35da0e06633a4839fafab0d60970efa59e7016bd8f41e9cc66d60c9546a6bf9f` |
| 密码输入辅助程序 | 766464 | `e4453bc1164bad8502af8bcaf34acd56a466eba178c50537f0c14e48e1aa8b64` |
| Linux Relay | 16480216 | `38a5a44b41ae7f40cf045f75c9c048ffc090e7ea3d6a6339db0370ac52617c2a` |
| MCP 完整 ZIP | 4713068 | `59a6ec055bbe0258ec6339779566e0ed414e90f3aafae37c989eab39041b5fb7` |
| 源码 ZIP | 1315207 | `5fae31f5bd06279fac3bfb7bad724faa713936fd2068006b4129fcc2b82289a3` |

交付位置：

- `C:\Users\ywb\Desktop\RemoteOps\0.4.0-jlai.9` 与 `Z:\RemoteOps\0.4.0-jlai.9`，各仅含一个同摘要 EXE；旧 `.8/.7` 四份摘要复核未变。
- 本机 MCP：`D:\App\AppData\CodexData\.codex\remoteops\candidates\protocol19-preview19-35da0e06633a`。Codex 与 pi 共用此程序及已有安装凭据。
- 本机配置备份：`D:\App\AppData\CodexData\.codex\remoteops\backups\protocol19-preview19-jlai9-20261105`，不进入交付归档。
- Y 盘：`Y:\MCP\RemoteOps`，含 `.19` 完整 ZIP、程序、脚本、skill、说明、许可证及递归清单；不含运行配置或凭据。原目录 17 份文件完整备份并验证至 `Y:\MCP\RemoteOps-backups\preview19-jlai9-20261105`，旧 `.16/.17` ZIP 已移入该备份。
- 本地完整归档：`D:\Projects\Other\RemoteOps-JLAI\artifacts\win32-single\0.4.0-jlai.9-20261105`。

## Relay 迁移与恢复材料

镜像：`remoteops-relay:0.2.0-preview.12-p19-jlai9-20261105`，ID `sha256:64508b0eb8b9d34baf33adc556b2479bbf0b52bae42a2f71d203542c0c7f2bf1`。仅更新 `remoteops-relay`，保持原运行用户、数据卷、TLS 和端口。

正式停服后备份 Compose、环境文件及完整数据卷，备份目录为 `/opt/remoteops/backups/remoteops-before-preview12-p19-jlai9-20261105-retry1`。目录权限 700，环境及数据 tar 权限 600；备份仅在远端保存，含摘要和完整标记。

无网络、无公开端口临时容器从备份副本启动新版，MCP SQLite 从 v1 迁到 v2，六个客户端和六个登记记录数量保持；迁移后所有工具管理授权默认 0。工具库新建后为空且关闭，所有数据库完整性检查通过。随后才替换正式环境文件中的镜像并重建单服务。最终业务复核显示 21 个在线 Agent、3 个会话和 27 个 Controller，未发现重启循环。

首轮预检失败原因是宿主 Python SQLite 不支持 `STRICT` 表；该阶段未改正式镜像，旧服务已恢复健康。失败日志及首轮备份留远端。重试改用与产品锁文件完全一致的 bundled SQLite，只读校验程序在无网络容器中以原运行 UID 执行；该辅助程序源码、锁文件和构建日志另行归档，不改变产品源码输入。

回滚入口为远端 staging 中 `relay-rollout.py rollback`。脚本先核验一致数据 tar 摘要及完整性，停止新版，将故障数据移入保留目录，再恢复完整升级前数据、Compose 与环境文件，重建旧镜像并检查健康。由于数据库迁移，不能只回退镜像。正式回滚未实际触发；不要将脚本审查和备份验证称为实际回滚演练。

## 验证命令与结果

- `Build-JlaiWin32Candidate.ps1 -BaseSourceRoot D:\Projects\Other\RemoteOps -WorkDirectory C:\Temp\RemoteOps-jlai9-p19-20261105\build-final -OutputDirectory C:\Temp\RemoteOps-jlai9-p19-20261105\delivery-final -TargetDirectory C:\Users\ywb\AppData\Local\Temp\RemoteOps-size-20261030-target`：候选格式、检查、11 项产品测试、严格 clippy、Release 及 Windows 配套构建通过。
- 快照内 `cargo test --offline --locked -p remoteops-agent-desktop-host -p remoteops-agent-native-prototype -p remoteops-relay -p remoteops-agent -p remoteops-application -p remoteops-controller-mcp -p remoteops-protocol -p remoteops-tool-center -p remoteops-resources -p remoteops-enrollment-store --lib --bins`：652 通过，0 失败，4 项环境用例默认忽略。
- 快照内 `cargo fmt --all -- --check`、`cargo check --offline --locked --workspace --all-targets`、`cargo clippy --offline --locked --workspace --all-targets -- -D warnings`：通过。
- `node --test tests/relay-admin-tools.test.cjs`：5 项通过。
- `Test-JlaiWin32Candidate.ps1`：170 项无窗口检查通过，84 次诊断子进程；`Test-JlaiExecutableIcon.ps1`：30 项通过、7 帧解码；`Validate-ToolCenterCandidate.ps1`：56 项来源与产物检查通过。
- `Test-JlaiWin32BuildScript.ps1`：10 项隔离检查通过；`Test-JlaiProduct.ps1`：14 个公共包仍固定既有 rev。正式产品根 Cargo.lock 和产品 Cargo.toml 摘要保持。
- `Test-McpSetupInstallers.py --platform windows --powershell powershell.exe --helper <最终MCP>`：版本静态 2 项通过，安装器 42 项完成，OK；3 个跳过为 macOS-only 及两个受 Windows symlink 权限限制的子用例。目录 junction 拒绝测试通过。
- Linux `Build-LinuxRelay.ps1 -SkipClean -ZigPath <既有Zig> -OutputDirectory <本批目录>`：通过，容器内实际版本与上传摘要一致。已有 Linux 专属 unused-mut/dead-code 及 Zig 旧优化参数警告保留在日志，Windows 工作区严格 clippy 无警告。
- MCP 包 `Test-ReleaseArtifacts.ps1` 许可证、路径与 ZIP 一致性检查通过；本机 `Test-RemoteOpsMcp.ps1` 使用两个可信摘要检查候选并验证系统凭据/TCP，通过。Y 盘递归摘要最终复核通过。
- 生产独立 STDIO 初始化、`tools/list`：44 个工具，包含 `manage_relay_tools`、`upload_relay_tool_package`；授权前管理请求被拒绝，授权后只读 snapshot 成功。旧 Codex `.17` 与 pi `.14` 独立进程仍可认证和只读接入。
- 新版隔离 Agent 真实连接生产中继后，后台确认 supported/online/synced 均为 true、applied_revision=0；没有配对、上传或工具执行。隔离 Agent 已退出，其私有状态不进入归档。
- 最终数据库只读复核：MCP schema=2、tool_management 授权数=1，工具库 enabled=false、tool_count=0，所有 quick_check=ok；健康、公网 TLS、重启数、挂载和用户复核通过。

源码回归覆盖旧字段往返、旧动作拒绝且文件字节/权限不变、中英文说明、授权/拒绝、上传与登记分离、停用、取消、断线、未知结果、迁移默认拒绝及权限持久化。首轮缺失测试样例与 clippy 注释问题已修正后重新冻结并验证，失败日志保留，未用失败结果冒充通过。

## 验收边界与生效说明

真实 `.9` 中文浅色设置窗口已观察到管理说明且没有旧资源编辑入口。随后用户按物理 Esc 停止 Computer Use，本任务未继续界面输入；暗色、英文交互、键盘循环及真实 DPI/缩放布局没有完成本轮验收，截图未持久化，不能声称明暗主题截图验收通过。无窗口诊断覆盖与真实交互检查分别记录。

MCP EXE 的 CLI/PE 版本确为 `.19`，但既有 `initialize.serverInfo.version` 常量仍显示 `.5`，这是当前源码的展示问题；本批用 EXE 版本、摘要及实际工具发现确认身份，没有把该字段误记为 `.19`。

交互桌面工具、其他系统/架构、真实跨机工具执行、弱网及满 1 GiB 压力仍待验；生产只验证授权管理读取、关闭策略与目录同步。没有在生产上传、启用或执行工具。权限数据库事实证明持久化，但本批未为单独验证授权而再次重启生产 Relay。

配置更新不代表当前宿主已经切换：完全退出并重开 Codex；pi 重开或 `/reload` 后生效。单独 reconnect 不会重读 pi MCP 程序路径。
