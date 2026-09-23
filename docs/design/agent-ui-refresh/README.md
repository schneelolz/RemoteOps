# Agent UI 改版

分支：`codex/agent-ui-refresh`。仅改动 Agent GUI、界面翻译和界面设置；远程协议、权限判定与真实 Agent 执行逻辑不变。

## 设计与实现

- [最终设计目标](design-target.png)：居中的控制码、复制图标、倒计时和功能栏。
- 系统标题栏只显示本地化产品名，版本移至设置菜单。
- 右上角设置提供中文 / English，以及跟随系统 / 亮色 / 暗色；使用原有 GUI storage 保存，兼容旧设置。
- 默认跟随系统；同步 egui 内容主题和原生窗口主题偏好。
- 控制码使用真正的粗体字体，小蓝色图标按钮保留复制、三秒对勾反馈和过期禁用逻辑。
- 日志入口移到底部，未读日志使用蓝点；连接详情、停止确认、证书确认、首次配置及日志页面使用语义配色。
- 字体使用 Windows 系统的 Segoe UI / 微软雅黑；macOS 本地预览使用 Arial / 黑体回退。未随仓库分发系统字体。
- `--demo` 使用虚构控制码与模拟已连接状态，不建立远端连接。权限行“完全控制已启用”仅为演示占位，尚未接入真实权限展示，也不会改变授权。
- 控制码加载使用三点渐亮动画；演示环境变量 `REMOTEOPS_DEMO_LOADING_SECONDS` 可调整等待时间，默认 2 秒、上限 120 秒，真实连接不受影响。
- 日志使用完整页面，支持筛选、文本选择、返回和 Escape；时间与请求编号合并一行。

## 本地真实截图

截图来自 macOS 上运行的原生 Rust / egui 应用，不是生成图。捕获工具提供 1600×1200 图片，原生系统标题栏与 Windows 不同。截图记录首次实现；当前默认内容尺寸已由 600×450 调整为 500×375 逻辑点，字体、图标与按钮随主布局缩小约 17%。

| 语言 | 亮色 | 暗色 |
| --- | --- | --- |
| 中文 | [中文亮色](zh-light.png) | [中文暗色](zh-dark.png) |
| English | [English light](en-light.png) | [English dark](en-dark.png) |

另附 [暗色日志](logs-dark.png)、[暗色停止确认](stop-confirmation-dark.png)。日志正文是原始模拟事件文本，因此不随界面语言翻译。

## 已完成检查

最终提交前在 macOS 完成全工作区 `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings` 和 `cargo test --workspace --locked`，均通过。

- `cargo test -p remoteops-agent-gui -p remoteops-i18n --locked`：24 项 GUI 测试和 3 项 i18n 测试通过。
- `cargo clippy -p remoteops-agent-gui -p remoteops-i18n --all-targets --locked -- -D warnings` 通过。
- `cargo build -p remoteops-agent-gui --locked` 通过。
- 四种语言／主题组合的固定窗口布局边界测试通过。
- 设置存储兼容性、语言／主题保存、系统主题变化和手动覆盖的测试通过。
- 手动操作：语言切换、亮暗切换、重启恢复偏好、复制对勾、连接详情开关、日志页面及 Escape 关闭、停止确认的取消和确认退出。

## Windows 验收

在此分支构建并启动模拟界面：

```powershell
cargo test -p remoteops-agent-gui -p remoteops-i18n --locked
cargo build --release -p remoteops-agent-gui --locked
.\target\release\remoteops-agent-gui.exe --demo
```

1. 通过齿轮菜单检查中文亮色、中文暗色、英文亮色、英文暗色；英文按钮不可换行或裁切。
2. 在 Windows 100%、125%、150% 显示缩放下检查窗口、复制按钮、设置菜单和弹窗；有多显示器时检查移动后的缩放。
3. 选择“跟随系统”，在 Windows 中切换应用亮／暗模式，确认内容和原生标题栏响应；手动选择主题后应保持该选择。
4. 复制控制码并粘贴到记事本，确认内容正确、对勾恢复；检查过期后不可复制。
5. 检查日志筛选／滚动、连接详情、停止取消和确认退出；重启确认语言和主题恢复。
6. 实际会话补验等待连接、已连接、断线重连、停止协助；测试真实会话前按现场流程授权。

Windows 构建、字体栅格化、DPI、原生主题事件与实际远程会话尚未在本机验证，不以 macOS 截图代替该验收。
