# Windows 原生界面小样待办与验收状态

记录日期：2026-10-30。分支：`codex/windows-native-ui-prototype`。用户决定 RDP 和本机缩放复验均稍后进行，恢复测试时先 RDP、再本机缩放；当前不改变缩放、不主动断开 RDP、不替换旧版、不提交或推送。

## 已保留的 0.1.2 基线构建

- 核心小样：`artifacts/native-ui-prototype/rdp-dpi-fix-20261030/delivery/native-core.exe`，0.1.2，6,045,184 字节；SHA-256：`d73fc939d22bdec448221df0eca474c48e2b7554d2176c3829c9be889bd8356c`。
- UI-only：同目录 `native-ui-only.exe`，1,522,688 字节；SHA-256：`b4ac280a0828b6555c83e17778ad60b50f2e869ca34172aa6ed65a3662cf44bb`。仅用于体积对比，不将核心版完整 GUI 验收直接算给 UI-only。
- 旧 GUI：`artifacts/size-optimization-20261030/delivery/remoteops-agent-gui.exe`，17,380,352 字节；SHA-256：`006aa138cfade721bf5596f5bc94271ad82dbe7d91f3592ee0389323395b1ce6`，继续保留。
- 日志区为标准 Unicode 多行 EDIT；详细设计、失败历史和证据见[评估文档](Windows原生界面小样评估.md)。结构化汇总：`artifacts/native-ui-prototype/rdp-dpi-fix-20261030/comparison.json`。

## 已验证的范围

- 核心候选在本机和 RDP 会话内，分别同进程五轮十万行：累计各 500,005 行，零传输丢行，pending 为零，显示游标追上模型，关闭前实际文本与模型逐字一致。最大批处理 69／65 毫秒。
- 两端独立十万行压力和五次输出中退出通过。单轮本机最大 98 毫秒，接近 100 毫秒门槛，不能据此保证任意负载均无卡顿。
- 两端按钮 UIA 各 25 项通过；核心／UI-only 单测 24／23 项；Clippy、格式、文档链接和耐久脚本 26 项回归通过。
- 这些结果是离线小样测试；RDP 会话内压力通过不等于断开重连通过，演示日志也不等于真实 SSH、串口或 Shell 链路通过。

## 尚未测试或尚未完成

| 项目 | 当前状态 | 下一步与通过条件 |
|---|---|---|
| 最新标准 EDIT 候选的真实 RDP 断开重连 | 用户要求稍后测试；未验收通过 | 用户准备好后启动持续百万行，在输出实际运行时手动断开约 3 秒并重连，不注销。核对 WTS Active→Disconnected→Active、pressure_disconnects≥1、同 PID/启动时间/主窗口句柄、日志完整、批处理≤100ms、显示追赶、正常退出。不要再主动断开用户连接。 |
| 最新候选真实高分屏往返 | 用户要求稍后，排在 RDP 后 | 仅本机屏幕 2，同进程两个窗口完成 100%→150%→200%→100%，记录实际 96→144→192→96 DPI、中文/emoji、只读/UIA、控件边界、字体 14→21→28→14px及退出。测完恢复 100%。 |
| 旧 renderer 已通过的 DPI 结果归属 | 仅旧构建通过 | `5101e97…` 的 `local-dpi-roundtrip-run2/report.json` 有 218 项完整通过；不能移作最新 EDIT 的验收。此前超时失败记录也保留。 |
| 本机较早一轮累计 600,005 行的原因 | 尚未归因 | `local-edit-clean-endurance` 严格失败，原始报告保留；后续同候选系列严格复验为 500,005 行并通过。若再次出现，记录操作来源及每轮计数，不能未经证据归因于用户或重入。 |
| UI-only 的完整 GUI / RDP / DPI 验收 | 未独立完成 | 当前主验收针对包含 Agent 核心的构建；若未来交付 UI-only，另行明确测试范围。 |
| 正式迁移决策 | 尚未完成，暂不迁移 | 补齐上述实际测试和下列产品能力后重新评估；目前小样与旧 GUI 功能范围不同，体积减少约 65% 不等于完整产品迁移后的最终收益。 |

## 0.2.0 产品工作进度

用户已确认两阶段实施，继续当前分支。首次 Relay 配置、控制码／期限、TLS 三种确认、现场与默认权限、偏好设置及连接详情已接入原生产品宿主；真实普通命令、SSH 和串口的共用观察事件桥、SSH 流式读取及 Plink 随包脚本也已实现，2026-10-31 最终本机验收通过。独立预览交付为 `artifacts/native-ui-product-preview-20261031/delivery/`，主程序 6.05 MiB、含组件整目录 7.30 MiB；哈希与 222 项单测、52 项产品、48 项环回、25 项 UIA、五轮压力等证据见[产品预览验收](Windows原生产品预览验收.md)。

已修复审查发现的仅本次 TLS 信任误落盘风险、权限确认顺序／超时、过期展示、解绑旧事件隔离、异常后台退出清理、串口写入前回显脱敏竞态和日志页未读计数。单测与最终 EXE 验收分别记录，不能只凭源码将现场通过预填。

- 本轮完整范围、运行方式、验收和限制见[原生产品预览验收](Windows原生产品预览验收.md)。
- 实际交换机 SSH、物理串口及此前特殊环境尚未联调验收。
- 新版真实 RDP 断开重连、高 DPI 完整往返仍按用户要求暂缓，旧 renderer／0.1.2 的历史结果不自动覆盖产品入口。
- 资源内嵌释放、按需下载资源站尚未实现，仍为后续分发架构评估事项。
- 正式迁移仍未决定；旧 GUI 和原 0.1.2 保留，不切换默认入口。

## 继续工作前检查

- 先校验待测 EXE 哈希与本表一致；保留旧版和原始失败报告。
- 询问用户是否方便实际 RDP 测试，准备好后再启动持续输出，避免等待期间输出已结束。
- RDP 与本机缩放分开进行，不在 RDP 测试中改变本机屏幕缩放。
- 不为获得通过结果放宽 100 毫秒、每十万行 15 秒、计数与实际文本一致性门槛。
- 磁盘清理只处理可再生缓存；报告、源码、最终候选与旧版程序保留。清理结果另记，后续构建可能需要重新生成已清除缓存。

## 磁盘清理记录

本次已清理约 1.16 GiB 可再生缓存和重复测试程序；旧版、最终小样、原始报告与失败记录保留，部分测试 EXE 重复副本已删除并记录同哈希保留位置。D 盘仍紧张，Codex 三处旧暂存受自动审批拦截尚未清理；详情与未处理项见[D 盘缓存清理记录](D盘缓存清理记录-20261030.md)。

## 新增需求：恢复旧版界面与共用观察窗

用户要求使用新原生方案恢复旧版界面，并让 SSH、串口和普通命令执行共用只读观察窗。已获确认并实施，分阶段范围见[迁移实施计划](Windows原生界面迁移实施计划.md)，最新进度见[原生产品预览验收](Windows原生产品预览验收.md)。原暂缓的 RDP／高 DPI 复验安排不变。

## 0.2.1 传输目录与 SSH 链路

2026-10-31 按用户确认继续当前分支，优先功能、界面样式最后处理。已完成固定传输根、区分控制端/Agent 目录、上传绝对路径回执、严格成功判定、实际 Plink 密码 SSH、显式主机密钥核对入口、跨进程信任库保存、关闭挂起请求与固定 UUID 的关闭目标。本机真实整链路 42 项、最终相关单测 368 项通过，3 项既有忽略；具体证据、失败历史与验证边界见[0.2.1 验收](传输目录与SSH链路完善验收-0.2.1.md)。

- 真实交换机和此前特殊环境没有可用目标，仍待验；当前 SSH 为 exec/无 PTY，不自动尝试交互 Shell。
- 原生 GUI 的真实 SSH 可视化全流程、物理串口仍不能用命令行 Agent 的 SSH 结果替代。
- RDP 和高 DPI 往返继续暂缓，未进行任何现场断开或屏幕缩放操作。
- 0.2.1 产品完整界面复验出现剪贴板占用与复制 Invoke 的 UIA 客户端超时，仍待后续界面阶段处理；编译并行时普通命令观察窗有一次 107ms 超标。编译结束后最终交付版独立真实环回 48/48 通过，最大 tick 81ms、交互页 74ms，实际文本追赶和退出正常。原始失败保留，不引用 0.2.0 的 52/48 项结果冒充本轮通过。
- 旧 GUI、0.1.2、0.2.0 全部保留；未替换安装或现场组件，未决定正式迁移。

## 0.2.2 复制与 UIA 修复

2026-10-31 当前分支继续，旧版含 0.2.1 保留。剪贴板调用已移出 UI 线程，1500ms 有界重试、超时反馈、同码合并、退出不 join；空闲不克隆日志快照。最终原生/UI-only 单测 42/29、独立剪贴板 24、真实输出并行 39 项通过，另保存并行剪贴板子检查 24 项；最终最大 tick 27ms、页转场 51ms，退出正常。旧 0.2.1 相同短占用用例失败，作为负对照保留。

详细改动、哈希及中间失败见[0.2.2 验收](复制按钮与UIA修复验收-0.2.2.md)。前候选完整演示曾有 167ms 转场，最终完整 52 项演示未重验；真实交换机/串口、RDP、高 DPI 和正式迁移仍待验。用户提供纪要中的 Win32/Slint 同核心同功能比较方向见[纪要对照](界面路线纪要对照-20261031.md)，本轮未开发 Slint 或安装设计技能。

## 0.3.0 双界面对照预览

2026-10-31 已在当前分支提取共享桌面宿主，提供 Win32 与 Slint 1.18.1 两套独立预览，旧版继续保留。共享/Win32/Slint 单测 17/28/18、产品界面 52/55、真实 GUI 回环 52/51、Win32 剪贴板并行子检查 24、Plink/传输回环 42 项通过。构建、哈希、命令、失败历史和验收范围见[双界面构建验收](双界面构建验收-0.3.0.md)。

真实现场、交换机、物理串口、RDP 与高 DPI 仍待验；Slint 渲染性能、同口径剪贴板压力和长期耐久尚未补齐。观察流有界，突发输出中间显示可被合并，不能将保留模型追赶理解为完整归档或零丢行。暂不决定正式迁移。

## 0.3.1 旧版样式确认与 Slint 退出

2026-10-31 用户确认改走 Rust + Win32 原生控件和少量自绘，并要求先运行不含业务逻辑的界面，确认样式后再绑定现有共享宿主。继续当前分支，旧 GUI、0.1.2 / 0.2.x / 0.3.0 Win32 生成物保留，不提交或推送；RDP 与真实缩放仍暂缓。

- 已删除活动 Slint 前端、3 个专属测试脚本、双界面打包脚本及专属许可目录；移除 workspace 成员、宿主 Slint 分支和锁中 174 个专属包。未升级其他依赖。历史对照报告及不可变旧测试包仅作历史证据，不再参与当前构建；不清扫其他项目的 Cargo 全局缓存。
- 新增 `apps/remoteops-agent-native-prototype/src/style_preview.rs`，沿旧 GUI 当前 500×375 紧凑主窗及精确浅深色，提供齿轮菜单、中英文、详情、权限、首次配置、TLS、退出确认、完整日志页及独立只读观察窗。使用系统字体、GDI 线性图标、标准 BUTTON / EDIT；不引入另一套 GUI 框架。
- `--style-demo` 为显式样式入口；专用 `--no-default-features --features style-preview` 构建双击默认显示样式，不含 Agent 运行时。主程序实际 311,808 字节（304.5 KiB）；这不是最终业务版本体积。
- 所有代码、控制码、权限和连接结果均为合成演示。复制仅模拟对勾反馈；保存、连接、SSH 和串口不执行真实行为，不读写旧配置或剪贴板，不建立远端连接。观察样例最多 200 行。
- 构建：`scripts/Build-NativeStylePreview.ps1 -OutputDirectory <不存在的新目录>`，显式使用 C 盘 CARGO_TARGET_DIR。窗口验证：`scripts/Test-NativeStylePreview.ps1 -Executable <EXE> -ExpectedHash <SHA256> -ExpectedVersion <产品版本> -OutputDirectory <不存在的C盘TEMP目录>`。生成物和实测截图见 `artifacts/native-style-preview-0.3.1-20261031`。
- 样式演示单测 30 项、严格 Clippy、格式检查通过；候选窗口/UIA检查 20 项及浅深色、中英文和各弹窗截图已实际复核。最终交付以同目录证据及 manifest 绑定的 SHA-256 为准。
- 失败历史保留：首次 UIA 夹具把 owner 的 owned 对话框重复纳入 Descendants，已按实际 GA_ROOT 过滤；首次标题/资源图标缺失已修正；变量重命名误改 CRLF 转义导致多行文本出现 ect，已修复、补两项真实拼接入口回归并重新截图，不能只用 UIA 内容检查代替视觉检查。

当前等待用户确认主窗及小窗口样式。随后把既有 `ProductHost` 的快照和动作绑定到确认后的视觉层，继续复用唯一业务核心；需重新验收真实 TLS / 权限 / 传输 / 普通命令 / SSH / 串口及退出。不将本次样式小样结果当作业务迁移完成。正式 RDP、高 DPI、真实交换机和物理串口仍未验。
## 0.3.2 标题对齐、观察窗入口与逐窗体亮暗相册

2026-10-31 继续当前分支，仅调整纯样式演示，不绑定业务、不替换旧版。参考 frontend-design 与 ui-ux-pro-max 的层级、对齐和焦点原则；本地检索未找到直接适用的 Win32 标题模板，因此以真实控件坐标和逐张截图验收。

- 系统标题缩短，版本移至版本信息；状态标题减轻字重，标题、圆点、齿轮纵向对齐。暗色调用系统 DWM 标题栏接口，不引入新的 GUI 框架。
- 主窗底部为“连接详情 / 实时日志 / 操作实况 / 退出 Agent”。操作实况按钮按真实可见状态显示或隐藏同一个观察窗；关闭和再次打开保留内容，打开不抢前台焦点。详情中的观察窗入口继续保留。
- 12 类窗体／状态各拍亮暗两张，共 24 张原生实拍：等待连接、设置菜单、连接详情、连接演示、权限、权限二次确认、退出、首次设置、TLS、日志、观察窗、版本信息。相册：[逐窗体亮暗对照](../artifacts/native-style-preview-0.3.2-20261031/gallery/index.html)。原生设置菜单仍跟随 Windows 系统颜色；未改色截图。全部实际 96 DPI，不代表高 DPI 验收。
- 最终独立生成物：`artifacts/native-style-preview-0.3.2-20261031/delivery/RemoteOps-Win32-StylePreview.exe`，313,856 字节（306.5 KiB），比 0.3.1 增加 2,048 字节；SHA-256：`cb8b034599af128037195d5080f5bc0ce4c92e2dbf2c1daab2059a17a633e2b2`。仍不含 Agent 运行时，不能作为正式产品体积。
- 验证：`cargo test --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview` 30 项通过；对应 Clippy `--all-targets -- -D warnings`、原生与共享宿主格式检查通过。
- `scripts/Test-NativeStylePreview.ps1` 30 项窗口回归通过，包含入口显隐、关闭重开、同 HWND / 内容保持、只读及不抢焦点；进程正常退出 0。`scripts/Capture-NativeStyleWindows.ps1` 独立截图流程 64 项通过，24 个文件摘要核验一致，两个主题测试进程均正常退出 0。候选和最终交付 EXE 的 SHA-256 完全一致，证据位于该交付目录 `evidence` 和 `gallery/index.json`。
- 完整截图脚本：`scripts/Capture-NativeStyleWindows.ps1 -Executable <EXE> -ExpectedHash <SHA256> -OutputDirectory <全新的C盘TEMP目录>`；`-SelfTest` 仅检查窗体清单与 HTML 转义，不启动 GUI。构建仍用 `scripts/Build-NativeStylePreview.ps1`，显式 C 盘缓存。

当前仍等待用户确认样式后再绑定共享业务宿主。所有控制码、权限、SSH／串口／命令输出均为合成数据。真实业务迁移、交换机、物理串口、RDP 断开重连及高 DPI 往返继续待验，本轮未改变屏幕缩放或操作远端。
## 0.3.3 首次设置输入框与版本信息精修

2026-10-31 用户明确“最后一个对话框”为网页相册末项版本信息窗。继续当前分支，仅调整这两处视觉，不修改已认可的其他窗体或真实业务；0.3.2 及更早生成物保留。

- 首次设置继续使用标准单行 Unicode EDIT，不用多行模拟输入框。根据实际选中字体的 TEXTMETRIC 高度居中，左右留白 10 DIP；父窗绘制细圆角外框和焦点色，并裁剪子控件区域，避免焦点重绘覆盖文字／光标。外框留白区域点击可聚焦原生编辑框。
- 版本信息窗采用既有品牌图标，名称、版本、说明分层；版本来自包版本常量。简化预览说明，统一 24 DIP 内容边距、细分隔线与确认按钮。保留系统标题栏和标准可访问按钮，不加入新字体、动效或 GUI 依赖。
- 生成物：`artifacts/native-style-preview-0.3.3-20261031/delivery/RemoteOps-Win32-StylePreview.exe`，314,880 字节（307.5 KiB），比 0.3.2 增加 1,024 字节；SHA-256：`9d164b1c3ed979ea6cd61404e76f246b533367e597e9ad9c4a352fd59f97c973`。仍仅为样式小样，不含 Agent 运行时。
- `cargo test --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview`：30 项通过；`cargo clippy --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview --all-targets -- -D warnings`、`cargo fmt -p remoteops-agent-native-prototype -p remoteops-agent-desktop-host -- --check` 通过。构建命令与脚本沿用 0.3.2 记录，使用全新交付目录与 C 盘缓存。
- 最终哈希对应的 `scripts/Test-NativeStylePreview.ps1` 38 项通过，进程退出 0。新增检查覆盖实际编辑控件垂直中心、仍为单行可编辑控件、长中文含空格值保留、版本／说明分层、原生按钮 Invoke、关于窗关闭恢复主窗，以及暗色英文输入框和版本信息。中文值验证为 UIA ValuePattern，不等同于人工输入法候选窗验收。
- 失败记录保留：最终构建首次复验在进入本轮修改窗体前，“从连接详情打开观察窗不抢焦点”取样不一致。原夹具基线在截图之前取得，未保留完整前后数据，不能确定失败原因；现改为同一个 Invoke 工作进程记录操作前后句柄，并与稍后快照核对，三个句柄一致后通过。未修改观察窗实现，也未放宽句柄一致的要求；原始失败见 `evidence/uia-first-final-failure.json`，最终证据见 `evidence/ui-regression/observer-focus-evidence.json`。

另保留两次相册夹具失败：一次菜单未收起、一次后台菜单未出现。截图客户端现先通过 UIA 聚焦本次进程的齿轮，再打开原生菜单；选择后向该主窗发送 WM_CANCELMODE 收起。独立菜单探针实测打开 1 个、收起后 0 个、正常退出，未改产品菜单实现；超时夹具新增最后快照记录。

最终相册入口：[逐窗体亮暗对照](../artifacts/native-style-preview-0.3.3-20261031/gallery/index.html)。截图实际数量、DPI、窗口边界、文件摘要及逐项检查见同目录 `index.json`；未进行 RDP、屏幕缩放、真实 SSH 或物理串口测试。正式业务绑定继续等待样式确认。
## 0.3.4 日志紧凑排列与主窗按钮统一

2026-10-31 用户确认继续，并授权同区域的小幅合理修整。当前分支不变；旧 GUI、此前核心及 0.3.1／0.3.2／0.3.3 样式生成物保留，9 项基线摘要一致。仅修改样式模块、相关测试和构建版本，不接入业务，不改 RDP 或缩放。

- `presentation_lines` 不再提供人为插入空段落的选项，记录间只拼接单个 CRLF；不删改记录内的换行或真正空记录。新增回归覆盖中文、emoji、真实空行和多行命令输出。
- 主窗底部四按钮统一为 107×37 DIP，间距 8 DIP，左右各 24 DIP；主窗仍为 500×375。按实际字体字宽将 17 DIP 图标、6 DIP 图文间隙和文字作为一组居中。键盘焦点框围整按钮；观察窗已打开时使用正常前景文字和蓝色边框，改善暗色文字辨认。
- 最终 EXE：`artifacts/native-style-preview-0.3.4-20261031/delivery/RemoteOps-Win32-StylePreview.exe`，314,880 字节（307.5 KiB），与 0.3.3 同体积；SHA-256：`28b64e4cc8921d5f52aa95371a63dfb7c70b6c62f98950b077ab57a4f206c8d0`。`candidate-pre-contrast` 仅为调整已打开按钮文字颜色前的候选，正式本轮入口为 `delivery`。
- `cargo test --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview`：31 项通过。`cargo clippy --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview --all-targets -- -D warnings`、`cargo fmt -p remoteops-agent-native-prototype -p remoteops-agent-desktop-host -- --check` 通过。
- `scripts/Test-NativeStylePreview.ps1 -Executable <最终EXE> -ExpectedHash <上述SHA256> -ExpectedVersion <产品版本> -OutputDirectory <全新的C盘TEMP子目录>`：最终 EXE 52 项窗口回归通过、退出 0。覆盖四按钮绝对尺寸／相同宽高／等间距、中文和英文含 Hide viewer 状态下的文字容纳、观察窗显隐／内容／焦点、日志四条无额外空段落、错误筛选、恢复全部、清空后仍只读，以及此前输入框和弹窗回归。
- 独立旧版 0.3.3 对照复现额外空段落，保留 4 条非空记录，按钮宽度分别 116／88／108／118；原始文本与尺寸见 `evidence/old-033-comparison.json`。只操作新启动的对照进程，未关闭用户的旧版预览。
- 本轮定向提供主窗、日志页亮暗 4 图，另附暗色英文观察窗打开状态图：[本轮亮暗对照](../artifacts/native-style-preview-0.3.4-20261031/gallery/index.html)。未重拍所有 24 张；相册明确链接 0.3.3 的完整窗体对照。截图均为原始 PNG，不改色；摘要见同目录 `index.json`。
- 中间失败保留：初次 Clippy 发现 UTF-16 长度转换风险，改用有界 `try_from`；首轮 UIA 字宽测量直接复用另一进程的 HFONT 导致默认字体测量偏大，夹具现读取实际 LOGFONT、在测试进程创建并释放副本。初次报告、错误测量快照和 Clippy 日志保留在 `evidence`；修正后尺寸与文字检查通过。独立只读代码复核已完成，补上其建议的绝对尺寸断言。

本轮仍是合成数据的纯样式预览；未验证真实业务、RDP 断开重连、高 DPI 往返、真实交换机或物理串口。既有待验事项不变，未决定正式迁移。
## 0.3.5 退出提示正文背景匹配

2026-10-31 根据用户截图修正退出确认卡片的灰白内块。问题是父窗绘制 danger_bg，而正文 STATIC（461）的 WM_CTLCOLORSTATIC 仍返回普通窗口底色画刷；TRANSPARENT 仅影响文字背景，不取消控件刷底。

- 仅对退出确认正文使用与父卡片一致的背景色，并返回设色后的系统 DC_BRUSH；不为重绘额外分配 HBRUSH。其他标签、编辑框、标题和按钮继续原有背景规则；退出行为不变。
- 本轮独立交付：`artifacts/native-style-preview-0.3.5-20261031/delivery/RemoteOps-Win32-StylePreview.exe`，314,880 字节（307.5 KiB），与 0.3.4 同体积；SHA-256：`4a03f564ea9103df375938eba4b76d024b8040678e7677aa753d1d91910478e6`。10 项旧版摘要一致，未替换旧版或修改用户正在查看的旧退出窗。
- `cargo test --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview` 31 项通过；`cargo clippy --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview --all-targets -- -D warnings`、`cargo fmt -p remoteops-agent-native-prototype -p remoteops-agent-desktop-host -- --check` 通过。
- 新增实际截图像素回归：`scripts/Test-NativeStyleExitBackground.ps1 -Executable <EXE> -ExpectedHash <SHA256> -OutputDirectory <全新的C盘TEMP目录>`。修复版 19 项通过，包括浅色、弹窗打开时切换深色／浅色、深色重新打开；每帧比较正文 3 个空白点、父卡片和外部普通底色，验证颜色一致且外部不受影响。另验证 owner 禁用、取消和关闭恢复主窗、正常退出 0。
- 对精确 0.3.4 EXE 以 `-ExpectMismatch` 运行同脚本，19 项负对照通过，四帧正文均与父卡片不同色，复现用户截图；不是把旧版当作修复通过。两次原始报告分别保留在 `evidence/exit-final` 和 `evidence/exit-old-negative`。
- 本轮实际 DPI 96；[修复后亮色](../artifacts/native-style-preview-0.3.5-20261031/evidence/exit-final/exit-light.png)、[修复后暗色](../artifacts/native-style-preview-0.3.5-20261031/evidence/exit-final/exit-dark-reopened.png) 已逐张查看，另外两张主题切换原图与 SHA 在同目录。只进行退出窗口定向回归，没有重跑 52 项完整窗口套件或重拍全部 24 图。

仍为独立样式预览；真实业务、RDP、屏幕缩放和真实设备联调待验。本轮没有更改系统主题、缩放或远端连接。

## 0.3.6 中性背景与统一原生文本区

2026-11-01 按用户确认继续当前分支，去掉退出正文大块粉色，保留红色确认退出按钮。主窗浅色底仍为 `#F7F8FA`，只读内容白色；暗色内容为 `#11151C`，以细边框和内间距分层。日志、操作观察窗及 TLS 指纹共用 `style_preview/readonly_view.rs`，标准 EDIT 继续承担只读、选择、复制、键盘、滚轮、滚动位置和 UIA。

- 滚动条仅在内容超出时显示，横纵互相占位按同一几何模型计算；视觉滑块为 6 DIP 圆角，原生有效命中宽度不缩小。透明且不激活的分层子窗只绘制轨道和滑块，鼠标仍落在 EDIT／原生 ScrollBar；没有增加 GUI 框架、远程终端或自制滚动引擎。跟踪重绘计时器仅在原生拖拽期间启动，隐藏、取消、销毁时清理。Manifest 补齐 Windows 8 及后续兼容声明以支持分层子窗。
- 同步修正三处实测问题：大段初始文本在 EDIT 创建后设置，避免创建失败；“返回最新”即使内容未变化也实际滚到底；附属窗口按所属显示器工作区约束初始位置，避免大窗顶部出屏。纯几何和窗口位置新增回归。
- 最终独立生成物：`artifacts/native-style-preview-0.3.6-20261101/delivery/RemoteOps-Win32-StylePreview.exe`，329,216 字节（321.5 KiB），比 0.3.5 增加 14 KiB；SHA-256：`f1bd05f30141dabe844915242024c4502a996abd40da762af6ddf4ea83497c27`。仅合成数据的样式小样，不含 Agent 业务运行时；不可与完整产品体积直接比较。11 项旧生成物摘要一致，未替换旧版。
- `cargo test --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview`：40 项通过。对应 `cargo clippy --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview --all-targets -- -D warnings` 和 `cargo fmt -p remoteops-agent-native-prototype -p remoteops-agent-desktop-host -- --check` 通过。
- `scripts/Test-NativeStyleScrollbars.ps1 -Executable <EXE> -ExpectedHash <上述SHA256> -OutputDirectory <全新的C盘TEMP目录>`：84 项通过、退出 0。覆盖亮暗三类短文本无多余滚动条、10001 条合成记录／579615 个 UTF-16 字符、横纵真实拖拽、按住期间的实际屏幕像素、松手位置不回跳、滚轮／键盘／选择、返回最新、原生 UIA ScrollPattern 及轨道 Thumb/RangeValue 实际驱动正文。四个命中点 `WindowFromPoint` 均为原 EDIT；GDI 31→32，USER 66→66。极长首行被 EDIT 内部格式化成多个显示行，不能用显示行数冒充原始记录数。
- `scripts/Test-NativeStyleTextCopy.ps1` 同口径精确 EXE 测试：7 项通过，实际原生复制中文选区且正文不变；测试前备份、测试后校验恢复原剪贴板的 4 种格式，不在证据中记录个人剪贴板正文。遇无法安全复制的格式或外部改写会中止或保留外部改写。
- `scripts/Test-NativeStyleExitBackground.ps1 ... -Surface Neutral`：19 项通过、退出 0；亮暗、打开中切主题、正文背景像素一致、取消／X 恢复 owner、正常退出。历史粉色对照复验需显式 `-Surface Tinted`，不能用新版中性预期套旧版。
- `scripts/Test-NativeStylePreview.ps1 -Executable <EXE> -ExpectedHash <上述SHA256> -ExpectedVersion <产品版本> -OutputDirectory <全新的C盘TEMP目录>`：最终完整 52 项通过、退出 0；主窗和详情两个入口的观察窗打开前／即时后／稍后快照句柄完全一致，原始取样均保存。另以 `-ClearBackOnly` 独立 3 项通过，覆盖清空后仍只读、返回主界面、取消退出及最终退出；未以此 3 项替代完整套件。桌面门禁纯决策矩阵 14 项通过，未人为锁屏或切换安全桌面。

中间失败如实保留：仅覆盖 EDIT 非客户区绘制时，Windows 在按住原生滑块的追踪循环中仍会画回白轨道；`candidate-failure` 留存实际截图及像素失败，最终通过透明装饰层解决。早期测试另暴露超长文本创建、窗口初始越界及原生拖拽时回跳检查要求，已修复后重验。锁屏时停止 GUI 验证，用户解锁后继续；两次整轮回归在清空后的快照阶段被旧桌面门禁拦截，第二次现场进程仍响应，没有可用快照，不能据此断言程序卡死。第三次诊断明确记录前台为 0，但线程桌面与输入桌面均 UOI_IO=true，证实严格前台门禁误拦合法失活转场。夹具现对门禁失败输出安全原因和桌面输入状态，原样归为环境不可用，不再伪报控件缺失；随后依据线程与输入桌面的输入状态和内部身份匹配处理前台为空的情况，未知状态及明确 LockApp／LogonUI 仍拒绝，实际 SendInput 的精确前台／命中校验保持不变。目标 GUI worker 另核对同一会话。三次门禁失败报告保留；第四次在主窗首次打开观察窗的焦点取样不一致，未保存当时完整前后句柄，不能确定原因。测试现增加该入口的完整前后句柄和快照记录，不放宽原有相等断言；该失败原始报告也保留。

Windows 实现依据：[分层子窗兼容要求](https://learn.microsoft.com/en-us/windows/win32/winmsg/using-windows#using-layered-windows)、[透明分层窗口命中行为](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features#layered-windows)、[GetScrollBarInfo](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getscrollbarinfo)。GetForegroundWindow 在失活转换时可以合法返回 NULL，单凭空值不可判定真实锁屏，参见[微软说明](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getforegroundwindow)。

本轮[亮暗实拍相册](../artifacts/native-style-preview-0.3.6-20261101/gallery/index.html)包含主窗、退出、日志、TLS、观察窗共 5 对／10 张原始 PNG，另附 4 张按住原生滑块时的真实屏幕图及返回最新图；并非重拍全部 24 张。原图摘要见同目录 `index.json`；交付证据汇总为 `evidence/verification.json`，中间失败未覆盖。实际为本机 96 DPI。未调整系统主题／显示缩放、未操作 RDP、未连接交换机或物理串口。正式业务绑定、真实设备、RDP 断开重连和高 DPI 往返继续待验，未决定正式迁移。

## 0.3.7 观察窗分类筛选与并发状态演示

2026-11-01 用户确认先在当前分支的纯样式小样演示，不绑定业务；旧版保留，RDP／缩放继续暂缓。

- 同一个观察窗顶部新增“全部／CMD／PowerShell／Shell／SSH／串口”。默认全部，按同一记录集合的原顺序显示；筛选依据结构化类别，不从命令文字猜测。单击分类保留同一个原生只读 EDIT，不新开窗口。空分类显示单独提示，不伪造输出记录。
- 蓝色边框表示正在查看，绿色稳定亮点加 `·N` 表示该类真实的合成活动数量；多类可同时亮，同类 SSH 可并发 2 项。全局状态明确“查看：…／执行中：N”，选其他分类不会隐藏整体活动状态，演示开始或结束不会自动切换筛选。
- 新增“演示并发”和“全部结束”，这两个按钮仅控制合成演示，不是正式观察窗的远端命令执行／取消入口。约 25 个每秒逻辑步骤演示 CMD、PowerShell、SSH 双任务、Shell 和串口交错输出，最终全部完成。每项有任务短号；全部结束保留历史并清空待执行计划，不会随后继续冒出旧输出。普通演示最多保留 200 条历史，活动计数独立于历史淘汰；显式大量日志演示仍保留 10001 条。
- “暂停”明确改为“暂停显示”：模型状态继续推进，正文冻结；筛选可立即查看该类快照，返回最新恢复显示且不改分类。底部数量是当前分类／全部缓存记录数，暂停时仍随模型更新，不表示当前屏幕正文行数。暂停时切主题／重建保留正文，隐藏期间结束再重开立即追赶状态；菜单“结束全部演示”不会擅自显示已隐藏的窗口。
- 修改集中在 `style_preview.rs` 和新增的 `style_preview/observer_demo.rs`；新增分类专用 GUI 夹具 `scripts/Test-NativeStyleObserverCategories.ps1`，构建／截图／已有测试版本适配 0.3.7。保持原生 BUTTON／EDIT、现代滚动条和系统字体，不增加 GUI 框架，不修改网络／Shell／SSH／串口业务代码。
- 精确交付为 `artifacts/native-style-preview-0.3.7-20261101/delivery/RemoteOps-Win32-StylePreview.exe`，355328 字节（347 KiB），SHA-256：`b1d43b93655bc3e58113943add05fbf497d6967a8db6e15dcfb7d6d2b53edbe9`。比 0.3.6 增加 25.5 KiB，仍不含 Agent 业务运行时。12 项旧生成物摘要均一致。
- 单测命令 `cargo test --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview`：53 项通过（新增结构化归类、并发计数、自然／手动结束、选择不跳转、历史有界和暂停重建）。同特性 `cargo clippy ... --all-targets -- -D warnings` 与 `cargo fmt -p remoteops-agent-native-prototype -p remoteops-agent-desktop-host -- --check` 通过。

- `scripts/Test-NativeStyleObserverCategories.ps1 -Executable <EXE> -ExpectedHash <上述SHA256> -OutputDirectory <新的C盘Temp目录>`：最终完整 86/86 通过，11 张原始 PNG，退出 0。覆盖亮暗所有分类、原文恢复、同 EDIT HWND、并发与不跳分类、暂停中切主题冻结正文、手动／自然完成、隐藏期间结束后重开不残留；32 次单次 Invoke 诊断完整保存。

- 同脚本 `-VisualOnly`：17/17 通过、退出 0。亮暗各一张，选 PowerShell 的蓝边与 SSH×2 绿点分别按原生控件坐标核验精确像素；自动显示前使用有效本次 Main 基线，无聚焦 Command 5043 显示观察窗后前台句柄相同且非 Observer。没有用手动聚焦流程替代不抢焦点检查。
- 同脚本 `-LargeOnly`：14/14 通过、退出 0。10001 条／579615 字符、Shell 筛选原样、PowerShell 真空且无多余滚动条、恢复全部原样、筛选全程同一 EDIT HWND。不是重跑旧 84 项滚动套件；本轮亦未重拍全部窗体或重跑旧 52 项整窗体套件。

- 本轮额外运行 `scripts/Test-NativeStyleTextCopy.ps1`，在备份阶段主动中止：只读探针确认当前剪贴板有 1 种不在安全克隆白名单内的数据，尚未启动测试小样、未写入测试内容，也未记录个人内容。该项记为“未完成／待验”，不能引用 0.3.6 的 7 项结果当成本轮实际复制通过；记录见 `evidence/native-copy/report.json` 和 `clipboard-readonly-probe.json`。分类本身的原生只读／同 HWND／UIA 检查已在上面 86 项中验证。

中间问题和证据保留：只读代码审查发现隐藏重开／菜单结束可能残留旧状态，已补主动同步；发现暂停中主题重建会推进正文，已保留旧正文并补单测。首次 GUI 夹具错误要求多行原生 EDIT 的 UIA ControlType 必须是 Edit；本机实际为 Document，已改为允许这两种系统映射，并独立核验真实窗口类 EDIT、ES_READONLY 和只读模式，未改产品控件。另有两次后台 UIA Invoke 没有产生预期按钮动作（第二轮暂停、首次打开观察窗），进程正常退出；现场不足以认定产品死锁。夹具新增显式 `-FocusBeforeInvoke`，只对核验后的本次按钮调用 UIA SetFocus，再单次 Invoke 并等待实际状态；默认行为不变，自动显示不抢焦点需单独以无聚焦的 Command 验证，不以“先聚焦”冒充无焦点验收。该调整与[微软关于非活动对话框 BM_CLICK 可能失败的说明](https://learn.microsoft.com/en-us/windows/win32/controls/bm-click)一致，但未声称已证明本机 UIA 内部具体消息实现。聚焦短探针 13 项通过；一次后续整轮因前台 HWND 在 PID 查询前已失效（错误 1400）而被桌面门禁中断，当时输入桌面有效，并非已确认锁屏。夹具对已确证销毁的前台句柄增加最多 11 次／约 500ms 有界重采，未知活窗口仍拒绝，并继续使用输入桌面身份校验；旧 14 项与新增 19 项纯决策矩阵通过。随后诊断确认前台曾指向本次小样的 Button，排除 worker 临时窗口猜测，但未唯一归因至 UIA 内部调用；尝试顶层 UIA SetFocus 被系统明确拒绝，已保留失败并恢复经过两次 13 项探针验证的原生按钮聚焦路径，不引入额外激活手段。相关失败与诊断保留。最终 GUI 结果与原始截图存交付目录 evidence，不将初次夹具失败当作产品失败或最终通过。

本轮[观察窗分类／并发亮暗相册](../artifacts/native-style-preview-0.3.7-20261101/gallery/index.html)提供 3 组亮暗对照与 3 张阶段图，共 9 张原始 PNG，图片摘要与尺寸见同目录 `index.json`。完整通过报告、未完成的复制复验、旧失败和 12 份旧版摘要统一保留在 `evidence`。

本轮仍为合成状态和合成输出；没有执行截图内的命令、没有读取真实凭据、没有连接设备。真实业务绑定、真实交换机／物理串口、RDP 断开重连、高 DPI 往返继续待验；不据此决定正式迁移。

## 0.3.8 观察窗标准最大化／还原入口

2026-11-01 根据用户“可以”确认，继续同一分支做小范围补充。观察窗由工具标题栏改为常规 Win32 标题栏，保留 `WS_OVERLAPPEDWINDOW`、主窗 owner 和自动显示不激活语义，显示标准最大化／还原按钮。最小尺寸计算同步按同一扩展样式处理。没有引入框架／依赖，不实现 F11 无边框全屏，不改业务、RDP 或缩放。

- `style_preview.rs` 既有窗显示使用 `SW_SHOWNA` 保留正常／最大化状态，去掉重复的普通显示；最小化窗口再次打开时使用 `SW_SHOWNOACTIVATE` 恢复普通可读尺寸。新窗仍不激活显示。关闭观察窗仍是隐藏，重开复用原 HWND、筛选和内容。
- 新增 `scripts/Test-NativeStyleObserverWindow.ps1`，专门验证系统窗口样式、WindowPattern 最大化／还原、工作区边界、原矩形／WindowPlacement、800×470／1000×640／640×360 内容区布局、筛选保持、显隐及正常退出。原生尺寸调用和边角 `WM_NCHITTEST` 证明可调边框；不冒充实际鼠标物理拖拽验收。
- 最终独立交付：`artifacts/native-style-preview-0.3.8-20261101/delivery/RemoteOps-Win32-StylePreview.exe`，355328 字节（347 KiB），与 0.3.7 同体积，SHA-256：`27373496b7bdd85e26b284c926ebfe26e2eecfc809044ed80876edf41e81b777`。13 项旧生成物摘要一致，旧版保留，未替换安装。
- `cargo test --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview`：53 项通过；同特性 `cargo clippy ... --all-targets -- -D warnings` 与 `cargo fmt -p remoteops-agent-native-prototype -p remoteops-agent-desktop-host -- --check` 通过。

- `pwsh -NoProfile -STA -File scripts/Test-NativeStyleObserverWindow.ps1 -Executable <最终EXE> -ExpectedHash <上述SHA256> -OutputDirectory <新C盘Temp目录>`：最终 37/37 通过，退出 0，8 次最大化／还原均实际使用系统 UIA WindowPattern，没有走原生后备。亮暗正常／最大化／还原共 6 张原图摘要核验一致；三种 client 尺寸、分类保持、最大化隐藏重开仍最大化、最小化重开恢复普通、同 HWND、自动显示有效前台不变均实际验证。[窗口亮暗实拍](../artifacts/native-style-preview-0.3.8-20261101/gallery/index.html)，完整报告为 `evidence/window-final/report.json`。

初次定向实测在前 29 项通过后发现最大化隐藏再重开会恢复为普通大小；原因是重复使用 `SW_SHOWNOACTIVATE`。已修正现有窗口显示方式，原始失败、6 张截图和正常退出记录保留在 `evidence/window-first-failure`。最终实际窗口测试结果单独保留，不引用首轮失败当作完成。

本轮仅纯样式窗口。业务绑定、真实设备、RDP、缩放继续待验；实际复制仍保留 0.3.7 剪贴板保护中止的待验记录，本轮没有重试个人剪贴板或接触登录界面。

## 0.4.0 已确认 Win32 界面接入共享业务宿主（进行中）

用户已确认界面基本可用，要求现有业务和观察窗分类／运行状态接真实数据。继续 `codex/windows-native-ui-prototype`，旧版生成物保留；RDP 与缩放仍暂缓。本轮不决定正式替换安装。

- 默认原生产品路由到已确认的同一 Win32 窗口层，`--style-demo` 保留隔离样式，`--product-demo` 保留明确的无网络产品演示；旧 self-test／诊断路径仍用于历史对照。新增 `style_preview/business.rs` 仅转发既有 `ProductHost` 动作和快照，不在界面重写配置、TLS、权限、SSH 或串口执行逻辑。正式界面没有合成并发按钮／场景菜单、假控制码或假认证状态。
- Agent 的本地 `OperationContext` 增加 `shell: Option<ShellKind>`，一次性命令使用真实请求类型，持久 Shell 按实际 `ShellRuntime`、ShellId、session 和关闭状态继承；Windows System 归 CMD，其他平台保留通用 Shell。复用实际资源表，没有旁路缓存或网络协议变更。
- Host 新增结构化 `ObservationCategory`、完整请求关联和 `active_operations`。Running 以真实执行事件为准；完成、失败、取消、中断和连接 epoch 切换清理；日志文字、stderr 或历史裁剪不决定活动状态。保留旧展示字段兼容旧消费者，Service 单测构造补新字段。
- 新增 `observer_data.rs`：真实模式没有演示数据，只保留一份分类记录，最多 2000 条和 262144 个 UTF-16 单元（含分隔符）；长记录有显式省略，NUL 可见化，裁剪／展示滞后计数可见。筛选、暂停和显隐不改变后台活动；同类并发按完整请求 ID 计数。
- 复用唯一既有异步剪贴板 worker，防止换码／过期后误显示旧复制成功；TLS 指纹变化即时更新，不能因选区保留旧证书。权限请求成功只表示已提交，以服务端快照确认；默认完全控制与本次临时授权分别确认。语言／主题保存和配置草稿保留由已有宿主规则驱动。
- 接收目录显示真实值并允许原生选择／水平查看完整路径，恢复“打开接收目录”入口及就地失败提示。正常用户停止失败保持界面；消息循环异常退出时先销毁 HWND 再释放 App／Panel，避免悬空回调。
- 新构建 `scripts/Build-NativeStyledProduct.ps1` 默认包含 Agent 核心、固定哈希 Plink 0.83、askpass 及许可证；C 盘缓存／fixture，D 盘只保留最终包与证据。初候选 EXE 6845952 字节（约 6.53 MiB），随包内容 8156607 字节（约 7.78 MiB，不含后生成清单）；实际交付以最终哈希为准，不用纯样式 347 KiB 代表业务版本体积。

最小验证已完成：Agent 单测129通过／1既有忽略；共享Host默认25／无运行时4；原生产品67／独立样式66；Service兼容3；相关严格Clippy通过。新 `scripts/test-styled-native-e2e.mjs` 仅静态自检完成，准备本机隔离 Relay/MCP/TLS／真实 CMD、PowerShell、持久Shell、Plink SSH×2、失败／断线与输出追赶，不能先称整链路通过。

真实交换机、此前特殊现场环境、物理串口无可用目标；单任务取消若无实际控制工具，仅以Agent／Host单测为证，不把断线代替取消。个人剪贴板实际复制复验仍保留0.3.7格式无法安全备份的待验记录。RDP／缩放未做。


### 0.4.0 桌面验收停止记录

用户明确反馈反复弹窗影响使用，已立即停止本轮 GUI 验收并关闭自建测试窗口，核对剩余测试 GUI 为 0；旧版未动。后续先做无需桌面交互的工作，需要桌面时先约时间，不能自动反复重跑。RDP／缩放继续暂缓。

核心真实命令、分类／并发、持久 Shell、Plink／SSH 双任务、断连清理与 5000 行输出已跑到最后退出；进程退出码 1，整场景仍未通过。首次设置 UIA 输入回读不一致，TLS 流程尚未通过；实际详情、默认全控、日志页与偏好保存的新 E2E 断言尚未跑到。定向退出复验被用户叫停，记为中断。未确认原因时不修改证书依赖或声称已修复。

完整范围、已有结果、待验项与独立候选目录见 [0.4.0 接入验收记录](Win32界面真实业务接入验收-0.4.0.md)。


### 0.4.0 不弹窗修复进展

已完成本机源码／无窗口验证：修复设置窗重建丢草稿、TLS预检结果通道异常未及时报告；补齐原生／后台退出失败报告，保留明确故障类别，防止再次只得到退出码1。原生76／独立样式75／Host30与4／TLS5／纯脚本11项通过，未启动产品或UIA。E2E新增显式 `--allow-desktop` 门禁，默认拒绝桌面动作。

原始UIA输入异常和断线退出码1仍待一次约定时段的界面复验；早先TLS流程实际未走到，不能称TLS现场失败或已经通过。新候选只构建不运行，旧版保留，RDP／缩放继续暂缓。详细区分见 [接入验收记录](Win32界面真实业务接入验收-0.4.0.md#2026-11-01-不弹窗排查与修复)。


### 2026-11-02 已约定桌面复验，完整验收仍未通过

观察窗重复测量／同步重绘、相同尺寸重建、同DPI字体创建已优化。e9a1aaa候选单轮shutdown 23项通过（35ms刷新、29ms页面），明确原退出1是质量检查未过，并非崩溃。最新d00d840候选的首次输入、TLS取消／保存／真实连接、目录、权限、日志和偏好功能断言通过；但自动弹窗阶段112ms和切换重建171ms超100ms，完整报告仍为失败，同配置TLS重启尚未走到。所有测试窗已关闭，旧版16个生成物未改。

先不再弹窗；下一步不弹窗分析自动窗口显示与中英／主题整窗重建。不得把旧哈希通过或功能项成功冒充最新候选全套验收通过。详见[本轮桌面结论](Win32界面真实业务接入验收-0.4.0.md#2026-11-02-桌面复验结论与待办)。真实设备／个人剪贴板／RDP／缩放继续待验。


### 2026-11-02 剩余测试补齐与CMD路径修复

TLS保存后同文件重启已实际验证，新00e8c866候选的完整本机回归已跑到最后，94项通过；期间发现并修复CMD一次性执行带引号脚本路径的真实缺陷（中文／空格／&等8项无窗口相关测试通过），没有只改夹具去掉引号。仅本次TLS、保存重启、UI动作、CMD/PS并发、持久Shell、Plink/SSH×2、5000行／断线／清理／模型一致性均有实际动作记录。

完整原报告保留失败：100ms性能仍未达标，最后原敏感输出校验把授权MCP原始结果当诊断。校验范围严格修正后，另做同hash单实例真实Plink口令隔离／窗口诊断脱敏补验，22项通过但性能326ms仍失败；未重写旧报告，也未声称单轮全绿。下一步优先不弹窗优化性能，正式迁移仍待完整验收。

旧版17个生成物未改，所有自建测试窗已关闭；个人剪贴板仍有1种不支持安全备份的格式，未写入。真实交换机、串口、RDP、缩放继续待验／暂缓。[详细验收记录](Win32界面真实业务接入验收-0.4.0.md#2026-11-02-本轮剩余可做测试与cmd修复)。


### 2026-11-02 观察窗阅读状态与任务栏修复

继续当前分支，旧版保留。观察窗改为无Win32 owner的任务栏工作窗口；首次仍按主窗定位，自动显示不激活并尊重最小化，主动入口可还原聚焦，关闭只隐藏，应用退出统一销毁。主题/布局重建保留首可见行、水平位置和UTF-16选区，按原先底部/暂停状态判断追尾；补齐隐藏滚动条返回默认范围时的底部识别。

最终源码103项原生单测、严格Clippy、无运行时样式编译通过。独立候选的98项无窗口检查、8项产品单测通过。新脚本Test-NativeProductObserverWindow.ps1在自启离线演示中47项功能检查通过，实际任务栏2→1→2，最小化/还原/隐藏/取消退出/确认退出无残留。

总质量报告仍失败：最终页面重建1269ms超原有100ms门槛，轮询峰值1ms，后台及原生清理正常，Exit1与性能报告一致。前三轮的前台锁、脚本取跨进程文本/计数问题，以及无滚动条实际边界均保留原始证据；不重写成单轮全绿。暂停合成夹具不代表真实持续日志性能。后续优先不弹窗分析重建耗时；正式迁移、真实设备、个人剪贴板、RDP/缩放仍不据此验收。

### 2026-11-02 页面重建性能优化

已在当前分支改为按声明差异复用Win32控件，保留只读EDIT、测宽缓存及选区；按需更新正文/字体/位置、合并只读布局，旧字体延期释放，重入合并。主题/尺寸变化同时保留暂停或选中时的实际正文。新增单帧微秒级分阶段计时与真实插桩绘制统计，原100ms门槛不变、首次构建不豁免。

原生118项、候选8项单测和严格Clippy、无运行时样式编译、最终EXE98项无窗口检查通过。首轮因前台变为其他应用而中止，原始失败保留；用户找到主窗并确认前台后，最终64项离线界面检查全部通过、脚本与应用Exit0。30帧页面构建峰值40.6ms、复用峰值39.0ms、111次插桩绘制峰值33.6ms，轮询5ms；12个无尺寸变化的观察窗复用帧峰值1.07ms且没有创建/销毁/赋字体/重写正文。18组原生控件身份均保持，主题阅读状态、最小化/还原/隐藏/退出联动及任务栏2→1→2通过。全部测试窗已关闭。

仅本轮离线页面/绘制性能通过，不冒充真实业务全套、语言/日志页/许可页全部差异清理或持续大量日志吞吐通过；真实设备、RDP和缩放等仍按原计划待验/暂缓，不自动正式迁移。

### 2026-11-02 现场功能复验

现场测试主机实际运行EXE的版本/大小/摘要与本轮交付一致。13项有界检查通过：三种Windows命令入口中文、持久Shell、66字节中文上传/回读/清理、上传事件中的绝对路径、分类和执行中1→0、红绿结果、5000行编号完整回传及观察窗末尾保留、只读属性、大量输出后的最大化/还原，以及Plink0.83/helper隔离释放校验启动和清理。Agent与控制连接保留，观察窗切回全部，未改业务代码/系统设置。

实际远端接收目录已核对；历史 `transfers` 目录不是本次接收目录。上传MCP直接返回仍缺少直观绝对路径（事件回执已有），持久Shell内部JSON/标识仍混入观察窗，列为后续改进，尚未修复。

当前MCP把两个命令顺序执行，不能据此证明真正并发；现场100ms逐帧报告未取，不沿用本机40.6ms充数。5000行不替代长期吞吐；串口枚举0个，真实交换机SSH仍缺目标。RDP断开重连/缩放继续暂缓，主题/语言/许可页/退出等现场全套未重做，不自动正式迁移。诊断探针的白名单/参数/目录假设/ValuePattern失败及纠正均保留。

### 2026-11-02 上传回执与Shell可读状态

该次现场运行的旧版MCP尚未包含上传最终回执的保留/合并，现场工具缺路径为旧服务未更新。新构建进一步明确跨平台绝对路径有效性、旧Agent未知路径状态、嵌套SHA冲突；summary直接显示Agent实际绝对路径，永不拿MCP本机根拼出Agent位置。观察窗按明确标注的响应事件隐藏Shell内部JSON，Open/Close/命令结束呈现易读状态，真实stdout JSON、分片无换行、脱敏、stderr/退出码、原wire字段保持。

无弹窗：MCP69通过/2原生凭据库测试忽略，观察投影39、隐藏Shell真实wire集成1、Host36、Native118、候选8及严格Clippy通过。新脚本真实debug与最终发布CLI Agent/Relay/MCP各24项上传检查通过；最终候选EXE98项无窗口检查通过。原始CMD夹具提示符/函数lint和发布MCP子系统预检失败均保留，修正后复验不放宽业务断言。

未弹窗、未操作现场、未切换MCP/Relay。新文本可见验收、启用后的现场回执待复验。RDP/缩放、真实设备、长期吞吐等继续暂缓/待验，不自动正式迁移。

## 2026-11-02 提交前整理与仍待验事项

用户确认整理本轮改动并推送当前候选分支。公共实现与定制产品分别提交到各自仓库；不合并 main，不把测试包、运行配置、凭据和原始日志提交到公共仓库。第三方 Plink 仅保留许可与固定摘要/打包规则，二进制不进入源码提交。Slint 试验源码及依赖不再参与活动构建，历史比较报告保留。

审查补齐 MCP preview.17 的安装器、验证脚本和 README 版本；独立 MCP 打包从 Cargo metadata.target_directory 取程序，跟随 CARGO_TARGET_DIR，避免误用工作区旧 target/release。未重新构建或重标已交付的定制候选内容哈希生成物。

本轮只执行无弹窗验证。RDP、100%→150%→200%→100% 缩放、真实交换机/物理串口、定制候选可见文案与现场性能门槛继续待验；并排安装后的 MCP 版本和现场连接复验不计入本轮。保持旧版和现有进程，未操作 Relay。

提交前新增核验：`cargo fmt --all -- --check`、host/native/MCP/operation_log 共 262 项测试通过（MCP 两项凭据存储测试保持 ignored）；Node 支持测试 27 项、公共 PowerShell 语法 30 文件、Node 语法 7 文件、Python AST 1 文件、定制产品 PowerShell 语法 3 文件、正式产品固定底座校验及已交付定制候选 EXE 的 98 项无窗口检查通过。实际 Cargo metadata 确认 CARGO_TARGET_DIR 指向 C 盘。不据此声称完整标准发布重构建、新安装登记、可见界面或现场功能已重验。

PuTTY LICENCE.txt 使用 Git `-text` 属性保留上游原始字节，避免 core.autocrlf 改写后破坏 Copy-BundledPlink 的固定摘要校验；暂存区 blob 另行核验官方 SHA-256。
