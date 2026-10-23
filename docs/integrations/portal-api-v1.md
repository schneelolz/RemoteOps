# 门户后端远程接入 API v1

本文规定 OpsCenter 等受控后端与公共 Relay 的接入契约。网页不持有 Relay 管理凭据、全局 Controller Token 或 Agent 恢复令牌。实际发布版本、已执行测试和生产部署状态以交付记录为准。

## 信任与部署

- REST 路径固定为 `/api/portal/v1`，通过 Relay 现有管理 HTTP 监听入口提供。公网入口必须由受信任 HTTPS 反向代理保护；开发验收只使用回环监听。
- 独立集成凭据认证调用方，服务端按资产白名单授权。请求中的系统名称、主机名、MAC、IP、显示标题均不能建立或扩大授权。
- 管理员建立资产到随机 `target_id` 的映射，再向准确目标机器安全下发登记文件。登记证明首次锁定 `agent_instance_id`；相同身份重试允许，其他身份不能抢占。后续 Agent 还必须通过自身恢复令牌验证。
- 受管资产在 Relay 中登记后不能改回自动签码模式，即使集成主体已经撤销。通用 GUI 保持独立的 `auto_per_boot` 流程。
- 门户 SQLite 是独立授权存储，包含凭据摘要和私有 HMAC 派生密钥，必须按凭据级别保护整个数据库及 WAL/备份。状态卷丢失不能通过 hostname 自动找回资产授权。

## 管理员初始化

以下入口仅接受既有管理认证，不能使用门户集成凭据；它们不是网页的业务接口。

| 方法与路径 | 请求 | 返回 |
|---|---|---|
| `POST /api/admin/portal/principals` | `name`、Relay 配置的 `owner_id`、`allowed_assets` 字符串数组 | `principal`、仅首次返回的 `token` |
| `POST /api/admin/portal/assets` | `asset_id`、`principal_id` | `asset`、仅首次返回的 `enrollment_token` |
| `POST /api/admin/portal/principals/{principal_id}/revoke` | 空对象 | 已终止的关联连接列表；重复撤销仍保持撤销状态 |

`asset_id` 为集成主体内部稳定资产键，只允许 ASCII 字母、数字、`-`、`_`、`.`、`:`，长度 1–128；同一主体不能重复登记同名资产。资产必须事先列入该主体白名单。管理初始化失败或响应遗失需由管理员核验，不盲目重复生成主体凭据。

集成凭据返回后保存在 OpsCenter 后端受保护的凭据库。不要保存到网页 localStorage、URL、前端配置、普通日志或源码中。资产登记文件使用 `{"target_id":"<受控返回的 target_id>","token":"<enrollment_token>"}`，通过 Agent 的 `managed_enrollment_file` 路径加载；普通配置仅保存文件路径，不包含凭据正文。

## 管理员授权变更与资产迁移

Relay `0.2.0-preview.10` 新增以下管理能力。只接受既有 Relay 管理认证；门户 `ropp1` 凭据、MCP 安装凭据和 Controller 凭据不能调用这些入口。沿用 32 KiB 正文限制、实际 TCP peer 限流、拒绝未知 JSON 字段以及 `no-store` 响应头。

| 方法与路径 | 请求或结果 |
|---|---|
| `GET /api/admin/portal/principals/{principal_id}` | 返回主体、当前 `revision`、白名单和撤销状态，不返回凭据；已撤销主体仍可由管理员核验 |
| `GET /api/admin/portal/assets/{target_id}` | 按准确目标返回当前归属、Agent 和资产修订号，不返回登记证明 |
| `POST /api/admin/portal/principals/{principal_id}/assets` | `idempotency_key`、主体 `expected_revision`、`add` 与 `remove` 字符串数组；返回管理操作记录 |
| `POST /api/admin/portal/assets/{target_id}/transfer` | `idempotency_key`、`expected_principal_id`、`destination_principal_id`、资产 `expected_revision`；返回管理操作记录 |
| `GET /api/admin/portal/operations/{operation_id}` | 返回原操作的 `pending`/`completed` 状态、前后快照和准确受影响目标，不代表这些资源此刻仍停留在原快照 |
| `GET /api/admin/portal/audit?limit=50` | 返回最近审计事件数组，`limit` 为 1–100；含操作来源、资源、结果和前后快照，不含凭据或控制码 |

白名单采用增量修改，不替换未提及资产。允许移除最后一项，结果可为空；同一次请求不能对同一资产同时增删。纯追加不撤销现有连接。移除只关闭该主体当前拥有的相应目标，撤销其未用邀请及控制连接，后续重新加入也不会恢复旧码或旧恢复能力。已绑定 Agent 的稳定身份保持，不能退回自动签码模式；首次登记仍需有效主体和白名单。

主体 ACL 修订号、资产修订号、连接修订号相互独立。管理员必须先读取对应资源，使用准确修订号。主体已撤销、归属不符、目标主体白名单不包含该资产、同名冲突、跨 Owner 或并发状态变化均拒绝。迁移不要求目标当前在线，也不要求重新登记：保留 `target_id`、`asset_id`、`agent_id`、登记证明摘要及 Agent 传输恢复身份；只改变资产归属及修订。旧主体的其他资产与权限保持。

迁移请求示例（UUID 均为虚构标识）：

```json
{
  "idempotency_key": "2a590f4e-9a0f-4c96-85de-d02c8c70ca0e",
  "expected_principal_id": "c76a9b13-304d-496f-ad45-e5c313d014ee",
  "destination_principal_id": "0544c9cf-76d5-4752-bc35-e2c847a88923",
  "expected_revision": 2
}
```

白名单修改请求示例：

```json
{
  "idempotency_key": "6e7e5df1-afc7-4ac7-bcfb-9ec5858da505",
  "expected_revision": 1,
  "add": ["linux-service-01"],
  "remove": []
}
```

两个请求是独立事务：先给目标主体追加白名单，再迁移；追加本身不会取得其他主体的资产归属。两个操作分别使用各自的幂等键和修订号，不隐式迁移或共享资产。

管理幂等键全局唯一，绑定操作类型、准确资源及规范化参数。参数完全相同的重试返回同一操作，不能改键掩盖失败；复用同键但改变参数返回 409 `idempotency_conflict`。并发修订错误为 409 `revision_conflict`，原归属错误为 409 `ownership_conflict`，目标同名为 409 `name_conflict`。门户业务已有错误契约保持。

归属/ACL、邀请和连接撤销、操作及成功审计在同一个 SQLite 事务内提交。操作先为 `pending`，对应目标暂时拒绝新签码、兑换、恢复和请求转发；Relay 精确清理旧目标会话并保存 JSON 后，才将操作标为 `completed` 并返回成功。不会关闭承载其他资产的整个 Controller 传输连接。纯追加的受影响目标为空。

若返回 503，数据库可能已提交，不能按“完全未变更”处理。使用同键同参数重试，或按已知 `operation_id` 查询；重试会完成待同步工作。重启会在接受连接前恢复未完成操作，恢复失败则拒绝启动。已经完成的原请求重试只返回原结果，不再次清理后来新建的会话。审计的 `actor` 来自已通过认证的 Relay 管理入口与实际 TCP peer；共享管理 Token 不能证明具体自然人身份。

成功表示 Relay 持久授权和运行态清理完成。Agent 解绑通过现有通知执行，没有同步停止回执；已经完成的操作副作用不回滚，不能把成功解释为 Agent 上所有任务均已确认停止。迁移不解除无图形、只读、系统账号或产品能力上限。

门户数据库从 schema v1 事务升级到 v2，保留原主体凭据摘要、登记证明、绑定和密钥。升级前一致备份 JSON、SQLite 及 WAL；旧 Relay 不识别 v2，回滚需要按既有恢复流程处理数据库，不能只替换旧二进制。REST `schema_version: 1`、线协议 18 均保持，本次只需更新 Relay；已兼容受管配对的 Agent/MCP 无需更新。

## 门户业务调用

每次请求携带 `Authorization: Bearer <portal-token>`。此身份由 Relay 解析；接口不接受客户端自报主体替代认证。所有响应设置 `Cache-Control: no-store`，不得被反向代理、浏览器或服务端响应缓存保存。

| 方法与路径 | 请求或结果 |
|---|---|
| `GET /api/portal/v1/assets/{asset_id}/remote-access` | 返回 `schema_version`、`asset`、`online`、当前 `target`、最近 `invitation` 和活动 `connection`，不返回控制码或凭据 |
| `POST /api/portal/v1/assets/{asset_id}/invitations` | `idempotency_key` UUID、可选 `expected_revision`、可选 `replace_active`（默认 false）；返回 `invitation`、一次码 `code` 和可选 `replaced_connection` |
| `DELETE /api/portal/v1/invitations/{invitation_id}` | 撤销未消费邀请；重复撤销幂等；已兑换返回冲突，须关闭对应连接 |
| `GET /api/portal/v1/connections/{connection_id}` | 返回准确目标和消费者绑定的连接状态，不含恢复令牌 |
| `POST /api/portal/v1/connections/{connection_id}/close` | JSON 对象，可含连接的 `expected_revision`；吊销恢复能力、旧绑定与关联旧码，重复关闭幂等 |

邀请创建示例，示例中没有真实目标或凭据：

```json
{
  "idempotency_key": "70c0f094-f744-44b9-b856-699d1e6df994",
  "replace_active": false
}
```

签发仅允许当前已认证、已就绪且租约有效的准确 Agent。资产未登记上线或当前离线时返回冲突，不生成一个等待未来任意机器使用的码。每次新邀请分配新 `session_id`，包含准确 Agent 与 `boot_id`；同一幂等请求重试恢复该邀请，不再新建会话、不延长到期时间。参数变化但复用同键返回冲突。

一个目标只能有一个未消费邀请和一个活动连接。新签发自动撤销该目标旧未用码；存在同次运行的活动连接时默认返回 409。需要替换时显式设置 `replace_active: true`，并提交状态接口中 `asset.revision` 的准确值，原活动连接将被关闭。普通关闭操作使用 `connection.revision`，两种修订号不可混用。

## 一次兑换、恢复与权限

- 一次码仅可在签发后 10 分钟内首次兑换，数据库事务保证并发消费只有一个赢家。过期后首次兑换失败。准确相同消费者与请求的响应遗失重试，只恢复原兑换结果；其他请求不能再消费同一码。
- 兑换通过新版通用 MCP 的现有配对工具完成。Relay 从已认证 Controller 连接取得 Owner、Controller 实例和安装身份，不信任兑换请求中的自报身份。
- 兑换建立单目标连接，恢复能力严格绑定该消费者、准确 Agent、boot、session；它不能用来控制另一台机器。MCP 在本进程内保存高熵恢复令牌，断线恢复不再提交一次码。
- Agent 同次运行或同一 MCP 实例的传输断线可以恢复；传输代次递增不会误判成新设备。Agent 进程重启、门户关闭、明确替换、主体撤销后，旧会话及恢复能力无效。新的 MCP 进程不继承旧 Controller 身份或内存恢复令牌。
- 10 分钟限制首次兑换，不表示已建立连接十分钟后自动结束。临时完全控制仍使用既有授权生命周期；有连接资格不等于已获得写入或完全控制权限，现有 MCP/现场审批和 Agent 策略继续执行。
- 门户与 GUI 共用九位码空间。签发跳过当前 GUI 自动码；当前门户数据库永久保留历史门户码的摘要墓碑，GUI 不复用这些数字，避免过期或已消费码产生歧义。数据库重建后不保证跨库永久唯一；码空间不足必须失败，不能静默复用。

## 一致性与错误处理

门户 SQLite 先提交新邀请/撤销，再由 Relay 在运行状态锁内更新 JSON 会话状态和清理绑定。两边不一致时拒绝授权：签发响应不提前交付短码，相同幂等键可重试恢复已提交邀请；不要改用新键不断重签来掩盖存储故障。

错误 JSON 包含稳定 `code` 和脱敏 `error`。常见状态：400 `invalid_request`，401 `unauthorized`，403 `forbidden`，404 `not_found`，409 `conflict`/`target_mismatch`，410 `expired`/`revoked`，429 `rate_limited`，503 `storage_unavailable`。JSON 结构或未知字段不合法统一返回 400，正文超过 32 KiB 返回 413 `payload_too_large`，错误正文不回显请求内容。调用方不得因状态接口失败而显示“已授权”或“已断开”。

HTTP 限流使用实际 TCP peer，并有全局上限；不能使用 `X-Forwarded-For` 规避。受信任反向代理还需配置公网侧限流。Controller 兑换失败有身份与 IP 限流。日志、遥测和审计只记录主体/目标/请求标识、状态及错误类别，不记录短码、HTTP Authorization、登记证明或恢复令牌。

## 版本、升级与验收

REST v1 与线协议版本独立管理。受管无码待命、一次兑换和恢复令牌需要协议 18 配套端；协议 17 只保留交互式自动签码兼容路径，不能据此开放受管配对。新的请求字段不能被旧端静默忽略后扩大权限。

升级和备份必须同时覆盖 Relay JSON、门户 SQLite 及 WAL、Agent 身份和配套二进制；恢复不同时间点的状态会触发身份不一致拒绝。恢复旧备份可能恢复旧授权，必须由管理员核验并按需吊销，不把回滚当作自动授权恢复。

本地验证入口：`cargo test --locked -p remoteops-portal --lib` 验证 ACL、登记锁定、幂等、十分钟首次兑换、跨句柄并发、恢复/撤销及持久化失败回滚；Relay 的门户测试验证实际 HTTP 认证分离、响应不缓存、请求上限和运行态授权。完整验收还需真实受管 Service、新版 MCP、OpsCenter 后端与受信任 HTTPS 入口一起验证。本文本身不表示已部署或完成生产验收。
