# 中继工具中心与 Agent 自动发现交付

日期：2026-11-05。本次保持全部已有未提交改动；不执行 Git 写操作，不部署或替换运行组件，不向生产中继上传包。

## 实现与运行规则

- Relay 工具中心使用状态文件同目录的 `tool-center/tools.sqlite3`，包单独放入 `tool-center/packages/<sha256>.zip`。全局功能与新登记工具均默认关闭。
- 管理后台增加工具中心：搜索、启用筛选、资料与说明、多个版本/平台包、ZIP 上传或 HTTPS 导入、测试记录、独立开关和节点同步状态。文件校验、平台测试与启用分别显示。
- MCP 客户端接入页独立授权 `tool_management`，旧数据库迁移后默认关闭，完全控制和旧共享 Token 均不能获得该权限。撤权与管理提交使用读写授权边界，撤权返回后不能继续提交。
- 工具管理与 Agent 会话无关；关闭运行功能时仍可维护。每个管理请求必须提供稳定 `request_id` 和 `expected_revision`，重复请求幂等，版本冲突拒绝覆盖。变更记录操作者、动作、结果及修订。
- 外链仅允许 HTTPS，支持签名查询参数；中继临时下载验证 ZIP、大小与摘要，不运行、不长期保存。列表、回执和 Agent 工具列表去掉查询参数。新地址需要重新导入校验，相同版本及平台不能更换包内容。
- 新版 Agent 从认证连接取得目录，不再读取本地资源站作为运行入口。旧配置字段保留读写兼容，不能绕过中继开关。目录记录精确提交事件，快速关闭再开启不会合并停用；Agent 同步发布门禁、先终止所有受影响空闲进程，再等待目录清理。同步失败不会显示已同步。
- 托管包在同一认证 TLS 连接中按 256 KiB 分块，下一偏移确认上一块；上传和下载各有独立状态与有界并发，下载不足一块时读到 EOF。包传输期间继续处理心跳和控制消息。
- 支持浏览器适配、标准 STDIO MCP 初始化/工具发现/调用，以及登记 JSON Schema 的 CLI 操作。CLI 使用包内原生可执行文件和 argv 数组，每次独立执行；返回退出码、stdout、stderr、文本或 JSON。工具调用继续遵循既有授权、平台与桌面门禁。
- 超时、取消、停用或断线使已提交调用无法确认时返回 `result_unknown`，不自动重放。Relay 立即拒绝停用工具的新启动和调用，Agent 回收进程树。

## 请求契约补充（MCP `0.2.0-preview.22`）

完整请求模型、默认值、服务端维护字段、CLI 参数映射及可解析 JSON 示例见 [工具中心请求契约](../deploy/skills/remoteops/references/tool-center-contract.md)。说明与示例随独立 Skill `0.1.3` 交付，最低 MCP 兼容版本仍为 `.21`；完整输入 Schema 与字段级本地诊断需要 `.22`。本次只交付源码、文档和测试，协议仍为 19，不包含生产登记、实际工具启用、安装或发布。以下既有交付与验证记录保留其原有版本和实测范围，不代表 `.22` 已部署。

## 接口

后台：`GET /api/admin/tools` 读取脱敏快照，`POST /api/admin/tools` 提交管理命令；复用管理认证、请求限制和 `no-store`。`PUT /api/admin/mcp/clients/{id}/tool-management` 独立授予/撤销权限。

MCP 新工具：`manage_relay_tools` 支持 `snapshot`、`set_policy`、`put_tool`、`set_tool_enabled`、`record_test`、`import_external`；`upload_relay_tool_package` 只接受本机 transfer-root 内 ZIP 相对路径，复用现有路径边界，逐块验证回执。不能通过通用管理工具内联上传任意 Base64。

管理请求示例：

```json
{"request_id":"unique-request-id","expected_revision":0,"command":{"action":"snapshot"}}
```

AI 工作流：实际测试 → 整理用途、说明、示例及执行定义 → 上传/导入 → 登记实际环境和结果 → 按用户指令启用。上传回执只证明文件安全检查与摘要校验完成，不表示程序可运行。

协议升级为 19，Relay 继续接受协议 17/18；仅协议 19 Agent 接收新目录消息。旧端集中执行明确要求升级。Relay `0.2.0-preview.12`、MCP `0.2.0-preview.19`、Agent 核心 `0.2.0-preview.32`、协议包 `0.2.0-preview.8`。JLAI 候选 `0.4.0-jlai.8`，保留 `.7`。

## 验证

- `cargo test -p remoteops-relay -p remoteops-agent -p remoteops-application -p remoteops-controller-mcp -p remoteops-protocol -p remoteops-tool-center -p remoteops-resources -p remoteops-enrollment-store --lib --bins`：489 通过，4 个环境相关用例默认忽略；其中真实双浏览器用例已另行显式执行通过。
- `cargo check --workspace --all-targets`：通过。
- `cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `cargo fmt --all -- --check`：通过。
- `node --test tests/relay-admin-tools.test.cjs`：5 通过。
- 超过 1 MiB ZIP 在实际认证协议帧流中逐块上传及下载，摘要一致；每块间心跳确认可达；独立/全局停用后下一下载块拒绝，撤权后上传块拒绝。此测试使用本机 duplex 传输，不声称跨机或公网 TLS 验收。
- 真实原生 CLI、通用 STDIO MCP，以及两个固定浏览器包的 headless 运行、状态隔离、取消和进程回收已验证。详见 [运行时验证](tool-center-runtime-validation.md) 与 [存储验证](tool-center-storage-validation.md)。两个用户给定 final-site ZIP 摘要已复核与实际测试包一致。
- 后台已完成 1440×900 和 390×844、明暗主题、焦点围栏、Esc、编辑与启停检查。截图见 `output/playwright/tools-center`。真实本机 Relay 已实际上传 192 字节说明 ZIP、登记/编辑工具、登记 pending 测试、切换全局及独立开关，最后 revision=7 且全部关闭；另实际注册 MCP 安装客户端并通过后台授予/撤销独立管理权。截图 `local-real-*` 对应真实 HTTP 服务；其余为 mock 预览。未执行包内程序。

## 验收边界

交互桌面浏览器、未测试 Windows build、其他系统/架构、真实跨机弱网和满 1 GiB 压力尚需相应环境验收。headless 测试未转换为平台通过。缓存与目录推送针对单 Relay 进程实例；多个进程共享同一工具库时不提供跨进程热通知。

候选主程序已构建，候选 `cargo test --lib` 11 项通过，候选格式/严格 clippy 通过；`Test-JlaiWin32Candidate.ps1` 首轮 159 项无窗口检查通过（最终带 manifest 为 169 项），实际 84 次诊断子进程、未观察到主窗口、没有请求 Relay 连接。通用 `Test-AgentGuiConsole.ps1` 的 help/version/无效配置 3 项通过，但其 `no-graphics-startup` 项依赖旧 GUI 的 `agent-gui-runtime` 日志，在 Win32 宿主不适用，未通过；采用上述真实 Win32 无窗口检查作为替代证据，未改写失败记录。

JLAI 现有正式薄产品检查通过（14 个公共包固定至既有 rev），构建脚本检查 47 项通过；该检查只确认正式产品边界，不能用作 `.8` 测试候选的来源证明。

## 候选产物及归档

- JLAI `0.4.0-jlai.8`：8,455,680 字节，实际文件/产品版本 `0.4.0-jlai.8`，协议 19；SHA-256 `e4f0ea64f611edaf713d9b65e72f7588eb96d4e303979ea53c0d5ab0ffec8f29`。
- Relay `0.2.0-preview.12`：11,121,152 字节；SHA-256 `797ba1e41c5139ffaa67c345f4d0278acfd0120f0a6b734ed36c88a77b24179d`。
- MCP `0.2.0-preview.19`：9,809,920 字节；SHA-256 `186a2e96eb4a0bb85affa7a8de6dd970278cc07f48ac459e182f59b93bac56ac`。
- 三者及其他配套均为未签名测试候选；Release 使用同一内容快照，正式薄产品固定 Git rev 不变。
- 构建命令：`Build-JlaiWin32Candidate.ps1 -BaseSourceRoot D:\Projects\Other\RemoteOps -WorkDirectory C:\Temp\RemoteOps-jlai8-tools-20261105\build -OutputDirectory C:\Temp\RemoteOps-jlai8-tools-20261105\delivery -TargetDirectory C:\Users\ywb\AppData\Local\Temp\RemoteOps-size-20261030-target`。候选及配套 `release-small` 均通过。
- `Validate-ToolCenterCandidate.ps1` 57 项通过：194 份底座、6 份产品、9 份额外后台输入逐文件摘要一致，产品依赖无锁文件漂移，协议/PE 版本、许可证及内嵌资源摘要符合记录。
- 最终带 manifest 的 `Test-JlaiWin32Candidate.ps1` 为 169 项通过（84 次诊断子进程）；`Test-JlaiExecutableIcon.ps1` 30 项通过，7 帧解码。
- 新 EXE 已分别放入 `C:\Users\ywb\Desktop\RemoteOps\0.4.0-jlai.8` 和 `Z:\RemoteOps\0.4.0-jlai.8`，两份摘要一致；旧 `.7` 两份摘要保持原值。
- 最终归档：`D:\Projects\Other\RemoteOps-JLAI\artifacts\win32-single\0.4.0-jlai.8-20261105`，含 EXE、`archive/companions` 配套、源码 ZIP、来源与锁文件、许可证、QA 脚本、日志、截图及摘要清单。后台额外输入由 `companion-web-inputs.json` 独立追溯，没有混入 Agent 核心来源标识。

本批候选完成本机验证，不表示生产部署、真实 MCP 应用宿主到远端 Agent 的完整跨机链路、交互桌面平台或 1 GiB 压力验收通过。
