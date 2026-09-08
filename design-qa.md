# Agent GUI 设计验收

## 验收结论

2026-09-08 在 Windows 原生窗口完成 `0.2.0-preview.9` 的视觉与交互验收。目标客户区为 `520 × 440`，100% 缩放下原生外框为 `536 × 479`。本次修正保持固定窗口尺寸和既有日志覆盖抽屉，不改变 v14 协议。

三张用户参考图均作为视觉事实来源，并与同状态实现截图并排对照：

- 初始状态：`C:\Users\ywb\AppData\Local\Temp\codex-clipboard-bb178db7-78f6-4282-90bc-ad2ac6f2cff3.png`，`522 × 474`。
- 标题与图标：`C:\Users\ywb\AppData\Local\Temp\codex-clipboard-92e6ad2f-4b02-46e5-80f8-f28348088c3d.png`，`527 × 476`。
- 连接详情：`C:\Users\ywb\AppData\Local\Temp\codex-clipboard-c937b8fc-8887-4dd3-96df-cd727e6a32b3.png`，`520 × 470`。

## 逐项结果

| 检查项 | 实现结果 | 验收结果 |
| --- | --- | --- |
| 初始界面缺少等待反馈 | 控制码尚未取得时显示蓝色旋转动画，并显示“正在获取临时控制码” | 通过；加载过程不改变布局，不显示伪控制码 |
| 原生标题和应用内标题重复 | 原生标题保留本地化产品名称，应用内标题改为 `RemoteOps Agent` | 通过；两个层级职责清楚 |
| 左上角仍显示旧图标 | 原生窗口和应用内标题均加载 `assets/brand/remoteops-mark.png` | 通过；两处均显示蓝色盾牌品牌图标 |
| 标题缺少程序版本 | 使用编译时 `CARGO_PKG_VERSION` 生成原生标题 | 通过；中英文标题均显示 `v0.2.0-preview.9`，没有硬编码版本文案 |
| 传输目录显示不全 | 去除 `\\?\` 扩展前缀，路径和右侧图标均可点击，悬停显示完整路径 | 通过；实测资源管理器打开 `C:\Users\ywb\AppData\Local\RemoteOps\transfers` |

参考图中的 Relay、权限和用户目录属于运行数据。验收截图使用 `--demo`，因此显示 `demo.invalid`、当前用户目录和未提升权限；这些差异不属于布局或交互偏差。

## 截图证据

- [中文初始等待状态](docs/assets/agent-gui/initial-loading-zh.png)
- [中文就绪状态](docs/assets/agent-gui/runtime-zh.png)
- [英文就绪状态](docs/assets/agent-gui/runtime-en.png)
- [可点击传输目录](docs/assets/agent-gui/advanced-settings-zh.png)
- [中文覆盖日志抽屉](docs/assets/agent-gui/drawer-zh.png)
- [英文覆盖日志抽屉](docs/assets/agent-gui/drawer-en.png)

对照过程覆盖完整主窗口、初始加载局部、标题局部和连接详情局部。最终复核未发现 P0、P1 或 P2 视觉问题，也未发现滚动条、文字裁切、元素重叠或点击目标过小的问题。

## 自动化验证与版本

以下验证均已通过：

- `cargo test -p remoteops-agent-gui -p remoteops-i18n`：25 项通过。
- `cargo clippy -p remoteops-agent-gui -p remoteops-i18n --all-targets -- -D warnings`：通过，无警告。
- `cargo check --workspace --all-targets`：通过。
- `cargo fmt --all -- --check`：通过。
- `cargo build -p remoteops-agent-gui`：Debug 构建通过。
- `cargo build --release -p remoteops-agent-gui`：Release 构建通过。
- `./scripts/Test-Documentation.ps1`：44 个 Markdown 文件链接检查通过。

Debug 与 Release 的 `remoteops-agent-gui.exe` 字符串版本均为 `0.2.0-preview.9`，Windows 数字文件版本均为 `0.2.0.9`。

## 验证边界

本次没有连接外部 Relay，也未操作真实 SSH 或串口硬件。不同 DPI、RDP 和全新低权限 Windows 账户未复测。原生视觉检查在 Windows 100% 缩放环境完成；本地候选未部署、未提交、未推送或公开发布。

final result: passed
