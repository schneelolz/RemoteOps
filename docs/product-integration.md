# 薄产品接入与迁移

## 边界与入口

公共底座维护 TLS、配对、连接恢复、协议、执行与取消、权限权威同步、配置加载与原子保存、隔离目录、脱敏日志、GUI 宿主和 Windows 资源工具。产品只提供名称、版本、图片、文案、默认偏好、固定策略、目录命名及小型界面扩展。

| 公共接口 | 职责 |
|---|---|
| `remoteops-product::ProductProfile`、`EndpointPolicy` | 产品标识、默认值、固定端点、路径及环境变量命名 |
| `AgentConfig::load_file_with_profile`、`resolve_product`、`save_for_product` | 一次解析缺字段与显式布尔值、输入合并后的策略检查、端点信任隔离、原子写入与便携回退 |
| `remoteops_agent_gui::run`、`run_default`、`resolve_config` | 同一个默认与外部产品宿主；无窗口、无网络的配置检查入口 |
| `GuiOptions`、`GuiExtension`、`GuiSnapshot`、`GuiAction` | 可选活动组件、Logo、外观、双语文案，以及受控呈现扩展 |
| `activity::ActivityModel` | 有界摘要、并发任务与明确终态；不以 stderr 或日志级别推断完成 |
| `remoteops_build::WindowsResources`、`compile_windows_resources` | 外部产品自行提供资源路径、产品版本与 EXE 名称 |
| `build_info` | 实际链接的 Agent、GUI、协议版本及源码提交、dirty 状态 |

产品配置目前适用于 Agent GUI 的公共宿主及显式调用产品配置 API 的宿主。既有 CLI、Service 不隐式读取 GUI 产品配置，仍保留原来的 RemoteOps 默认值。默认 GUI 关闭活动摘要，维持原布局。新增本地事件枚举变体可能要求外部 Rust 消费者更新穷尽匹配；线协议版本未更改。

`GuiSnapshot` 不暴露可变 App、绑定令牌或恢复凭据。扩展只能提交动作；保存错误、确认、请求超时和权限执行均由宿主处理。产品组件使用 `remoteops_agent_gui::egui` 的重导出，不能自行引入另一版 egui。

## 最小产品

参考 [中性示例](../examples/example-assist/README.md)。其 `profile()` 声明 `Example Assist`、独立目录与环境前缀、`relay.example.invalid:7443` 固定端点及默认完全控制。`options()` 提供中性图标、活动摘要和一条测试环境提示。入口只调用公共 `run`，没有复制配置、运行时、权限或 GUI 源码。

`.invalid` 是测试域名，正常连接不会到达生产 Relay。视觉验收使用 `--demo`；自动验证使用 `--check-config`。不要为测试关闭 TLS 校验。真实产品若需要自签名证书，仍使用既有指纹核对或管理员提供 CA 的显式信任流程。

## 配置契约

产品默认仅填补缺字段：保存的 `false` 和 `true` 都优先；`null` 不是布尔值。固定端点对配置文件、命令行、产品专属环境变量、设置保存统一检查，首次设置不可编辑。配置错误恢复使用同一产品配置，保留能成功解析的偏好。默认权限保存成功才更新下次自动授权偏好，保存失败保留原值；保存本身不修改当前会话。

默认值不是有效授权。有效权限来自当前 Relay 快照，仍受只读和强制外部审批上限约束。新绑定先查询最新快照，再以绑定、连接代次及修订号申请默认授权。明确撤销会抑制本 Agent 进程内后续默认自动授权；重连不恢复，进程重启才重新读取默认偏好。自然到期、初始同步和陈旧事件不会触发该抑制。重复 MCP 完全控制不替换有效现场授权的来源、修订号和期限；重复相同 Agent 上限不清除授权。

切换 Relay 或 TLS 服务名会清除原 CA/指纹并隔离身份状态；明确为新端点传入的 CA 或指纹在清理后应用。不同产品使用各自配置、状态、交换目录、GUI 偏好和诊断目录。默认 RemoteOps 继续兼容旧配置发现路径。

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

启动时 `ProductProfile.version` 由产品包 `env!("CARGO_PKG_VERSION")` 提供。库内同名宏表示底座包版本，不能用作产品版本。编译时在产品 `build.rs` 调用资源工具，图标路径从**产品自身** `CARGO_MANIFEST_DIR` 构造，设置 `binary_name` 与产品资源信息。工具只链接到指定二进制，不向依赖者传递底座的图标资源。不得复制仓库资源脚本。

库依赖不会交付 CLI、服务、SSH 密码窗口、凭据窗口等伴随程序。由独立发布步骤从同一个审核 rev 构建所需程序，或取得明确记录源码提交和协议版本的兼容发布产物，并验 SHA-256。构建清单应读取实际依赖 `build_info`/Cargo metadata 和实际产物版本，不能手填陈旧协议号。源码归档无法确定 Git 来源时明确报告 `unknown`，不可伪造提交。未提交源码会标记 dirty，不能当作可固定的发布 rev。

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

Linux 构建和无窗口测试不能代替 Windows 原生资源和交互验收。仓库 Windows CI 执行独立示例的 EXE 资源验证及原有权限 E2E。GUI 的视觉与人工信任流程仍需 Windows 人工验收。CI 只能验证已提交且被推送的源码；本次任务不自动提交、推送、合并或部署。
