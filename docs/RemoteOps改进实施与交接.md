# RemoteOps 改进实施与交接

本轮按已确认的 R01—R04 计划实施。基线日期：2026-11-05。开始时 19 个文件有用户未提交改动，基线补丁保存在忽略目录 `artifacts/improvements-baseline`；全部保留，不进行 Git 写操作。

## 实施与验收状态

| 项目 | 当前状态 | 验证边界 |
|---|---|---|
| R01 资源仓库和两种浏览器工具 | 实现、本机 headless 验证及最终包复验完成 | 双来源 TLS、安全边界和两个真实后端已通过；交互桌面及跨 Windows 平台待验收 |
| R02 串口重复显示 | 实现及本地回归完成 | 三条显示路径、累计更新及 egui 有界队列通过；真实 COM 和窗口截图待环境 |
| R03 SSH 排查 | 审查及 Plink 本机环回完成 | 17/17 环回通过；无 PTY 单次 exec，交换机、服务器及整链路仍待验收 |
| R04 密码窗口语言 | 实现及本地回归完成 | 目标语言同步、旧端回退及文案通过；真实双语窗口因桌面锁定未验收 |

## 环境与恢复

真实 COM、代表性交换机、服务器与 Windows 平台矩阵尚未提供。它们只阻塞对应验收，不影响本地代码和环回测试推进。现场密码只能通过凭据提示器采集，不进入对话、日志或仓库。

资源候选版本固定；上游依赖或包不可获取时保留脱敏错误证据，不生成虚假的摘要或通过记录。继续任务先读本文与本地工作区差异，再补对应环境并执行未完成项。

## 验证记录

最终代码验证已完成，日志位于忽略目录 `artifacts/improvements-verification`。定向证据见 [串口与原生资源站验证记录](串口显示与原生资源站验证记录.md)、[SSH与密码语言验证记录](RemoteOps-R03-R04验证记录.md) 和 [资源工具验证记录](resource-tools-validation.md)。

| 最终检查 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过，`fmt-verified.log` |
| `cargo check --workspace --locked` | 通过，`workspace-check-verified.log` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过，`clippy-verified.log` |
| `cargo test --workspace --locked` | 947 通过、0 失败、6 忽略，63 个测试套件；`workspace-test-verified.log` |
| Windows MCP 安装脚本隔离回归 | 40 项运行无失败，3 项跳过；版本模板另 2/2 通过 |
| 标准 JSON Schema 自检及两个清单 | 首轮 jsonschema 4.23.0 通过，`schema-validation.log`；最终资源清单另以 4.25.1 Draft 2020-12 复核通过 |

6 个默认忽略项分别为 3 个交互桌面/控制台测试、2 个原生凭据库写入测试、1 个真实资源候选测试。最后一项另行使用含许可证的最终 ZIP 实际运行，两个后端均通过（325.52 秒）；其余环境型跳过项保持未验收。

## 当前实现与使用入口

- R01：新增 `crates/remoteops-resources`，共享清单、HTTPS来源门禁、固定包缓存与两种后端；Agent本地配置键为 `resource_tools`。egui设置菜单和两种Win32界面有来源入口，默认关闭。规范见 [公开资源仓库协议](公开资源仓库协议.md)。
- MCP新增 `list_resource_tools`、`start_resource_tool`、`call_resource_tool`、`stop_resource_tool`，同时检查当前Relay欢迎消息和目标能力。启动、每次调用均按高风险授权；外部Human审批入口支持精确资源动作。
- Controller CLI新增 `resource-list/start/call/stop`。需要连续调用时用现有交互模式保持连接：`resource-start <目标> <工具ID> <版本> <摘要> --approve`；`resource-call <目标> <实例> <工具名> <JSON参数文件> --approve`。一次性CLI退出可能撤销会话，不应靠它持有长期浏览器状态。
- R02：显示旁路按稳定记录累计更新，三条窗口路径及egui活动摘要均接入；原始串口输入、远程输出和查询数据不变。
- R03/R04的证据、命令与恢复步骤见 [SSH与密码语言验证记录](RemoteOps-R03-R04验证记录.md)。

## 审查修复

独立审查补齐：空白清单地址不报告启用；串口空白边界不跨段合并；egui摘要按结构化累计记录更新，长正文截断不影响次数；资源调用取消及传输失联保留“结果未知、禁止自动重放”语义，请求标识与参数摘要用于核实。egui 两段无界事件转发改为复用既有有界订阅、锁外重绘唤醒；资源清单重定向使用最终 URL 解析相对包地址，非法来源白名单直接拒绝。会话启动取消隔离与未知实例名额回收已补回归。应用层按原请求类型保留资源失败详情，明确启动前失败可供 AI 选择其他候选；传输丢失和 Agent 主动报告的未知结果均保留原请求、实例/工具和参数摘要，不自动重放。

## 平台及现场边界

本机Windows build 26200。当前Windows桌面锁定（LockApp），既有门禁拒绝真实密码窗口与原生窗口截图；没有绕过门禁。已异步请求用户解锁，未得到可用桌面前该项保持未验收。无界面浏览器候选测试单独记录，不等同于交互桌面通过。

真实COM、交换机、服务器、Windows 10与Server 2016及以上平台矩阵仍需对应环境。未完成项不能标为已修复或已通过。

## 本轮候选版本

| 产物 | Cargo 版本 |
|---|---|
| Agent | `0.2.0-preview.31` |
| egui Agent GUI | `0.2.0-preview.33` |
| 原生 Agent | `0.5.0` |
| Agent Service | `0.2.0-preview.15` |
| Controller MCP | `0.2.0-preview.18` |
| Relay | `0.2.0-preview.11` |
| credential-prompt / Controller CLI / Controller GUI | `0.2.0-preview.6` |
| protocol crate | `0.2.0-preview.7`，线协议仍为 v18／兼容 v17 |
| example-assist / example-service-agent | `1.1.0` |

Windows 和 macOS MCP 安装模板同步本轮 MCP 版本；`python scripts/Test-McpSetupInstallers.py --check-versions` 2/2 通过。发布产物门禁夹具 `pwsh -NoProfile -File scripts/Test-ReleaseArtifactGate.ps1` 通过，包含错误版本、摘要、大小及缺少 askpass 等拒绝路径。Windows 正式候选在独立构建目录编译，最终文件版本、摘要和许可证以生成后的 `manifest.json` 与实际门禁结果为准。

## 最终审查补充

- GUI 正常停止改为等待后台线程实际退出后再关闭窗口；订阅在 shutdown 后可能丢弃 Stopped 事件，因此用线程完成状态作为清理完成边界，并在等待期间显示中英文状态。合成后台线程回归已通过，未代替真实窗口验收。
- 资源取消后的临时目录清理任务由共享收集器追踪，停止会话和 Agent 退出等待其有界完成；未清理返回 `cleanup_pending`。断线/撤销记录固定脱敏诊断，显式停止/关闭将失败返回调用方，紧急停止不会把清理失败写成成功。
- Playwright 启动健康检查包含独立 profile 的固定 `about:blank` 探针；两种后端均在首次用户操作之前验证浏览器可启动。
- Windows 启动契约拒绝批处理，保持原生可执行文件及参数数组；MCP 接收循环区分迟到响应和服务端请求。

最终工作区检查以 `artifacts/improvements-verification/*-verified.log` 为准，最后清理修复已纳入上述 947 项通过结果。资源真实候选的独立运行证据与全工作区默认测试分别统计。

临时脚本字节码 `scripts/__pycache__/build-resource-packages.cpython-315.pyc` 的清理被自动审批拒绝（`blocked by policy`），暂留本地，不属于候选包内容。

## 最终交付目录

- Windows x64 程序候选：`artifacts/release/r01-r04-20261105/windows-x64-verified`；11 个主程序 Cargo/PE 文件版本、SHA-256 与清单一致，617 个 Rust 依赖许可证及随包 Plink 已生成，发布门禁与路径脱敏检查通过。构建日志 `build-windows-verified.log`。
- MCP Windows 安装候选：`artifacts/release/r01-r04-20261105/mcp-verified/RemoteOps-MCP-Windows-x64-0.2.0-preview.18.zip`；SHA-256 `fcaa0f8cb0845036c5a87889b62554a331ce39e8392a679bbf1683fb69395e35`。安装目录、ZIP 与 Windows 发布目录 MCP 摘要一致，helper 摘要一致，许可证与路径脱敏门禁通过。
- 静态资源候选站点：`artifacts/resource-candidates/final-site`，入口 `remoteops/v1/index.json`，仅包含本次清单引用的两个最终 ZIP。含许可证的最终两包严格回归均通过；两次独立打包清单、大小和 SHA-256 一致，详见资源分记录。
- `windows-x64`、`windows-x64-final` 和资源 `site` 中的历史包是中间产物，不作为本次交付。

资源清单默认保留 `requires_desktop=true` 及平台 `pending`，Server 2016 为 `unsupported`。当前 headless 测试显式在测试内部放行，不能照抄该放行到正式配置；正常启动必须等待实际交互平台通过。资源站默认关闭，尚未部署到外网；部署前需填写发布者、配置 HTTPS 和本地来源白名单。

## 剩余验收及恢复顺序

1. 解锁本机普通交互桌面，运行 R04 窗口脚本并复核中英文、空输入、取消、提交与记忆；补串口和资源设置截图、DPI 与键盘交互。
2. 提供真实 COM、代表性交换机和一台服务器，按分记录验证连接、流式输出、断线及进程清理。交换机若需要 PTY、交互 Shell 或旧算法，先采证再建立具体兼容任务。
3. 在目标 Windows 版本逐组合验收两浏览器，完成后仅将实际通过的 build 范围标为 passed；macOS 密码窗口需单独环境。
4. 提供 HTTPS 发布地址，发布 final-site 并实际从该地址验证下载和运行；不以本机 TLS 夹具冒充外网站点部署。

本轮没有提交、创建分支、合并或推送。用户既有改动保留，基线和本轮日志可用于后续核对。

最终文档链接检查已覆盖 83 个 Markdown 文件；`git diff --check` 通过，132 份既有源码行尾与实施前快照一致。Git 提示部分文件下次写入会按其 autocrlf 设置转换，该提示不代表本轮已转换行尾。PowerShell 隔离验证还在仓库下产生 `Microsoft/Windows/PowerShell/ModuleAnalysisCache` 缓存，保留本地，不属于源码或候选包。

Linux Relay 候选补充构建及门禁通过：发布脚本最初缺少 Zig，已将固定 `ziglang==0.13.0` 安装至本机 Temp 独立目录，未新增项目依赖。执行 `scripts/Build-LinuxRelay.ps1 -SkipClean -ZigPath <临时zig.exe> -OutputDirectory artifacts/release/r01-r04-20261105/linux-x64-verified`，目标 `x86_64-unknown-linux-gnu.2.36`，版本 `0.2.0-preview.11`，11661960 字节，SHA-256 `393ebecee93aa04612063a701ae217deecb01b086dca39b8cd382457031c67ce`；许可证、摘要与路径脱敏门禁通过。日志为 `build-linux-verified.log`、`linux-final-gate.log`。Zig 链接器提示忽略旧优化参数 1，不影响成功退出；未扩大修改既有发布配置。本机 WSL 未配置，尚未执行 Linux 运行验收。

MCP 安装脚本按单独 Cargo 特性图构建，与多程序联合构建的 MCP 二进制摘要不同。沿用现有安装打包流程，将其正式安装版同步至 Windows 候选并更新清单，随后两端门禁重新通过（`windows-final-gate.log`、`mcp-final-consistency.log`）。Windows 最终 `manifest.json` 摘要为 `f54f1215186ea510c059ed909db1a19857d9d94a49e8b688d5fbbe88ccb5c32c`。

发布根目录额外提供 `交付说明.md` 和 `交付清单.json`，集中列出最终目录、版本与摘要；不会将中间构建包混作最终候选。Linux Relay 候选已生成，但 Linux 实际运行仍是独立待验收项。

## 最终资源复验结论

含许可证的最终两 ZIP 已完成严格 headless 实测：1 个集成测试通过、0 失败，325.52 秒；两个后端均验证导航、快照、表单交互、跨会话拒绝、未知结果不重放、profile 与进程回收。日志 `artifacts/resource-candidates/final-real-test.log`（SHA-256 `71b2a1ffc215b18dc1bba5c8d0280761ac80c0b6b7e74302081789221c6566a9`），机器记录 `final-verification.json`。

最终验证曾发现测试核实器将仍可查询但 `HasExited=true` 的 Chrome 对象误计为存活。补齐 Windows 长路径前缀归一、真实退出状态和有界等待后，Playwright 与 agent-browser 均确认真实存活数为 0；证据与旧失败保留在资源分记录。仅修正测试辅助逻辑，没有修改运行时代码或放松持续存活断言。修正后资源默认测试 12 通过、1 忽略，资源 all-targets clippy（`-D warnings`）和全局 fmt 再次通过，产品候选无需重建。
