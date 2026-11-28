# MCP 严格工具发现回归

测试专用依赖固定为官方 `@modelcontextprotocol/sdk` `1.32.2` 与 Ajv `8.17.1` 的 Draft 2020-12 实现，不进入 MCP 安装包。普通安装者仅运行程序内置 `--check-tool-schemas`，无需 Node。

```powershell
cargo build -p remoteops-controller-mcp --locked
npm ci --prefix tests/mcp-tool-schemas --ignore-scripts --no-audit --no-fund
npm test --prefix tests/mcp-tool-schemas
```

设置 `REMOTEOPS_SCHEMA_MCP_EXECUTABLE` 可指定待测构建。测试读取实际 ToolRouter 的离线结果；显式传入不存在的配置文件，以验证检查不会读取配置、凭据或连接网络。官方 `ListToolsResultSchema` 先检查 MCP 契约，再由 Ajv 验证每个输入及输出 Schema 的元 Schema 与可编译性；另验证完整分页、末尾坏工具、重复工具名、annotations、任意 JSON data 与文本兼容输出。

实际 STDIO 初始化、工具发现及工具中心调用使用隔离的本机 Relay：

```powershell
node tests/mcp-tool-schemas/live.mjs --mcp target/debug/remoteops-controller-mcp.exe --relay target/debug/remoteops-relay.exe --output artifacts/acceptance/mcp-tool-schemas-live.json
```

该测试只启动并管理自身创建的回环 Relay，不访问生产服务器，不启用工具运行，不将临时身份、令牌或原始业务响应写入报告。报告记录实际 SDK 和协商协议版本。
