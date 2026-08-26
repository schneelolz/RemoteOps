# Agent GUI 设计 QA

- source visual truth: `C:\Users\ywb\.codex\generated_images\01a0370b-1815-7f91-b69e-059e6ac35875\exec-cab6aeeb-4599-4d5d-82cd-0250dc982b12.png`
- implementation screenshot: `C:\Users\ywb\.codex\visualizations\2026\08\25\01a0370b-1815-7f91-b69e-059e6ac35875\agent-ui-redesign\runtime-zh-final.png`
- comparison image: `C:\Users\ywb\.codex\visualizations\2026\08\25\01a0370b-1815-7f91-b69e-059e6ac35875\agent-ui-redesign\comparison-source-implementation.png`
- viewport: Windows 原生窗口内部 `520 × 410`，外框截图 `536 × 449`
- pixels and normalization: 源图 `1344 × 1172`，裁取主窗口后缩放为 `536 × 468`；实现图保持原始 `536 × 449`，系统缩放下按实际像素比较
- state: `--demo`，服务已就绪，等待工程师连接，中文界面

## 全图对比

- 字体与排版：中文、英文均采用现有微软雅黑优先字体链；状态、控制码和操作层级与确认稿一致，英文长文案无裁切。
- 间距与布局：实现按目标压缩为固定内部高度 `410`，标题、状态、控制码、连接状态、能力摘要和底部操作栏全部可见，无重叠或溢出。
- 色彩与视觉标记：保持白色工具界面、绿色就绪状态、深色正文、蓝色连接状态和红色停止操作；对比度清晰。
- 图标与图像：界面仅使用项目现有 Phosphor 图标库，无缺失图片、占位资源或临时绘制图标。
- 文案与内容：中文和 English 使用同构布局；普通页未出现本地权限、逐项确认、完全控制或 Controller 授权明细。

## 重点区域

- 高级设置：已检查 `advanced-settings-zh.png`，Relay、进程权限、传输目录和 SSH 凭据入口完整可见，长路径未破坏布局。
- 停止确认：已检查 `stop-confirmation-zh.png`，警告、影响说明和继续/停止按钮完整可用。
- 英文运行页：已检查 `runtime-en-final.png`，`Advanced settings` 与 `Stop remote assistance` 在固定宽度内完整显示。

## 对比历史

- 第一次原生截图发现底部操作栏被 `410` 高度裁切；压缩状态区、能力区和控制码字号后重新截图，操作栏已完整可见。
- 第一次高级设置交互发现同帧外部关闭判断会让模态框立即消失；移除该判断并保留明确关闭按钮后重新验证通过。

## 结论

- 未发现 P0、P1 或 P2 问题。
- 可接受差异：实现使用真实 Windows 标题栏；为满足固定 `520 × 410`，控制码和能力图标略小于概念稿。
- final result: passed
