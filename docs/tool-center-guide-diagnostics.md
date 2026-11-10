# 工具中心适配、全局指引与权限验收

日期：2026-11-06。本轮仅修改源码、文档并本地验收；未安装现有组件、更新生产中继、发布 Y 盘或执行 Git 写操作。已有未提交工作保留。

## 修改摘要与文件

- `crates/remoteops-resources`：新增显式 `browser_adapter`，保留省略字段的旧行为；新增有界启动诊断、过滤、独立记录及保留期限。诊断可选且装箱，避免增大所有资源错误的栈尺寸。
- `crates/remoteops-tool-center`：`guide_markdown`、`set_guide`、数据库 1→2 迁移，复用修订、幂等、审计和权限。仅更新说明不撤销正在运行的工具。
- `apps/remoteops-agent`、`apps/remoteops-relay`、`apps/remoteops-controller-mcp`、`crates/remoteops-application`：能力门禁、说明同步及候选返回、诊断透传与相应测试。
- `relay-admin-prototype` 与前端测试：全局说明编辑、模板、安全展示、失败保留草稿；安装权限分类及手机端排列。
- `docs/resource-tools-v1.schema.json`、公开资源协议、独立 Skill 契约与浏览器示例：明确 CLI/MCP 路径注入差异，`operations` 只适用于通用 CLI。

源码版本：Agent `0.2.0-preview.34`、Relay `0.2.0-preview.14`、MCP `0.2.0-preview.23`、Skill `0.1.4`（最低 MCP `.23`）；线协议仍为 19。定制产品版本和已安装组件未升级。

## 验证命令与结果

- `cargo test -p remoteops-resources -p remoteops-tool-center -p remoteops-agent -p remoteops-relay -p remoteops-controller-mcp -p remoteops-application -p remoteops-enrollment-store --lib --bins`：491 项通过，4 项默认忽略。日志见 `output/tool-center-tests.log`。
- 末次 `cargo test -p remoteops-resources --lib`：25 项通过、1 项真实包测试默认忽略（已单独执行通过），覆盖新增的截断凭据行与不透明凭据过滤用例；结果见 `output/tool-center-diagnostics-tests.log`。
- `cargo test -p remoteops-resources real_candidates_headless_navigation_state_isolation_and_unknown_results -- --ignored --nocapture`：真实固定包通过；Playwright MCP 0.0.84、agent-browser CLI 0.41.2、agent-browser STDIO MCP 0.41.2 均通过本地页面导航、快照、表单操作、会话隔离、未知结果不重放和进程/profile 回收。新增 STDIO 路径另验证标题、URL、截图文件。
- `cargo check --workspace --all-targets`、相关五组件 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --all -- --check`：通过；检查日志在 `output/tool-center-workspace-check.log`、`output/tool-center-clippy.log`。
- `node --test tests/relay-admin-tools.test.cjs`：7 项通过；`node --test tests/mcp-tool-schemas/contract.test.mjs`：15 项通过，使用实际 MCP 工具发现 Schema。
- `node scripts/test-tool-center-guide-browser.cjs`：隔离本地 Relay 与 Chromium 通过；保存、刷新读取、UTF-8 限额、HTML 转义、授权与撤权均验证，无外部请求、无页面错误。环境通过 `REMOTEOPS_PLAYWRIGHT_MODULE` 指向已有 Playwright，未安装新依赖。
- 临时目录构建独立 Skill 后运行 `python scripts/Test-RemoteOpsSkill.py --package <临时包> --powershell pwsh --python-only`：22 项通过；只操作隔离测试目录，不修改现有安装或宿主配置。

## 截图复核

已实际打开并检查桌面 1440×900、手机 390×844 截图。全局说明按源文展示，编辑区及按钮正常换行；客户端权限在手机端纵向排列，无文档横向溢出。长内容在原后台内容区滚动。

- 桌面说明（本机归档 `output/playwright/tool-center-guide/desktop-guide.png`）
- 桌面编辑（本机归档 `output/playwright/tool-center-guide/desktop-guide-editor.png`）
- 手机说明内容（本机归档 `output/playwright/tool-center-guide/mobile-guide-content.png`）
- 手机编辑及操作（本机归档 `output/playwright/tool-center-guide/mobile-guide-editor-actions.png`）
- 桌面权限（本机归档 `output/playwright/tool-center-guide/desktop-permission.png`）
- 手机权限（本机归档 `output/playwright/tool-center-guide/mobile-permission.png`）

## 审查与剩余边界

主会话专项复核了旧登记兼容、显式适配门禁、目录修订及权限隔离、失败清理、输出上限、凭据过滤和说明的安全展示。子代理连续因 `SESSION_ID_CONFLICT` 无法启动，未执行独立子代理审查，不能将其记为通过。

未核对或修改实际生产网页、现场 Agent 和宿主加载状态；本地真实包测试不能替代报告所述现场二进制的精确失败归因，也不代表 macOS/Linux 或所有 Windows 版本已验证。stderr 为不可信自由文本，过滤不能识别所有可能的秘密，上游不得输出凭据；不回传原始 JSON-RPC 参数和结果。

默认忽略的真实包测试已单独执行通过；其余 3 项为原有原生桌面轻量探测和系统凭据库相关测试，本轮未运行。安装器与凭据存储实现未修改。

以后上线需先备份。工具数据库迁移后旧 Relay 无法直接读取格式 2，回滚需恢复完整备份；显式适配应配套新 Relay 和支持能力的 Agent，不能仅依赖协议号相同。全局说明初始为空，A/B/C 模板需由管理者按真实工具能力填写，不自动选择某个浏览器为首选。
