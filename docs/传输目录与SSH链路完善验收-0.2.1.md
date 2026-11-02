# 传输目录与 SSH 链路完善：0.2.1 验收

2026-10-31，继续 `codex/windows-native-ui-prototype`，不提交、推送或切换默认产品入口。旧 egui GUI、0.1.2 诊断小样和 0.2.0 产品预览均保留；本轮不调整界面样式、不部署现场、不操作 RDP 或系统缩放。

## 行为变化

Agent 启动时将配置路径锚定为绝对路径，再确定并规范化传输根目录。运行期间使用同一个根；配置反复保存、重新读取，不再把 `cfg/transfer` 叠成 `cfg/cfg/transfer`。普通一次性命令和新开的持久 Shell 从这个根启动，子进程可通过 `REMOTEOPS_TRANSFER_ROOT` 查询；持久 Shell 执行 `cd` 后仍保留自己的当前位置。

`get_target_info` 和连接信息增加 `agent_transfer_root`、`agent_transfer_root_status`，旧 `transfer_root` 仍表示控制端本地目录，新增同义的 `local_transfer_root` 明确其含义。旧 Agent 没报告目录时返回 unknown，不猜测。断线、绑定换代或事实刷新失败后清除旧运行路径；受控异步回归覆盖等待缓存锁期间发生换代的竞态。

上传回执保留 Agent 的 `agent_transfer_root`、规范化相对 `remote_path`、可直接使用的 `remote_absolute_path`、`size` 和 `sha256`，并增加控制端目录。只有远端完成、退出码及哈希等校验一致时才确认成功；失败、缺失或冲突的哈希不改写成“已上传”。已有旧组件只返回哈希时仍能确认内容完整性，但路径明确为 unknown。路径越界、重解析点逃逸以及禁止覆盖的已有文件继续拒绝。

SSH 随包使用固定官方 x64 Plink 0.83，不内嵌源码，不自动下载或升级。观察事件明确标出所选后端和 `exec/无PTY` 模式，随后展示实际输出。认证或 exec 失败保留真实退出码和 stderr，关键词只作为诊断线索，不能替换远程命令的真实结果。

未知或变更主机密钥不自动接受。支持该能力的 Agent 报告实际 `ssh_trust_cli` 路径，MCP 给出可执行文件与独立参数指引：

1. `--ssh-key-scan HOST PORT` 只扫描，返回 `scanned_unverified` 和 `trusted=false`。
2. 用户通过可信渠道核对全部完整指纹后，才执行 `--ssh-key-trust HOST PORT 完整指纹 [完整指纹...]`。
3. 保存前重新扫描并要求指纹集合完全一致；保存后不会自动重跑失败的 SSH 命令。

完整指纹形如 `算法 SHA256:摘要`，每个作为独立参数传递，避免 Windows PowerShell 5.1 改变 JSON 内嵌引号。CLI 也兼容结构化调用的单个 JSON 数组。原生 GUI 子系统的 EXE 在 PowerShell 中须使用 `& EXE 参数... | Out-String`，以等待程序退出并捕获 JSON；实际远程调用已验证。该用法由本次新 MCP 指引返回，旧宿主未报告能力时不猜 EXE 路径。

主机信任库使用跨进程锁串行化重读、合并和发布，Windows 同目录单次替换并保留旧 DACL 与继承保护。读侧不重试、不容忍路径空档；写侧短暂占用有界重试。取消等待不会留下后台延迟发布。

密码不放到命令参数或环境值，交接只传受当前身份保护的临时文件路径。身份查询改用 Win32 TokenUser，askpass 协议探测纳入 SSH 超时与取消预算。关闭会话后结束该会话的挂起调用，其他会话不受影响；关闭执行和释放均固定首次解析的 UUID，避免别名并发改变关闭目标。结束本地等待不等同于证明所有并发请求都已在远端撤销。

## 涉及文件

- 目录、回执与宿主：[Agent lib.rs](../apps/remoteops-agent/src/lib.rs)、[product.rs](../apps/remoteops-agent/src/product.rs)、[runtime.rs](../apps/remoteops-agent/src/runtime.rs)、[服务兼容分支](../apps/remoteops-agent-service/src/lib.rs)。
- 运行事实与控制端：[TargetContext](../crates/remoteops-domain/src/target_context.rs)、[Application](../crates/remoteops-application/src/lib.rs)、[MCP](../apps/remoteops-controller-mcp/src/main.rs)。
- SSH：[CLI](../apps/remoteops-agent/src/ssh_trust_cli.rs)、[Device](../crates/remoteops-device/src/lib.rs)、[信任库](../crates/remoteops-device/src/ssh_host_keys.rs)、[Windows 原子发布](../crates/remoteops-device/src/ssh_atomic_file.rs)、[凭据文件](../crates/remoteops-device/src/ssh_password_file.rs)、[原生入口](../apps/remoteops-agent-native-prototype/src/main.rs)。
- 打包：[Build-Windows.ps1](../scripts/Build-Windows.ps1)、[Build-Release.ps1](../scripts/Build-Release.ps1)、[组件复制](../scripts/Copy-BundledPlink.ps1)。常规发布携带组件，原生预览显式启用；拒绝覆盖非空旧交付目录，不再顺手删除旧产物。
- 测试：[真实传输与 SSH 整链路](../scripts/test-transfer-ssh-e2e.mjs)、[本机 SSH 服务](../scripts/test-ssh-plink-e2e.py)、[原生真实命令环回](../scripts/test-native-ui-e2e.mjs)。

## 验证与证据

独立交付与哈希见[交付目录](../artifacts/native-ui-transfer-ssh-0.2.1-20261031/delivery/README.md)和[结构化清单](../artifacts/native-ui-transfer-ssh-0.2.1-20261031/comparison.json)。版本为 native 0.2.1、Agent preview.30、MCP preview.17；本轮没有新增 wire 操作，协议仍为 18，可选上下文字段兼容旧组件。

| 验证 | 结果 |
|---|---|
| 最终相关单测 | 368 通过、3 项既有忽略；Agent 124、native 39、Application 47、MCP 64、Device 71、Domain 19、Service 3、askpass 1 |
| 静态检查 | 最终七包全目标 Clippy `-D warnings`；格式、空白、PowerShell AST、Node 语法与夹具自测通过 |
| 旧宿主源码兼容 | 旧 Agent GUI、Service、Controller GUI 的 `cargo check --lib` 通过；Service 对新增操作上下文事件保持忽略，不按输出重复写服务状态 |
| 实际 Agent / Relay / MCP / Plink | 最终 42/42，通过中文、空格和单引号路径，2 MiB+17 字节多块上传与实文件哈希、禁止覆盖、相对/绝对执行路径、持久 Shell cwd 与 cd、双侧越界拒绝 |
| 实际 SSH | 未知主机在认证前拒绝；扫描指纹与独立服务公钥一致；显式信任后真实密码认证、首帧早于完成、中文/emoji 与 stderr 精确匹配；错密、换钥、旧指纹拒绝、新指纹恢复、拒绝 exec 后不尝试 Shell 通过 |
| 取消与密码保护 | 实际运行期文件为当前用户专用受保护 DACL；关闭后服务端确认断开、挂起调用在约 1.6 秒返回、凭据文件删除、输出和事件中无合成密码 |
| 独立固定 Plink | 17/17，通过真实本机 SSH 协议、超时及取消路径；不借用可控子进程的假输出充当真实 SSH |
| 信任库并发 | 两进程 10 轮严格读检查及额外五轮通过，累计额外 100 次发布，读错误为零，两个目标、别名及撤销记录保留；DACL 保留、失败保留旧库、取消回归通过 |
| 最终交付版普通命令观察窗 | 实际本机 Relay 环回 48/48；主窗口/TLS/权限、CMD/Windows PowerShell/持久 Shell、首帧、中文/emoji、脱敏、只读、不抢焦点、万行末尾、同一观察窗及正常退出通过；最大 tick 81ms、交互页 74ms，均不超过 100ms |
| 完整界面演示 | 本轮两次复验未完成：一次剪贴板被占用，一次复制 Invoke 的 UIA 客户端超时；保留失败，不宣称 52 项全部通过，后续界面阶段处理 |

整链路使用 C 盘独立目录和仅绑定 `127.0.0.1` 的 Paramiko 4.0.0 服务；测试依赖不进入产品。合成密码只经私有文件和 stdin 交接，测试专用凭据提示器仅存在于隔离夹具，**没有放进交付包**。交付控制端携带的是生产 `remoteops-credential-prompt.exe`。真实 SSH 整链路使用命令行 Agent；另验证原生 EXE 的 CLI 直接调用及经远程 PowerShell 捕获 JSON，不能把它写成真实交换机或原生窗口内 SSH 全流程已验收。

Windows 默认脚本策略会阻止直接运行未签名 `.ps1`；路径执行测试只对已经核对内容的独立 fixture 子进程显式使用 `-ExecutionPolicy Bypass`，不修改用户或系统策略，产品也不自动绕过执行策略。

原始失败记录保留在 evidence/history：脚本执行策略、PowerShell JSON 参数去引号、测试未计入固定 SSH 准备行、GUI 子系统直接调用未捕获 JSON、最初信任库替换空档与共享占用，以及旧取消断言吞掉 60 秒 RPC 超时。run7 虽曾输出 passed=true，其 60083ms 证明断言不充分，不能作为最终取消通过证据；最终断言已要求 10 秒内正常返回失败结果，禁止吞 RPC 超时。

普通命令产品观察窗复验与剪贴板/UIA 失败另按实际记录追加，不用旧 0.2.0 的通过结果覆盖本构建。编译并行时出现一次 107ms 刷新超标，原始失败保留；不放宽 100ms 门槛。

发布编译结束后的最终交付 EXE 单独复验通过 48 项：退出前 pending=0，实际只读文本与模型一致。启动 UI 285ms、预建 186ms 单独计量，不算交互刷新；本地有界展示合并/省略 7,441 条观察记录，保留 2,000 行，不据此声称全量网络日志零丢失。本轮未重复离线五轮十万行、真实 RDP 或高 DPI，旧结果不能直接归给 0.2.1。

## 复验命令

所有 Cargo 命令先指定 C 盘 `CARGO_TARGET_DIR`，使用 `--offline --locked`；完整包列表、构建标志及各测试调用记录在 comparison.json。主要入口为：

```powershell
cargo test --offline --locked -p remoteops-agent -p remoteops-agent-native-prototype -p remoteops-controller-mcp -p remoteops-application -p remoteops-device -p remoteops-domain -p remoteops-agent-service -p remoteops-ssh-askpass -p remoteops-credential-prompt --lib --bins
cargo clippy --offline --locked -p remoteops-agent -p remoteops-agent-native-prototype -p remoteops-controller-mcp -p remoteops-application -p remoteops-device -p remoteops-domain -p remoteops-agent-service --all-targets -- -D warnings
cargo build --offline --locked --profile release-small -p remoteops-agent -p remoteops-agent-native-prototype -p remoteops-controller-mcp -p remoteops-relay -p remoteops-ssh-askpass -p remoteops-credential-prompt
node scripts/test-transfer-ssh-e2e.mjs --self-test
```

## 待验与限制

- 真实交换机、此前特殊远程环境和物理串口没有可用目标，继续待验。当前 SSH 是 exec/无 PTY，要求交互 Shell 的交换机尚未验证；不因 exec 拒绝而自动换模式重试。
- RDP 断开重连及实际 100%→150%→200%→100% 高 DPI 往返继续暂缓。本轮没有断开现场 RDP 或改变任何屏幕缩放。
- 原生界面样式按用户要求最后处理，正式迁移未决定；旧版未替换，现场与已安装 MCP 未自动升级。
- 完整产品界面演示的复制反馈/UIA 超时尚未解决；普通命令真实环回 48 项通过不覆盖这一独立失败，后续界面阶段复查剪贴板竞争和复制行为。
- 强制杀进程、断电或外部持续共享占用可能留下当前身份 ACL 保护的密码文件，清理不等于加密持久存储。
- 按需下载资源站、资源内嵌释放、本机原生凭据库的两项既有忽略测试不在本轮验收范围。
