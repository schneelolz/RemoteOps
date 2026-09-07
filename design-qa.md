# Agent GUI 设计验收

## 范围与结论

2026-09-08 本地 Windows 原生界面验收，目标客户区为 `520 × 440`；100% 缩放下原生外框为 `536 × 479`。本次实现使用原有 Rust/egui 界面与 Agent 运行层，未增加前端框架，也未改变 v14 远程协议。

采用此前确认的白底设计方向：居中主状态与控制码、紧凑能力摘要、分开的底部操作，以及从右侧覆盖主界面的日志抽屉。保留真实九位控制码格式；参考图中的六位数字只属于视觉占位。界面标题采用“本机已就绪”。

主界面与中英文日志抽屉已在 Windows 原生窗口完成操作验收。截图使用 `--demo` 的虚构控制码、请求 ID 和模拟日志，不代表连接真实 Relay。真实命令到日志事件的链路由本地双向流集成测试验证。

## 行为验收

| 检查项 | 结果 |
| --- | --- |
| 中文、英文主界面完整显示控制码、四项能力、日志入口和底部按钮 | 通过；主界面无需滚动 |
| 日志默认隐藏，点击入口显示 | 通过；未读计数关闭时增加，打开后清零 |
| 抽屉从右侧覆盖底层，不增加窗口宽度、不挤压主界面 | 通过；抽屉宽 360，窗口尺寸始终相同 |
| 关闭按钮、Esc、抽屉外部点击 | 通过；外部点击不会同时触发底层连接详情 |
| 历史记录与独立滚动 | 通过；可滚至历史和末尾，收起再打开保留记录 |
| 新日志到达时查看历史 | 通过；不会强制跳到末尾 |
| 筛选与空状态 | 通过；无匹配记录显示筛选空提示 |
| 清空日志 | 通过；记录数归零，只清理当前界面的内存 |
| 原有连接详情和退出确认 | 通过；详情不包含 SSH 密码入口 |
| 控制码、能力区及底部按钮对齐 | 通过；能力区用文字图标，日志入口固定在右侧 |

历史最多保留当前进程最近 1000 条记录。日志按完整行合并并复用现有脱敏规则；超长行和超过每流展示上限的输出会省略，仍显示执行终态。这是现场诊断视图，不替代完整审计，也不跨进程保存历史。

## 迭代修正

1. 第一轮实机检查发现抽屉被 egui 默认区域约束撑成整窗，已明确宽度及覆盖位置，并对框体边距和滚动区域高度计算可用空间。
2. 英文能力区曾挤掉串口或日志入口，已移除重复标题和小卡片背景，按可用宽度分配能力区与日志入口。
3. 复核补齐紧急停止、Agent 关闭和断线时在途操作的日志终态；输出省略后依然展示结果。
4. 新增跨协议分片的凭据与 PEM 边界测试，超长行采用有界滚动上下文，避免截断破坏脱敏边界。
5. 共享运行层的新事件已适配 Service；服务忽略操作日志，不按输出行重写状态文件。

## 截图证据

- [中文主界面](docs/assets/agent-gui/runtime-zh.png)
- [英文主界面](docs/assets/agent-gui/runtime-en.png)
- [中文覆盖抽屉](docs/assets/agent-gui/drawer-zh.png)
- [英文覆盖抽屉](docs/assets/agent-gui/drawer-en.png)
- [连接详情](docs/assets/agent-gui/advanced-settings-zh.png)
- [退出确认](docs/assets/agent-gui/stop-confirmation-zh.png)

交互过程截图及验收辅助脚本保存在 Git 忽略的 `artifacts/ui-audit/20260908-log-drawer`。图片只包含本地演示数据。

## 自动化验证与版本

以下命令均已通过。相关测试共 65 项（Agent 41、GUI 20、Service 1、语言包 3）；Clippy 无警告；Debug/Release 六个 EXE 均已通过 Windows 文件版本与产品版本字段核对。

- `cargo test -p remoteops-agent -p remoteops-agent-gui -p remoteops-agent-service -p remoteops-i18n`
- `cargo clippy -p remoteops-agent -p remoteops-agent-gui -p remoteops-agent-service -p remoteops-i18n --all-targets -- -D warnings`
- `cargo check --workspace --all-targets`
- `cargo fmt --all -- --check`
- `cargo build -p remoteops-agent-gui -p remoteops-agent -p remoteops-agent-service`
- `cargo build --release -p remoteops-agent-gui -p remoteops-agent -p remoteops-agent-service`
- `./scripts/Test-Documentation.ps1`
- `git diff --check`

GUI 版本 `0.2.0-preview.8`；Agent 与 Service 版本 `0.2.0-preview.7`。遵循当前 Technical Preview 的独立预览序号约定。Windows 数字文件版本同步为 GUI `0.2.0.8`、Agent/Service `0.2.0.7`，避免预览序号只出现在字符串版本中。

## 验证边界

本次没有连接外部 Relay，也未操作真实 SSH 或串口硬件；未验证不同 DPI、RDP 或全新低权限 Windows 账户。截图在 Windows 100% 缩放环境完成，不代表其他缩放比例已经验收。当前改造是本地候选，未部署、未提交、未推送或公开发布。
