# 公共底座与产品定制分离交付记录

## 当前状态

2026-10-16，交付分支 `codex/public-product-separation`，实现基线 `ee73417b3aad6e40a7c5786f541b72ec71b9fab0`。用户已授权提交并推送该分支，未授权合并。本次未部署或操作生产 MCP/Relay，也没有迁移或读取实际私有产品及本地 Windows 文件。

工作区文件丢失后，本轮从相同公共基线重建实现，并重新执行以下验证。旧测试日志没有作为本轮验收结果。当前公共实现和外部依赖构建闭环已经具备可审核结果，Windows 原生验收尚未完成。

## 职责与公共接口

公共底座维护连接、TLS、协议、权限、配置保存、数据隔离、运行事件、脱敏日志、GUI 宿主和资源工具。产品仅维护品牌和版本、默认配置及固定策略、命名、资源、小型呈现扩展、薄入口与交付配置。

- `remoteops-product`：`ProductProfile`、`EndpointPolicy`、`ProductPaths`。
- Agent：`AgentConfig::{for_product,load_file_with_profile,resolve_product,save_for_product}`；旧公开配置 DTO 保持布尔字段，内部一次解析保留字段是否缺失。CLI/Service 不隐式采用 GUI 产品配置。
- GUI：`run`、`run_default`、`resolve_config`、`build_info`；`GuiOptions`、只读 `GuiSnapshot`、`GuiAction`、`GuiExtension`、三个扩展区域。通过公共重导出的 `egui` 保持控件类型一致。
- 活动：`ActivityModel`、有界摘要、重复合并、Unicode 限长、明确成功/失败/取消/中断终态；连接代次阻止迟到事件恢复旧动画。
- 构建：`remoteops-build::{WindowsResources,compile_windows_resources,emit_source_info}`。资源由产品自身路径和版本提供，并只链接到指定二进制。

四项公共权限修复均落在 Agent、MCP、Relay：最新快照后申请默认授权；主动撤销后进程内抑制重连自动授权；重复 MCP 授权保留有效现场来源/修订号/期限；相同权限上限重复上报不清空授权。协议维持 16，未增加线协议字段。

## 主要实现

| 文件组 | 变更 |
|---|---|
| `crates/remoteops-product` | 产品配置、固定策略、路径发现和命名校验 |
| `apps/remoteops-agent/src/product.rs`、`lib.rs` | 缺字段默认、显式布尔值、端点信任与身份隔离、原子保存及便携回退、权限修复和连接事件代次 |
| `apps/remoteops-agent/src/operation_log.rs` | 任务终态与脱敏日志保持独立，增加取消/中断和事件代次 |
| `apps/remoteops-agent-gui/src/lib.rs`、`main.rs` | 复用现有实现提取公共宿主，默认产品使用同一入口 |
| GUI `activity.rs`、`extension.rs`、`appearance.rs` | 可选四行摘要、扫描/等待动画、只读扩展及公共主题类型 |
| MCP `main.rs`、Relay `relay.rs`/`relay_control.rs` | 幂等授权与上限上报；权威状态、代次、修订号和上限校验继续生效 |
| `crates/remoteops-build`、`build/windows-resource.rs` | 可独立使用的 Windows 资源工具、实际源码版本记录 |
| `examples/example-assist` | 中性薄产品、资源、独立 Cargo.lock、3 项公共接口测试 |
| 验证脚本、control-smoke、CI | 真正外部工程、真实 TLS 断线重连、重复授权/上报及 Windows 资源验证 |
| 架构、功能索引、权限、发布及产品接入文档 | 边界、迁移、伴随程序和固定同一 Git rev 的构建规范 |

其余 manifest/lock 修改用于共享构建工具和依赖版本接线；少量原有 Clippy 文档标记与 Duration 写法修正用于通过现行严格检查，不改变业务规则。

## 示例接入与构建

[Example Assist](../examples/example-assist/README.md) 自定义产品名称、PNG/ICO、目录、环境前缀，固定测试端点 `relay.example.invalid:7443`，默认完全控制，并启用活动组件。没有复制公共 GUI、配置或授权实现。

```sh
cargo build --locked --manifest-path examples/example-assist/Cargo.toml
python3 scripts/Test-ExternalProduct.py
```

实际外部验收工程在 `/tmp/remoteops-external-6tzzwfmj/example-assist`，不属于 RemoteOps workspace；使用绝对 path 依赖与自己的锁文件，成功后已清理临时目录。开发时明确更新了一次示例锁文件，随后 check/test/build 均使用 `--locked`。公开修改提交后，按 [接入文档](product-integration.md) 将所有 RemoteOps 公共包统一到相同 Git URL 和完整 rev。上述实现基线 SHA 本身不包含本分支新增的接口。

实际产物记录：产品 `1.0.0`、GUI `0.2.0-preview.28`、Agent `0.2.0-preview.27`、协议 16；实际来源基线如上，`source_dirty=true`。SHA-256 与完整版本信息见 `artifacts/acceptance/external-product-linux.json`，不能把该未提交构建当作可复现发布版本。

## 本轮验证

| 命令/验证 | 实际结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| `cargo check --workspace --locked` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| `cargo test --workspace --locked --no-fail-fast` | 471 通过、1 失败、1 原有忽略 |
| `cargo test -p remoteops-agent -p remoteops-agent-gui -p remoteops-build --locked` | 最终宿主回归 102 项通过 |
| `python3 scripts/Test-ControlPermissions.py` | 通过：隔离回环 Relay、真实 MCP/Agent、实际 TLS 重连与拒写验证 |
| `python3 scripts/Test-ExternalProduct.py --update-lock --output artifacts/acceptance/external-product-linux.json` | 外部 check/test/build 通过，3 项测试通过；固定环境变量策略和单一公共类型来源检查通过 |
| `Test-Documentation.ps1` | 通过 |
| `Test-ReleaseArtifactGate.ps1` | Linux 上的发布门禁回归通过，不是 Windows 原生资源验收 |
| `git diff --check` | 通过 |

完整测试唯一失败是未修改的 `remoteops-device::unix_inventory_tests::actual_unix_process_inventory_is_json`：系统 `ps` 返回 `fatal library error, lookup self`。直接执行 `/usr/bin/ps -eo pid,comm --no-headers` 也失败。未跳过、伪造或修改该测试；需要正常 Linux CI 复跑。

日志在 `artifacts/acceptance`。GUI 无窗口渲染测试覆盖中英与明暗主题，不能代替 Windows 字体、DPI、窗口、RDP 与人工信任验收。

## 未完成验证与剩余风险

1. Windows CI 尚未运行：原生编译、EXE 图标/文件资源、平台条件代码与现有 Windows E2E 仍待验证。CI 配置已加入独立产品资源检查；提交推送已获授权，远端运行结果及 Windows 人工验收需另行确认。
2. Windows 人工 GUI 验收：首次设置、TLS 指纹确认、权限交互、缩放、字体、语言主题、日志入口、最小化和 RDP。
3. 新增本地 Agent 事件及终态枚举可能要求直接匹配这些枚举的外部 Rust 消费者补充分支；线协议保持兼容。通常只依赖 GUI 公共快照的薄产品不需要处理内部事件。
4. 外部产品要维护自己的 Cargo.lock 和构建设置。库依赖不会自动交付 CLI、服务、SSH 密码窗口等伴随程序；必须取得同一来源提交或明确验证兼容的产物，检查真实版本与 SHA-256。
5. 这里没有实际私有产品迁移的证明；中性独立示例证明的是公共依赖边界和构建闭环。

## 私有产品迁移清单

- 盘点产品差异，将品牌、版本、固定端点、默认偏好和目录命名移入配置及产品资源。
- 删除复制的公共实现；以薄 main、GuiOptions 和少量只读扩展接入。
- 统一所有公共包的 Git rev、维护产品锁文件、同源伴随程序和构建清单。
- 显式迁移旧配置，保留用户 false/true；备份身份并按端点处理信任，不把旧身份跨产品/端点盲目复制。
- 执行本例的配置、固定策略、隔离、保存失败、撤销重连和资源兼容测试；完成 Windows 原生及人工验收。
- 后续上游升级只更新依赖 rev 与锁文件并跑兼容测试，不复制源码或重新应用定制补丁。

## 完整修改文件

- `.github/workflows/ci.yml`
- `Cargo.lock`
- `Cargo.toml`
- `apps/remoteops-agent-gui/Cargo.toml`
- `apps/remoteops-agent-gui/src/appearance.rs`
- `apps/remoteops-agent-gui/src/main.rs`
- `apps/remoteops-agent-service/Cargo.toml`
- `apps/remoteops-agent-service/src/main.rs`
- `apps/remoteops-agent/Cargo.toml`
- `apps/remoteops-agent/src/lib.rs`
- `apps/remoteops-agent/src/operation_log.rs`
- `apps/remoteops-controller-cli/Cargo.toml`
- `apps/remoteops-controller-gui/Cargo.toml`
- `apps/remoteops-controller-mcp/Cargo.toml`
- `apps/remoteops-controller-mcp/src/main.rs`
- `apps/remoteops-credential-prompt/Cargo.toml`
- `apps/remoteops-relay/Cargo.toml`
- `apps/remoteops-relay/src/admin.rs`
- `apps/remoteops-relay/src/enrollment.rs`
- `apps/remoteops-relay/src/relay.rs`
- `apps/remoteops-relay/src/relay_control.rs`
- `apps/remoteops-serial-demo/Cargo.toml`
- `apps/remoteops-ssh-askpass/Cargo.toml`
- `build/windows-resource.rs`
- `crates/remoteops-enrollment-store/src/lib.rs`
- `crates/remoteops-i18n/resources/en-US.json`
- `crates/remoteops-i18n/resources/zh-CN.json`
- `crates/remoteops-i18n/src/lib.rs`
- `docs/ARCHITECTURE.md`
- `docs/README.md`
- `docs/control-permissions.md`
- `docs/功能索引.md`
- `docs/发布与产物说明.md`
- `docs/项目结构规范.md`
- `scripts/Build-Windows.ps1`
- `tests/remoteops-control-smoke/src/live.rs`
- `tests/remoteops-control-smoke/src/main.rs`
- `tests/remoteops-mcp-smoke/Cargo.toml`
- `apps/remoteops-agent-gui/src/activity.rs`
- `apps/remoteops-agent-gui/src/extension.rs`
- `apps/remoteops-agent-gui/src/lib.rs`
- `apps/remoteops-agent/src/product.rs`
- `crates/remoteops-build/Cargo.toml`
- `crates/remoteops-build/src/lib.rs`
- `crates/remoteops-product/Cargo.toml`
- `crates/remoteops-product/src/lib.rs`
- `docs/product-integration.md`
- `docs/product-refactor-delivery.md`
- `examples/example-assist/Cargo.lock`
- `examples/example-assist/Cargo.toml`
- `examples/example-assist/README.md`
- `examples/example-assist/assets/example.ico`
- `examples/example-assist/assets/example.png`
- `examples/example-assist/assets/example.svg`
- `examples/example-assist/build.rs`
- `examples/example-assist/src/lib.rs`
- `examples/example-assist/src/main.rs`
- `scripts/Test-ControlPermissions.py`
- `scripts/Test-ExternalProduct.py`


## 2026-10-16 本地复核修复（阶段记录）

在分支 `codex/public-product-separation` 的 `68bf2ff` 基础上完成以下修复；本节记录当时尚未提交的验证状态：

- GUI 显式身份路径先合并，再按产品与端点隔离；通用产品切换端点并保存配置后，重启仍使用同一派生身份文件。旧恢复凭据不自动跨端点迁移。
- 公共保存函数在创建临时文件前拒绝目录目标，避免 Windows 打包应用路径重定向把目录保存误判为成功；保留原失败测试并增加目录回归。
- 活动摘要仍限制 64 条，保护最近 16 条终态，允许淘汰旧结果，确保新任务输出始终可见；增加长连接连续任务与输出洪峰回归。
- `GuiExtension::mode` 默认追加；产品可按区域选择 `ExtensionMode::Replace`。替换只影响呈现，权限动作仍由宿主执行；验证单区替换保留其他区域与底部停止入口。
- Windows/macOS MCP 安装、检查、卸载与说明同步为 `preview.14`。安装测试从 Cargo 清单读取版本，增加不构建即可执行的版本一致性检查；测试工具需要 Python 3.11+。
- 修正 Windows 原生安装验收测试的 Duration 写法，解除严格 Clippy 阻塞。
- 独立产品增加显式状态路径跨产品/端点隔离回归。Windows PowerShell 5.1 和 PowerShell 7 的隔离安装测试各 27 通过、1 项 macOS 专用测试跳过，版本检查各 2 项通过。测试环境仅对进程设置 UTF-8 与执行策略，不改变系统策略或实际用户配置。

本地最终验证结果：

| 命令/验证 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过，保留修改前的文件行尾 |
| `cargo check --workspace --locked` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| `cargo test --workspace --locked --no-fail-fast --quiet` | 495 通过、0 失败、3 项既有忽略 |
| `python scripts/Test-ExternalProduct.py` | 独立工作区 check/test/build 通过，4 项测试通过，Windows EXE 图标/文件资源通过 |
| `python scripts/Test-ControlPermissions.py` | 真实回环 TLS Relay、MCP、Agent 的授权、撤销与重连验收通过 |
| `python scripts/Test-McpSetupInstallers.py --check-versions` | 2 项版本一致性测试通过 |
| 隔离安装测试 `--platform windows --powershell powershell.exe` / `pwsh`，指定已构建 `--helper` | 每组 27 通过、1 项 macOS 专用测试跳过；各自版本检查 2 项通过 |
| `scripts/Test-Documentation.ps1`、`git diff --check` | 通过 |

本轮没有执行被忽略的原生凭据库/完整注册与交互桌面观察测试。未执行 macOS 原生验收、生产部署或真实 JLAI 迁移；未提交或推送，因此远端 CI 尚未包含这些修复。
