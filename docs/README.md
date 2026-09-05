# RemoteOps 文档中心

源码、自动化测试和实际部署环境是事实来源。带“验收”“交接”或具体阶段名称的文档保存历史背景，不应当作当前默认配置。

架构边界和审查结论见：[架构说明](ARCHITECTURE.md) · [代码审查报告](CODE_REVIEW.md)。

RemoteOps `0.2.0-preview.5` 当前源码候选不内置公共 Relay 地址；该版本计划作为首个 GitHub Technical Preview。部署者必须通过配置文件、环境变量或命令行参数提供自己的 Relay；公网 CA 使用系统可信根，私有 CA 可以配置 PEM，Agent GUI 也支持人工核对并固定 SHA-256 指纹。

公开开发状态、当前门禁和未来阶段见 [项目状态](PROJECT_STATUS.md) 与 [路线图](ROADMAP.md)。图形化能力暂不进入第一阶段，调研结果见 [Visual Provider 调研](VISUAL_PROVIDER_RESEARCH.md)。

## 安装和使用

- [Relay 部署说明](Relay部署说明.md)
- [现场被控端 GUI 使用说明](现场被控端GUI使用说明.md)
- [Agent Windows Service 部署说明](../deploy/agent-service/windows/README.md)
- [RemoteOps MCP 使用手册](RemoteOpsMCP使用手册.md)
- [Codex MCP 接入说明](CodexMCP接入说明.md)
- [macOS MCP 接入说明](macOSMCP接入说明.md)
- [控制端 GUI 使用说明](远程AI运维协助GUI使用说明.md)
- [本地串口 AI 交互 Demo](本地串口AI交互Demo.md)

## 部署、发布和安全

- [部署与三机验收手册](部署与三机验收手册.md)
- [远程 Pwsh 链路部署测试](远程Pwsh链路部署测试.md)
- [发布与产物说明](发布与产物说明.md)
- [安全模型](安全模型.md)
- [项目结构规范](项目结构规范.md)
- [文档维护规范](文档维护规范.md)
- [项目维护审计](项目维护审计.md)

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

## 安全底线

- Token、密码、私钥、控制码、恢复令牌和客户信息不得进入源码、文档、日志或安装包。
- Agent 只主动出站连接 Relay，不监听客户公网端口。
- 同一 Agent Session 只允许一个 `ControllerOwnerId`；Human 和 AI 可以作为同一 Owner 的不同角色协作，不能形成两个互相独立的控制者。
- MCP 使用本地 STDIO；默认按每个 Agent 的 `session_id` 逐项确认，用户可临时开启一小时空闲 TTL 的完全控制。独立 Human Controller 审批保留为兼容和后续多人协作流程。
- 所有目标操作绑定不可变 `session_id`，不能根据主机名或别名猜测目标。
- 发布包由 CI 从源码构建，并附带 SHA-256；`artifacts` 只是本地临时目录。
- `target`、运行状态文件、审计日志、控制码、恢复令牌、TLS 私钥和任何本地 Token 都不属于公开发布内容。
