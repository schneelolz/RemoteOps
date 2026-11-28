# PuTTY / Plink 随包组件

本轮固定使用用户已验证的 Windows x64 Plink 0.83，不内嵌源码。

- 本机文件：`D:\Tools\putty\PLINK.EXE`。
- SHA-256：`50479953865b30775056441b10fdcb984126ba4f98af4f64756902a807b453e7`，与[官方 0.83 摘要](https://the.earth.li/~sgtatham/putty/0.83/sha256sums)的 w64/plink.exe 一致。
- 官方组件地址：[Plink 0.83 x64](https://the.earth.li/~sgtatham/putty/0.83/w64/plink.exe)。官方较新版本不自动替换此固定组件，升级需重测。
- 许可：[MIT / PuTTY 许可](https://the.earth.li/~sgtatham/putty/0.83/htmldoc/AppendixD.html)。本目录 LICENCE.txt 来自已核对 SHA-256 的官方 putty-src.zip 中 LICENCE，随组件一同分发。

运行位置为新程序同目录 `tools/plink.exe`。用 `scripts/Copy-BundledPlink.ps1 -SourcePath <官方匹配EXE> -DestinationDirectory <新版目录>` 打包，不改旧版安装。OpenSSH fallback 需要加 `-AskpassPath <本次构建的remoteops-ssh-askpass.exe>` 一并携带。

`scripts/Build-Windows.ps1` 常规发布会在全新的 Windows 输出根携带 Agent 所需的 `tools/plink.exe`、`tools/Plink-LICENCE.txt` 与 `remoteops-ssh-askpass.exe`。`-PlinkSourcePath` 默认指向上述本机已验证文件，其他构建机须显式指定官方匹配文件；不自动下载。`-IncludeNativePreview` 显式增加 `native-preview` 独立目录及同一组侧工具，不替换旧 `remoteops-agent-gui.exe`，默认不把预览当成正式版本。输出目录只允许不存在或为空的普通目录，拒绝覆盖已有交付；也拒绝链接/重解析路径。上层 `Build-Release.ps1` 同样在构建前拒绝已有非空目录，不再删除原发布根目录；旧 `-CleanLegacyArtifacts` 开关会明确拒绝执行，避免顺手删除仍需保留的旧版。上层也支持传递 `-PlinkSourcePath` 和 `-IncludeNativePreview`。

组件复制在写入前核验固定 Plink SHA-256、官方许可证 SHA-256 `914288c99f1b97fdfba323ad56cd935b0f195ce399a650ea6b1039abb0e2d879` 和 helper 的 `--protocol-version` 输出 `remoteops-askpass-file-v1`。目的文件已存在时只接受相同摘要，许可证和 helper 也不例外；复制后再次核验。发布目录的 `bundled-tools.json` 记录组件摘要和 helper 协议，第三方清单另列 Plink 0.83 / MIT 及完整许可。

仅密码认证、未指定 identity_file 且现有 known_hosts 存在可精确映射的目标公钥时启用 Plink；以 SHA256 指纹传入 -hostkey 并使用 -batch，未知/变更密钥失败。未匹配、哈希主机名、证书/撤销规则和 identity_file 等保留 OpenSSH 兼容路径，不能静默丢掉传入的信任或私钥参数。新密码路径不再在首次连接时自动 keyscan/TOFU；未知目标或非默认端口缺少对应记录会明确失败，需先核对并固定主机密钥。这是对旧版自动信任行为的有意收紧，旧发布程序不修改。

密码仅写入创建时即有当前身份专用 DACL 的随机临时凭据文件，Plink 使用 -pwfile；不放认证密码到进程命令参数/环境。OpenSSH fallback 改用新版 askpass 的 REMOTEOPS_SSH_PASSWORD_FILE 安全文件交接，环境只携带随机文件路径。新版 helper 不接受旧明文 REMOTEOPS_SSH_PASSWORD；旧 helper 未携带时明确失败，不降级回明文环境。RAII 在正常、启动失败、超时或取消路径尝试清理；Windows 共享锁会在 500 毫秒内重试，外部持续占用超过此界限时可能残留仅当前身份可读的文件。进程在操作系统强制杀死/断电时仍可能残留受 ACL 保护的文件，这不等于加密持久存储；不能宣称磁盘没有留下任何痕迹。

SSH 的实际交换机回归需用户提供可用环境；模拟输出与本机 ACL 测试不代替现场通过。

新密码请求只接受默认 known_hosts 中可精确核对的明文目标记录；仅有哈希主机名时会明确拒绝，需由受控的主机密钥信任流程提供精确目标记录，不自动扫描或改写。Plink 使用随机不存在的 -load 会话；已核对 0.83 官方源码，该路径从编译默认值开始，不加载用户 Default Settings。显式 -hostkey 固定允许的目标指纹。

OpenSSH fallback 的 UserKnownHostsFile 按单个配置值内部双引号引用，Windows 分隔符规范为正斜杠，引号／反斜杠转义，百分号按字面量传递；拒绝控制字符及 `${...}` 环境展开，避免空格或配置 token 把固定信任文件变成另一组路径。回归使用本机无认证端口的 ssh -vvv 实际扩展诊断，带空格及百分号的路径应恰为一个完整文件，未引用负对照会分裂；没有用 -G 的合并打印冒充文件列表验证。规范见 [OpenSSH ssh_config](https://man.openbsd.org/ssh_config#UserKnownHostsFile)。

## 本机真实 SSH 测试夹具

`scripts/test-ssh-plink-e2e.py` 使用 [Paramiko 的 SSH 服务端 API](https://docs.paramiko.org/en/latest/api/server.html) 和 [Transport](https://docs.paramiko.org/en/latest/api/transport.html)，只绑定 `127.0.0.1` 的系统分配端口，不调用系统 Shell、不扫描外部地址、不使用真实设备或密码。私有主机密钥只存在于测试进程内存。Paramiko 仅安装于 C 盘独立测试 venv，不加入 Cargo、产品或发布包；固定使用 [PyPI Paramiko 4.0.0](https://pypi.org/project/paramiko/4.0.0/)。本轮环境为 Windows x64 / Python 3.15.0，解析出的完整固定依赖如下：

```powershell
$fixtureVenv = Join-Path $env:LOCALAPPDATA 'Temp\RemoteOps-ssh-paramiko-4.0.0-venv'
python -m venv $fixtureVenv
& "$fixtureVenv\Scripts\python.exe" -m pip install --only-binary=:all: --index-url https://pypi.org/simple `
    'paramiko==4.0.0' 'bcrypt==5.0.0' 'cryptography==50.0.2' 'invoke==3.0.3' `
    'PyNaCl==1.6.2' 'cffi==2.1.1' 'pycparser==3.12'
& "$fixtureVenv\Scripts\python.exe" scripts/test-ssh-plink-e2e.py test `
    --plink D:\Tools\putty\PLINK.EXE `
    --report "$env:LOCALAPPDATA\Temp\RemoteOps-ssh-plink-validation\run.json"
```

上面只向官方 PyPI 安装固定版本测试依赖，`--only-binary` 缺少适用 wheel 时失败，不隐式编译或改全局 Python。脚本要求 Paramiko 精确匹配 4.0.0；其他 Python 版本的 wheel 可用性另验。

`test` 实际启动固定摘要 Plink，先复制到 C 盘临时“中文 空格”路径，以真实 SSH 协议验证：正确密码认证、首帧早于最终结果、stdout 中文/emoji 逐字一致、stderr 独立、错密拒绝、exec 拒绝、主机密钥轮换后旧 pin 拒绝及新 pin 允许、首帧后的超时看门狗和显式取消、服务端确认连接关闭、各路径凭据删除。密码由随机值生成，凭据文件通过 `CREATE_NEW` 与当前用户专用受保护 DACL 同时创建，不先写文件再收紧权限。报告只保存检查、时间、公开公钥/指纹、端口、摘要和计数，不保存密码或捕获的认证输出。超时/取消由本测试 runner 终止其自己的 Plink；产品实际 `RunSsh` 的超时、取消和文件清理还必须经 Agent/MCP 链路复验，不能据此宣称已通过。

### Agent / MCP 集成接口

父 runner 启动同一 Python 的 `serve` 子命令，用 UTF-8 stdin/stdout NDJSON 交互。首条输入为 `{"password":<本轮父进程随机生成的合成密码>}`；不接受命令行密码，不把密码回传 stdout。首条公共输出示意：

```json
{"event":"ready","host":"127.0.0.1","port":12345,"username":"remoteops-fixture","algorithm":"ecdsa-sha2-nistp256","public_key":"<裸base64公钥>","fingerprint":"SHA256:<摘要>","paramiko_version":"4.0.0","stats":{}}
```

父 runner 可把 `[127.0.0.1]:<port> <algorithm> <public_key>` 写入其独立测试 profile 的 `known_hosts`。合成密码只通过 stdin 或创建时即有当前用户专用 ACL 的 C 盘文件交给测试安全提示器，不驱动真人密码窗口。服务端只允许以下固定 exec 字节串：

| 命令 | 实际 SSH 行为 |
| --- | --- |
| `fixture-stream` | 立即 stdout `FIXTURE-FIRST 中文 😀\n`；等待 1.2 秒；stderr `FIXTURE-STDERR\n`；stdout `FIXTURE-FINAL\n`；退出 0 |
| `fixture-reject` | 拒绝 exec 请求，不产生伪造 Shell 输出 |
| `fixture-timeout` | 立即发送同一首帧，此后等待客户端断开或服务停止 |

其他 exec、PTY、Shell、子系统、转发和公钥认证均拒绝。后续 stdin 支持 `{"action":"rotate_key"}`（下一条连接使用新密钥，返回 `rotated` 及公共新指纹）、`{"action":"stats"}`、`{"action":"stop"}`；stdin EOF 也停止并关闭该服务所有连接。`stats` 返回认证接受/拒绝、exec 接受/拒绝、流完成、等待场景的客户端断开等计数，不保存凭据。轮换不强制断开已认证连接，验证新 pin 时须发起新连接。

此夹具覆盖本机真实 SSH 协议与固定组件，不替代真实交换机、串口和现场网络验收，也不把直接 Plink 检查当作完整 Agent/MCP 通过证据。

### 0.2.1 显式信任入口与整链路

新版 Agent 的 `--ssh-key-scan HOST PORT` 只返回未信任的扫描结果；用户通过可信渠道核对全部指纹后，使用 `--ssh-key-trust HOST PORT 完整指纹 [完整指纹...]`。每个完整指纹是一个独立 argv，避免 Windows PowerShell 5.1 改变 JSON 内嵌引号；结构化调用仍兼容单 JSON 数组。原生 GUI EXE 从 PowerShell 调用时使用 `& EXE 参数... | Out-String` 等待退出并捕获 JSON。MCP 返回真实支持该能力的 EXE 路径及参数指引，不自动信任、也不自动重试 SSH。

实际 Agent/Relay/MCP/Plink 验收脚本为 `scripts/test-transfer-ssh-e2e.mjs`，使用上述隔离服务和仅存在于本轮 C 盘 fixture 的测试凭据提示器，不给生产程序添加测试密码入口。最终 42 项通过，包括实际传输根/多块上传回执、真实 SSH、认证/换钥拒绝、首帧、中文/emoji、取消和凭据清理。测试提示器不进入交付包，产品控制端携带生产 helper。完整命令、构建哈希与限制见[0.2.1 验收](../../docs/传输目录与SSH链路完善验收-0.2.1.md)。
