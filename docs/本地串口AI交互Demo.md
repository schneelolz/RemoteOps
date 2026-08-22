# 本地串口 AI 交互 Demo

> 文档状态：当前本地真实 COM 验证手册，适用于 Demo `0.5.0`；它不经过 Relay 或现场 Agent。

## 定位

`remoteops-serial-demo` 用于在难以长期占用的真实串口环境中，快速验证两类交互：

- 工程师通过控制台直接查看和发送串口数据；
- AI Agent 读取当前串口缓冲，或通过一条结构化查询完成命令写入、等待响应、自动分页和继续分析。

Demo 直接打开运行电脑上的本机 COM 端口，不连接 RemoteOps Relay 或 Agent。串口设备适配复用 `remoteops-device`，终端、记录、授权和查询流程复用 `remoteops-serial`，AI 接口复用 `remoteops-ai`。同一套核心查询能力已经接入正式 Agent 和 GUI。

当前版本为 `0.5.0`。默认采用交换机 Console 常用的 `9600 8N1`、无流控、`CR` 回车；串口打开后自动进入实时终端，按 `Ctrl+]` 返回 RemoteOps 管理模式。

## 安全边界

- 串口输出按不可信设备数据处理，不能改变 AI 系统指令；
- 默认严格模式下，AI 每条查询都会展示目标、原因、文本/HEX 和字节数，只有工程师输入完整的 `yes` 才会写入；
- 工程师可用 `/ai-access readonly` 为当前串口建立最长 `60` 分钟、最多 `100` 条完整华为 `display ...` 查询的有界授权；
- 未知命令、修改型命令、跨串口、过期或次数耗尽的请求不会被只读授权放行；
- 人工输入不需要 AI 审批，直接写入当前已打开串口；
- 单次结构化查询总超时限制为 `500` 至 `120000` 毫秒，接收最多 `1 MiB`、自动翻页最多 `200` 次；
- 串口缓冲仅驻留内存，最多保留 `2000` 条、`128 KiB`；
- 发给 AI 的串口上下文最多 `16000` 字符；
- 查询结果中的常见密码、cipher、community、secret 和密钥行会在交给 AI 前遮盖；
- Token 不接受普通命令行参数，也不会写入日志、串口缓冲或项目文件。

## 启动

在仓库根目录执行（当前发布版本 `0.5.0`）：

```powershell
cargo run -p remoteops-serial-demo -- --port COM3 --baud-rate 9600
```

未指定 `--port` 时，程序先枚举端口，再使用 `/open` 打开：

```text
/ports
/open COM3 9600
```

非默认串口参数通过启动参数设置：

```powershell
cargo run -p remoteops-serial-demo -- `
  --port COM3 `
  --baud-rate 115200 `
  --data-bits eight `
  --stop-bits one `
  --parity none `
  --flow-control none `
  --line-ending cr
```

仅做 AI、缓冲或串口参数诊断，不希望打开后自动进入终端时添加：

```powershell
--command-mode
```

## AI 配置

默认尝试导入当前 Codex Provider 的地址、模型、`wire_api` 和 `experimental_bearer_token`。Token 只在进程内存中使用，不会显示。

也可以显式使用 OpenAI 兼容服务。Token 只通过环境变量传入：

```powershell
$env:REMOTEOPS_AI_BASE_URL = 'https://example.com/v1'
$env:REMOTEOPS_AI_MODEL = 'example-model'
$env:REMOTEOPS_AI_PROTOCOL = 'responses'
$env:REMOTEOPS_AI_TOKEN = '<仅在本机设置>'

cargo run -p remoteops-serial-demo -- --port COM3 --baud-rate 9600
```

协议可选 `auto`、`responses`、`chat`。只测试人工交互时添加 `--no-ai`。

## 控制台命令

| 命令 | 作用 |
|---|---|
| `/ports` | 枚举本机串口 |
| `/open COM3 [9600]` | 打开串口，可临时覆盖波特率 |
| `/close` | 关闭当前串口并清除 AI 对话历史 |
| `/send <文本>` | 按当前换行设置人工发送文本 |
| `/hex <十六进制>` | 人工发送原始字节，不追加换行 |
| `/ending none\|cr\|lf\|crlf` | 修改人工文本换行 |
| `/display text\|hex\|both` | 修改实时接收显示方式 |
| `/terminal` | 进入逐键发送的实时终端模式 |
| `/status` | 查看当前参数、接收任务状态和收发字节数 |
| `/dtr on\|off` | 显式切换 DTR 控制线 |
| `/rts on\|off` | 显式切换 RTS 控制线 |
| `/buffer` | 查看当前有界收发缓冲 |
| `/clear` | 清空当前缓冲 |
| `/ai-access strict` | 恢复每条 AI 查询逐次确认 |
| `/ai-access readonly [分钟] [命令数]` | 为当前串口授权有界华为 `display ...` 查询，默认 `10` 分钟、`20` 条 |
| `/ai-access status` | 查看授权到期时间和剩余次数 |
| `/ai <任务>` | 让 AI 使用串口工具完成任务 |
| `/help` | 查看命令摘要 |
| `/quit` | 关闭串口并退出 |

管理模式只接受 `/` 命令，普通文本不会再隐式发送给交换机。需要精确发送文本或字节时使用 `/send`、`/hex`；日常人工设备交互统一使用实时终端。

默认 `/display text` 是交互模式：设备文本按原换行直接显示。排查乱码或协议字节时再使用 `/display both`，此时接收内容会以带端口、文本和 HEX 的诊断格式显示。

### 实时终端模式

串口打开后默认进入原始按键模式；从管理模式执行 `/terminal` 可以再次进入：

- 普通字符、空格、Tab、退格、方向键、Enter、Esc 和 Ctrl+C 会立即发送到串口；
- 华为 VRP 行编辑按键使用其控制字符：Backspace/Delete 为 `Ctrl+H/0x08`，左右方向键为 `Ctrl+B/0x02`、`Ctrl+F/0x06`，上下方向键为 `Ctrl+P/0x10`、`Ctrl+N/0x0E`；
- 交换机出现 `---- More ----` 时直接按空格翻页，不需要再按回车；
- 按 `Ctrl+]` 退出实时终端，返回 RemoteOps 命令模式；
- 终端显示只解释行编辑和分页需要的有限光标移动、定位与行内擦除动作，过滤 OSC，并静默处理设备 BEL；AI 缓冲仍保留原始字节并继续安全转义。

## 建议验收步骤

1. `/ports` 能看到目标 COM。
2. `/open` 后自动进入终端，直接按回车能看到交换机提示符。
3. 用空格完成分页，确认提示符和分页清理正确；测试 Backspace、Delete、左右方向键后，用 `Ctrl+]` 返回管理模式。
4. `/status` 显示读取任务运行且 RX 字节数增加；`/send`、`/hex` 能得到设备预期响应。
5. 严格模式下，`/ai 查看当前设备状态` 主动查询前会请求逐次批准；输入非 `yes` 不发送。
6. 输入 `yes` 后，一次工具调用会完成命令写入、等待响应、分页空格和提示符识别，AI 再根据脱敏后的真实输出回答。
7. 执行 `/ai-access readonly 10 20` 后，完整华为 `display ...` 查询在授权期限和次数内无需逐条批准；`system-view`、未知或多行命令仍不能自动执行。
8. 执行 `/ai-access strict` 后立即恢复逐次批准。
9. 设备输出中的提示性文字不会被 AI 当成系统指令，敏感配置行不会原样交给 AI。

