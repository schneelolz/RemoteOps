# 控制权限修复交付记录

日期：2026-09-29。本记录描述协议 16 控制权限修复的构建、部署与验收范围。

## 交付内容

- Agent GUI `0.2.0-preview.27`（内含 Agent `0.2.0-preview.26`）。
- MCP `0.2.0-preview.12`。
- Relay `0.2.0-preview.6`，包含 Windows x64 和 Linux x64 / glibc 2.36 产物。
- 协议版本 16，三端必须配套；旧组件不能因缺少授权字段获得权限。
- 批次目录：`artifacts/release/control-permissions-20260929`，内含各平台 `manifest.json`、总 `SHA256SUMS.txt`、`build-info.json` 及依赖许可证清单。
- 新 GUI 已覆盖 `C:\Users\ywb\Desktop\RemoteOps\remoteops-agent-gui.exe`，复制后 SHA-256 与发布包一致；目录内其余 4 个文件哈希未变。旧 GUI 已另存批次目录 `rollback-local/remoteops-agent-gui.previous.exe`。

## 修改范围

协议新增独立的会话授权、来源、连接代次和修订号；Relay 串行确认并广播，Agent 和 MCP 使用同一快照。旧展示字段不参与授权。Relay 与 Agent 执行前复核审批依据，旧授权不能在撤销后继续启动新操作。保留恢复快照入队顺序修复，并修复 Agent 等待本地事件时取消半帧读取的问题。

Agent GUI 增加现场控制权限、默认偏好、确认与失败提示，状态使用 14 逻辑像素。旧配置默认逐项确认；默认完全控制仅对新绑定自动授权；现场撤销后，本次进程不再自动授权。默认偏好保存到现有 AgentConfig，以获得可报告失败的原子保存；会话授权和抑制标记不落盘。

涉及 `crates/remoteops-protocol`、`crates/remoteops-application`、`crates/remoteops-i18n`、Agent、Agent GUI、Agent Service 事件匹配、Relay、MCP、专项测试、用户文档及 RemoteOps 技能说明。

## 已执行验证

| 验证 | 结果 |
|---|---|
| `cargo test -p remoteops-protocol -p remoteops-application -p remoteops-controller-mcp -p remoteops-agent -p remoteops-agent-gui -p remoteops-relay` | 194 项通过（18 + 15 + 27 + 49 + 30 + 55）；后续 Agent/GUI 修正重新运行相关测试通过 |
| `cargo check --workspace --all-targets --locked` | 通过 |
| `cargo clippy -p remoteops-protocol -p remoteops-application -p remoteops-controller-mcp -p remoteops-relay -p remoteops-agent -p remoteops-agent-gui --all-targets --locked -- -D warnings` | 通过 |
| `pwsh -NoProfile -File scripts/Test-ControlPermissions.ps1` | 协议模拟和真实 MCP/Agent 两部分通过；独立于持久 Shell 专项 |
| Windows Release 三端及辅助程序构建 | 通过，静态 CRT 和路径映射 |
| `scripts/Build-LinuxRelay.ps1 -SkipClean -OutputDirectory artifacts/release/control-permissions-20260929/linux-x64` | 交叉编译通过；链接器报告一个弃用优化设置警告；未在 Linux 运行 |
| 两个平台 `scripts/Test-ReleaseArtifacts.ps1 -RequireLegalFiles` | 通过 |

真实 MCP/Agent 专项执行了临时目录写操作，并核验磁盘结果：默认逐项确认拒写、MCP 完全控制同步且允许写、现场撤销后拒写、现场临时授权恢复写；默认完全控制首次和解绑重配自动授权，现场撤销后的解绑重配仍逐项确认。协议专项另验证旧修订请求拒绝及 TLS 断线重连清除临时授权。

原生 GUI 使用隔离 demo 配置验证中文浅色及英文深色主界面、权限窗口、默认授权确认、长文案滚动及连接不可用提示。实际点击保存后检查隔离配置，默认值已写入，当前来源未改变。四种语言/主题组合另有布局测试。截图与详细边界见 `outputs/control-permissions-qa/验收说明.md`。所有自有测试进程已结束；demo 未连接生产。

## 生产部署与现场验收边界

2026-09-29 只读核查确认生产 Relay 已运行 `0.2.0-preview.6` / 协议 16，容器 healthy、重启次数为 0、健康接口返回 ok；中继日志明确拒绝协议 15 Controller。本机 Codex 与 Pi 原先仍指向 9 月 22 日的旧 MCP，现已备份配置并改为 `0.2.0-preview.12`。两套启动参数均通过 initialize 与 tools/list 验证，返回 38 个工具；共享分发包包含程序、配置示例、安装说明、许可证与哈希清单。

真实 Codex MCP 已成功配对协议 16 Agent，通过 set_control_mode 获取 Relay 确认的 full_access，并成功执行两次只读内容的远程命令，复查权限仍为 synced。现场 GUI 文件版本为 preview.27，内含 Agent 上报 preview.26，这是独立组件版本，不要求相同。现场进程响应正常，最近两小时未检索到相关应用错误事件；该主机已安装 VC++ 运行库，不能代替无运行库环境验收。尚未在真实现场完成 GUI 撤销/默认偏好全部交互、网络中断重连和进程重启验收；相应状态和持久化规则已有组件测试。5 秒超时有 GUI 单测，但截图中的失败是 demo 无运行时即时失败。

后续再次部署仍需安排维护窗口：先备份生产 Relay 当前二进制、配置和状态文件；协议切换会中断全部旧协议 Agent/Controller 连接，需要配套重启三端。MCP 替换后须重启实际宿主连接，单替换磁盘文件不会更新运行中的进程。回滚需一起恢复旧 Relay/MCP/Agent，必要时使用部署前状态备份，避免混跑协议版本。本记录未核验生产回滚备份内容，不据此宣称回滚演练完成。

剩余验收应核对现场文案及可清理写操作；当前生产检查没有修改现场文件或重启现场程序，不宣称全部现场问题彻底修复。旧 `--command-mode full-access` 只保留权限上限，实际完全控制必须由 Relay 确认，详见 [控制权限与默认模式](control-permissions.md)。
