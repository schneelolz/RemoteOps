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
