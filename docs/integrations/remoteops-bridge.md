# RemoteOpsBridge 接入与候选验收

更新日期：2026-11-19。此功能已进入本机候选验收，不代表生产 Relay、已安装 MCP 或真实门店项目已升级。

## 用途与项目位置

`apps/remoteops-bridge` 是独立 Rust 程序，产出 `RemoteOpsBridge.exe`，直接链接 `remoteops-agent` 公共库。C# 通过子进程标准输入输出接入，不加载 Rust DLL，也不启动普通 Agent EXE。普通九位码 Agent 可与同机两个不同实例的 Bridge 并存。

`crates/remoteops-bridge-protocol` 定义本机消息；`examples/bridge-csharp-host` 提供 .NET Framework 4.6.2 和 .NET 10 宿主、初始化模板、模拟用户/门店数据及实际 IP 查询。正式 GRClient、UDServer、CafeCenter、AI管家业务本轮没有接入。

门店业务流程：AI管家通过既有 CafeCenter 链路唤起目标；GRClient 或 UDServer 启动 Bridge，将就绪控制码经受保护的业务通道交回 AI管家后端。后端使用配套控制端连接 Relay，后续系统查询及业务查询直达目标 Bridge。模型不保存授权码、不直接连接门店。相关业务设计保存在 AI管家项目 `docs/design/RemoteOpsBridge接入方案与实施计划.md`。

## 控制码与协议

| 场景 | 控制码及协议 |
|---|---|
| 普通 Agent | 九位码；默认线协议 20 |
| 现有专用测试入口 | 保持测试码与协议 21 的既有语义 |
| Bridge | Relay 随机签发 UUID v4；扩展线协议 22 |
| C# 与 Bridge | 独立本机协议 1，UTF-8 JSON-RPC 2.0，每行一条 |

长码是访问凭据，实例 ID 是业务定位信息，恢复令牌是身份凭据；三者不得混用。Bridge 同次运行、租约内经过身份校验的断线恢复保持原码，进程重启换码。默认心跳 15 秒，租约 10 分钟；离线不能新配对，租约过期后旧码和关联授权失效，不能通过迟到心跳复活。

控制端先检查 Relay 声明的扩展协议版本；不支持时明确失败。Relay 同时检查目标 Agent 的协议与 `host_business_v1` 公告，不把新操作转发给旧 Agent。受管一次性邀请仍走原有路径。

## C# 接入步骤

1. 使用 `ProcessStartInfo` 禁用 Shell 执行、重定向 stdin/stdout/stderr、设置 UTF-8，启动 Bridge。
2. 发送 `initialize`，提供 `protocolVersion`、`role`、`instanceId`、`relay`、`serverName`、能力允许列表、证书信任及可选业务描述。示例代码会登记模拟业务能力。
3. 等待 `bridge.status` 通知中的 `state=ready`；初始化响应只表示配置已接受。只有就绪状态才能把 `accessCode` 经可信业务链路交给 AI管家后端。
4. 响应 Bridge 的 `host.invoke` 反向请求。校验准确实例、能力 ID、版本、参数和返回字段，按原请求 ID 返回结果。
5. 使用 `status` 查看状态，使用 `shutdown` 有序退出；父进程管道关闭也会停止 Bridge。

协议支持字符串或整数 ID、超时与 `$/cancelRequest`；默认业务超时 30 秒、并发 16、单条消息最大 256 KiB（含换行）。业务结果另外限制为 240 KiB，为协议外壳预留空间。标准输出只传协议，诊断走标准错误或实例日志。超时、异常或结果未知时不自动重放调用。

控制端类库提供 `describe_host_capabilities`、`query_host_capability` 对应接口；MCP 提供同名工具。业务结果在结构化响应 `details` 中。能力发现包含服务、实例、能力与版本；当前业务扩展只允许声明为只读的查询。输入/输出 Schema 验证实现常用类型、对象字段、必填项、禁用额外字段及数组等子集，不能替代宿主自身业务验证。本轮不创建或发布规划中的 `JLAI.AI.CapabilityProtocol` 包。

## 目录、能力与证书

GRClient 默认使用运行账号的 LocalAppData，UDServer 默认使用 ProgramData，可通过绝对 `dataRoot` 显式覆盖。目录继续按角色、实例和 Relay 端点摘要分隔；身份、锁、传输、工具缓存、临时文件、日志、诊断均位于对应实例目录。Bridge 保护实例目录 ACL，拒绝路径中的重解析/符号链接；安装程序仍应妥善管理上级目录。相同实例不能重复启动抢占连接。

`allowedCapabilities` 支持 `cmd`、`windows_power_shell`、`power_shell`、`ssh`、`serial`、`file_transfer`、`port_probe`、`tcp_exchange`、`tcp_forward`、`system_operations`、`visual`、`resource_tools`、`host_business`。限制同时作用于公告、初始化和执行检查；完全控制也不能开启被宿主禁用的能力。

默认只读、无桌面。UDServer 禁止启用桌面；GRClient 要开启桌面需显式配置 `visualEnabled` 并允许 `visual`。系统身份不自动提高能力。Shell 查询继续使用现有命令分类与授权检查，不是任意代码的操作系统沙箱。

信任方式为系统可信根、预置 CA 或固定 SHA-256 指纹。正式 TLS 连接仍检查名称、有效期及指纹；Bridge 不弹窗、不自动信任未知证书。信任错误以 `tls_trust_failed` 返回。授权码、恢复令牌及控制端凭据不得进入命令行或普通日志。

## 启动优化与验证方式

版本、系统详情和桌面探测移出出码关键路径；禁用能力不初始化，未准备好的能力不公告为可用。已固定指纹的 Agent 不再重复进行预探测，但正式连接校验保留。脱敏计时覆盖身份及 TLS 配置、基础环境可用性、TLS 连接、登记和控制码就绪。

主要命令：

```powershell
cargo fmt --all -- --check
cargo test --workspace --locked -- --test-threads=1
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p remoteops-relay long_code_real_ten_minute_expiry --locked -- --ignored
dotnet build examples/bridge-csharp-host/BridgeHost.csproj -c Release
python scripts/Test-BridgeIntegration.py --skip-build --binary-directory <配套程序目录> --bridge-executable <Bridge路径>
python scripts/Test-ExternalProduct.py --update-lock
pwsh -NoProfile -File scripts/Test-Documentation.ps1
```

隔离联调用回环地址、临时 Relay、临时身份及显式空 MCP 配置，覆盖两个长码 Bridge 和一个九位码 Agent、双向模拟业务、.NET 4.6.2/.NET 10 实际子进程调用、禁用命令、重复实例、异常消息和证书拒绝。性能样本记录五次新数据目录与五次复用数据目录启动；这是本机启动测试，不代表公网耗时或操作系统磁盘缓存完全冷启动。

候选交付目录中的验收报告和 JSON 记录实际结果。Windows Service 的 SYSTEM 实际运行、真实门店业务、跨平台和现场网络仍需专门验收。源码及候选包默认未签名；本轮不部署生产 Relay、不替换已安装 MCP、不执行 Git 写操作。
