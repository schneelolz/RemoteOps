# 门户授权管理与资产迁移交付记录

日期：2026-10-23。功能分支：`codex/portal-authorization-transfer`。Relay 候选为 `0.2.0-preview.10`，门户数据库 schema v2，HTTP 业务 v1 与线协议 18 保持。

## 修改范围

- `remoteops-portal`：管理员查询、白名单增删、同 Owner 资产迁移、并发校验、持久幂等、成功/失败审计及 schema v1 升级。
- Relay：新增管理 HTTP 契约、目标级撤权、失败期间授权门控、同键重试及启动恢复。
- Agent/MCP 源码与协议未修改。已有协议 18 的受管 Agent 和通用 MCP 无需随本次功能更新；OpsCenter 保持门户业务 v1 即可继续调用。

迁移保留原主体凭据、资产 target、Agent 身份、登记证明及 Agent 传输恢复身份。旧目标的邀请、控制连接与恢复能力失效，其他资产保持。门户撤权后 Agent 仍可使用原身份重连，但不能取得控制授权或退回自动签码。

## 本地验证

| 命令 | 结果 |
|---|---|
| `cargo test --locked -p remoteops-portal -p remoteops-relay --quiet` | 门户 21 项、Relay 120 项通过 |
| `cargo clippy --locked -p remoteops-portal -p remoteops-relay --all-targets -- -D warnings` | 通过 |
| `cargo fmt --all -- --check` | 通过 |
| `scripts/Test-Documentation.ps1` | 文档链接通过 |
| `scripts/Test-ControlPermissions.ps1` | 本机隔离 TLS Relay、真实 Agent 与通用 MCP 控制授权及门户配对回归通过 |

新增测试包含普通门户凭据不能调用管理 API、ACL 精确增删、空白名单、修订/归属/同名/Owner/撤销主体冲突、并发迁移、数据库事务失败回滚、v1 升级与错误 Owner 回滚、敏感信息不入审计、JSON 落盘失败、未完成操作重启恢复、完成态重试不打断新连接，以及撤权后同 boot/新 boot 的 Agent 重连。

独立审查发现的 ACL 撤权后 Agent 重连问题已经修正，并有运行态回归。TLS 实机库测试使用本机临时目录和前台受管宿主，不等同于真实 SCM、生产 HTTPS、Linux 现场或整机重启验收。

## 测试接入步骤与边界

1. 由部署负责人一致备份 Relay JSON、门户 SQLite 及 WAL，替换为本次 Relay 候选。旧 Relay 不识别 schema v2，不能仅回退二进制。
2. 使用 Relay 管理认证读取原主体 A、测试主体 B、Linux 准确 target 和当前修订号。
3. 给 A 追加 Linux 资产键，再以新的幂等键将准确 target 从 B 迁至 A；每次请求使用对应资源的修订号。
4. 核验 A 的原凭据可读取 Windows/Linux，B 不能再读取或控制原 Linux；旧码和旧连接恢复均拒绝。
5. 在 OpsCenter 关联准确 Linux，初始禁用，管理员显式开启后签发原生码，用正常 RemoteOps MCP 配对，验证环境指引、能力上限、撤码、禁用和主动断开。

接口、请求示例、操作查询和错误码见 [门户 API v1](portal-api-v1.md)。控制码只能首次兑换一次，服务器作为独立控制端与 MCP 不能分别重复兑换同一张码。OpsCenter 原有 Agent 通道可以独立保留。

本次未部署或重启真实 Relay，未迁移真实资产、修改现场 ACL、重装 Agent 或替换用户 MCP。定制产品同步底座引用不等于现场 Relay 已更新。
