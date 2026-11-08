# RemoteOps 文档中心

源码、自动化测试和实际部署环境是事实来源。带“验收”“交接”或具体阶段名称的文档保存历史背景，不应当作当前默认配置。

架构边界和审查结论见：[架构说明](ARCHITECTURE.md) · [代码审查报告](CODE_REVIEW.md)。

RemoteOps `0.2.0-preview.5` 当前源码候选不内置公共 Relay 地址；该版本计划作为首个 GitHub Technical Preview。部署者必须通过配置文件、环境变量或命令行参数提供自己的 Relay；公网 CA 使用系统可信根，私有 CA 可以配置 PEM，Agent GUI 也支持人工核对并固定 SHA-256 指纹。

公开开发状态、当前门禁和未来阶段见 [项目状态](PROJECT_STATUS.md) 与 [路线图](ROADMAP.md)。图形化能力暂不进入第一阶段，调研结果见 [Visual Provider 调研](VISUAL_PROVIDER_RESEARCH.md)。

## 安装和使用

- [Relay 部署说明](Relay部署说明.md)
- [现场被控端 GUI 使用说明](现场被控端GUI使用说明.md)
- [控制权限与默认模式](control-permissions.md)
- [Agent Windows Service 部署说明](../deploy/agent-service/windows/README.md)
- Linux Headless Agent：`deploy/agent-service/linux`（Ubuntu 24.04 x86_64，systemd）
- [RemoteOps MCP 使用手册](RemoteOpsMCP使用手册.md)
- [Codex MCP 接入说明](CodexMCP接入说明.md)
- [MCP 一次性安装码与配置文件](mcp-one-time-setup.md)
- [macOS MCP 接入说明](macOSMCP接入说明.md)
- [控制端 GUI 使用说明](远程AI运维协助GUI使用说明.md)
- [本地串口 AI 交互 Demo](本地串口AI交互Demo.md)

## 部署、发布和安全

- [部署与三机验收手册](部署与三机验收手册.md)
- [远程 Pwsh 链路部署测试](远程Pwsh链路部署测试.md)
- [发布与产物说明](发布与产物说明.md)
- [MCP 与 Skill 独立发布规范](mcp-skill-release.md)
- [安全模型](安全模型.md)
- [项目结构规范](项目结构规范.md)
- [文档维护规范](文档维护规范.md)
- [项目维护审计](项目维护审计.md)
- [只读边界与执行生命周期加固](safety-lifecycle-hardening.md)

## 功能和维护

- [功能索引](功能索引.md)
- [远程 AI 运维协助交接文档](远程AI运维协助交接文档.md)
- [本地串口 AI 交互 Demo 交接文档](本地串口AI交互Demo交接文档.md)
- [第一阶段验收报告](第一阶段验收报告.md)
- [脱敏三机验收结果](history/evidence/lab-e2e-result-2026-07-31.json)

## 公开项目说明

- [项目状态](PROJECT_STATUS.md)
- [路线图](ROADMAP.md)
- [Visual Provider 调研](VISUAL_PROVIDER_RESEARCH.md)
- [项目价值与图形化路线评估](项目价值与图形化路线评估.md)

## 安全底线

- Token、密码、私钥、控制码、恢复令牌和客户信息不得进入源码、文档、日志或安装包。
- Agent 只主动出站连接 Relay，不监听客户公网端口。
- 同一 Agent Session 只允许一个 `ControllerOwnerId`；Human 和 AI 可以作为同一 Owner 的不同角色协作，不能形成两个互相独立的控制者。
- MCP 使用本地 STDIO；全新 Agent 默认逐项确认。当前配套协议为 19，现场临时授权和持久默认模式见[控制权限与默认模式](control-permissions.md)。MCP 授权保留一小时空闲 TTL；只读、外部审批及宿主独立授权仍然有效。
- 所有目标操作绑定不可变 `session_id`，不能根据主机名或别名猜测目标。
- 发布包由 CI 从源码构建，并附带 SHA-256；`artifacts` 只是本地临时目录。
- `target`、运行状态文件、审计日志、控制码、恢复令牌、TLS 私钥和任何本地 Token 都不属于公开发布内容。

- [控制权限修复交付记录](control-permissions-delivery.md)：构建版本、验证结果及部署边界。

## 公共底座与薄产品

Agent GUI 现提供公共库入口；产品配置与构建资源分别位于 `remoteops-product` 和 `remoteops-build`。RemoteOps 默认入口与独立中性示例复用同一宿主。职责、实际接口、固定依赖版本、外部构建验证及迁移步骤见 [薄产品接入与迁移](product-integration.md)。

- [公共底座与产品定制分离交付记录](product-refactor-delivery.md)：实际接口、完整文件清单、验证结果与待完成验收。
- [定制服务 Agent 公共接口与兼容矩阵](integrations/custom-service-agent.md)、[门户 API v1](integrations/portal-api-v1.md)、[本轮验收记录](integrations/custom-service-delivery.md)、[门户授权管理与资产迁移交付](integrations/portal-admin-delivery.md)。

- [R01—R04 改进实施与交接](RemoteOps改进实施与交接.md)：本轮实现、验证和现场待验收项。

- [中继工具中心交付记录](tool-center-delivery.md)：集中目录、管理权限、执行契约、候选来源及验证边界。
- [工具中心适配、全局指引与权限验收](tool-center-guide-diagnostics.md)：2026-11-06 本地实现、浏览器回归、截图和未验证边界。
- [工具中心配套上线与定制交付](tool-guide-jlai11-rollout.md)：Relay `.14`、MCP `.23`、Skill `0.1.4`、定制 `.11` 的实际备份、升级和交付。

- [UI、SSH 与 MCP 合并修复交付记录](ui-ssh-mcp-delivery.md)：定制 `.10`、MCP `.21`、Relay `.13` 的验证、部署与现场边界。

- [浏览器凭据、平台验证与资源准备进度](browser-credentials-progress-jlai13.md)：协议20、单次网页登录凭据、精确平台证据及准备进度。

- [Relay 管理前后端分离与工具中心调整（preview.17）](relay-admin-split-preview17.md)

- [工具中心布局与详情展示（管理前端 0.1.1）](relay-admin-0.1.1.md)
