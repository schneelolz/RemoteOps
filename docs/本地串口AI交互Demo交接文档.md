# 本地串口 AI 交互 Demo 交接文档

> 文档状态：真实串口复测交接记录。普通使用先阅读[本地串口 AI 交互 Demo](本地串口AI交互Demo.md)。

更新时间：2026-08-04 23:59:00

## 一、当前结论

`remoteops-serial-demo` 已升级至 `0.5.0`。真实 `COM7` 现场已经确认串口参数、华为交换机 RX/TX、普通查询、分页、终端/管理模式切换、Backspace/Delete/方向键，以及旧版 AI 主动申请并获批执行 `display version` 后等待真实响应和继续分析。

`0.5.0` 将已经验证的终端、记录、授权、查询运行器和脱敏能力抽到 `remoteops-serial` 核心类库，Demo、正式 Agent 和 GUI 统一复用。AI 不再依赖多轮 `write_serial + wait_for_serial` 工具循环，而是通过一次 `run_serial_query` 完成写入、等待、分页、提示符识别和脱敏，解决现场出现的“工具调用超过最大轮数”。当前状态是“本地串口人工交互和旧版单次 AI 主动查询通过；`0.5.0` 新查询运行器与正式远程 Agent/GUI 路径等待真实 COM 复测”，不能表述为新架构硬件验收通过。

Demo 只测试本机串口通讯：

- 人工查看串口输出；
- 人工发送文本或 HEX；
- AI Agent 读取当前有界缓冲；
- AI Agent 发起结构化串口查询；
- 默认由人工拒绝或逐次批准，也可显式建立限时限次的华为只读授权；
- 核心查询运行器等待设备响应、自动翻页、识别提示符、脱敏并返回 AI 分析。

Demo 不连接 RemoteOps Relay 或远程 Agent，不验证远程串口协议。

## 二、交付位置

| 内容 | 路径 |
|---|---|
| Windows Release 程序 | `artifacts/windows-x64/remoteops-serial-demo.exe` |
| Demo 源码 | `apps/remoteops-serial-demo` |
| 串口核心类库 | `crates/remoteops-serial` |
| 共享 AI 核心 | `crates/remoteops-ai` |
| 详细使用说明 | `docs/本地串口AI交互Demo.md` |
| Windows 构建脚本 | `scripts/Build-Windows.ps1` |

发布文件信息：

- Cargo 版本：`0.5.0`
- 文件版本：`0.5.0`
- 产品版本：`0.5.0`
- 文件长度：`6185472` 字节
- SHA-256：`c447fab3db4ccdce0052c66fcabb042cb236da11acbeb588e70c26f71cb52a31`

## 三、已完成验证

以下验证已通过：

```powershell
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
.\scripts\Build-Windows.ps1
```

结果：

- Workspace 编译通过；
- Clippy 零告警；
- `0.5.0` 的 Workspace 检查、Clippy 和 `125` 项测试全部通过；
- Release 构建和发布清单生成成功；
- Demo EXE 的 Cargo、文件和产品版本一致；
- 当前 Codex Responses 模型配置可以被 Demo 安全加载，Token 未输出；
- 当前工作电脑曾枚举到 `COM1`、`COM3`、`COM4`，但这不是目标远程环境的确认结果，下次必须重新枚举。

## 四、首次真实环境结果

- `COM7`：FTDI USB Serial Port，VID `0403`、PID `6015`；
- `/open COM7 9600`：首次因端口占用被拒绝，释放后成功；
- 人工发送 `dis`、`dis int b`：程序记录到 TX；
- `/buffer`：只有 TX，没有任何 RX；
- 结论：`0.1.0` 未达到交换机终端交互要求，不能继续进行 AI 验收。

## 五、第二次真实环境结果

- 使用 `0.2.0` 直接打开 `COM7` 成功；
- 设备返回华为交换机登录提示、`<HUAWEI>` 提示符和 `dis int b` 输出；
- 人工命令和 RX 文本链路通过；
- 输出包含 `---- More ----` 分页提示以及 ANSI 光标控制序列；
- 发现 `0.2.0` 只能整行发送，无法在分页提示处即时发送单独空格或 Tab；
- 结论：串口参数和 RX/TX 基础链路通过，逐键终端交互未通过，已进入 `0.3.0` 修复。

## 六、尚未验证

- `0.5.0` 的 `/ai-access readonly 10 20` 和一次 `run_serial_query` 主动查询；
- `0.5.0` 自动分页后不再出现工具轮数耗尽；
- 未授权、过期、次数耗尽、未知和修改型命令在真实环境中均被拒绝；
- 正式 GUI 勾选任务级授权后，经 Relay、Agent、`COM7` 执行主动查询；
- AI 写入被拒绝时设备确实没有收到数据；
- 长时间运行、设备拔插、串口断开和重新打开行为。

## 七、第三次真实环境结果

- `0.3.0` 已进入原始终端并执行华为交换机查询；
- 交换机能够进入分页并返回提示符，说明逐键终端路径工作；
- 因 CSI `D` 光标回退被直接过滤，分页提示后的 `<HUAWEI>` 没有回到行首；
- Backspace 使用 `0x08` 时设备没有删除字符，并返回 BEL；
- 结论：问题已缩小到终端显示和按键编码，`0.4.0` 增加安全 CSI 行首处理、默认终端、`DEL/0x7F` Backspace 和 BEL 静默过滤。

## 八、第四次真实环境结果

- `0.4.0` 默认终端和 `Ctrl+]` 管理模式切换通过；
- AI 已主动申请发送只读命令 `display version`，现场输入 `yes` 后设备收到命令并返回完整版本信息；
- AI 等待响应后正确识别 CE5850-48T4S2Q-EI、VRP V8.80、软件版本和补丁版本，主动查询闭环通过；
- Backspace、Delete 和左右方向键仍无法正常完成行编辑；
- 代码复核发现输入仍混用了 `DEL/0x7F` 和 ANSI 方向键，同时接收过滤器将 CSI 左移错误降级为回到行首；
- `0.4.1` 已改为华为 VRP 控制字符并实现有限的跨平台光标移动、定位和行内擦除；随后现场确认这些行编辑与分页显示问题通过复测。

## 九、第五次真实环境与 `0.5.0` 重构结果

- 现场确认此前 Backspace、Delete、左右方向键和分页显示问题已经通过复测；
- 旧版 AI 在尝试连续执行命令时出现“工具调用超过最大轮数”，并发生命令文本与管理提示符交错；
- `0.5.0` 新增确定性的 `run_serial_query`，一次核心调用负责发送完整命令、等待分片响应、发送分页空格、识别设备提示符和生成脱敏结果；
- 新增 `/ai-access strict` 与 `/ai-access readonly [分钟] [命令数]`，默认仍逐次批准，显式授权只覆盖当前串口上的完整华为 `display ...` 查询；
- `0.5.0` Windows Release 已完成，但当天远程设备已经关机，因此新查询运行器尚未在真实 `COM7` 验收。

## 十、下次开始前必须确认

不要猜测端口或通讯参数。环境恢复后，先向环境提供者确认：

1. 运行 Demo 的 Windows 机器已经开机且可操作。
2. 串口设备已经上电并连接。
3. 目标 COM 名称。
4. 波特率。
5. 数据位、停止位、校验位和流控，常见值为 `8N1/无流控`，但必须以设备为准。
6. 设备要求的行结束符。
7. 一条确定安全、只查询不修改设备状态的测试命令。
8. 该命令正常情况下的预期响应或关键标识。

不要在文档或聊天中记录串口设备密码、Token、私钥或业务敏感数据。

## 十一、下次直接执行步骤

### 1. 核对发布文件

```powershell
cd <仓库根目录>

Get-Item .\artifacts\windows-x64\remoteops-serial-demo.exe |
  Select-Object FullName, Length, @{N='FileVersion';E={$_.VersionInfo.FileVersion}}

Get-FileHash `
  .\artifacts\windows-x64\remoteops-serial-demo.exe `
  -Algorithm SHA256
```

如果运行 Demo 的不是当前电脑，将 EXE 复制到连接真实 COM 的 Windows 机器。只复制 EXE，不复制 Codex 配置、Token 或其他凭据。

### 2. 先做人工模式枚举

```powershell
.\artifacts\windows-x64\remoteops-serial-demo.exe --no-ai
```

进入程序后执行：

```text
/ports
```

将结果与环境提供者确认的 COM 对照。发现多个端口时不得试写排查。

### 3. 使用确认参数启动

以下仅为命令模板，参数必须替换成现场确认值：

```powershell
.\artifacts\windows-x64\remoteops-serial-demo.exe `
  --port COM_TARGET `
  --baud-rate 9600 `
  --data-bits eight `
  --stop-bits one `
  --parity none `
  --flow-control none `
  --line-ending cr `
  --display text `
  --no-ai
```

打开后会自动进入实时终端。直接按一次回车并执行安全查询；出现分页时按空格，确认 `<HUAWEI>` 回到行首。随后按 `Ctrl+]` 返回管理模式并执行：

```text
/status
```

应看到交换机提示符，且 `/status` 中读取任务为“运行中”、RX 字节数大于零。若仍为零，在确认现场串口工具的 DTR/RTS 配置后，可显式尝试 `/dtr on` 或 `/rts on`，不要盲目切换控制线。

确认接收正常后，使用环境提供者给出的安全查询命令进行人工验证：

```text
/send <安全查询命令>
```

如果使用了 `--command-mode`，需要验证分页、Tab 或方向键时执行：

```text
/terminal
```

进入后直接按空格翻页；按 `Ctrl+]` 返回命令模式。不要在终端模式下输入 `/ai` 等本地命令。

需要发送原始字节时使用：

```text
/hex <已确认的十六进制字节>
```

### 4. 开启 AI 并先验证只读

退出人工模式后，去掉 `--no-ai` 重新启动。默认读取当前 Codex Provider；使用其他 OpenAI 兼容服务时，只在运行机器本地设置：

```powershell
$env:REMOTEOPS_AI_BASE_URL = 'https://example.com/v1'
$env:REMOTEOPS_AI_MODEL = 'example-model'
$env:REMOTEOPS_AI_PROTOCOL = 'responses'
$env:REMOTEOPS_AI_TOKEN = '<仅在本机安全录入>'
```

先让 AI 只查看缓冲，不要求写入：

```text
/ai 只读取当前串口缓冲，说明已经观察到的事实，不要向设备发送任何数据。
```

验收点：AI 必须调用读取工具，并区分事实、推断和建议。

### 5. 验证 AI 写入拒绝

```text
/ai 使用已经确认的安全查询命令检查设备状态，并分析响应。
```

出现写入审批后，第一次输入除 `yes` 以外的内容进行拒绝。

验收点：

- 控制台展示目标、原因、文本/HEX 和字节数；
- 拒绝后没有出现对应 `TX`；
- AI 明确知道写入被拒绝，不得声称已经发送。

### 6. 验证 AI 写入批准和连续分析

再次发起同一安全任务，核对内容无误后输入完整的：

```text
yes
```

验收点：

- 出现对应 `TX`；
- 设备返回 `RX`；
- 一次 `run_serial_query` 调用完成等待、分页和提示符识别；
- 最终回答引用真实设备输出；
- AI 没有执行未批准的第二次写入。

### 7. 验证有界只读授权

返回管理模式后执行：

```text
/ai-access readonly 10 20
/ai 主动执行 display version，等待真实响应并总结版本信息。
```

验收点：

- 完整 `display ...` 查询不再逐条要求输入 `yes`；
- 多页输出由查询运行器自动发送分页空格；
- 不再出现“工具调用超过最大轮数”；
- `/ai-access status` 会减少剩余次数；
- `system-view`、未知命令和包含换行的命令不会被该授权放行；
- `/ai-access strict` 能立即恢复逐次批准。

## 十二、建议验收记录

下次完成后，在本文末尾追加一条记录，至少包含：

- 验收时间，格式为 `YYYY-MM-DD HH:mm:ss`，时区 `Asia/Shanghai`；
- 环境和设备的脱敏名称；
- COM 和通讯参数；
- 使用的安全查询命令类型，不记录密码等敏感内容；
- 人工读取、人工写入、AI 读取、AI 拒绝、AI 批准和连续分析的结果；
- 是否出现乱码、超时、断开、重复发送或提示注入问题；
- 结论：通过、部分通过或失败；
- 下一步是否接入正式 RemoteOps GUI/Controller。

不要粘贴包含密码、Token、完整业务数据或设备敏感配置的原始串口日志。

## 十三、已知边界和处置

- COM 被其他程序占用：关闭其他串口工具后使用 `/close`，再重新打开；
- 读取任务诊断：使用 `/status`，若读取任务已停止或 RX 始终为零，停止 AI 测试并记录现场串口参数；
- 终端模式退出：使用 `Ctrl+]`，不要使用 `Ctrl+C` 退出 Demo；`Ctrl+C` 会发送给交换机；
- 参数错误导致乱码或无响应：停止写入，向环境提供者重新核对参数；
- 设备断开：程序会显示读取失败，使用 `/close` 后等待设备恢复再 `/open`；
- AI 调用失败：先保留人工串口测试，不为排查 AI 而修改设备；
- AI 输出或设备输出中的控制字符会被转义，不能直接控制终端；
- 结构化查询总超时最多 `120000` 毫秒、接收最多 `1 MiB`、自动翻页最多 `200` 次；
- 只读授权最长 `60` 分钟、最多 `100` 条，并绑定创建授权时的当前串口；
- 串口缓冲仅驻留内存，退出后不会保留；需要验收证据时只记录脱敏摘要。

## 十四、完成标准

只有以下条件全部满足，才把 T-011 标记为完成：

1. 明确目标设备和通讯参数。
2. 人工持续读取通过。
3. 人工文本或 HEX 安全查询通过。
4. AI 只读缓冲分析通过。
5. AI 写入拒绝不会产生 TX。
6. AI 写入批准后设备收到数据并返回响应。
7. `run_serial_query` 能在一次工具调用内处理响应、分页和提示符，不再耗尽工具轮数。
8. 有界只读授权只放行当前串口的完整华为 `display ...` 查询。
9. 正式 GUI 经 Relay/Agent 的同一结构化查询路径通过真实 COM 验收。
10. 全程没有凭据落盘、未授权写入或敏感日志泄露。

## 十五、下次会话建议开场

可以直接向 Codex 说明：

```text
继续 RemoteOps 本地串口 AI Demo 真实验收。先阅读
docs/本地串口AI交互Demo交接文档.md，当前环境已经恢复，
目标串口和参数是：<填写 COM、波特率、数据位、停止位、校验、流控、换行>。
安全查询命令是：<填写已确认命令>。按交接步骤执行并记录结果。
```

