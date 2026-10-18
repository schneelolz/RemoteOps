# 定制服务 Agent 接入交付与验收记录

日期：2026-10-18。验证环境：Windows 10.0.26200、Rust 1.97.1，普通用户权限。本文描述本轮源码实现和本地验证，不能作为生产部署完成证明；后续集成使用 Git 中准确的完整提交 SHA 固定依赖。

## 修改范围

| 范围 | 关键文件与结果 |
|---|---|
| 公共产品与 Agent 运行库 | `crates/remoteops-product`、`apps/remoteops-agent/src/runtime.rs`：正式产品能力上限、延迟图形工厂、执行入口统一拒绝、当前宿主事实与目标指引 |
| 原生事实与服务宿主 | `crates/remoteops-native-host`、`apps/remoteops-agent-service/src/lib.rs`：原生 Session/桌面元数据、公共 SCM/前台宿主、产品配置、停止与重连；`deploy/agent-service/windows` 复用脚本接受定制服务名，保留开机启动及异常恢复 |
| 上下文与协议 | `crates/remoteops-domain/src/target_context.rs`、`crates/remoteops-protocol`：协议 18、上下文结构版本 1、受管邀请/恢复模式、敏感 Debug 脱敏 |
| Relay 与门户 | `apps/remoteops-relay/src/relay_portal.rs`、`portal.rs`、`crates/remoteops-portal`：独立认证、资产 ACL/准确身份登记、十分钟一次码、撤销、故障时拒绝访问与幂等恢复 |
| 通用 MCP 与恢复 | `crates/remoteops-application`、`apps/remoteops-controller-mcp/src/main.rs`：按 session 返回上下文、未知能力不放权、配对响应丢失重试、断线不重放执行请求、主动关闭持久授权 |
| 示例与测试 | `examples/example-service-agent`、Relay 门户测试、`tests/remoteops-control-smoke/src/managed_portal_live.rs`、`pair_result_proxy.rs`、`target_context_live.rs`、两个控制权限测试脚本 |

保留客户 GUI 的 `AutoPerBoot`：新启动换码，同一次启动断线保码。受管产品显式使用 `ManagedOnDemand`，上线默认无码。没有修改 OpsCenter 或 RemoteOps-JLAI；薄服务示例与公共接口见[接入说明](custom-service-agent.md)、[门户接口](portal-api-v1.md)。

## 已执行验证

| 命令 | 本地结果 |
|---|---|
| `cargo test --workspace --locked -- --test-threads=1` | 573 项通过、0 失败、3 项原有原生环境测试忽略；包括 Agent 82、Relay 103、Application 39、Protocol 20、Portal 9、MCP 57 |
| `cargo check --workspace --all-targets --locked` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| `cargo fmt --all -- --check` | 通过 |
| `./scripts/Test-ControlPermissions.ps1` | 通过，实际回环 TLS Relay、Agent 运行库、门户 HTTP 和通用 MCP；包括首次成功 PairResult 丢失恢复 |
| `./scripts/Test-LocalE2E.ps1` | 通过，CLI/MCP、同一控制端内持久 Shell、精确审批/一次消费、文件 SHA-256、接管/取消、Relay 重启保码和 Agent 进程重启换码换会话 |
| `python scripts/Test-ExternalProduct.py` | 工作区外 GUI 示例 check/test/build、4 项测试、固定端点、实际协议 18、Windows 图标与版本资源验证通过 |
| `cargo check --locked --manifest-path examples/example-service-agent/Cargo.toml` | 独立服务示例锁文件编译检查通过 |
| `./scripts/Test-Documentation.ps1` | 文档链接检查通过 |

Python 脚本语法、示例及公共部署 PowerShell 脚本语法/定制参数已检查，未运行安装/卸载。跨平台 `scripts/Test-ControlPermissions.py` 已更新相同门户测试路径，但本机执行的是 PowerShell 入口；Linux/macOS 及 GitHub CI 未在本轮远端执行。现有 CI 已增加独立服务示例的 Linux/Windows 编译检查，推送后方可取得远端结果。

全工作区测试原始本地记录为 `artifacts/acceptance/custom-service-workspace-tests.log`；外部 GUI 构建报告为 `artifacts/acceptance/external-product.json`；综合 E2E 脱敏结果及临时目录清单为 `artifacts/acceptance/local-e2e-20261018.json`（另有同名 `.txt`）。`artifacts/` 不提交到仓库，重新运行相应命令生成实际证据。报告中的基线提交加 `source_dirty=true` 不能代替将来发布的准确提交与产物哈希。

额外广测时修正了旧 E2E 的过时假设：持久 Shell 必须在同一个 Controller 生命周期内读写，不能跨独立 CLI 退出继承；无效审批分别按命令工具错误与上传失败响应核验明确错误码；Agent 重启必须换码、换会话，Relay 重启则保留同 boot 会话。这些修正只调整测试，没有放宽会话清理或授权规则。

旧脚本诊断期间用 `-KeepOnFailure` 保留了 8 个本次专属临时目录。测试结束后的精确路径清理被自动审批拒绝，返回原因仅为 `blocked by policy`，未改用其他语言或方式绕过；目录尚未删除。它们包含隔离本地测试的凭据和状态，不是生产凭据，未纳入仓库。准确目录与脱敏测试结果单独记录在本地验收产物，后续清理需遵循执行策略。

## 需求验收对应

| 验收项 | 已验证范围与限制 |
|---|---|
| 真实 Windows Service、Session 0、无人登录 | 已提供公共宿主和可复现 SCM 验收脚本；本机非管理员，未安装或执行真实 SCM 验收 |
| 活动 RDP、图形开关和 FullAccess 不能突破产品上限 | 模拟可用桌面事实与启用请求的策略矩阵、合法授权协议测试通过；真实 RDP 组合未验 |
| 直接图形请求被拒且实际 Provider 构造/调用为零 | Agent duplex 协议路径逐项拒绝六类图形请求；工厂遇调用即 panic 的计数测试通过；信息查询不探测图形 |
| 同一 MCP 并连 GUI/服务目标隔离 | 实际通用 MCP 连接两个协议模拟目标，通过产品/指引/能力隔离与零误发图形断言；模拟事实不冒充真实 SCM |
| 码过期、重复兑换、关闭、重签、撤权 | SQLite 并发/过期/幂等、Relay 生命周期和真实门户→Agent→MCP 双向关闭与新 session 验证通过 |
| 重连、重新配对、重启不扩大权限 | 启动边界、旧 boot 拒绝、受管恢复 token、撤销后拒绝、首次登记中断恢复、落盘失败及双目标批量撤权故障测试通过 |
| 不同兼容客户端使用公共 MCP | 公共 MCP STDIO/工具流程已验，不需要 OpsCenter 专用 MCP；未逐一运行 DeepSeek Harness、Pi、GPT Desktop |
| 旧/新三端和未知字段 | 协议及 Relay 拒绝测试、协议 17 Hello 故意附加 18 环境元数据、协议 18 缺上下文时零新操作/零图形测试通过；完整矩阵见接入说明 |

额外修复了三类故障：配对成功响应丢失后保留同一次请求的幂等身份；MCP 主动关闭同时撤销门户持久恢复资格；首次受管登记中断保留准确身份，且仅未完成阶段接受有效部署证明恢复。完成后的登记仍须恢复令牌，受管最终确认不能持久落盘时不标记就绪。

## 发布与现场边界

当前协议为 18，Relay 只为自动签码模式兼容 17，拒绝 16。现有 JLAI 固定旧 rev 属于协议 16，正式升级前必须同步更新其公共依赖和伴随程序，不能直接替换生产 Relay 后继续期待旧二进制兼容。

本地验收完成后，变更在独立分支 `codex/custom-service-agent-integration` 通过草稿 PR 进入集成验证。GitHub CI 的实际状态以该 PR 对应提交的检查结果为准，不将已配置工作流写成远端检查通过。JLAI 等使用方先固定该分支的完整提交 SHA，完成适配、产物编译和实际环境测试；CI 与定制产物验收均通过后，再由维护者确认合并 `main`。

尚未合并 `main`，未替换任何生产 Relay/MCP/Agent。真实 SCM、无登录、RDP、长时间运行、生产 HTTPS/备份恢复和实际门户使用方适配仍需现场验收。可执行入口见[服务示例](../../examples/example-service-agent/README.md)，不能用前台或模拟上下文替代真实服务验收。
