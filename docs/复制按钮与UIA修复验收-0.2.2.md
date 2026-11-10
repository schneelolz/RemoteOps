# 复制按钮与 UIA 修复：0.2.2

2026-10-31，分支 `codex/windows-native-ui-prototype`。本轮仅处理复制按钮的剪贴板占用、UIA 响应与退出生命周期；旧版保留，不切换默认入口，不操作 RDP、显示缩放或现场 Agent。

## 改动与边界

原先复制按钮在 WM_COMMAND/UI 线程内直接 OpenClipboard、EmptyClipboard、SetClipboardData。短暂占用直接失败；EmptyClipboard 还会通知原剪贴板 owner，可能等待别的程序。UIA Invoke 接受调用也不代表复制成功。参见 [Microsoft 剪贴板操作](https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-operations)和 [InvokePattern.Invoke 的阻塞说明](https://learn.microsoft.com/en-us/dotnet/api/system.windows.automation.invokepattern.invoke?view=windowsdesktop-10.0)。旧超时记录不单凭这段代码认定唯一根因，本轮用可控真实阻塞验证修复行为。

- 复制请求改由一个专属工作线程处理。它拥有自己的 message-only HWND，负责剪贴板调用和消息泵；不使用主窗指针。队列只保留最新请求和一个结果，不为重复点击创建更多线程。
- UIA/WM_COMMAND 只提交请求，不在调用栈内同步刷新控件。界面定时器显示复制中、成功或失败。只有系统写入成功才显示 ✓。
- OpenClipboard 失败每 25ms 重试，预算 1500ms；同码重复点击不延长预算。持续占用明确报错，恢复后可重新点击。
- UI 自行判定超时，不等待工作线程。系统调用返回后重新核对请求代次、截止时间及退出标记，过期请求不得再主动 SetClipboardData 或上报成功。
- 无复制待办时直接跳过轮询快照，避免每个 UI tick 重复克隆整组有界日志；不改日志渲染实现。
- 退出取消待办，不在界面线程 join；工作线程不访问已经销毁的 App。已进入的 Win32 同步调用不能强行撤销，已发生的系统清空/写入也不能回滚；取消保证的是返回后不再继续提交过期内容。进程退出时由操作系统结束仍在系统调用内的自有线程，不强制释放其他程序的剪贴板。

## 涉及文件

- [clipboard.rs](../apps/remoteops-agent-native-prototype/src/win32/clipboard.rs)：线程、窗口、重试、内存和取消生命周期。
- [product_ui.rs](../apps/remoteops-agent-native-prototype/src/win32/product_ui.rs)、[win32.rs](../apps/remoteops-agent-native-prototype/src/win32.rs)：提交、反馈、轮询和关闭挂接。
- [UIA 测试客户端](../scripts/NativeGui-TestClient.ps1)：实际 Invoke 耗时、成功/失败反馈及受限测试同步门。
- [剪贴板专项](../scripts/Test-NativeGuiClipboard.ps1)、[产品演示](../scripts/Test-NativeGuiProduct.ps1)、[真实环回](../scripts/test-native-ui-e2e.mjs)：真实占用、旧 owner 阻塞与输出并行测试。
- native 包版本和 Cargo.lock 更新为 0.2.2；[Session 验收](../scripts/Test-NativeGuiSession.ps1)仅增加版本识别，没有执行 RDP 或 DPI 测试。

## 测试方法与已验证结果

环境为本机 Windows 11 专业版 10.0.26200。构建缓存和测试夹具放 C 盘；GUI 测试顺序执行，不与发布编译同时运行。没有据此宣称 Windows 10、Server 2016、RDP 或高 DPI 已通过。

剪贴板夹具只持有自己成功取得的锁，不读取原有用户剪贴板。内容比较仅在 owner 属于本轮目标/夹具 PID 时进行，只保存布尔结果。先开自己的 owner 窗口，再延迟它的 WM_DESTROYCLIPBOARD 处理 3 秒，以实际阻塞产品工作线程的 EmptyClipboard；不以普通 busy 重试冒充阻塞测试。控制码内容只接受九位数字或 3-3-3 连字符格式，格式比较有 13 项纯文本自测。

| 验证 | 结果 |
|---|---|
| 原生版单测 | 42/42，含新增 3 项请求合并、超时、取消/退出及旧结果隔离回归 |
| 仅界面特性单测 | 29/29；未交付或宣称 UI-only 完整 GUI 验收 |
| 静态检查 | 原生全目标 Clippy `-D warnings` 通过，脚本 AST、C# 夹具编译、Node 语法和差异检查通过 |
| 剪贴板专项 | 24/24：短占用后成功、持续占用有界失败/可重试、重复点击不延长截止、实际旧 owner 阻塞、主窗响应、无过期成功/晚 Set、占用期间退出 |
| 最终专项实际耗时 | UIA Invoke 最大 86ms；旧 owner 阻塞用例 Invoke 55ms、明确失败 1525ms；阻塞期间原生进程退出 474ms，退出码 0 |
| 保留旧版的负对照 | 相同夹具、相同短占用释放场景，0.2.1 在“占用释放后复制成功”失败；0.2.2 通过。旧 EXE 哈希不变，未为对照改旧程序 |
| 最终真实输出并行 | operations 场景 39/39，内含独立保存的 24/24 剪贴板并行子检查；实际输出序号 108→1910，最后标记到达；复制期间最大 Invoke 56ms |
| 最终呈现与退出 | 最大 tick 27ms、页转场 51ms，不超过 100ms；退出前 pending=0、实际观察文本与模型一致，正常退出码 0 |

最终 EXE 的 SHA-256 为 `0256527a1e8ab29fa3a93a17a63a3c37ad586742690a555b9e0c3a0aab9cd79e`。交付与真实并行结果见运行说明（本机归档 `artifacts/native-ui-clipboard-0.2.2-20261031/delivery/README.md`）和结构化清单（本机归档 `artifacts/native-ui-clipboard-0.2.2-20261031/comparison.json`）。

## 失败历史和剩余项

所有原始失败保留，不能当作通过覆盖：

- 专项 run1：测试错误地将带连字符的演示控制码按九位原样比较。修正为严格支持这两种合法格式，不更改产品行为迁就测试。
- 专项 run2：退出计时混入 UIA 客户端的进程收尾。修正后先直接测原生 WaitForExit(2000)，再等待测试客户端；两秒门槛未放宽，run3 通过。
- 完整产品演示 run1：观察窗打开前后焦点不同；失败后前台窗口既非主窗也非观察窗，缺少 before 轨迹，不能归因于用户、UIA 或产品。原判定与失败记录保留。
- 完整产品演示 run2：复制反馈及焦点检查通过，但一次页转场 167ms 超过既有 100ms 门槛，报告按原规则判失败。这不是工作线程 join 或复制退出卡住的证据，仍需后续界面阶段复查，当前不能声称 52 项完整产品演示全部通过。
- 真实环回 run1：在自动观察窗焦点相等检查失败，另暴露测试客户端的未观察异步拒绝。已给挂起请求补失败收尾并增加前台 HWND/PID/归属证据，保留原业务异常、焦点相等和 100ms 门槛；不记录外部窗口标题或内容。
- 并行候选测试暴露上一轮 ✓ 尚未刷新，被测试客户端当作本轮成功。独立用例增加最多 5 秒的空闲前置条件；客户端须观察新状态变化才接受完成。重复点击用例仍在同一待办中运行，不等待空闲或延长产品预算。
- 中间候选在组合测试/无争用对照分别出现 1345ms/1083ms 日志刷新；旧 0.2.1 同环境对照 111ms，也超出原门槛。没有据此断言单一根因。去掉新增空闲快照开销后生成最终构建，最终并行结果为 27ms；历史失败保留，不解释为任意环境下的耗时保证。
- 最终构建早期取焦点基线时 before 为本进程其他窗口、after 为第三方。原确认辅助只等 Invoke 返回，没有确认模态框已销毁。测试补入“自有模态框数为零、前台连续两次稳定”的前置条件，不设置焦点，仍严格比较前后相等；最终通过。不将过渡态记录归因为用户操作。

真实交换机、物理串口、RDP 断开重连与高 DPI 往返继续待验。未重新执行前版五轮十万行压力或 SSH 全矩阵，不把历史结果自动记给新构建。正式迁移仍未决定。

最终 39 项 operations 验收没有重复完整 TLS 三选择和 52 项产品演示。167ms 演示转场属于前一个候选，最终构建完整产品演示仍需后续复验；不能用本轮复制通过替代所有产品验收。

## 纪要衔接

已阅读用户提供的六页讨论纪要并核对页面，方向见[纪要对照](界面路线纪要对照-20261031.md)。后续只比较 Rust + Win32 与 Rust + Slint，共用业务核心、同功能比较；本轮没有启动 Slint 开发、安装设计技能或扩展为完整终端模拟器。

用户更新技能后，按 ui-ux-pro-max 的定向 `ux` 检索复核异步状态、失败反馈和恢复路径，采用平台通用交互原则；不引入网页动画或视觉重做。标准 BUTTON/键盘入口保持，未单列新增键盘实机验收。
