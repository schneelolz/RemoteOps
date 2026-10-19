# 定制 Service Agent 公共接入接口

定制产品通过公共 `remoteops-agent-service` 宿主或 `remoteops-agent` 运行时接入。产品维护资料、端点规则、能力上限和部署配置；连接、身份恢复、权限、命令执行、取消和服务停止继续由公共实现处理。门户签码与资产授权见[门户后端 API v1](portal-api-v1.md)，GUI 薄产品及依赖固定见[薄产品接入与迁移](../product-integration.md)。

## 接入入口

| 公共接口 | 用途与约束 |
|---|---|
| `remoteops_agent_service::run(profile, options)` | 使用公共 SCM 生命周期、前台诊断、配置检查、状态文件和停止处理；每个服务进程启动一次 |
| `ServiceOptions::new(service_name)` | 设置 SCM 内部服务名；服务宿主始终保留无图形能力上限 |
| `ServiceOptions::managed()` | 显式选择 `ManagedOnDemand`；部署登记凭据后以无码状态待命 |
| `ServiceOptions::with_paths(config, status)` | 设置机器级配置及短期运行状态路径；显式命令行路径优先 |
| `ServiceOptions::read_only()` | 在无图形上限上继续限制为只读；完全控制授权不能突破此上限 |
| `ServiceOptions::with_guidance(Vec<TargetGuidance>)` | 附加当前产品的操作说明与案例引用；说明不建立授权 |
| `AgentConfig::load_service_file_with_profile(path, profile)` | 从服务配置目录加载产品默认值，缺省身份与交换目录跟随机器级配置目录 |
| `AgentRuntimeOptions::service(profile)` | 为自行管理生命周期的宿主创建无图形运行选项；调用方仍须明确选择配对模式 |
| `run_agent_with_options(config, events, shutdown, permission, options)` | 运行公共 Agent，接收事件、停止信号、本地权限控制器和可信宿主选项 |
| `run_agent_with_scoped_options(...)` | 需要区分传输代次事件的表现层入口，使用同一运行及权限边界 |

`ServiceOptions::new` 和 `AgentRuntimeOptions::service` 为兼容既有部署仍默认 `AutoPerBoot`。只有显式调用 `.managed()` 或设置可信宿主选项 `pairing_mode = PairingMode::ManagedOnDemand`，才使用门户受管流程。不要仅凭程序名称包含 Service 推断其已进入受管模式。

## 薄服务示例

以下示例展示产品入口。端点使用保留的 `.invalid` 域名，不能用于真实连接；实际产品提供经审核的端点及独立目录。依赖包均须固定为同一公共源码提交。

```rust,ignore
use remoteops_agent::{EndpointPolicy, ProductProfile};
use remoteops_agent_service::ServiceOptions;

fn main() -> anyhow::Result<()> {
    let profile = ProductProfile {
        id: "example-service-agent".into(),
        name: "Example Service Agent".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        directory_name: "ExampleServiceAgent".into(),
        config_file_name: "agent-config.json".into(),
        state_file_name: "agent-state.json".into(),
        env_prefix: "EXAMPLE_SERVICE_AGENT".into(),
        endpoint: EndpointPolicy::Fixed {
            relay: "relay.example.invalid:7443".into(),
            server_name: "relay.example.invalid".into(),
        },
        default_full_control: false,
        legacy_remoteops_paths: false,
    };
    remoteops_agent_service::run(
        profile,
        ServiceOptions::new("ExampleServiceAgent").managed(),
    )
}
```

该入口不会安装 Windows 服务。发布步骤按准确 EXE 路径、服务名、服务账号和配置路径注册 SCM；不要在产品中再实现一套远控循环。`--check-config` 检查配置、TLS 文件、登记文件和目录后退出，不连接 Relay；`--console` 以真实前台运行时连接，用于安装前验证，不注册 SCM，也不代表已经验证 Session 0。

定制 Service 缺省配置位于 `%ProgramData%/<directory_name>/Agent/<config_file_name>`；服务身份文件和 `transfer` 目录默认位于配置目录。`with_paths` 或命令行覆盖后应核对服务账号的读写权限。产品配置、身份文件、交换目录和状态文件不得与另一产品或另一安装实例共用。配置加载仍遵守固定端点、显式布尔值和端点信任隔离规则。

## 受管上线与生命周期

1. 管理员在 Relay 创建独立集成主体及资产白名单，登记资产并取得 `target_id` 和登记证明。
2. 安全部署登记文件，仅供目标 Agent 服务账号与维护管理员读取；配置 `managed_enrollment_file` 路径。普通配置、产品上下文、日志和网页都不保存证明正文。
3. 以 `.managed()` 启动公共服务宿主。首次登记先持久化稳定 `agent_instance_id`，再以部署证明向 Relay 绑定该身份。仅在初始登记尚未完成时，允许以同一稳定身份和有效证明恢复首次握手；登记已经完成后仍必须通过 Agent 自身恢复令牌验证，不能用部署证明替代丢失的恢复令牌。
4. Agent 在线后默认无码，门户后端显式签发十分钟一次邀请，通用 MCP 兑换后建立单目标连接。
5. 同进程断线允许恢复；新 boot、门户关闭、显式替换、主体撤销或 MCP 主动关闭撤销原连接能力。再次连接需要新的邀请。

登记文件格式为 `{"target_id":"<登记返回的目标标识>","token":"<登记证明>"}`。它必须是普通文件且不超过 16 KiB；Unix 下不能允许组或其他用户读取，Windows 部署应设置服务账号专用 ACL。机器身份与 Relay 恢复令牌由公共运行时持久化；`boot_id` 每次公共运行时启动随机生成，只保留在本次运行内，同进程传输重连不重建。

受管登记不把 hostname、MAC、业务系统标识当作认证。已登记 Agent 不能通过删除模式字段或改为 `AutoPerBoot` 绕回自动签码。完整幂等、过期、重签、消费者身份、恢复凭据及存储一致性约束以[门户 API 契约](portal-api-v1.md)为准。

## 产品上限与真实宿主事实

`AgentRuntimeOptions` 是可信产品宿主的构建输入，包含 `product`、`run_mode`、`capability_policy`、`visual_enabled`、延迟的 `visual_provider_factory`、`pairing_mode` 和 `guidance`。普通 JSON 配置、环境变量、远程请求或自由文本指引不能扩大编译宿主设定的上限。

`AgentCapabilityPolicy::headless()` 禁止内建图形操作；`.with_read_only()` 进一步限制写入。Service 宿主在 SCM 模式报告 `WindowsService`，`--console` 报告 `Console`，两种入口均保留无图形上限。图形工厂只在产品策略、宿主模式及本机运行事实都允许时调用。服务产品不会因为 `REMOTEOPS_VISUAL_PROVIDER_ENABLED`、完全控制授权或有人通过 RDP 登录而开启图形 Provider。

本机运行事实通过公共原生宿主采集，包括进程 Windows session、交互桌面可用性、采集时间与来源。图形是否可用不能只看环境变量或会话编号。未知、过期或采集失败不会推断为可用；实际图形执行前仍复查当前事实。`headless` 限制的是公共图形能力，不是任意 Shell 的操作系统沙箱；Shell 等其他操作继续受公共权限策略与服务账号权限约束。

## 目标上下文与通用 MCP

`TargetContext` 按准确目标会话传递，结构版本与线协议版本分别管理。它包含产品 ID、名称和版本、真实宿主模式、`TargetRuntimeFacts`、每项能力的 `TargetCapabilityStatus`，以及有版本和来源的 `TargetGuidance` / `GuidanceReference`。上下文不包含登记证明、恢复令牌或控制码。

`get_target_info` 返回当前目标上下文和能力状态；刷新使用公共 `GetTargetContext` 操作，不触发截图、窗口枚举或图形 Provider 初始化。MCP 按不可变 `session_id` 隔离多个目标的说明；一个目标的 Service 限制和产品指引不能覆盖另一目标。未知结构或旧端缺失信息显示 `unknown`；刷新失败显示陈旧状态并收紧图形判断，不能用旧可用标记继续放行。

交互产品启动时仅探测桌面、窗口和前台指纹，不扫描 UIA 树或截图。显式调用
`desktop_capabilities` 时，MCP 先刷新该目标的上下文；已知的临时探测失败可以通过
已授权的只读图形观察重试，Agent 在执行前重新检查产品上限和当前桌面事实。
失败不会开放文本输入、控件或键鼠操作；服务、产品强禁、未知上下文均不能借此初始化
实际 Provider。探测采用 35 秒外层预算和 3 秒冷却，避免截断内部 30 秒的子进程预算；
GUI 诊断日志的 `visual_probe` 记录稳定原因码、耗时及截断脱敏的底层错误。
本次不改变协议 18；旧 Agent 的启动失败状态仍需重启或升级才能解除。

产品指引可说明日志位置、常见诊断顺序或案例库引用，但属于不可信参考资料。它不能改变用户授权、MCP 工具边界、只读策略或可信宿主能力上限，也不能自动执行引用内容。无需为每个产品复制或硬编码一套 MCP。

上下文 JSON 总长最多 32 KiB，能力最多 64 项，指引最多 8 项、正文合计最多 16 KiB，引用合计最多 8 条。每项指引和引用必须提供非空 ID、版本及来源；ID 最多 128 字节、版本 64 字节、来源 128 字节，引用 URI 最多 2048 字节且不能包含控制字符。长度按 UTF-8 字节计算；未知结构版本不能用于放行能力。

## 兼容矩阵与升级顺序

下面的版本均指线协议，不能用 EXE 文件版本或 Cargo 包版本代替。

| 组件组合 | 自动签码与启动边界 | 门户受管与恢复凭据 | 上下文及图形判断 |
|---|---|---|---|
| Relay 18 + Agent 18 + MCP 18 | 支持 `AutoPerBoot`，重启换码、同 boot 重连保留 | 支持受管无码待命、一次邀请、单目标恢复和撤销 | 使用协议 18 上下文与实时事实，图形仍受产品及权限限制 |
| Relay 18 + Agent 17 + MCP 18 | 仅支持协议 17 的 `AutoPerBoot` 兼容流程 | 不允许受管注册或把旧端当作受管能力持有者 | 旧端上下文按 `unknown` 处理；新版 MCP 不据此放行图形 |
| Relay 18 + Agent 18 + Controller/MCP 17 | 仅允许自动签码兼容路径 | 旧 Controller 兑换受管邀请被拒绝，不能消费一次码 | 旧消费者不能承担新版上下文及恢复协议契约 |
| Relay 18 + 任一协议 16 Agent/Controller | 握手拒绝，不降级绕过 boot 边界 | 不支持 | 不提供兼容推断 |
| MCP/Agent 18 + 旧 Relay 17 或更早 | 不作为支持组合；新版客户端不能直接使用旧 Relay | 不支持 | 先升级 Relay，再部署配套端 |

协议 17 即使附带某些相似字段，也不能冒充协议 18 的受管或上下文语义。Relay 18 兼容 17 仅用于迁移自动签码客户端，不表示它兼容协议 16。

JLAI 当前已固定的旧 `rev` 仍对应协议 16，需要升级薄产品的公共依赖到包含协议 18 的同一个已审核提交，并同步产品 `Cargo.lock` 与伴随程序。只更新 Relay、只改包版本或复制新协议号不能获得新能力。公开底座尚未提交的工作区不能作为发布 `rev`；正式发布需从已审核提交构建并记录实际依赖来源与产物哈希。本文不表示已经修改或发布 JLAI 产品。

## 验证与交付边界

各项执行结果由当前交付验收记录统一填写，本接口说明不单独宣称生产通过。验证应区分以下层次：

| 验证层次 | 可以证明的范围 | 不能替代的范围 |
|---|---|---|
| 公共单元及 Relay 生命周期测试 | 能力上限、上下文隔离、授权状态机、协议拒绝、幂等及故障恢复 | 原生服务账号和桌面环境 |
| 实际本地前台 Agent runtime + 回环 TLS/HTTP | 真实运行时、消息及连接路径，包括前台 headless 和停止处理 | SCM 安装、Session 0、开机无登录 |
| Windows SCM 原生验收 | 服务安装、启动/停止、服务账号、Session 0、配置 ACL | 尚未执行的登录态变化、RDP 或生产配置 |
| 无登录、锁屏、RDP 连接/断开及重连验收 | 相应真实系统状态中的能力收紧和生命周期 | 其他机器、账号或未覆盖环境 |
| 生产端到端验收 | 配套 Relay、受管产品、MCP、门户后端、HTTPS 及运维备份流程 | 未执行的故障恢复或长期运行情形 |

本地前台真实运行不能写成“Windows Service 已验收”；SCM、Session 0、开机无登录、RDP 和生产部署未实际执行时必须分别列为未验证。升级验收还应覆盖旧协议拒绝、同 boot 重连、新 boot 撤销、产品上限不能被 FullAccess 扩张、目标信息查询不触发图形，以及多个目标上下文互不污染。
