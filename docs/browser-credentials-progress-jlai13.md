# 浏览器凭据、平台验证与资源准备进度交付

本批为协议20配套改进，目标版本：Relay preview.16、公共Agent preview.36、MCP preview.25、凭据弹窗preview.7、Skill 0.1.6、定制Win32 Agent 0.4.0-jlai.13。

## 行为及接口

- 平台状态为verified/unverified/failed/unsupported。compatible保留为正常启动验证资格，不是实际兼容性的否定判断；精确build证据独立保留，headless不替代桌面测试。
- start_resource_tool支持validation_mode与wait_for_completion；受控测试不写入通过记录。get_resource_tool_status查询同一原请求，任务最长15分钟，MCP短等待不重放启动。
- 进度由Agent准备链路产生，同时进入Agent主窗口、观察窗和MCP任务状态；下载按实际字节计数，校验/解压/启动独立展示，健康检查通过才就绪。MCP通知显示受宿主支持影响，查询始终可用。
- fill_browser_password仅接受会话、实例、预期来源、主文档唯一密码选择器及用户名。密码由本机原生弹窗获取，按独立HPKE用途加密，绑定一次页面/字段，不进入公开参数、argv或文件。不缓存凭据、不自动提交。
- 首版支持agent-browser 0.41.2的MCP/CLI与Playwright MCP 0.0.84主文档；跨域iframe、MFA、验证码、Basic Auth不在本批范围。
- 新Relay接入17/18/19客户端；新操作仅对p20及显式能力开放，新事件不转发旧控制端。现有p19 SSH密文保留独立兼容读取，网页密文不回退旧用途。

## 验收与交付记录

本次真实日志保存在C盘本批验证目录。构建成功、文件交付、配套部署、宿主重载及用户现场测试分别记录；未验证的平台和现场业务不冒记通过。定制Agent由用户退出旧版后自行启动。

## 本批实际交付结果

- 生产Relay：`0.2.0-preview.16` / 协议20，healthy、重启0，公网TLS1.3系统信任及主机名校验通过。完整备份为`/opt/remoteops/backups/remoteops-before-preview16-p20-jlai13-20261106`；隔离副本格式2→2核验通过，数据库integrity均为ok。目录修订保持11，工具、权限、启用状态、端口及挂载保留；没有扩大平台验证范围。
- 日志仍有旧受管Agent上下文不足及不完整Hello错误；本批采样其他错误0、panic0，不能称生产零错误。
- pi/Codex及默认Codex入口已更新为MCP `0.2.0-preview.25`，helper `0.2.0-preview.7`。身份配置字节、凭据引用、其他MCP及超时保留；两个独立新进程均发现46个默认工具并完成生产只读snapshot。当前宿主未重开，不声称本聊天已加载新版。
- 独立Skill `0.1.6`已安装到标准`.agents`、自定义CodexHome及pi三处，原目录由独立安装器备份在搜索目录之外。
- `Y:\MCP\RemoteOps`已发布MCP `.25` / Skill `0.1.6`的独立包与组合包，61项摘要通过；旧发布62个文件完整备份并逐项核验。备份目录：`Y:\MCP\RemoteOps-backups\20261106-155612-381858c6`。
- 定制Agent `0.4.0-jlai.13`：11,352,064字节，两份SHA-256相同：`88bec345a8857b7e5eecef53761247a7c5f790e753d72ed582ef310b4ef9c2ee`。
  - `C:\Users\ywb\Desktop\RemoteOps\0.4.0-jlai.13\精灵AI-Agent-Win32-0.4.0-jlai.13.exe`
  - `Z:\RemoteOps\0.4.0-jlai.13\精灵AI-Agent-Win32-0.4.0-jlai.13.exe`
- 源码及配套材料归档：`D:\Projects\Other\RemoteOps-JLAI\artifacts\win32-single\0.4.0-jlai.13-20261106`。未执行Git提交、分支或推送，保留原未提交改动和旧Agent文件。

## 验证命令与结果

本批日志根目录：`C:\Temp\RemoteOps-browser-jlai13-20261106`。

- `cargo test`：Agent 156项、MCP 87项、Relay 127项、协议22项及凭据弹窗2项通过；桌面宿主45项、应用层52项、资源库32项、工具中心17项通过。默认忽略项仍单独列明；真实浏览器测试已显式运行，不以ignore替代验收。
- `cargo test -p remoteops-resources real_candidates_headless_navigation_state_isolation_and_unknown_results -- --ignored --nocapture`：三个真实固定候选均通过，覆盖初始化、导航、快照、表单、密码填写、错误站点、重复绑定、字段替换、脱敏、未知结果及进程/profile清理；见`real-browser-final.log`。
- `node tests/mcp-tool-schemas/live.mjs ... --agent ...`：完整MCP→Relay→Agent真实回环通过，实际上传/下载225,329,722字节包，观察download/verify/extract/starting进度和5条标准进度通知；查询不重启。通过合成密码取消、加密填写、只填不提交、随后单独提交登录及Agent/audit无明文检查；见`browser-live-4.json`。原生弹窗与加密链路分别验证：回环使用隔离合成密码helper，原生helper另做真实窗口测试。
- 受影响组件`cargo clippy --all-targets -- -D warnings`、`cargo check --workspace --all-targets`及`cargo fmt --all -- --check`通过。Linux编译仍有两个既有的非Windows警告，未称跨平台Clippy通过。
- 官方MCP SDK/Ajv Schema 15项、最终Release MCP回环10项通过；Agent候选无窗口181项/86子进程、图标30项/7帧通过。
- Windows安装器50项中3项因环境跳过；一项在编译负载时30秒超时，单独复验通过，其他用例通过。独立Skill 23项通过，组合发布13项在短临时目录下通过；用户Temp深路径首次触发Python Windows路径枚举/清理问题，保留原始失败证据，Y盘实际完整备份及摘要另行验收通过。
- SSH及网页原生密码弹窗中文/英文、空密码、取消及单次填写通过；SSH记忆选项回归通过。最终Release helper网页窗口检查通过。只输入合成测试密码，截图不含真实密码。
- 后台真实隔离Relay桌面1440×900、手机390×844截图与交互通过，已查看代表截图；原生主窗口代表截图已查看。旧全窗口相册脚本在日志窗截图处失败，不记整套相册通过；任务进度原位更新另有宿主单测和真实远程进度回环覆盖。

## 构建来源与复核边界

公共底座内容摘要：`1e0c77782712f7d3ff0f23b23899c8b9a7975e33d1851ca8ae4fed9e1b4dac16`；定制产品内容摘要：`96ce0b017d89ad0cc31a86e9c7bfa86a1005ab29d0cec0e2feffb83cb56450bb`。程序均由同一内容快照及锁文件生成，不更改定制正式固定Git rev。最终MCP包采用该快照的程序，安装脚本及README在修正helper版本后单独记录模板摘要。快照后的Rust差异仅为进度单元测试补分号，不改变生产编译内容；详见`post-snapshot-differences.json`。

本机备份：`D:\App\AppData\CodexData\.codex\remoteops\backups\browser-jlai13-20261106`；Skill备份由各独立安装器另记。生产回滚脚本保留在本批完整备份内；后续若有新数据，回滚前必须重新保存并核对，不直接覆盖后续写入。

## 用户现场验收

1. 完全退出并重开Codex；pi重开或`/reload`，开启新会话加载Skill。
2. 现场退出旧Agent，启动桌面或Z盘中的`.13`，重新连接并核对协议20及`browser_password_v1`、`resource_tools_progress_v1`、`resource_tools_validation_v1`能力。
3. 未验证平台通过显式`validation_mode=true`测试，启动使用`wait_for_completion=false`并查询原请求，观察Agent和MCP的同一进度；不把测试启动自动登记passed。
4. AI填写用户名后调用`fill_browser_password`，在控制端本机窗口核对站点并输入密码；AI收到filled后单独点击登录。目标变化应拒绝，取消应不填写。

定制Agent为未签名候选，真实路由器登录、用户现场Agent显示/网络、Server2016、DPI/RDP及macOS真实安装均未据本机测试宣称通过。没有启动或替换用户现场Agent进程。

## 涉及源码

| 功能 | 主要文件 |
| --- | --- |
| 平台状态、证据与受控启动 | `crates/remoteops-resources/src/platform.rs`、`manifest.rs`、`runtime.rs`；`crates/remoteops-tool-center/src/store.rs` |
| 网页密码适配与输出过滤 | `crates/remoteops-resources/src/browser_password.rs` |
| 凭据用途、操作与进度事件 | `crates/remoteops-protocol/src/credential.rs`、`message.rs`；`crates/remoteops-domain/src/event.rs`；`crates/remoteops-policy/src/lib.rs` |
| Agent执行与下载进度 | `apps/remoteops-agent/src/lib.rs`、`runtime.rs`、`tool_transport.rs`、`operation_log.rs`；`crates/remoteops-resources/src/cache.rs`、`progress.rs` |
| 本机弹窗、MCP任务与查询 | `apps/remoteops-credential-prompt/src/main.rs`；`apps/remoteops-controller-mcp/src/main.rs`、`browser_tools.rs`；`crates/remoteops-application/src/lib.rs` |
| Relay门禁、旧端兼容与后台 | `apps/remoteops-relay/src/relay.rs`、`relay_tools.rs`；`relay-admin-prototype/tools.js` |
| Agent界面 | `crates/remoteops-agent-desktop-host/src/lib.rs`、`runtime.rs`；`apps/remoteops-agent-native-prototype/src/activity_view.rs`、`style_preview/observer_data.rs`、`style_preview/readonly_view.rs` |
| Skill与交付 | `deploy/skills/remoteops`；Windows/macOS MCP安装自检脚本；组件Cargo版本及锁文件；JLAI候选Cargo模板 |
| 自动化验收 | 上述模块测试、`tests/mcp-tool-schemas/browser-live-support.mjs`、`live.mjs`、`contract.mjs`、`scripts/Test-CredentialPromptWindow.ps1`及安装器/发布回归脚本 |

最终归档559个文件已生成摘要；最终增加此交接文件索引后重新核对清单。原生界面库最终回归123项通过，格式检查通过。
