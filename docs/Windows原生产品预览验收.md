# Windows 原生产品预览 0.2.0 验收记录

## 交付范围

沿用 `codex/windows-native-ui-prototype`，分两阶段恢复原生产品界面并接入共用操作观察窗。原 egui GUI 的源码入口、旧发布 EXE 与 0.1.2 诊断小样保留；不切换默认入口，不提交或推送。本轮构建和运行临时文件放 C 盘。

主窗口按旧版恢复紧凑布局：连接状态、大号控制码、复制反馈、期限、活动状态、设置、连接详情、实时日志和退出。补齐首次 Relay 设置、中文／英文、浅色／深色／系统主题、TLS 取消／仅本次信任／保存信任、现场临时权限与默认权限分离确认。使用已有 Agent、配置和授权核心；新版不是旧 GUI 的通用 GuiExtension 插件 API 替代品。

预览使用独立的 `RemoteOpsNativePreview` 配置和身份目录，便携配置文件名为 `native-agent-config.json`，不自动读取或迁移旧版 `agent-config.json`。首次运行需要填写 Relay；也可用 `--agent-config <路径>` 指定专用测试配置。请勿为了对比直接让两版共用同一 state_file。

## 主要文件

- 原生窗口与产品状态：[win32.rs](../apps/remoteops-agent-native-prototype/src/win32.rs)、[product_ui.rs](../apps/remoteops-agent-native-prototype/src/win32/product_ui.rs)、[product.rs](../apps/remoteops-agent-native-prototype/src/product.rs)、`src/product/runtime.rs`（0.2.0 原入口，0.3.0 已移至[共享宿主 runtime.rs](../crates/remoteops-agent-desktop-host/src/runtime.rs)）。
- 公共执行与观察：[Agent lib.rs](../apps/remoteops-agent/src/lib.rs)、[operation_log.rs](../apps/remoteops-agent/src/operation_log.rs)、[observation.rs](../apps/remoteops-agent/src/observation.rs)。旧 GUI 只增加对新上下文事件的兼容忽略分支。
- SSH 与凭据：[Device lib.rs](../crates/remoteops-device/src/lib.rs)、[ssh_plink.rs](../crates/remoteops-device/src/ssh_plink.rs)、[ssh_password_file.rs](../crates/remoteops-device/src/ssh_password_file.rs)、[askpass](../apps/remoteops-ssh-askpass/src/main.rs)、[公共脱敏](../crates/remoteops-audit/src/lib.rs)。
- 分发与验收：[Copy-BundledPlink.ps1](../scripts/Copy-BundledPlink.ps1)、[产品测试](../scripts/Test-NativeGuiProduct.ps1)、[真实环回测试](../scripts/test-native-ui-e2e.mjs)、[UIA 客户端](../scripts/NativeGui-TestClient.ps1)、[Plink 许可与说明](../third-party/putty/README.md)。

## 共用只读观察窗

- 普通 CMD、Windows PowerShell、PowerShell 7、持久 Shell、SSH、串口共享一个窗口，记录时间、来源和请求编号。真实开始新任务时自动出现，不抢焦点；关闭观察窗不取消任务，同一任务后续输出不反复弹窗。
- 窗口只读，可选择复制、暂停跟随，从连接详情手动打开。stderr 与最终成功／失败状态分别展示；收到请求不会写成执行成功。
- 原生订阅容量 1024，主模型保留最近 2000 行和有界字符量，历史日志保留 1000 条。显示落后会明确提示合并的中间显示记录；关键状态保留顺序。普通旧 GUI 的原事件接口保持兼容，不把无限输出送进旧版无界队列。
- Agent 对相邻同一 stdout／stderr 流按 8 毫秒／16 KiB 合并微片，保持接收顺序并在终态前提交尾片，降低网络消息和逐帧审计开销；不扩大 Relay 的慢消费者保护队列。
- 主窗固定控件在首次显示前预建，启动初始化独立记录 `startup_prewarm_ms`／`startup_ui_ms`；后续交互的完整刷新和转场仍按 100 毫秒验收，不能把启动初始化时间解释为交互刷新指标。
- 新版真实流式订阅不再在 128 行后永久停止展示。正常运行报告中的 produced_lines／dropped_lines 为 null，表示未测网络总量；不得把本地界面计数解释为网络零丢包。
- SSH 输出由实际 stdout／stderr 分片读取。Plink 随包、不内嵌源码，固定官方 x64 0.83 与 SHA-256，携带 MIT 许可；明确私钥或不适用的 known_hosts 格式走严格 OpenSSH 兼容路径。
- SSH 密码使用创建时即限制当前身份的临时文件，参数／环境只传路径。OpenSSH 需要随包新版 askpass，拒绝旧明文密码环境传递；未知目标须先核对固定主机密钥，不自动 keyscan 信任。
- 串口按会话资源持续解码中文与处理脱敏；原始写入一律隐藏，认证阶段的结构化输入和设备回显隐藏。无法安全判断的未换行内容不原样抢先显示，认证／命令提示符使用固定状态提示。二进制显示摘要。

## 验证记录

2026-10-31 最终本机验收完成；所有以下 GUI 结果对应同一 EXE：SHA-256 `2a91c3063f6d6aa5a8c4b43ec0115bbf7dec66b41afa283bbae45bac782f248f`。

交付：运行说明（本机归档 `artifacts/native-ui-product-preview-20261031/delivery/README.md`）、结构化汇总与验证命令（本机归档 `artifacts/native-ui-product-preview-20261031/comparison.json`）。主程序 6,345,728 字节（6.05 MiB），较保留旧 GUI 的 17,380,352 字节减少 63.5%；包含 Plink、askpass、许可和说明的整目录为 7,658,652 字节（7.30 MiB）。这不是单 EXE 总大小。

| 验证 | 实际结果 |
|---|---|
| 相关单元测试 | 222 通过，1 项既有交互桌面探测按原设置忽略；native 39、Agent 119、Device 58、Audit 5、askpass 1 |
| 仅界面特性 | 单测 26 通过、Clippy 通过；未另交付或宣称 UI-only 完整 GUI 验收 |
| 格式／静态检查 | fmt、五包全目标 Clippy `-D warnings`、旧 GUI `cargo check --lib` 通过 |
| 产品界面 | 52/52；布局、中文/英文、主题、控制码复制、日志筛选、配置错误、只读、返回与退出 |
| 真实本机 Relay 环回 | 48/48；TLS 三选择及重启、现场权限 No/Yes、普通 CMD/Windows PS/持久 PS、实时首帧、中文/emoji、已知凭据脱敏、关闭/重开、不抢焦点、万行日志与未读清零 |
| 环回呈现 | 最大 tick 49ms、完整交互页转场 60ms；最后一个实例启动 UI 初始化 168ms，独立记录；关闭前 pending=0、实际观察文本与模型一致 |
| 十万行离线严格压力 | 7,823ms（门槛 15,000ms），最大批处理 9ms；另 5 次输出中退出均通过 |
| 标准按钮 UIA | 25/25；产品模式的可见按钮也由产品/E2E 实际 Invoke 验收 |
| 同进程五轮十万行 | 累计 500,005 行（包含 5 行明确演示），最大批处理 37ms，离线队列零丢行，pending=0、真实文本与模型一致 |
| 验收脚本回归 | 26 项通过，覆盖实际共享锁、计数、超时和文本一致性门槛 |
| 组件打包 | 官方 Plink 哈希匹配；中文空格路径 `-V`、新版 askpass 协议探测、创建时 ACL 与正常/失败/短暂占用清理回归通过 |

运行期展示仍是有界历史。真实万行环回的本地展示合并数为 7,955，实际接收进显示模型 2,407 条（含状态与合并提示），并保留最新 2,000 条；不是全量网络日志的零丢包证明。Controller 收到真实完成结果，观察窗实际追到万行末尾。离线耐久的零丢行只属于离线有界生产/消费测试。

原始失败保留在本轮 `evidence/history`：包括 UTF-8 测试客户端解码、启动等待小于核心既有探测预算、147/146/116ms 转场、隐藏父窗后错误查询 UIA 子项、未合并输出洪峰的连接中断，以及任务完成后未等待分块呈现追赶就关闭的失败。最终未放宽 100ms／每十万行 15 秒／实际文本一致性门槛；输出微片合并后真实万行命令通过，关闭前显式验实际末尾已呈现。

SSH 传输使用可控子进程和既有信任/ACL单测验证，串口使用真实阻塞 Mock reader/write 回显验证，不能把这些结果改写为现场交换机或物理串口已通过。

## 尚未验收与限制

1. 用户明确暂缓真实 RDP 断开重连和实际 100%→150%→200%→100% 缩放。本轮不改变系统缩放或现场连接；后续需对最终产品入口单独复验。
2. 真实交换机 SSH、物理串口、此前特殊网络／认证环境仍需现场验证。可控子进程、分片脱敏单测和本机普通命令不等于现场 SSH／串口通过。
3. 临时凭据文件会在正常、失败、超时、取消路径尝试清理；Windows 外部文件共享锁仅有界重试 500 毫秒，持续占用或强制杀进程／断电时可能残留受当前身份 ACL 保护的文件。不是加密持久存储。
4. 当前身份查询与 askpass 协议探测使用同步子进程调用，尚未纳入后续 SSH 执行的 120 秒统一超时／取消预算；相关系统进程长期无响应的故障注入尚未验证。
5. 资源内嵌释放及按需下载资源站属于后续分发架构，不在本次实现范围。

正式迁移仍待上述必要现场验收和用户评估。本轮只交付可独立运行的产品预览，不替换旧版。

## 0.2.1 后续记录

传输目录、上传路径回执和 SSH 链路的后续完善见[0.2.1 验收](传输目录与SSH链路完善验收-0.2.1.md)。原第 4 项同步身份查询/askpass 预算限制已在新构建中改为 TokenUser 与异步受控探测；此处保留 0.2.0 的历史边界。两版独立交付，不把本页的体积或 GUI 通过结果自动归给新 EXE。现场交换机、物理串口、RDP、高 DPI 和正式迁移仍按后续记录待验。
