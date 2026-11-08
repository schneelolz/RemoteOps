# 工具中心配套上线与定制交付

日期：2026-11-06。用户明确授权更新生产中继、本机 Codex/pi MCP，并将公共代码同步进定制款后按原位置交付；随后授权将 MCP 和 Skill 发布到 Y 盘。未执行 Git 写操作、未改现场 Agent 安装或启动状态。

## 版本与来源

| 组件 | 实际结果 |
| --- | --- |
| 生产 Relay | `0.2.0-preview.14`，协议 19；工具数据库格式 1→2 |
| Windows MCP | `0.2.0-preview.23`，密码 helper `0.2.0-preview.6` |
| 独立 Skill | `0.1.4`，三处均有 8 个受管文件，最低 MCP `.23` |
| 定制 Win32 单 EXE | `0.4.0-jlai.11`，公共 Agent `.34`、desktop-host `0.3.2`、Win32 `0.5.2` |

构建批次：`C:\Temp\RemoteOps-tool-guide-20261106`。`Build-JlaiWin32Candidate.ps1` 冻结的公共输入摘要 `84d4d0e382c8630db74636554112a6596f1f739f030e84f63a71923b322a4679`；产品输入摘要 `0b906139e5f714497524e3e6a1215db469a409d316770f42b819aea87bb2905f`。运行、资源构建及配套程序均来自同一快照。定制项目只更新候选薄入口版本和说明，正式固定 Git rev 未改，不复制历史公共目录。

Linux Relay 目标 `x86_64-unknown-linux-gnu.2.36`，SHA-256 `9da7f46b9f45787faf720ea222e6631d089e4401e303bcac354b40af7fa7a92e`。Windows MCP EXE SHA-256 `7e2591d376069f7c25768dbdb7914161ab38e9e96ad57e75ec778a3b4097771e`。

## 中继更新

按 [remoteops-relay-admin 技能](C:/Users/ywb/.agents/skills/remoteops-relay-admin/SKILL.md) 和私有运行手册，经固定主机密钥 SSH 更新。镜像 `remoteops-relay:0.2.0-preview.14-p19-jlai11-20261106`，ID `sha256:b7421077c9e9f16c8d99729dad2c33d7f5cb25fe77d0747af16f3e8c62657290`。

停服后完整备份 Compose、环境和数据卷，包括 JSON、SQLite、WAL；仅留服务器 `/opt/remoteops/backups/remoteops-before-preview14-p19-jlai11-20261106`，目录 700、机密及 data.tar 为 600。先在无网络、无发布端口的数据副本上验证迁移，再仅重建 Relay。回滚脚本恢复完整旧数据和镜像配置，不删除数据卷，保留故障材料。

迁移后 MCP 登记库仍格式 2，6 个客户端和管理授权数 1 保持；工具库为格式 2，两个工具的资料摘要、目录修订 8、运行开关开启均保持，各库完整性 ok。运行用户、端口、挂载保持；healthy、重启 0、健康接口和公网 7443 系统可信 TLS 1.3 校验通过。

日志有“受管 Agent 需要新版协议及目标上下文”“读取客户端 Hello 失败”两类接入错误，采样无其他错误类别或 panic，不能声称零错误。接入实现 `relay.rs` 与旧 `.13` 快照摘要完全一致，未为这些客户端放宽协议或授权。本轮未执行现场工具。

## 本机更新

备份：`D:\App\AppData\CodexData\.codex\remoteops\backups\tool-guide-jlai11-20261106`。Codex 使用 `Install-RemoteOpsMcp.ps1 -UpgradeExisting`，pi 用结构化 JSON 修改，仅更新 RemoteOps command，指向 `protocol19-preview23-7e2591d37606-88c66fdb13c4`。身份配置字节、系统凭据、其他 MCP、参数、审批及超时保持。

默认 `C:\Users\ywb\.codex` 实为指向自定义 CodexHome 的 junction，已共用新版配置，无需重复登记。pi 原 `--config` 路径保留。标准 `.agents/skills/remoteops`、自定义 CodexHome `skills/remoteops`、pi `agent/skills/remoteops` 均升级至 Skill `0.1.4`，旧目录由独立安装器备份至搜索路径之外。

两条配置分别启动独立新 MCP 连接生产，完成 `.23` 初始化、44 工具严格发现及工具库只读 snapshot，全局指引字段存在、两个工具及开关保持；验证进程均关闭。**当前宿主未重启，不能声称当前聊天已加载新版。Codex 须完全退出重开，pi 重开或 `/reload`。**

## 定制交付

- `C:\Users\ywb\Desktop\RemoteOps\0.4.0-jlai.11\精灵AI-Agent-Win32-0.4.0-jlai.11.exe`
- `Z:\RemoteOps\0.4.0-jlai.11\精灵AI-Agent-Win32-0.4.0-jlai.11.exe`

两份均 11,276,800 字节，SHA-256 `c33664434a029c875dd245313b2ec51fc31e8bfb396931f650bf8d5487c093d8`。旧 `.7/.8/.9/.10` 八份摘要保持；新目录不替换配置或日志，仍为未签名候选。

归档：`D:\Projects\Other\RemoteOps-JLAI\artifacts\win32-single\0.4.0-jlai.11-20261106`，含公共/产品源码、锁、许可、官方扫描器 ZIP、单 EXE、配套程序、Linux Relay、MCP/Skill 包和验证材料。安装器竞态修复在冻结后完成；包清单另记实际模板摘要，不冒记 EXE 来自修改后的源码。

## 外盘发布

使用 `scripts/Publish-WindowsMcpDelivery.ps1`，以本批次已验证的组件包为输入，发布至 `Y:\MCP\RemoteOps`，未重新编译或更改本机安装。MCP 为 `0.2.0-preview.23`，Skill 为 `0.1.4`；独立 MCP、独立 Skill、全套包、根 README、组件清单及 SHA-256 清单齐全。

旧目录完整备份至 `Y:\MCP\RemoteOps-backups\20261106-082827-5452b303`，52 个旧文件逐项摘要一致。发布脚本退出码 0，两组件包验证通过，根摘要清单 61 项逐项验证通过。发布日志和校验记录为 `C:\Temp\RemoteOps-tool-guide-20261106\publish-y.log`、`y-publish-verification.json`。

## 验证与限制

- 候选 fmt/check/test/clippy/Release 通过，产品测试 11 项；无窗口检查 181 项、86 子进程，未观察主窗口、未请求网络；图标 30 项、7 帧；快照脚本 10 项通过。
- Linux Release、路径及许可检查通过；存在既有 unused/dead_code 和链接器提示，未声称 Linux 严格 Clippy 通过。
- 最终 Release MCP 的 SDK/Ajv Schema 15 项、隔离真实 MCP→Relay 10 项通过，默认/测试工具数 44/46。
- Windows 安装器最终 50 项完成、0 失败、3 环境跳过。初轮离线 `--inspect-codex` 进程快速退出与空白 stdin 写入竞争，改为空输入关闭管道后完整重验通过。
- 独立 Skill 安装、升级、旧 3/7 文件布局兼容等 23 项通过；补齐 `browser-startup.md` 受管文件，实际三处均安装完整。
- 发布组合测试 13 项通过。首轮长临时路径导致深层复制/清理失败，失败证据保留；仅将临时根改为 `C:\Temp` 后完整通过，未放宽断言。
- 已安装 MCP 使用明确候选目录和外部可信双 EXE 摘要自检通过，版本、helper、清单、44 工具和凭据存在性通过。首次默认根目录自检不适用于并排安装，其失败记录保留。

本轮源码补充修改为安装器版本常量、升级目录名、离线 stdin、Skill 受管文件列表、定制候选版本和相应测试文档。未修改用户工具登记或全局选择内容。

现场仍需运行 `.11` 验证实际连接及新增能力；真实 Linux/H3C、串口、Server 2016、DPI/RDP 和 macOS 未验。独立子代理因会话元数据冲突未启动，主会话已专项复核，不能记为独立审查通过。
