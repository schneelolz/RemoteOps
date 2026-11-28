# 浏览器适配防误配与全组件交付

日期：2026-11-06。用户批准代码修复、生产中继更新、指定工具登记修正、本机 pi/Codex 升级、Y 盘发布及定制 Agent 双目录交付。现场 Agent 不代为关闭或启动，最终现场测试由用户完成。未执行 Git 写操作。

## 问题与修改

生产只读检查确认：`agent-browser` / `0.41.2` 包摘要为 `499683b78ec6dc3b57e0ba096eed4e26924f67400399585da65378fb36ba7b0a`，登记 argv 为 `mcp --tools core`，但浏览器路径非空、`browser_adapter` 为空，触发旧 Playwright 参数注入。该包与本地真实回归包一致。

- `remoteops-tool-center` 管理写入：新增或修改非空浏览器路径的 STDIO MCP 启动配置，必须显式指定适配模式，缺失返回 `browser_adapter_required`。旧数据库及目录加载、旧启动配置的纯说明修改保持兼容，不按文件名推断工具。
- `remoteops-resources`、Agent 错误转换：诊断新增可选 `launch_context`，记录接口、最终模式、来源、适配器追加参数及环境变量名。路径使用固定占位符，不含原始登记参数和变量值；CLI 版本检查与浏览器探针分别记录对应上下文。旧诊断可读取，保留错误码、重试和清理语义。
- 后台工具详情显示登记及生效模式，旧默认 Playwright 规则明确警告；编辑器提供四种启动示例、提交前检查并保留草稿。
- MCP 工具说明、公开资源协议、Schema、Skill 与独立安装器版本同步。数据库格式仍为 2，线协议仍为 19。

## 本地验证

本批材料：`C:\Temp\RemoteOps-adapter-jlai12-20261106`。

- 相关六组件 Rust 测试通过；最终资源库单独复验 27 项通过、1 项真实包测试默认忽略，真实测试已单独执行通过。
- 固定 agent-browser 0.41.2 MCP/CLI、Playwright MCP 0.0.84：真实页面打开、标题、URL、截图、表单及会话隔离、未知结果不重放、进程和 profile 回收通过。
- 受影响五组件 `cargo clippy --all-targets -- -D warnings`、工作区 `cargo check --workspace --all-targets`、`cargo fmt --all -- --check` 通过。首轮新增测试过长导致 Clippy 失败，拆分为独立行为测试后通过；首轮示例新增显式字段使通用 fixture 继承了不适用的模式，修正 fixture 初始化后复验通过，未放宽产品约束。
- `node --test tests/relay-admin-tools.test.cjs`：9 项通过。官方 MCP SDK/Ajv Schema：15 项通过。独立真实回环 MCP→Relay：10 项通过，默认/测试工具数 44/46。
- 独立 Skill 安装与旧布局兼容：23 项通过。
- 隔离真实后台验证全局说明、权限交互和适配提示；桌面 1440×900、手机 390×844 四张适配截图已实际查看，无文档横向溢出。旧目录示例只注入隔离页面用于展示，管理约束另由真实工具中心测试覆盖。

截图：桌面警告（本机归档 `output/playwright/browser-adapter/desktop-adapter-warning.png`）、手机警告（本机归档 `output/playwright/browser-adapter/mobile-adapter-warning.png`）、桌面编辑（本机归档 `output/playwright/browser-adapter/desktop-adapter-editor.png`）、手机编辑（本机归档 `output/playwright/browser-adapter/mobile-adapter-editor.png`）。

## 构建与部署

| 组件 | 实际结果 |
| --- | --- |
| 生产 Relay | `0.2.0-preview.15`，healthy、重启 0，系统信任及主机名校验通过，TLS 1.3 |
| 本机 pi/Codex MCP | `0.2.0-preview.24`，配置更新、双独立新进程验证通过；宿主尚未重开 |
| 独立 Skill | `0.1.5`，标准 `.agents`、自定义 CodexHome、pi 三处均为 8 个受管文件及安装收据 |
| 定制 Win32 Agent | `0.4.0-jlai.12`，公共 Agent `.35`；已交付，未运行现场连接 |
| Y 盘发布 | MCP `.24` / Skill `0.1.5`，独立包、组合包及清单齐全，61 项摘要通过 |

公共内容哈希 `7d1d98f4549a0bf756ae4c1660ecbf639f8314b20a1ace7cc562813276c762f2`；产品内容哈希 `0357ec161c692c4fab7c2d34031a50bf202afc8d462ac2af271263b78ee805a0`。运行、资源构建及配套程序均来自同一快照，不修改定制正式固定 Git rev。最终 MCP Schema 15 项、独立真实接入 10 项、Windows 安装器 50 项（3 环境跳过）、发布组合 13 项均通过。候选产品测试 11 项、无窗口检查 181 项/86 子进程、图标 30 项/7 帧通过。

生产镜像 `remoteops-relay:0.2.0-preview.15-p19-jlai12-20261106`，Linux 二进制 SHA-256 `5520cb026827f878237a7ae8bd1e31a214b2e46e51f828bc94510596cb511cd3`。部署前停服完整备份，隔离无网络副本验证格式 2→2，工具、全局说明、安装登记及授权保持；数据完整性 ok，仍有 6 个 MCP 客户端和 1 项工具管理授权。原运行用户、端口及挂载保持。

仅通过管理 API 将已确认包的 `browser_adapter` 从缺失改为 `agent_browser`，目录修订 8→9。包、argv、路径和开关保持，其他工具未改；启动定义改变对应的旧平台证据清空并标 pending，没有重用旧的通过状态，也没有启动现场工具。同步采样：264 个已知节点，20 个在线，其中 2 个已同步、18 个待同步，244 个离线待同步；不据此声称用户目标节点已应用修订 9。

日志采样存在旧受管 Agent 协议/上下文不足和客户端 Hello 不完整两类接入错误，没有其他错误类别或 panic；不能宣称零错误。接入实现 `relay.rs` 与前批 `.14` 快照摘要一致，没有为旧节点放宽协议或授权。

中继完整备份及部署失败回滚脚本：`/opt/remoteops/backups/remoteops-before-preview15-p19-jlai12-20261106`；原登记及请求/响应的受限备份也在此目录。配置、状态和诊断原文不复制到发布包。部署失败回滚脚本仅用于本批失败恢复；后续回滚需先保存最新数据并核对并发变更，不能直接覆盖后来产生的数据。

本机备份：`D:\App\AppData\CodexData\.codex\remoteops\backups\adapter-jlai12-20261106`。两宿主只改变 RemoteOps command，所有身份配置字节、凭据、其他 MCP 和超时保持。新版路径为 `protocol19-preview24-f8e734d0f487-a36bd84940cb`，MCP EXE SHA-256 `f8e734d0f487ba739e964845103465e768d4cbc7991b78535512c8d2d99df555`；候选双程序可信摘要自检通过。Skill 旧目录由独立安装器保存在技能搜索路径之外。

外盘固定入口 `Y:\MCP\RemoteOps`；旧发布完整备份 `Y:\MCP\RemoteOps-backups\20261106-103630-d23ae68b`，62 个旧文件逐项摘要一致。

Agent 两份均为 11,279,360 字节，SHA-256 `5d0f5c22ca49a9a56fe22e72c9c2c6a3670f303e30fe005c6529e4d701d3b574`：

- `C:\Users\ywb\Desktop\RemoteOps\0.4.0-jlai.12\精灵AI-Agent-Win32-0.4.0-jlai.12.exe`
- `Z:\RemoteOps\0.4.0-jlai.12\精灵AI-Agent-Win32-0.4.0-jlai.12.exe`

旧 `.7/.8/.9/.10/.11` 共 10 份 EXE 摘要保持。归档：`D:\Projects\Other\RemoteOps-JLAI\artifacts\win32-single\0.4.0-jlai.12-20261106`，包含源码快照、锁、许可、构建清单、配套程序、独立包和验证材料，最终摘要清单为 `SHA256SUMS-final.txt`。

## 验证命令与交接

主要命令及日志：

- `cargo test -p remoteops-resources -p remoteops-tool-center -p remoteops-agent -p remoteops-relay -p remoteops-controller-mcp -p remoteops-application --lib --bins` → `adapter-all-tests.log`；最终资源测试 → `adapter-resources-final.log`。
- `cargo test -p remoteops-resources real_candidates_headless_navigation_state_isolation_and_unknown_results -- --ignored --nocapture` → `adapter-real-candidates.log`。
- `cargo clippy -p remoteops-resources -p remoteops-tool-center -p remoteops-agent -p remoteops-relay -p remoteops-controller-mcp --all-targets -- -D warnings`、工作区 check、fmt → 对应 `adapter-*.log`。
- `node --test tests/relay-admin-tools.test.cjs`、`node --test tests/mcp-tool-schemas/contract.test.mjs`、`node scripts/test-tool-center-guide-browser.cjs` → 对应前端、Schema、截图日志。
- `Test-McpSetupInstallers.py --platform windows --powershell pwsh --helper <最终MCP>`、`Test-RemoteOpsSkill.py --package <最终Skill> --powershell pwsh --python-only`、`Test-McpSkillDelivery.py --mcp <最终包> --skill <最终包> --powershell pwsh` → 安装及组合测试日志。
- 定制 `Build-JlaiWin32Candidate.ps1`、`Test-JlaiWin32Candidate.ps1`、`Test-JlaiExecutableIcon.ps1` → 构建、无窗口及图标材料；独立 MCP 生产只读报告为 `production-mcp-codex.json` / `production-mcp-pi.json`。

用户现场测试：完全退出重开 Codex，pi 重开或 `/reload`；现场退出旧 Agent，运行 `.12`，重新连接后核对公共 Agent `.35`、浏览器适配能力、目录应用修订 9 和显式 `agent_browser`。随后测试启动、页面操作；失败时检查 `details.diagnostics.launch_context`、阶段、退出码及 stderr。控制码以实际界面为准。现场仍为待验证。

## 审查与边界

主会话复核了管理层新旧配置区分、修订与权限复用、执行定义变化的测试证据失效、安全诊断字段及 CLI 阶段、旧目录读取和诊断透传。两次独立子代理均因 `SESSION_ID_CONFLICT`（缺少可靠且一致的 thread-id 与元数据）启动失败，未完成独立审查，不能记为通过。

本地真实回归不能代替用户现场测试。未验证真实 Linux/H3C、串口、Server 2016、DPI/RDP 或 macOS 安装；不替换当前现场进程，不保证控制码改变。新 Agent 为未签名候选。控制端宿主重开前不能声称本聊天已加载新版 MCP/Skill。
