# 薄产品接入与迁移

## 受管服务与门户后端

长期运行的产品 Service 使用公共 `ManagedOnDemand` 流程：先由受控后台登记资产并安全下发部署证明，Agent 认证在线后默认不展示或签发控制码。OpsCenter 等产品后端通过独立、限定资产白名单的门户凭据，显式签发十分钟一次邀请。消费者仍使用公共 MCP；代码签发、单目标绑定、断线恢复、权限和撤销由公共 Relay/Agent 执行，不在产品中复制一套远控逻辑。

资产映射使用稳定 `target_id` 与经登记证明锁定的 `agent_instance_id`，主机名、MAC、业务系统字符串只作为展示。产品网页不能持有 Relay 管理或全局 Controller 凭据。通用 GUI 继续每次运行自动换码，受管 Service 不因伪装模式字段退回自动签码。门户接口、幂等及凭据边界见[门户后端 API v1](integrations/portal-api-v1.md)；公共服务入口、能力上限、目标上下文、兼容矩阵和原生验收边界见[定制 Service Agent 公共接口](integrations/custom-service-agent.md)。受管模式由产品宿主显式选择 `.managed()`，兼容入口不会仅因运行于服务进程而自动切换。

## 边界与入口

公共底座维护 TLS、配对、连接恢复、协议、执行与取消、权限权威同步、配置加载与原子保存、隔离目录、脱敏日志、GUI 宿主和 Windows 资源工具。产品只提供名称、版本、图片、文案、默认偏好、固定策略、目录命名及小型界面扩展。

| 公共接口 | 职责 |
|---|---|
| `remoteops-product::ProductProfile`、`EndpointPolicy` | 产品标识、默认值、固定端点、路径及环境变量命名 |
| `AgentConfig::load_file_with_profile`、`resolve_product`、`save_for_product` | 一次解析缺字段与显式布尔值、输入合并后的策略检查、端点信任隔离、原子写入与便携回退 |
| `remoteops_agent_gui::run`、`run_default`、`resolve_config` | 同一个默认与外部产品宿主；无窗口、无网络的配置检查入口 |
| `GuiOptions`、`GuiLayout`、`GuiExtension`、`GuiEvent`、`GuiSnapshot`、`GuiAction` | 布局模板、可选活动组件、Logo、外观、双语文案及受控呈现扩展 |
| `remoteops_agent_service::run`、`ServiceOptions` | 公共 SCM 与前台宿主、机器级配置、状态、停止和受管模式入口 |
| `AgentRuntimeOptions`、`AgentCapabilityPolicy`、`run_agent_with_options` | 可信宿主能力上限、延迟图形初始化和目标上下文 |
| `activity::ActivityModel` | 有界摘要、并发任务与明确终态；不以 stderr 或日志级别推断完成 |
| `remoteops_build::WindowsResources`、`compile_windows_resources` | 外部产品自行提供资源路径、产品版本与 EXE 名称 |
| `build_info` | 实际链接的 Agent、GUI、协议版本及源码提交、dirty 状态 |

产品配置适用于公共 GUI 宿主、公共 Service 宿主及显式调用产品配置 API 的自定义宿主。Service 使用机器级产品配置入口，不读取 GUI 用户配置；通用 CLI 兼容入口继续使用 RemoteOps 默认产品。默认 GUI 关闭活动摘要，维持原布局。新增本地事件枚举变体可能要求外部 Rust 消费者更新穷尽匹配。本轮线协议升级为 18；Relay 仅保留协议 17 的自动签码兼容路径，协议 16 被拒绝，受管模式及完整目标上下文要求配套协议 18。

`GuiSnapshot` 不暴露可变 App、绑定令牌或恢复凭据。扩展只能提交动作；保存错误、确认、请求超时和权限执行均由宿主处理。产品组件使用 `remoteops_agent_gui::egui` 的重导出，不能自行引入另一版 egui。

`GuiExtension::mode(slot)` 默认返回 `ExtensionMode::Append`，保留既有追加行为；产品可对页头、连接卡片或活动区域返回 `ExtensionMode::Replace`，替换该区域的默认内容。替换页头时由产品接管包含设置菜单在内的页头布局，底部操作栏和宿主权限对话框仍保留；扩展动作继续交给底座执行。示例：

```rust,ignore
fn mode(&self, slot: ExtensionSlot) -> ExtensionMode {
    if slot == ExtensionSlot::Activity {
        ExtensionMode::Replace
    } else {
        ExtensionMode::Append
    }
}
```

公共活动摘要最多保留 64 条记录，优先保护最近 16 条终态；更旧的结果允许淘汰，新任务输出不会因历史终态占满队列而被丢弃。

`GuiOptions.layout` 默认为 `GuiLayout::Standard`；启用公共活动摘要或扩展时使用 540×560 逻辑像素。需要紧凑产品界面时明确选择 `GuiLayout::Compact`，启动及运行中均为 500×375，活动区域紧接控制码，并省略页头重复 Logo。产品可让活动区域返回 `Replace`，复用其已有的纯呈现组件，而不恢复旧连接、配置或权限实现：

```rust,ignore
GuiOptions {
    layout: GuiLayout::Compact,
    activity: false,
    extension: Some(Box::new(ProductActivity::default())),
    ..GuiOptions::default()
}
```

`GuiExtension::observe(&GuiEvent)` 默认不处理，产品可按需消费只读呈现事件。宿主先校验连接代次，且仅在受控连接中分发 `OperationLog`（已脱敏）及 `OperationState`（含请求 ID 与真实阶段）；控制端连接发送 `ControllerConnected`，连接清理或控制端归零发送 `Reset`。扩展应在重置时清理摘要及动画，不应从 stderr 或成功文字推断终态，也不应把任务间隙提示当作模型实际推理状态。此回调不提供连接、配置、权限或执行的可变访问；宿主动作仍通过 `GuiAction` 处理。

产品自行负责其纯呈现组件的有界缓存、按需重绘、字体/缩放和裁切验证。公共窗口模板不能自动保证任意扩展高度均适配；上线前应观察真实日志、并发任务、断线和任务间隙的界面。

## 最小产品

参考 [中性示例](../examples/example-assist/README.md)。其 `profile()` 声明 `Example Assist`、独立目录与环境前缀、`relay.example.invalid:7443` 固定端点及默认完全控制。`options()` 提供中性图标、活动摘要和一条测试环境提示。入口只调用公共 `run`，没有复制配置、运行时、权限或 GUI 源码。

`.invalid` 是测试域名，正常连接不会到达生产 Relay。视觉验收使用 `--demo`；自动验证使用 `--check-config`。不要为测试关闭 TLS 校验。真实产品若需要自签名证书，仍使用既有指纹核对或管理员提供 CA 的显式信任流程。

## 配置契约

产品默认仅填补缺字段：保存的 `false` 和 `true` 都优先；`null` 不是布尔值。固定端点对配置文件、命令行、产品专属环境变量、设置保存统一检查，首次设置不可编辑。配置错误恢复使用同一产品配置，保留能成功解析的偏好。默认权限保存成功才更新下次自动授权偏好，保存失败保留原值；保存本身不修改当前会话。

默认值不是有效授权。有效权限来自当前 Relay 快照，仍受只读和强制外部审批上限约束。新绑定先查询最新快照，再以绑定、连接代次及修订号申请默认授权。明确撤销会抑制本 Agent 进程内后续默认自动授权；重连不恢复，进程重启才重新读取默认偏好。自然到期、初始同步和陈旧事件不会触发该抑制。重复 MCP 完全控制不替换有效现场授权的来源、修订号和期限；重复相同 Agent 上限不清除授权。

切换 Relay 或 TLS 服务名会清除原 CA/指纹并隔离身份状态；明确为新端点传入的 CA 或指纹在清理后应用。`--state-file` 和产品状态环境变量指定的是身份文件基名，GUI 入口统一按产品 ID 与端点派生隔离文件；保存配置后重启仍使用相同派生路径，不能回退到旧端点文件。旧显式路径不自动迁移恢复凭据，升级后可能需要重新配对；既有 CLI/通用 Service 的默认路径保持兼容；定制 Service 的缺省身份与交换目录跟随机器级配置目录，详见公共服务接口。不同产品使用各自配置、状态、交换目录、GUI 偏好和诊断目录。默认 RemoteOps 继续兼容旧配置发现路径。

## 独立构建与版本固定

示例自带 `[workspace]` 和自己的 `Cargo.lock`。开发阶段使用 path 依赖：

```sh
cargo build --manifest-path examples/example-assist/Cargo.toml --locked
python3 scripts/Test-ExternalProduct.py
```

脚本会把示例复制到 RemoteOps workspace 之外的临时目录，改为当前源码的绝对 path 依赖，运行 `check`、`test`、`build --locked`，并检查没有重复的公共协议、领域或 egui 包。生成的验收清单记录实际链接版本、实际源码提交与构建文件 SHA-256；临时工程在成功后清理。开发者明确更新依赖时使用 `--update-lock`，其后提交产品自己的锁文件；CI 不更新锁文件。

公共实现提交后，将产品中所有 RemoteOps 直接依赖统一到**相同 Git URL、完整 rev**：

```toml
[dependencies]
remoteops-agent-gui = { git = "https://github.com/schneelolz/RemoteOps.git", rev = "<已审核的完整提交SHA>" }
[build-dependencies]
remoteops-build = { git = "https://github.com/schneelolz/RemoteOps.git", rev = "<同一个完整提交SHA>" }
```

不要混用 tag、branch、不同 URL 或不同 rev。若产品直接依赖 domain、protocol 等公共包，也使用完全相同来源；通过 `cargo tree -d` 和集成测试检查类型来源。依赖内部的路径包由同一 Git checkout 提供。维护产品自己的 Cargo.lock、工具链、目标平台、链接器和构建设置，升级通常只调整 rev 与锁文件并重新验证。

启动时 `ProductProfile.version` 由产品包 `env!("CARGO_PKG_VERSION")` 提供。库内同名宏表示底座包版本，不能用作产品版本。公共 GUI 在首次启动和运行中切换语言时统一生成窗口标题：`<当前语言的产品名称> v<ProductProfile.version>`。通用版传入自身 GUI 包版本，外部产品传入自己的产品包版本；版本为空或只有空白时仅显示名称，不回填底座版本。产品文案只提供名称，不应自行再拼接版本。编译时在产品 `build.rs` 调用资源工具，图标路径从**产品自身** `CARGO_MANIFEST_DIR` 构造，设置 `binary_name` 与产品资源信息。工具只链接到指定二进制，不向依赖者传递底座的图标资源。不得复制仓库资源脚本。

库依赖不会交付 CLI、服务、SSH 密码窗口、凭据窗口等伴随程序。由独立发布步骤从同一个审核 rev 构建所需程序，或取得明确记录源码提交和协议版本的兼容发布产物，并验 SHA-256。构建清单应读取实际依赖 `build_info`/Cargo metadata 和实际产物版本，不能手填陈旧协议号。源码归档无法确定 Git 来源时明确报告 `unknown`，不可伪造提交。未提交源码会标记 dirty，不能当作可固定的发布 rev。

## Windows GUI 渲染兼容

默认启动先尝试 DX12/WGPU；仅在应用创建回调尚未进入且返回 WGPU 初始化错误时，公共宿主在旧 UI 线程退出后重新运行同一产品 EXE，结构化保留原参数并显式追加 `--renderer glow`。兼容进程只尝试一次；手动指定渲染器不会触发自动回退。Glow 需要 OpenGL 2.0，纯 OpenGL 1.1 环境不在该兼容保证内。

产品入口在调用 `run(profile, options)` 前应只准备配置、资源和呈现组件，不应连接网络、签发授权或执行不可重复业务动作；自动兼容回退会重新执行产品入口。父进程等待期间不创建 Agent，实际连接及权限校验仅在子进程按原产品配置执行。服务宿主及其产品能力上限不受 GUI 渲染回退影响。

## 从源码副本迁移

1. 列出私有副本中的产品差异、通用修复和伴随程序需求；这一步由私有仓库维护者在其可访问环境完成。
2. 将名称、版本、端点策略、默认偏好和命名迁入 `ProductProfile`；资源及文案放入产品自己的目录。
3. 删除复制的 GUI、配置解析和授权实现，以薄 `main`、公共 `GuiOptions` 和少量扩展替代。
4. 建立独立 Cargo 工程、锁文件和构建脚本，固定所有公共依赖到同一个审核 rev。
5. 为旧私有配置制定一次性、显式迁移：备份、读取用户已有选择、处理旧端点信任；不得复制旧身份到不同端点。底座不会自动迁移真实私有产品。
6. 执行缺字段/显式 false、固定端点、隔离目录、保存失败、撤销后重连与伴随程序兼容测试。
7. 在 Windows 原生环境验证 EXE 图标/文件资源、主题语言、缩放、字体、首次设置、TLS 提示、日志入口、最小化/RDP，按产品规则打包交付。
8. 后续上游升级更新 rev 与锁文件，运行同样的兼容测试；不修改公共内部文件或重放定制补丁。

## 验收边界

Linux 构建和无窗口测试不能代替 Windows 原生资源和交互验收。仓库 Windows CI 执行独立示例的 EXE 资源验证及原有权限 E2E。GUI 的视觉与人工信任流程仍需 Windows 人工验收。CI 只能验证已提交且被推送的源码；提交、推送、合并和部署遵循维护者明确授权；合并前须等待最新提交的 CI 通过。
