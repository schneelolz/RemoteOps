# Windows-MCP stdio 集成验收

此目录中的 `Test-WindowsMcpProtocol.py` 只读验证锁定的 Windows-MCP 版本是否真正完成 MCP 初始化。脚本要求当前 Python 环境中的 `windows-mcp` 版本为 0.8.5，以无控制台子进程启动 `python -m windows_mcp serve --transport stdio`，发送 `initialize`、`notifications/initialized` 和 `tools/list`，限制单帧 4 MiB、总响应超时 60 秒，并在结束时回收子进程。

2026-09-10 在提交 `08ddee78c26182b103d62c1c84c1fbec82a280b2` 的上游工作区中使用 Python 3.14.7 和锁定依赖运行通过：协商协议为 `2025-11-25`，服务端版本为 `4.0.3`，发现 21 个工具（包括 `Snapshot`、`Screenshot`、`Click`、`Type`），子进程退出码为 0。该结果只证明上游 MCP 生命周期和工具发现成功，不代表 RemoteOps 已完成内容块到 `VisualObservation` 的转换或 Named Pipe 适配；Provider 仍必须在初始化失败、超时和转换失败时报告错误并禁止宣称 ready。

同日将相同源码、锁定依赖和 Python 3.14 运行时部署到 Windows 11 目标机的交互式 RDP Session，验证结果同样为初始化成功、协议 `2025-11-25`、21 个工具、子进程退出码 0；只读 `Snapshot` 还返回了显示器、Focused Window 和 UI Tree。该结果证明上游在目标交互式 Session 可运行，但仍不等于 RemoteOps 已接管它的输入权限。

RemoteOps 的 `REMOTEOPS_WINDOWS_MCP_PROTOCOL` 默认使用 `stdio`。仅当显式设置为 `remoteops-pipe` 时才启用旧的自定义 Named Pipe 适配器；这样上游 Windows-MCP 不会再被错误地以 `--pipe` 参数启动。stdio 客户端会在 Provider 初始化期间校验锁定文件、完成 MCP 握手、发送 `initialized`、发现 `Snapshot`/`Screenshot`，任一步失败都会阻止图形 Provider 宣称 ready。Native UIA 仍负责把目标转换为 RemoteOps 的稳定窗口指纹；Named Pipe 的 Service→Provider 隔离桥接仍需后续接入，不能把 stdio 握手证据当成已完成的 ACL 桥接。

preview.18 的原生观察与动作校验共用前台窗口查询：优先调用 `GetForegroundWindow`，句柄为空时调用 `GetGUIThreadInfo(0)`，只接受系统返回的非空活动句柄。两者均失败时保留拒绝状态，不从窗口列表猜测前台窗口。诊断会返回实际句柄来源与回退 API 的错误码。`GUITHREADINFO` 包含末尾 `rcCaret`，自动化测试会校验本机结构大小、字段偏移并实际调用 Win32 API，避免 Rust 构建通过但内嵌 P/Invoke 结构无效。

截图失败会保留已采集的窗口与 UIA，并在请求 UIA 树时通过 `ui_tree.screenshot_error` 返回原因；控制器能力汇总将截图标为不可用。窗口枚举能力独立报告，不能由非空窗口列表推导输入或 UIA 可用。

现场 preview.17 复核曾返回 Session 1、`WinSta0`、线程及输入桌面 `Default`、STA，但前台句柄仍为 0。该证据只能排除当次进程位于 Session 0，不能单独确定 RDP 是否断开或保证前台回退有效。preview.18 的 RDP 动作验收仍待完成，不能将本地测试通过表述为现场 UIA 点击成功。

当前图形状态等待已统一支持 window、text、control、hash 条件，并由 Provider 按 250ms 间隔轮询至超时；共享领域测试和真实 RDP 图形动作验收已完成。坐标输入的多显示器 DPI 变换、截图、鼠标移动和拖拽已在目标机现场验证。

MCP `send_input` 的坐标目标支持 `move`、`click`、`double_click`、`right_click`、`middle_click`、`wheel_up`、`wheel_down`、`drag`、`drag_left` 和 `key:<键名>`。拖拽目标必须同时带有 `end_x` 与 `end_y`；旧版单点坐标 JSON 仍按普通单点目标解析。

UIA `invoke_control` 还支持 `submit`（调用目标控件的 InvokePattern）和 `press_enter`（聚焦目标后发送 Enter），用于完成文本输入后的提交动作。坐标移动和拖拽会在后置观察中读取系统光标物理坐标并校验目标位置；点击类动作仍要求观察到同一前台窗口内的 UIA 状态变化才报告 `effect_verified=true`。

截至 2026-09-12，PC130 现场 Agent 在线但观察结果为 `no_interactive_desktop`，前台句柄和截图句柄均无效，因此真实 RDP 动作逐项验收尚未完成；不能据此宣称整体图形控制能力已完成。

后续现场复测已恢复交互桌面：截图为 2560×1440，前台句柄、Session 1、`WinSta0\Default` 和 UIA 均可用。`0.2.0-preview.19` Agent 的 `move` 已实测 `action_sent=true`、`effect_verified=true`，后置光标坐标与目标一致；从 `(1200,700)` 到 `(1300,800)` 的拖拽也已实测 `action_sent=true`、`effect_verified=true`，最终光标为 `(1300,800)`。点击、双击、右键、中键、滚轮和键盘组合均已实测发送成功，但在空白 Agent 窗口上没有可观察 UIA 状态变化，因此按协议返回 `effect_verified=false`。资源管理器地址栏 UIA `type_text` 输入 `C:\Windows` 和 `press_enter` 提交均已实测 `action_sent=true`、`effect_verified=true`。

2026-09-13 PC130 现场纯图形界面链路复测（Agent `0.2.0-preview.20`）已完成：先对桌面左上角“此电脑”图形项执行坐标双击，前台窗口由 `Program Manager` 变为 `此电脑 - 文件资源管理器`；随后在“此电脑”截图中对 `本地磁盘 (U:)` 图标执行坐标双击，前台窗口变为 `本地磁盘 (U:) - 文件资源管理器`；最后在 U 盘根目录截图中对 `BaiduNetdiskDownload` 文件夹图形项执行坐标双击，前台窗口变为该文件夹。三步均使用目标窗口指纹和截图前后校验，没有使用地址栏输入、命令行 `explorer` 或直接路径导航。第一步动作返回的自动效果校验为 `false`，但前后截图和窗口标题已证明双击生效；后两步均返回 `action_sent=true`、`effect_verified=true`。同一现场还实测了右键菜单、Escape 关闭菜单、返回按钮、滚轮和 `Ctrl+A` 输入发送。Explorer 的虚拟化 `listview` 目前仍主要返回容器节点，已在 `0.2.0-preview.21` 加入 Raw View 遍历和更深层级枚举；在 UIA 项目不可访问时，验收采用带目标窗口指纹的截图坐标回退，并保留上述前后截图证据。

2026-09-22 PC130 现场图形能力复测（Agent `0.2.0-preview.24`，RDP Session 1 交互桌面）验证了观察链路修复：UI 树 JSON 序列化深度由 8 提升到 32 后不再出现字符串化节点，Explorer 的 8 个文件项目全部以 `ControlType.ListItem` 返回，111 个节点携带 `rect`，树深度由 3 层恢复到 6 层；`invoke_control` 借助与观察一致的 RawView 遍历能直接打开文件项目（`action_sent=true`、`effect_verified=true`，窗口标题变为 `7ZipSfx.000 - 文件资源管理器`）。

同轮实测：以 `rect` 中心 `(749,474)` 坐标双击命中 `7ZipSfx.000`；以“向上一级”按钮 `rect` 中心 `(399,353)` 坐标点击后导航回 `Temp`；`type_text` 写入地址栏、`key:ESC` 关闭开始菜单、`wait_for_visual_state` 等待窗口条件均得到预期结果；对 `cmd.exe` 发送 `ESC` 正确返回 `effect_verified=false`，无效操作不会被误报。

本轮同时确认两个观察缺口与处置：右键菜单属于无标题的顶级弹出窗口，既不在前台窗口的 UIA 子树中，也被窗口枚举的标题过滤排除，因此菜单确实弹出但动作返回 `effect_verified=false`；`0.2.0-preview.25` 在观察结果中新增 `popups` 字段，枚举无标题顶级窗口（含 `rect`，最多 4 个），并把弹出层纳入效果变化比较。控制器本地 MCP `0.2.0-preview.10` 早于拖拽终点字段引入的提交，`end_x`/`end_y` 在传输中被忽略，拖拽返回“拖拽输入缺少终点”；`0.2.0-preview.11` 的 MCP 组件已包含该字段与 `drag_to` 编码，需与控制端一并更新后再做现场拖拽复测。

截至本轮结束，`drag`、`wheel_up/down`、`middle_click` 与 `popups` 的现场复测待新 MCP 与 Agent 部署后完成。
