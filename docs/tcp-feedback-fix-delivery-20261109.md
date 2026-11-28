# TCP 转发反馈修复与交付

本批修复目标白名单缺少自查与在线维护入口、现场默认权限初始化状态不一致、空指引缺少原因，以及只读命令 Shell 参数契约问题。协议保持 20，持续 TCP 扩展使用独立能力门禁。

## 实现边界

- `crates/remoteops-protocol/src/tcp_forward.rs` 定义目标策略、作用域、到期时间、流帧和双向信用。每方向最多 256 KiB 未确认数据，另限制四个未确认数据帧，短写同样消耗信用。确认发生在目标 TCP 实际写入之后。
- `apps/remoteops-controller-mcp/src/tcp_forward.rs` 维护回环监听及本地连接。MCP 进程最多八条监听，每条最多两个并发连接，双向累计上限 512 MiB，空闲十分钟关闭。数据出口使用有界队列。
- `apps/remoteops-relay/src/relay_tcp_forward.rs` 校验 Controller、Agent、会话、连接代次、当前授权及目标规则，路由有序帧，记录关闭审计。规则支持固定主机、IP、CIDR、端口、现场、安装身份和有效期。
- `apps/remoteops-agent/src/tcp_forward.rs` 执行现场权限及能力校验、DNS 解析和实际目标地址检查，建立固定目标连接。链路本地、广播、组播和已登记的云元数据特殊地址被拒绝。
- `apps/remoteops-relay/src/relay_tcp_admin.rs` 提供管理员 GET/PUT `/api/admin/tcp-forward-policy`，使用 `expected_revision`，冲突返回 409，响应不可缓存。后台的“TCP 转发策略”在线保存后立即生效，撤销会清理受影响的转发。
- `get_tcp_forward_policy` 只返回当前绑定适用的策略和申请字段。申请由 Relay 管理员处理，现场完全控制不授予策略管理权。

## 反馈行为

1. 未批准目标返回 `tcp_target_not_approved`、申请联系说明和申请字段，控制端可先查询策略。权威配置持久化在 Relay 状态的 `tcp_forward_policy`，通过管理 API 在线修改。
2. 支持初始化握手的 Agent 在核对现场默认授权前使用 `initialization_pending`。MCP 配对最多等待三秒，未完成时明确返回 `initializing`，拒绝修改操作。默认完全控制完成后返回 `local_default` 和现场授权告知，Relay 记录控制初始化及权限变化审计。
3. `refresh_global_guide` 返回正文、修订、状态、原因、错误码及缓存是否过期。空且修订零表示尚未配置，读取失败、旧 Relay 不支持和缓存回退分别呈现。MCP 初始化通过运行时 `ServerInfo.instructions` 注入首次取得的内容。
4. `run_readonly_command` 的 `shell` 在 Schema 中为必填，工具描述同步更新。`get_remoteops_info` 提供实际 MCP 版本、协议、Relay 状态和转发能力版本。

## 版本

| 组件 | 最终版本 |
| --- | --- |
| MCP | `0.2.0-preview.28` |
| Relay | `0.2.0-preview.20` |
| 公共 Agent | `0.2.0-preview.39` |
| Skill | `0.1.9` |
| 管理前端 | `0.1.3` |
| JLAI Win32 | `0.4.0-jlai.16` |

本批 `.27/.19/.38/.15` 为中间候选，后续复核补充了短帧信用和 IPv6 元数据目标限制，最终交付采用新版本，旧产物保留。JLAI 通过公共脏工作区的受控内容快照构建，保留品牌、固定 Relay、产品身份及配置目录。候选未签名。

## 验证范围

已执行全量 Rust 测试、严格 Clippy、格式检查、真实隔离 Relay/Agent/MCP 集成测试，覆盖权限初始化、撤权、目标策略热更新、大数据并发、慢读、半关闭和顺序重连。另有单字节写入等待帧信用及传输出口背压的回归测试。

Relay Admin 的单元测试及构建通过，真实浏览器流程覆盖策略保存、修订冲突保留草稿、全局指引保存、桌面及手机明暗主题。MCP/Skill 安装测试覆盖独立组件、身份保留、降级拒绝和发布故障恢复。Windows 深层临时目录和运行时自带 PowerShell曾影响发布测试，使用短路径临时目录及系统 PowerShell 7.6.9 复跑发布测试全部通过。

Linux 交叉编译存在既有的资源模块 `unused_mut`、`dead_code` 和 Zig 链接器提示，Windows 工作区严格 Clippy 结果独立记录。真实 HTTP/HTTPS、SSH、RDP、目标域名、证书、NLA 及宿主重载属于现场验收，须分别记录。

## 交付与现场状态

生产 Relay `.20` 与后台 `0.1.3` 已完成部署，两者健康且重启计数为零。备份位于服务器 `/opt/remoteops/backups/remoteops-before-preview20-p20-jlai16-20261109-final`，含完整数据、配置、镜像记录、隔离副本兼容验证及回滚入口。工具目录和两类指引、数据库、运行配置、卷和端口逐项核对保持一致，原测试连接重新上线。

Codex、默认 Codex 入口和 pi 的配置已更新到 MCP `.28`，三个 Skill 目录均为 `0.1.9`。分别按 Codex 与 pi 的安装配置启动独立进程，均确认 MCP `.28`、52个工具、TCP 扩展 v2、生产连接、必填 Shell 和有原因的空指引。当前聊天宿主的旧进程仍需重开，pi 需重开或 `/reload`。

Windows MCP、独立 Skill 和组合包已通过统一发布脚本交付到 `Y:/MCP/RemoteOps`，原目录完整备份在 `Y:/MCP/RemoteOps-backups/20261109-125711-b6deaeae`。MCP ZIP SHA-256 为 `a200c3f4cc3bee7ca6d3ae7841bd0edad1773153dd138904b9aea6746c47b924`，Skill ZIP 为 `3a875173cf47113c403830faefebeb6c75fa112ddcc6dac0fbef02bcbd53bd71`。

JLAI `.16` 已完成181项无窗口检查并交付到桌面和Z盘对应版本目录，两份摘要一致。候选 SHA-256 为 `08a3fafb93509e6a96e18e4fef3c1df7cb2b375118ed1e30f0de2fdd0b7262c3`，大小11,543,040字节。公共与定制源码快照、同源配套程序及构建材料归档在定制仓库的 `artifacts/win32-single/0.4.0-jlai.16-20261109-final`。

最终工作区测试1040项通过，6项既有现场测试忽略。格式和严格 Clippy 通过，真实三端专项通过，后台单测16项、浏览器167项、Skill安装23项、发布故障恢复13项通过。浏览器最终复跑为异步列表增加了就绪等待，使用临时测试驱动，仓库原脚本保留。

现场目标由用户指定为 `192.168.10.254:80`，待现场运行 `.16` 并提供新控制码后继续验收。临时规则将限制到本次 Agent 和当前 MCP 安装，验收后移除。目前生产策略仍为空，尚未添加临时放行规则。真实浏览器目标、HTTPS、SSH和RDP结果保持待验。

未执行 Git 提交、分支、合并、推送或清理，两个工作区已有未提交及未跟踪内容保留。

## 现场验收结果

2026年11月9日，用户启动 JLAI `.16` 后完成真实生产链路验收。当前聊天仍使用旧 MCP，因此另启动已安装 `.28` 的独立 MCP 进程进行新版契约和转发测试。

- `.28` 配对直接返回 `full_access/local_default/synced` 和明确现场默认授权告知，未调用权限切换。空指引返回“Relay 尚未配置 RemoteOps 全局指引”，错误码为空。
- 空策略拒绝创建并返回申请指引。通过管理员 API 在线加入一条限定本次现场和 MCP 安装、有效期15分钟的单目标规则，策略修订由0变为1，未重启服务。
- 默认本地 Host 和显式目标 Host 均收到 HTTP 200。清理前一批连接后，两路并发请求均成功。关键 JavaScript 单独传输成功，大小2,338,030字节。
- 真实 Chrome 导航到路由器 `/cgi-bin/webui/admin`，标题为 Router，但关键 JavaScript 加载失败且页面空白。同期转发状态明确记录“本次本地连接被拒绝，已有两个活动连接”，说明此前确认的每转发两连接上限不适合此页面的并发资源加载。
- 已删除临时规则，策略修订变为2、规则数归零。转发状态变为关闭且活动连接归零，本地端口连接返回 `ECONNREFUSED`。浏览器及独立 MCP 测试进程已退出。

本次网页验收未通过。HTTPS、SSH、RDP/NLA未执行。脱敏记录和首次加载截图位于 `output/playwright/onsite-tcp-20261109` 及本批发布归档。

## 待确认的连接数调整

建议每条转发上限由2提高到8，同时增加 MCP 和 Agent 各32条活动 TCP 连接、Relay 总计128条活动 TCP 连接的上限。保留固定目标、现场及安装身份范围、当前会话授权、512 MiB累计流量、256 KiB窗口、四帧信用、8 MiB/s发送限速和空闲超时。

实现涉及 `crates/remoteops-protocol/src/tcp_forward.rs` 的共享限制、两个端点 `tcp_forward.rs` 的连接计数与状态说明，以及 `apps/remoteops-relay/src/relay_tcp_forward.rs` 的全局计数。补充六条持续连接与额外资源并发、达到上限后的拒绝、关闭后回收、撤权清理及真实浏览器关键脚本加载测试。

确认后发布 MCP `.29`、Relay `.21`、Agent `.40` 和 JLAI `.17`，复用后台 `0.1.3` 与 Skill `0.1.9`，依照既有备份和交付流程更新。需要现场运行新候选后再次验收。
