# RemoteOps R03／R04 验证记录

日期：2026-11-05。本文区分实际 SSH 环回、自动化单测和现场验收；未使用真实现场设备或凭据。

## R03：SSH 审查与本机环回

已复核 `remoteops-device` 的 SSH 后端选择、参数、认证、信任、流式输出、总超时及受管理进程树路径。没有发现需要据此修改 SSH 行为的明确缺陷，保留现有实现：

- Windows 密码模式仅在启动前选择可精确映射固定主机密钥的随包 Plink；运行参数含 `-T`、`-batch` 与显式 `-hostkey`。
- OpenSSH 使用 `StrictHostKeyChecking=yes`、独立 `known_hosts`，不叠加系统信任；不自动信任未知密钥。
- 密码经受保护的随机安全文件传递，不进入命令行。RAII 覆盖正常返回、启动失败、超时及取消。
- 外层超时覆盖准备与执行，实际进程由既有生命周期管理器回收；认证或 exec 失败后不换后端重放命令。
- stderr 的文本匹配仅提供诊断线索，不伪造认证或远端执行阶段。未放宽 SSH 算法。

使用固定官方 Plink 0.83（SHA-256 `50479953865b30775056441b10fdcb984126ba4f98af4f64756902a807b453e7`）与隔离 Paramiko 4.0.0 服务，实际运行 `scripts/test-ssh-plink-e2e.py test`，**17／17 通过**。

| 验证项 | 实际结果 |
| --- | --- |
| 复制后的随包 Plink 摘要 | 通过 |
| 正确密码认证与 Unicode stdout 逐字匹配 | 通过 |
| stdout／stderr 分离 | 通过 |
| 首帧在最终完成前到达 | 通过 |
| 程序／凭据中文及空格路径 | 通过 |
| 错密拒绝 | 通过 |
| exec 请求拒绝 | 通过 |
| 换钥后旧 pin 拒绝 | 通过 |
| 显式新 pin 允许 | 通过 |
| 首帧后超时终止 | 通过 |
| 首帧后显式取消 | 通过 |
| 启动失败时凭据清理 | 通过 |
| CREATE_NEW 不覆盖既有凭据文件 | 通过 |
| 服务端确认两个取消／超时传输均已关闭 | 通过 |
| 所有路径凭据文件删除 | 通过 |
| 创建时即有受保护的当前用户专用 DACL | 通过 |
| 服务端实际观测到认证与 exec 拒绝 | 通过 |

另运行 `cargo test -p remoteops-device ssh --locked`，**24／24 通过**，覆盖主机密钥信任与并发、精确 Plink 参数、受保护密码文件、OpenSSH known_hosts 路径、流式首帧、启动失败清理及超时取消等本机回归。这组 Rust 测试与上面的实际 Plink 环回分别统计。

报告不包含密码或认证输出；测试服务仅绑定 `127.0.0.1` 的临时端口，不执行系统 Shell。以上是直接 Plink 环回证据，**不能替代 Agent／Relay／MCP 整链路或真实交换机、服务器验收**。需要 PTY、交互 Shell 或旧算法的设备仍待取得现场证据。

重跑命令（`$fixturePython` 是独立测试 venv 的 Python，`$plinkExecutable` 是已核对摘要的 Plink，`$r03Report` 必须位于 C 盘本地 Temp 下）：

```powershell
& $fixturePython scripts/test-ssh-plink-e2e.py test `
    --plink $plinkExecutable --report $r03Report
cargo test -p remoteops-device ssh --locked
```

## R04：目标语言与密码窗口

已实现：

- `TargetContext.ui_language` 可缺省，明确报告 `zh-CN`／`en-US`；旧端缺失字段继续可解析。
- GUI 和共享原生宿主通过 `AgentUiLanguage` 同步当前选择，连接重试沿用共享状态；图形探测缓存不能覆盖新语言。
- MCP 每次真正弹窗前刷新目标上下文，明确传递 helper 的 `--language`。旧 Agent 未报告时使用控制端现有国际化规则回退；刷新失败不使用陈旧语言。
- helper 补齐中英文标题、密码标签、说明、按钮和错误。Windows 空输入在原窗口内提示并保持窗口；macOS 同样重新等待有效输入。
- 凭据采集仍发生在 Controller／MCP 机器；加密传输、内存十分钟缓存及取消语义保持原有规则。
- 两个既有整链路 fixture 安全提示器同步校验新增 `--language` 参数，未给正式产品加入测试密码入口。

| 验证 | 当前结果 |
| --- | --- |
| `cargo test -p remoteops-credential-prompt --locked` | 2／2 通过，候选 helper `0.2.0-preview.6` |
| `cargo test -p remoteops-domain ui_language --locked` | 1／1 通过 |
| `cargo build -p remoteops-credential-prompt --locked` | 通过，实际 Windows helper 已生成 |
| 实际 helper 的中／英文无效摘要校验 | 2／2 通过；进程退出 2、stdout 为空、stderr 含对应本地化错误；验证阶段未创建窗口 |
| `cargo test -p remoteops-agent language_switch --locked` | 1／1 通过，覆盖运行中切换、图形缓存与目标隔离 |
| `cargo test -p remoteops-controller-mcp credential_prompt --locked` | 2／2 通过，目标语言／旧端回退、取消与记忆选择 |
| `cargo test -p remoteops-agent-desktop-host language_switch --locked` | 1／1 通过；共享原生宿主切换后立即同步运行状态 |
| `node --check scripts/test-transfer-ssh-e2e.mjs` | 通过 |
| `node --check scripts/test-styled-native-e2e.mjs` | 通过 |
| `Test-CredentialPromptWindow.ps1` PowerShell 解析与嵌入 C# 编译 | 通过 |
| 真实 Windows helper 窗口自动化与中英截图 | **未完成：桌面锁定，现有桌面门禁报告 `desktop_locked_or_unavailable`／`lock_process_seen`** |
| macOS 实际窗口 | 未验证，当前环境为 Windows |

窗口回归脚本只操作本次 helper 的 PowerShell 子进程窗口，逐次校验窗口归属，不读取密码框、不使用全局鼠标或键盘，仅注入固定合成密码。覆盖两种语言下的取消、空输入、仅本次使用和记住十分钟选择；截图在密码输入前生成。锁屏门禁拒绝后未启动密码窗口、未绕过门禁，也没有生成截图。

解锁本机交互桌面后恢复：将 `$promptExecutable` 指向本轮构建的正式 helper，`$r04Output` 指向全新输出目录。

```powershell
cargo test -p remoteops-agent language_switch --locked
cargo test -p remoteops-controller-mcp credential_prompt --locked
cargo test -p remoteops-agent-desktop-host language_switch --locked
$promptHash = (Get-FileHash -LiteralPath $promptExecutable).Hash
pwsh -NoProfile -STA -File scripts/Test-CredentialPromptWindow.ps1 `
    -Executable $promptExecutable -ExpectedHash $promptHash `
    -OutputDirectory $r04Output
```

实际窗口通过后，仍需人工复核中英文截图、在目标运行中切换语言后再次发起密码输入、以及真实交换机和服务器场景。模拟提示器或单测不能写成这些现场项目已通过。
