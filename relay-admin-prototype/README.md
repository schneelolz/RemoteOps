# RemoteOps Relay Admin - 管理控制台

本项目是 **RemoteOps Relay** 内置的内部运维管理控制台前端，由 `apps/remoteops-relay/src/admin.rs` 通过 `include_str!` 直接嵌入编译至 Relay 二进制中，负责提供集群状态监控、会话管理、Agent 节点拓扑、凭据状态核验、安全设置与审计日志追踪。

---

## 🚀 架构与访问方式

管理控制台采用纯原生 Web 技术栈构建（HTML5 + CSS3 + Vanilla JavaScript，无外部运行时依赖与 CDN 依赖），在 Relay 启动时由内置 Web 服务提供同源页面与 RESTful 管理 API：

### 生产访问

在 Relay 服务运行的主机或管理内网中，通过浏览器访问 Relay 管理端口（默认 `:18081`）：
```
https://<relay-host>:18081/
```
通过管理员凭据登录后即可进入管理控制台。控制台通过同源 HttpOnly Cookie 自动维持 Session，所有操作直连后端 `/api/admin/*` 接口。

### 本地开发预览

如需对前端界面进行独立调试，可运行本地静态服务：
```bash
cd relay-admin-prototype
python3 -m http.server 8080
```
并在浏览器访问 `http://localhost:8080`（注：需配合运行中的 Relay 实例进行 API 交互）。

直接双击打开 `index.html` 时会进入本地静态模式，自动加载本地数据，不需要输入密码，也不会请求任何 Relay API。通过 Relay 管理地址访问时，页面仍使用真实的管理员用户名和密码登录。

---

## 🧭 功能模块与界面规范

### 1. 登录页 (`/login`)
- **同源安全登录**：输入管理员账号密码，调用 `POST /api/admin/login` 完成鉴权。
- **状态维持**：采用 HttpOnly Session Cookie 机制，浏览器端不持久化敏感 Token 或明文密码。

### 2. 系统概览 (`/overview`)
- **4 大实时指标**：在线 Agent 节点数、活跃会话数、已连接 Controller 数、待处理审批数。
- **Relay 运行状态卡片**：运行版本、连续运行时间 (Uptime)、管理端主机地址、Owner UUID 脱敏摘要及 Controller 连接指标。
- **最近实时事件**：按时间顺序展示最新安全审计流水。
- **活跃会话预览**：直观展示当前活跃会话状态并支持快速跳转与详情查看。

### 3. 会话管理 (`/sessions`)
- **多维度筛选与搜索**：支持按关键词 (Session ID / Agent)、会话状态 (Active / Degraded)、Controller 类型 (AI MCP / Human) 以及权限模式进行即时过滤。
- **Session 详情抽屉 (Drawer)**：平滑滑出查看会话完整拓扑、Agent 规格、Controller 实例 ID、权限模式、待审批计数与进行中请求。
- **受控关闭与紧急停止**：
  - **关闭会话**：向 `/api/admin/sessions/{session_id}/close` 发起请求，安全切断 Controller 控制权。
  - **🛑 紧急停止 (Emergency Stop)**：向 `/api/admin/sessions/{session_id}/emergency-stop` 发送强制停止信号，需手动输入 `STOP` 解锁高危确认。

### 4. Agent 节点管理 (`/agents`)
- **节点拓扑看板**：展示已注册的 Agent 实例 ID、计算机名、MAC 地址、操作系统、绑定 Session、控制码状态、最近心跳及就绪状态。

### 5. Relay 身份与凭据 (`/identity`)
- **双列响应式布局**：宽屏下并排展示，窄屏 (`<= 1024px`) 自动折叠为单列。
- **Owner 身份标识**：展示完整的 Owner UUID 与复制按钮，附带脱敏摘要与地址说明。
- **Controller 凭据状态**：
  - **AI Controller Token (MCP)**：展示配置状态与 SHA-256 指纹（支持自动换行与一键复制）。
  - **Human Controller Token**：以紧凑信息行形式展示配置状态。
  - **安全说明**：明确 Controller 认证令牌由服务端启动配置维护，控制台提供只读状态与指纹核验。

### 6. 安全设置 (`/settings`)
- **修改管理密码**：调用 `POST /api/admin/password` 修改管理员密码（密码长度至少 12 字符，服务端以加盐哈希保存）。修改后自动登出并提示重新登录。

### 7. 审计日志 (`/audit`)
- **操作流水审计**：记录操作时间戳、操作源、操作类型 (AUTH / SESSION_CLOSE / EMERGENCY_STOP / PASSWORD_CHANGE 等)、目标对象、结果状态、来源 IP 及详细摘要。
- **CSV 导出**：支持将筛选后的审计日志导出为标准 CSV 文件以供归档核验。

---

## 🎨 视觉设计规范

详见 [`DESIGN_SYSTEM.md`](./DESIGN_SYSTEM.md)。

## 2026-09 管理界面更新

- 登录及全部管理页面支持跟随系统、亮色、暗色；首次访问默认跟随系统，手动选择会保存在当前浏览器。
- 正文采用 15px，辅助文字最低 14px，扩大按钮和表格行间距；宽表格内部滚动，节点操作列固定右侧。
- Agent 节点支持搜索、状态筛选、多选关闭和关闭全部在线 Agent。全选只选中当前筛选内可关闭的节点；关闭全部不受筛选限制，仅包含在线且支持关闭指令的节点。
- 关闭确认不再要求输入主机名，改为居中展示目标清单与影响说明。仅退出 Agent 并结束远程任务，不会关闭整台服务器。
- 确认时固定连接代次，逐个调用原有鉴权接口，展示每个节点的成功或失败；不会自动重试失败目标，需刷新后重新选择。
- 验证命令：node --test scripts/test-relay-admin.mjs（从仓库根目录执行）。验收记录见 [设计验收](design-qa.md)。

## MCP 客户端一次性接入

侧栏的 **MCP 客户端接入**（`#tab=mcp`）提供三步管理流程：

1. 首次配置客户端实际可访问的 Relay TLS `host:port`、证书 `server_name` 和完整 HTTPS `enrollment_url`。可分别填写 Relay TLS 与注册 HTTPS 服务的公开 PEM 信任证书；公共 CA 部署可留空。地址不会从管理页面地址或 `0.0.0.0` 等监听地址推导。
2. 为一台客户端生成一次性设置，可选名称与首次兑换有效期（1 小时、24 小时默认、7 天）。新生成弹窗可复制设置代码或下载 `.remoteops-setup` JSON 文件；两种格式描述同一份设置。代码与文件含一次性凭据，须通过安全渠道提供给目标使用者。关闭、导航、离开页面或退出登录后，页面会清除内存及 DOM 中的代码；列表接口不能重新获取它，也不会将它写入 URL、日志或 localStorage。复制后的剪贴板与已下载文件由使用者自行保管、清理。
3. 查看设置的 pending / redeemed / expired / revoked 状态与已注册客户端、注册时间、最后在线时间，分别撤销设置或客户端凭据。撤销设置阻止兑换及重试；已注册客户端凭据需要单独撤销。

有效期只约束**首次兑换**：已兑换的同一安装实例在过期后仍可重试或续接，但不能借同一设置注册第二个客户端。该流程只安装与配置 Codex MCP，不执行 Agent 配对，不授予 FullAccess。

前端使用 `/api/admin/mcp/settings`、`/api/admin/mcp/setups`、`/api/admin/mcp/clients` 及相应的 `/revoke` 管理接口。演示模式只展示结构，不请求这些 API，不签发模拟凭据。生成及撤销采用提交锁；请求完成前关闭、导航或退出登录时，迟到响应不能重新打开弹窗或覆盖新页面。生成响应失败且结果不明确时，必须关闭弹窗并核对列表，不能直接重复签发。

验证：`node --test scripts/test-relay-admin.mjs`。测试全部使用不可用的本地合成数据，覆盖有效期与设置校验、HTML 转义、代码/文件一致、凭据清理、重复点击和过期异步响应。

### 可选：隔离浏览器冒烟测试（Windows / macOS / Linux）

测试入口为 [`scripts/test-mcp-admin-browser.cjs`](../scripts/test-mcp-admin-browser.cjs)，不会由普通 Node 单元测试自动启动。需要 Node.js 18+、外部工具环境中已安装的 Playwright，以及 Playwright 自带 Chromium 或兼容的本机 Chromium 浏览器；无需给仓库根目录新增依赖。

从仓库根目录显式执行：

```sh
node scripts/test-mcp-admin-browser.cjs
```

默认通过标准 Node 模块解析加载 `playwright`，使用它已安装的 Chromium。可选环境变量：

- `REMOTEOPS_PLAYWRIGHT_MODULE`：已安装的 Playwright 包目录或可解析模块名。未设置时使用 `playwright`；也可通过标准 `NODE_PATH` 配置外部模块搜索目录。
- `REMOTEOPS_BROWSER_EXECUTABLE`：要测试的本机 Chromium / Chrome / Edge 可执行文件完整路径。未设置时使用 Playwright 自带 Chromium。
- `REMOTEOPS_BROWSER_ARTIFACTS`：截图与合成下载文件的输出目录。未设置时在操作系统临时目录（`os.tmpdir()`）中新建唯一目录；完成后打印实际路径。

例如，PowerShell 中使用已有外部工具安装（路径请改成自己的）：

```powershell
$env:REMOTEOPS_PLAYWRIGHT_MODULE = 'C:\browser-tools\node_modules\playwright'
# 可选；如已安装 Playwright Chromium 则无需设置。
$env:REMOTEOPS_BROWSER_EXECUTABLE = 'C:\Program Files\Google\Chrome\Application\chrome.exe'
node scripts/test-mcp-admin-browser.cjs
```

测试拦截测试浏览器中的所有页面网络请求：页面文件来自当前仓库，所有同源管理 API 由脚本内合成数据响应，其他来源请求会被阻止。它不监听端口、不启动真实 Relay、不登录真实账户、不请求注册服务，也不读取或使用实际凭据。复制按钮通过页内模拟剪贴板验证，不修改操作系统剪贴板。下载的 `.remoteops-setup` **仅为不可用的测试样本，切勿用于安装**。

覆盖首次配置、1 / 24 / 168 小时设置、代码与下载 JSON 一致性、四种状态、设置和客户端撤销、重复提交、延迟生成与列表读取、导航与退出后的清理，以及亮色、暗色和 390px 窄屏截图。浏览器进程在失败或完成后都会关闭。此测试为模拟 API 的浏览器验收，不能替代临时真实 Relay 的端到端集成测试。若环境禁止浏览器启动，保留失败原因，在允许启动浏览器的测试环境运行，不视为已经通过。
