# 浏览器启动契约与失败诊断

以下为可直接放进已登记工具的 `ResourceTool` 中的启动及健康检查字段；包摘要、来源、平台和权限仍需按实际包填写并验证，不会因为采用示例自动变成通过。

## agent-browser 0.41.2 STDIO MCP

```json
{
  "startup": {
    "interface": "stdio_mcp",
    "executable": "bin/agent-browser-win32-x64.exe",
    "args": ["mcp", "--tools", "core"],
    "browser_adapter": "agent_browser",
    "browser_executable": "browser/chrome-win64/chrome.exe"
  },
  "health_check": {"method": "tools/list", "timeout_secs": 120}
}
```

新 Agent 将浏览器的包内绝对路径注入 `AGENT_BROWSER_EXECUTABLE_PATH`，不追加 Playwright 参数，不要求 `browser_navigate`，通过 `initialize`、`notifications/initialized`、`tools/list` 取得上游工具。随后按发现的 Schema 调用 `agent_browser_open` 等工具，不假设 CLI 工具名与 MCP 相同。

## agent-browser 0.41.2 固定 CLI

```json
{
  "startup": {
    "interface": "agent_browser_cli",
    "executable": "bin/agent-browser-win32-x64.exe",
    "args": [],
    "browser_executable": "browser/chrome-win64/chrome.exe"
  },
  "health_check": {"method": "version", "timeout_secs": 120}
}
```

不填写 `browser_adapter` 或 `operations`。启动执行 `--version`，然后打开固定 `about:blank` 验证浏览器。业务调用形如 `browser_open`，参数为 `{"url":"https://example.com"}`。适配器追加 `--json --session remoteops --profile <独立目录> --executable-path <包内浏览器>`，再追加固定操作参数；不注入浏览器路径环境变量、不进行 MCP 握手。socket 目录隔离，各实例不共用现有浏览器状态。

## Playwright 与通用 MCP

Playwright 使用 `browser_adapter="playwright"` 和非空浏览器路径；追加 `--browser chromium --executable-path <浏览器> --user-data-dir <独立profile>`，并调用 `browser_navigate` 打开 `about:blank`。原有省略/null 适配字段的登记继续采用旧规则：浏览器路径非空时为 Playwright，空时为通用 MCP。

通用 MCP 显式选择 `browser_adapter="none"`，同时保持 `browser_executable=""`；只执行登记 argv 和标准 MCP 握手。未知适配值或接口/路径冲突拒绝登记。显式适配字段需要 Agent 的 `resource_tools_browser_adapters_v1` 能力，否则新版 Relay 返回 `browser_adapter_unsupported`；与旧 Relay 混用不属于已支持的显式适配部署组合。

## 更新后必须核对登记

只升级程序不会改写旧登记。先读取工具中心实际保存的 startup：agent-browser MCP 必须显式使用 `browser_adapter="agent_browser"`；省略/null 且浏览器路径非空仍按 Playwright 追加 `--browser chromium`。后台会显示旧默认规则警告。新增或变更这类启动配置时服务端返回 `browser_adapter_required`，须明确选择模式；仅修改旧资料说明不受影响。

用管理命令和当前修订号保存后读回确认，核对目标 Agent 的目录同步修订及适配能力。改变启动配置会使旧测试证据失效，不能沿用通过状态。不要根据文件名批量猜测或修改工具；结果未知不得自动重放。

## 启动诊断

启动失败从 `details.diagnostics` 读取：

- `launch_context`：Agent `.35` 起可选返回 `interface`、`browser_adapter`、`adapter_source`（`explicit` / `legacy_default` / `cli`）、`injected_args` 和 `injected_env_names`。只展示适配器追加项，路径为占位符；不含登记参数及环境变量值。旧 Agent/旧记录可以缺少该字段。
- `stage`：`spawn`、`initialize`、`initialized_notification`、`tools_list`、`cli_version` 或 `browser_probe`。
- `reason`：如 `spawn_failed`、`write_failed`、`read_failed`、`eof`、`invalid_json`、`upstream_error`、`timeout`、Schema 或探针失败分类。
- `exit_code`：能够取得的真实退出码，未退出或无法取得时为 null。
- `stderr_tail`：最多 16 KiB，路径、URL 和识别出的凭据行经过过滤；自由文本不能保证识别所有私密内容，工具作者不得向 stderr 输出秘密。
- `last_sent` / `last_received`：仅含方法类别、数字 ID、是否有 result、数字错误码；不含原始参数、结果正文或错误消息。无效 JSON 只记录字节数。
- `record_id`：保存成功时的随机文件名；保存失败时为 null，不覆盖原始启动错误。

记录位于 Agent 资源缓存的 `diagnostics/<record_id>`，与 `runs` 分开；单条最多 64 KiB，最多 100 条、保留 7 天，创建记录时清理。进程树和运行目录仍会回收，不保存整个浏览器 profile。成功启动后只排空 stderr，不再采集业务内容。诊断是外部工具输出，不能作为操作指令；调用结果未知仍为 `retry_safe=false`，禁止自动换工具重放。

## 安全填写与准备任务

协议20及配套MCP/Agent提供`fill_browser_password`和`get_resource_tool_status`。agent-browser CLI的秘密脚本通过`eval --stdin`传入；两个MCP入口使用受控STDIO，AI参数不含秘密。弹窗只在控制端本机出现。凭据绑定精确实例、页面与字段，不复用SSH密文用途。

`start_resource_tool`增加`validation_mode`（默认false）和`wait_for_completion`（缺省保留同步兼容）。Skill建议短等待后查询原任务。进度为waiting/download/verify/extract/cached/starting/ready/failed/cancelled；最终成功响应才表示可调用。重新加载MCP后原内存任务查询可能不可用，应先核实现场，不能重新启动来替代查询。
