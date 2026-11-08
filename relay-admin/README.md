# Relay 管理前端

Vue 3、TypeScript、Vite 与 Vue Router 构建的独立管理前端。生产源码位于本目录，`relay-admin-prototype` 仅保留历史原型与旧回归材料，不再编译或发布。

## 开发与验证

需要 Node.js 22.12+ 或 24 LTS。后端默认监听本机 `18081`，也可通过 `REMOTEOPS_ADMIN_API` 指定本机隔离实例。

```powershell
cd relay-admin
npm ci --ignore-scripts
npm run dev
npm test
npm run build
npm run format:check
```

`src/views` 保存各管理页面，`src/components` 保存弹窗、详情及表格；`src/models.ts` 和 `src/console-types.ts` 定义界面契约。`src/services` 按状态、通用操作、MCP 接入及工具目录分组，保留经过回归的 JavaScript 业务操作；Vue 组件、路由及构建配置使用 TypeScript。没有旧页面的 HTML 注入、内联事件或运行时模板编译。

## 独立交付

```powershell
.\scripts\Build-RelayAdmin.ps1 -OutputDirectory C:\Temp\RemoteOps-Admin-0.1.1
```

在仓库根目录执行，生成静态文件、版本、锁文件、许可证及逐文件摘要。该脚本不运行 Cargo，不操作 Agent、MCP、Skill 或生产服务。已有输出目录会拒绝覆盖。

生产使用 `deploy/relay-admin/Dockerfile` 的独立镜像；Nginx 以非 root 用户运行，根文件系统只读，只有 `/tmp` 可写。页面和 `/api/*` 共用访问地址，通过内部网络转发 API，继续使用 HttpOnly Cookie。HTML 不缓存，带摘要的静态资源长期缓存。Relay 后端不再携带 HTML、JS、CSS，后端根路径返回 404。

前端 `0.1.1` 需要 Relay `0.2.0-preview.17` 的节点同步状态接口。后续前后端可分别构建、发布和回退，需核对对应 API 最低版本。首次由单容器迁移为前后端两个容器；业务 TLS 端口、数据卷和管理入口保持原值。

## 浏览器回归

在仓库根目录运行 `scripts/test-relay-vue-browser.cjs`。脚本启动隔离 Relay 和前端预览，使用随机测试身份与临时数据，执行真实登录、目录写入、ZIP 上传和 MCP 接入操作，不连接现场 Agent。`REMOTEOPS_PLAYWRIGHT_MODULE` 可指向现有 Playwright 安装；截图默认保存在 `output/playwright/relay-vue`。

节点多行状态由 266 条展示夹具覆盖；实际节点分类由 Relay Rust 测试覆盖。截图不包含一次性设置代码，不使用真实路由器或生产密码。
