# RemoteOps 控制端 GUI 使用说明

> 文档状态：人工 Controller GUI 使用手册，适用于 `0.2.0-preview.1` Technical Preview。现场被控端请阅读[现场被控端 GUI 使用说明](现场被控端GUI使用说明.md)。

## 当前定位

GUI 是人工 Controller 的表现层壳子，直接调用 `remoteops-application`，不启动 CLI 子进程，也不复制 CLI 或 MCP 的业务规则。

同一个 Relay 会话可以同时存在：

- GUI：`ControllerKind::Human`，负责人工输入、接管、审批和恢复 AI；
- Codex MCP：`ControllerKind::Ai`，负责通过 STDIO MCP 发起只读诊断或申请精确审批。

两者通过同一个不可变 `session_id` 关联。GUI 当前已经可以接收统一事件流中的 AI 审批请求，并决定批准或拒绝。

## 启动演示模式

演示模式只使用虚构的 LAB-WIN-A / LAB-WIN-B 数据，不连接真实 Relay：

```powershell
cd <仓库根目录>
cargo run -p remoteops-controller-gui -- --demo
```

## 连接真实 Relay

GUI 不保存 Controller Token。启动真实连接时从进程环境或参数读取人工 Controller 配置：

```powershell
$env:REMOTEOPS_RELAY = '<relay-address>'
$env:REMOTEOPS_SERVER_NAME = '<relay-server-name>'
$env:REMOTEOPS_CA_CERT = '<ca-certificate-path>'
$env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN = '<human-controller-token>'

cargo run -p remoteops-controller-gui
```

GUI 启动后不会自动配对。点击连接标题右侧的 `+`，输入现场 Agent 显示的九位控制码和可选客户名称，配对成功后连接才会出现在列表中。

也可以构建后直接运行：

```powershell
.\target\debug\remoteops-controller-gui.exe
```

## 在应用内配置 AI

侧栏底部点击“AI 设置”，可以直接配置：

- 接口协议：自动、Responses API、Chat Completions；
- 服务地址；
- 模型名称；
- Bearer Token 或 API Key。

“从 Codex 配置导入”会读取当前 Codex 选中的 Provider、模型、`wire_api` 和
`experimental_bearer_token`。非敏感设置写入当前用户的本地应用配置目录，Token
只转存到 Windows 凭据管理器的 `RemoteOps/AI/BearerToken`，不会进入普通配置文件、
日志或审计文件。

点击“测试连接”只发送最小文字请求，不调用远端 Agent。点击“保存并应用”后，后端
AI 客户端立即更新，无需重启 GUI。Responses 工具调用会回传上一轮完整输出和
`function_call_output`，兼容不支持 HTTP `previous_response_id` 的 Codex Provider。

## 当前交互

- 连接列表和当前目标锁定；
- 会话 ID 展示；
- 只读诊断命令；
- 变更命令审批；
- Codex/MCP AI 审批卡片；
- 暂停 AI 操作；
- 恢复 AI 操作；
- 跟随系统、浅色、深色主题；
- V4 风格的协作时间线、终端结果卡片和大输入区；
- 手动配对弹窗：输入校验、失败提示、连接中禁用和成功后自动选中；
- 4.1.0：Controller GUI 静态界面完成 `zh-CN` / `en-US` 迁移，语言按保存设置、启动参数、环境变量和系统区域自动选择，也可在界面内切换；
- 4.1.0：串口切换目标和关闭原生子窗口会主动关闭旧会话；默认行尾为华为 Console 常用的 `CR`，文本/HEX 和二进制回显使用真实字节，单次发送限制为 `64 KiB`；
- 4.1.0：可写串口打开和写入无论 `ApprovalRequired` 或 `FullAccess` 都进入审批队列，审批显示端口、参数、预览、字节数和 SHA-256；只读串口禁用人工写入和 AI 主动查询；
- 4.1.0：普通 AI 和串口 AI 请求改为后台任务，运行期间仍可关闭串口、切换目标、断开连接和处理审批；关闭串口或远程连接时清理对应对话历史；
- 2.0.0：串口工作台复用 `remoteops-serial` 核心类库。默认 AI 只分析已有缓冲；工程师勾选“授权本次 AI 任务主动执行华为只读查询”后，本次任务可在 10 分钟内执行最多 8 条完整 `display ...` 查询；一次查询完成命令写入、响应等待、自动分页、提示符识别和敏感行脱敏；
- 2.0.0：新增协议 v3 的 `RunSerialQuery`，Relay 和 Agent 会重新计算命令风险，未知、修改型或虚假只读声明不会被任务级授权放行；原始 `WriteSerial` 仍按修改操作审批；
- 1.8.1：聊天消息改为由实际内容自然撑高，修复长回答、工具卡片和展开详情导致的消息重叠；时间线高度扣除卡片上下内边距，修复非全屏窗口无法滚到回答末尾以及底部输入卡片被裁剪的问题；
- 1.9.1：修复宽屏、高 DPI 真实会话中聊天头像继承滚动区剩余空间，导致 AI 对话气泡被挤出并出现整屏蓝色块的问题。
- 1.9.0：新增独立原生串口工作台。串口参数、实时终端、文本/HEX 显示、人工输入、审批和有界 AI 分析均在独立窗口中呈现；串口写入始终经过工作台内的人工审批，不因完全权限模式绕过。
- 1.8.0：按 `session_id` 保存并隔离最近 12 轮 AI 对话，Responses 和 Chat Completions 均会携带同一远程会话的前文；断开连接后清除对应上下文，并限制历史字符数；重新构建策略依赖，修复发布包误拒绝 `Get-CimInstance Win32_OperatingSystem` 的问题；
- 1.7.3：进一步修复 egui 消息列的宽度和高度分配，避免父级布局再次把气泡拉伸到整行；同步 Windows 发布包、真实测试启动器和版本记录；
- 1.7.2：修复聊天消息列被横向布局拉伸、气泡背景撑满时间线的问题；气泡宽度按实际文字和工具内容测量并限制最大值；左侧连接项支持右键“设为当前目标 / 断开连接”；Toast 移到右上方；重新计算底部输入区安全空间，避免边框和阴影贴边裁剪；
- 1.7.1：默认非最大化窗口缩小为 `1100×680`；配对输入框文字垂直居中；AI 对话改为带头像的左右消息气泡并弱化工具面板；清理纯文本气泡中的 Markdown 加粗符号；底部输入区增加安全间距，避免描边和阴影被窗口裁剪；
- 1.7.0：AI 模式改为自上而下的聊天流，依次显示用户问题、AI 工具状态和最终回答；远程工具原始输出默认折叠到对应 AI 气泡内，避免与人工命令混排；配对输入框增高；发送按钮支持下拉选择“回车发送”或“Ctrl+回车发送”，默认回车发送并持久保存；
- 1.3.1：Relay 连接状态可靠显示，真实连接时显示“在线”而不是永久“连接中”；
- 1.3.1：配对弹窗增加模态遮罩、背景操作拦截和明显的按钮悬停/按下状态；
- 1.3.1：控制码查看器支持 Agent 正在写入日志时读取，并提供 `--validate` 脱敏自检入口；
- 低高度 Windows 桌面下的紧凑审批卡片；
- 1.5.1：同一远程请求的流式输出合并为一张终端卡片，完成事件不再重复完整输出；低高度和高 DPI 窗口为时间线预留空间，输入区保持可见并增高；AI 未配置或调用失败时保留明确状态卡片；
- 1.6.2：GUI 启动时显式安装 Rustls `ring` 加密 Provider，修复内置 AI 启用后后端因 `No provider set` 异常退出、Relay 一直显示连接中的问题；
- 1.6.1：修复 AI 生成 `ipconfig` 时因命令解释器类型不匹配而被只读策略误拒绝的问题；AI 只读工具会按命令选择 Windows PowerShell 或 Cmd，同时明确限制为单条、无管道、无重定向命令；
- 1.6.0：侧栏增加应用内“AI 设置”；支持从 Codex 配置导入 Provider、模型、Responses 协议和 Bearer Token；Token 转存到 Windows 凭据管理器；支持 Responses API、Chat Completions、自动探测、连接测试和运行时立即生效；
- 脱敏审计继续由核心应用服务负责。

“本次会话完全访问”只有在 Relay 认证、Agent 本地可信 Owner 授权和当前会话权限同时满足时才生效。它不会绕过 TLS、身份绑定、审计和串口审批；可写串口打开及写入始终需要人工确认。

串口工作台中的任务级只读授权不是“完全访问”：它只绑定当前 `serial_session_id`，只在本次 AI 任务内有效，只覆盖完整华为 `display ...` 命令。查询结果交给 AI 前会遮盖常见凭据行；人工终端仍能看到设备原始输出。

## CLI 兼容入口

已有 CLI 仍然可用，人工接管和恢复命令分别为：

```powershell
remoteops-controller-cli.exe takeover <session-id>
remoteops-controller-cli.exe release-takeover <session-id>
```

GUI、CLI 和 MCP 必须继续复用同一个 `session_id`，不得使用别名猜测目标。

## 当前验收边界

已完成：

- Rust Workspace 编译和 Release 构建；
- V4 视觉还原和 Windows 原生截图验收；
- GUI 演示模式启动路径；
- GUI 事件、审批、主题和低高度布局状态映射；
- Human/AI 共享会话的 Relay 单元测试；
- 人工接管后释放接管的 Relay 测试；
- 结构化串口查询协议、风险归一化、授权边界、分页、提示符、脱敏和审计回归测试；
- 首发候选已通过 Workspace 格式、编译、全量测试和零警告 Clippy；Controller GUI 串口及国际化回归测试通过。

已通过：

- CLI、MCP、Relay 和 LAB-WIN-A / LAB-WIN-B 的 `1.3.0` 三机真实 E2E；
- GUI 启动参数不含 `--pair`，启动时保持空连接列表；
- GUI 手动配对输入校验和后端配对事件单元测试。
- 1.3.1 控制码查看器已在 LAB-WIN-A / LAB-WIN-B 实机自检通过；
- 1.3.1 真实 GUI 已连接保留 Relay，截图确认在线状态和配对弹窗遮罩。

仍待人工体验：

- 在保留的三机环境中完成 GUI 手动添加两台 Agent，并与 Codex MCP 同时连接同一 Relay；
- 在真实远程 Agent + `COM7` 环境中确认可写打开审批、人工写入、AI 主动查询、自动分页、脱敏结果、拔插和断线恢复；
- 交换机 SSH 现场硬件验收。

