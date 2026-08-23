# RemoteOps Relay Admin - 原型运行与交互指南

本项目是基于 [`原型设计需求.md`](./%E5%8E%9F%E5%9E%8B%E8%AE%BE%E8%AE%A1%E9%9C%80%E6%B1%82.md) 构建的内部运维管理后台界面，同时可直接连接 Relay 管理 API。

---

## 🚀 快速启动与预览

本页面采用原生 Web 技术栈构建（HTML5 + CSS3 + Vanilla JavaScript）。生产使用时由 Relay 在 `18081` 端口提供页面和 API：

### 本地静态预览

仅查看界面时可以使用本地静态 HTTP 服务：
```bash
cd /Users/schnee/Project/RemoteOps/relay-admin-prototype
python3 -m http.server 8080
```
然后在浏览器访问：`http://localhost:8080`

静态预览需要由同源 Relay 管理服务提供登录接口；生产页面使用管理员用户名和密码登录，页面不会把密码或 Token 写入浏览器持久化存储。

---

## 🧭 交付画面与核心交互验证清单

### 1. 登录页 (`/login`)
- **交互验证**：
  - 点击右上角退出登录图标即可回到登录页。
  - 支持切换管理 Token 的「显示/隐藏」状态。
  - 输入管理员用户名和密码，页面会调用 `/api/admin/login`，成功后使用 HttpOnly Cookie 访问管理 API。
  - 登录后进入“安全设置”可修改管理页面密码；修改成功后所有登录 Session 失效，需要使用新密码重新登录。

### 2. 概览页 (`/overview`)
- **核心要素**：
  - 顶部 4 大指标卡（在线 Agent、活跃会话、已连接 Controller、待处理审批）。
  - Relay 详细运行状态卡片（Uptime、业务 TLS 端口 `:7443`、管理端口 `:18081`、连接数利用率）。
  - 实时事件日志流水（Realtime Events）。
  - 活跃会话预览表与快捷操作。

### 3. 会话管理核心页 (`/sessions`)
- **表格与筛选**：
  - 支持按关键词搜索、状态筛选、Controller 类型筛选、权限模式筛选。
  - 点击任意 Session ID 或 Owner UUID 单元格，可触发微提示（Toast）并将内容复制至剪贴板。
  - 点击表格行或「详情」按钮，平滑滑出 **Session 详情抽屉**。

### 4. Session 详情抽屉 (`Session Detail Drawer`)
- **功能点**：
  - 展示完整的 Session ID、Agent 主机名、操作系统、能力列表（Capabilities）、连接代次与租约倒计时。
  - 展示已连接的 AI Controller (MCP)、Human Controller 状态与统一 Owner UUID。
  - 底部专属 **Danger Zone**：提供「关闭会话」与高危「🛑 紧急停止」操作。

### 5. 危险操作确认弹窗 (`Modals`)
- **关闭会话弹窗**：明确展示目标 Agent 与 Session，说明将切断 Controller 控制权。
- **紧急停止弹窗 (STOP 防误触机制)**：强制要求管理员手动输入 `STOP` 才能解锁红色确认按钮，模拟向 Agent 发送紧急停止请求。

### 6. Agent 节点管理页 (`/agents`)
- **脱敏安全设计**：控制码状态展示为 `控制码已生成，租约剩余 08:42`，不向管理员默认暴露明文控制码。
- 展示 Agent 在线状态、主机名、操作系统、历史配对次数与最近心跳。

### 7. Relay 身份与凭据页 (`/identity`)
- **Owner UUID 管理**：展示完整 UUID、脱敏摘要与架构说明（AI/Human Controller 统一身份）。
- **AI Controller Token 管理**：展示 SHA-256 指纹与轮换时间，点击「轮换 Token」可弹出一次性展示新凭据的密匙弹窗（One-time Secret Modal）。
- **Human Controller Token**：优雅展示「未启用 / 暂不管理」阶段性占位。

### 8. 审计日志页 (`/audit`)
- 包含时间戳、操作者、操作类型、目标对象、执行结果与来源 IP。
- 支持按操作类型与结果筛选，并支持一键**导出审计日志为 CSV 文件**。

---

## ⚡ 场景切换器 (Scenario Switcher)

页面顶部保留调试栏，可一键切换以下 8 种系统状态画面：

1. **✅ 正常运行 (Normal State)**：所有服务与指标正常运作。
2. **⚠️ Relay 性能降级 (Degraded)**：Relay 连接数高负载状态。
3. **❌ Relay 离线 (Offline / Disconnected)**：Relay 节点断开连接。
4. **📭 无活跃 Session (No Active Sessions)**：空会话占位图与提示。
5. **🤖 Relay 无 Agent 注册 (No Agents)**：无 Agent 接入的空状态。
6. **🔍 筛选无匹配结果 (Filter Empty)**：搜索或筛选无结果的友好提示。
7. **⏳ API 请求中 (Loading Skeleton)**：骨架屏加载动效。
8. **💥 API 请求失败 500 (Network / API Error)**：服务异常与带「重试连接」按钮的错误页。

---

## 🎨 设计系统与组件文档

详细的设计规范（颜色 Token、字体阶梯、间距、状态色彩矩阵、响应式断点）请参阅同目录下的 [`DESIGN_SYSTEM.md`](./DESIGN_SYSTEM.md)。
