# 工具中心请求契约（协议 20）

本说明随独立 Skill `0.1.9` 携带，最低兼容 MCP 为 `0.2.0-preview.28`。显式浏览器适配与启动诊断需要 Agent `0.2.0-preview.34`，全局说明及能力检查需要 Relay `0.2.0-preview.14`，新登记约束需 Relay `0.2.0-preview.15`，可选 `launch_context` 需 Agent `0.2.0-preview.35`；旧登记读取保持兼容。线协议为 20。本说明不表示组件已安装或工具已上传、测试、启用。

## 请求与公开动作

浏览器路径注入、agent-browser MCP／CLI 最小配置及诊断字段见 [浏览器启动契约与失败诊断](browser-startup.md)。

`manage_relay_tools` 的参数是 `ToolCenterRequest`：

| 字段 | 类型与必填性 | 含义 |
| --- | --- | --- |
| `request_id` | 必填 string | 每次操作使用稳定唯一标识；只能含字母、数字、点、下划线、连字符或加号，最长 128 字节，不能是 `.`、`..` |
| `expected_revision` | 必填非负 integer（u64） | 最近 `snapshot` 返回的目录修订号；变更冲突须重新读取 |
| `command` | 必填 object | 按 `action` 选择下表分支；没有默认动作 |

| `command.action` | 除 `action` 外的字段 | 行为 |
| --- | --- | --- |
| `snapshot` | 无 | 读取策略、工具和审计；忽略修订号不一致 |
| `set_policy` | 必填 `enabled: boolean` | 设置全局运行开关 |
| `set_guide` | 必填 `guide_markdown: string` | 设置全局介绍与选择指南，最多 32 KiB UTF-8，不含 NUL；需要工具管理权，使用当前修订号 |
| `put_tool` | 必填 `tool: ManagedTool` | 新增或更新完整工具资料；已有版本和包不能删除，同版本同平台不能更换包内容 |
| `set_tool_enabled` | 必填 `tool_id: string`、`enabled: boolean` | 设置单工具运行开关 |
| `record_test` | 必填 `tool_id: string`、`version: string`、`sha256: string`、`record: PlatformTest` | 为已登记精确版本、包及平台追加真实测试记录 |
| `import_external` | 必填 `url: string`；可选 `expected_sha256: string/null`，默认 null | 临时下载 HTTPS ZIP，校验安全性并登记文件身份，不执行文件、不长期保存 ZIP |

`begin_upload`、`upload_chunk`、`finish_upload`、`cancel_upload` 是程序内部传输动作，不出现在通用管理输入 Schema 中，调用时被拒绝。托管 ZIP 统一调用 `upload_relay_tool_package`，其参数为必填 `local_path: string`（MCP 本机 transfer-root 内相对路径）和 `expected_revision: u64`。上传器按 256 KiB 分块确认，最大压缩包为 1 GiB。

管理权限由 Relay 为当前安装身份单独授予 `tool_management`；现场完全控制不授予该权限。管理请求不需要 Agent 会话。相同请求标识及相同内容可取得幂等结果；相同标识用于不同内容会拒绝。结果未知时先核对实际状态，不自动重放变更。

## 工具资料模型

下表“必填”指 JSON 字段必须出现；非空、大小、身份一致性、已校验包、权限和测试事实等限制仍由领域与服务逻辑检查。默认值只在字段省略时应用，不能把任意必填字段写成 null。

### `ManagedTool`、`ToolVersion` 与 `ToolPackage`

| 模型 | 字段 | 类型、默认与约束 |
| --- | --- | --- |
| `ManagedTool` | `id` | 必填 string；稳定标识，与每个 `ResourceTool.id` 一致 |
| | `name`、`purpose`、`instructions` | 必填 string；名称、用途、使用说明非空，名称最长 256 字节 |
| | `examples`、`dependencies` | string 数组；默认 `[]`，分别为调用示例与人工准备的依赖说明，不自动安装依赖 |
| | `enabled` | boolean，默认 false；服务忽略登记输入，新工具强制关闭，更新保留既有开关 |
| | `versions` | 必填 `ToolVersion[]`，1～128 项；完整列出已有版本，不支持删除 |
| `ToolVersion` | `version` | 必填 string；精确版本，不能是 `latest`，同工具内唯一 |
| | `packages` | 必填 `ToolPackage[]`，1～32 项 |
| `ToolPackage` | `tool` | 必填 `ResourceTool`；其版本与外层 `ToolVersion.version` 一致 |
| | `source` | 必填 `PackageSource` |
| | `file_verified` | boolean，默认 false；由服务根据先前真实导入／上传回执设置，不接受输入授予通过 |
| | `tests` | `PlatformTest[]`，默认 `[]`；服务保留适用的旧记录，新输入不能批量伪造测试事实，应使用 `record_test` |

稳定工具标识与版本沿用上述安全标识字符规则，不含路径。相同版本的包身份包含摘要及平台集合；修改同平台包内容应增加版本。单工具资料与全部目录还受 2 MiB 等现有服务限制。

`PackageSource` 是带标签对象：

- 托管包：`{"kind":"hosted"}`，不需要 URL；先完成上传。
- 外部包：`{"kind":"external","url":"https://实际来源/实际包.zip"}`，`url` 必填。先用 `import_external` 校验同一包身份；允许签名查询参数，禁止用户信息、片段和非 HTTPS。中继下载不跟随重定向。

列表与公开回执去掉外链查询和片段。服务会按已校验摘要和基址恢复已导入的完整签名地址；签名地址变化时需重新导入。不要把外链凭据放进使用说明、操作示例或结果文本。

### `ResourceTool`

| 字段 | 类型、默认与含义 |
| --- | --- |
| `id`、`version` | 必填 string；与外层工具／版本一致 |
| `documentation` | 可省略 `ToolDocumentation`，默认各字符串 `""`、各数组 `[]`；向 Agent 发布目录时由外层 `ManagedTool` 资料覆盖 |
| `description` | 必填非空 string，简短用途 |
| `capabilities` | 必填非空 string 数组；能力标签不授予权限 |
| `documentation_url` | 必填非空 string；HTTPS 或相对说明地址；不要放令牌 |
| `publisher`、`license` | 必填非空 string；发布者与许可证说明 |
| `limitations` | 必填 string 数组，允许 `[]` |
| `release_notes` | 必填 string，当前版本变更说明 |
| `permissions` | 必填非空 string 数组；权限说明不绕过本地权限模式 |
| `requires_desktop` | 必填 boolean；是否必须有真实交互桌面 |
| `platforms` | 必填非空 `Platform[]` |
| `package` | 必填 `ResourcePackage` |
| `startup` | 必填 `Startup` |
| `health_check` | 必填 `HealthCheck` |
| `timeout_secs` | 必填非负 integer（u64），领域限制为 1～300 秒 |

`ToolDocumentation` 字段为 `name`、`purpose`、`instructions`（string）及 `examples`、`dependencies`（string 数组）。对象内各字段可省略，默认分别为空字符串和空数组。

### 平台、包与启动

| 模型 | 字段 | 类型、默认与含义 |
| --- | --- | --- |
| `Platform` | `os`、`arch` | 必填 string；使用 Rust 名称，例如 `windows`、`linux`、`macos` 及 `x86_64`、`aarch64`；不能用 `x64` 代替 `x86_64` |
| | `os_versions` | 必填非空 string；实际系统范围说明 |
| | `status` | 必填 string；`passed`、`pending`、`unsupported`，登记时服务先置 pending，再应用真实记录 |
| | `min_windows_build`、`max_windows_build` | 可选非负 integer（u32）或 null；默认 null。Windows 运行候选必须有已验证的最小／最大 build，且当前 build 在闭区间内 |
| `ResourcePackage` | `url` | 必填 string；上传／导入后服务按来源覆盖：托管为 `relay://packages/<sha256>.zip`，外部为已校验 HTTPS 地址 |
| | `size` | 必填非负 integer（u64）；精确 ZIP 字节数，必须大于 0 且不超过 1 GiB，与回执一致 |
| | `sha256` | 必填 string；64 位小写十六进制，必须与回执一致 |
| | `format` | 必填 string；唯一支持值 `zip`，这是压缩格式 |
| `Startup` | `interface` | 必填 `ExecutionInterface`；`stdio_mcp`、`agent_browser_cli`、`cli`，这是调用协议，不能填 `zip` |
| | `executable` | 必填 string；包内相对原生程序路径；Windows 必须是 `.exe`，禁止批处理的隐式 Shell |
| | `args` | 必填 string 数组；固定 argv，不经 Shell；只支持 `${package}`、`${run}`、`${profile}` 路径替换 |
| | `browser_executable` | string，默认 `""`；包内浏览器路径，`agent_browser_cli` 必须非空，Windows 为 `.exe` |
| | `browser_adapter` | 可选 `none`、`playwright`、`agent_browser` 或 null；仅用于 `stdio_mcp`；省略/null 时沿用旧规则，详见下文 |
| | `operations` | `CliOperation[]`，默认 `[]`；`cli` 接口必须有 1～256 个命名操作 |
| `HealthCheck` | `method` | 必填 string；`stdio_mcp` 对应 `tools/list`，`agent_browser_cli` 对应 `version`，`cli` 对应 `schema` |
| | `timeout_secs` | 必填非负 integer（u64），领域限制为 1～120 秒；初始化和健康检查共享此超时 |

包内路径禁止绝对路径、反斜杠、冒号、`..`、链接逃逸及 Windows 保留名等不安全形式。不要把系统安装目录当成包内路径。声明平台名称不会产生实际测试证据；未知系统、架构或未经验证的 Windows build 不得声称可运行。

### CLI 操作与参数映射

`operations` 仅用于 `interface="cli"`。`agent_browser_cli` 忽略它并提供固定的 `browser_open(url)`、`browser_snapshot()`、`browser_click(ref)`、`browser_fill(ref,text)`、`browser_eval(script)`、`browser_close()`，不执行 MCP 握手。

`CliOperation` 含必填 `name: string`（稳定且唯一）、`description: string`、`input_schema: JSON`、`args: CliArgument[]`；可选 `executable: string/null` 默认 null，继承 `Startup.executable`；`result_format: CliResultFormat` 默认 `text`，可选 `text` 或 `json`。Windows 操作覆盖程序也必须是包内 `.exe`。

`input_schema` 必须为根类型 object 的有效 JSON Schema，不能含 `$ref`、`$dynamicRef` 或 `$recursiveRef`。`CliArgument` 按 `kind` 分支：`literal` 必填固定 `value: string`；`parameter` 必填顶层字段名 `name: string`，字段必须在 `input_schema.properties` 中声明。映射字段运行时必须存在且为 string、boolean 或 number；不能直接映射 null、数组或对象。参数逐项作为 argv 传递，用户文本不再解释为 Shell 或路径占位模板。

下面是可放入 `startup.operations` 的操作定义；调用输入 `{"text":"hello world"}` 产生两个参数 `--text` 和 `hello world`：

```json
{
  "name": "echo",
  "description": "回显指定文本",
  "executable": null,
  "input_schema": {
    "type": "object",
    "properties": {"text": {"type": "string"}},
    "required": ["text"],
    "additionalProperties": false
  },
  "args": [
    {"kind": "literal", "value": "--text"},
    {"kind": "parameter", "name": "text"}
  ],
  "result_format": "text"
}
```

## 测试记录与服务维护字段

`PlatformTest` 既用于快照中的 `tests[]`，也用于 `record_test.record`：

| 字段 | 类型、默认与约束 |
| --- | --- |
| `os`、`arch` | 必填 string，与已登记包中的平台组合一致 |
| `environment` | 必填非空 string，实际系统、依赖、桌面／headless 模式等，最长 8192 字节 |
| `desktop` | 必填 boolean；是否真实交互桌面 |
| `windows_build` | 可选 u32/null，默认 null；Windows 的 passed 记录必须提供真实 build |
| `status` | 必填 string：`passed`、`failed`、`pending` |
| `result` | 必填非空 string，真实步骤与观察结果，最长 32768 字节 |
| `tested_at` | 必填 RFC 3339 时间 string；必须替换为实际测试时间 |
| `recorded_by` | string，默认 `""`；服务始终用真实管理操作者覆盖 |

需要桌面的工具不能用 `desktop=false` 的记录标记通过。服务使用该平台最后追加的记录：passed 可提升平台状态，否则平台保持 pending；Windows passed 将最小与最大 build 都设为本次实际 build，不推断其他 build 通过。每个包最多 256 条记录。

登记会根据真实包回执设置 `file_verified`，保留适用旧测试，忽略输入伪造的 `tests` 和开关；更改启动契约或桌面要求会清空不再适用的旧测试。向 Agent 发布时，`documentation` 来自外层工具资料。全局开关和新工具均默认关闭；仅 `set_policy`、`set_tool_enabled` 修改相应开关。目录修订、审计、操作者、包摘要校验和运行权限由服务负责。

管理成功返回 `ToolCenterResponse`：`request_id: string`、`revision: u64`、`error: null`、`data: JSON`。领域失败的 `error` 为 `ToolCenterError`（`code`、固定脱敏 `message`），`data` 通常为 null。上传／导入的 `data` 是 `PackageReceipt`（`size`、`sha256`、`file_verified`、`source`），仅证明文件验证成功；登记／开关／测试回执不表示 Agent 已同步或执行。

`snapshot.data` 是 `ToolCenterSnapshot`：`revision`、`enabled`、`guide_markdown`、`tools: ManagedTool[]`、`audit: ToolAudit[]`。`ToolAudit` 含 `request_id`、`actor`、`action`、`result`、`revision`、`created_at`，由服务维护。认证连接内部的 `ToolCatalog` 同样包含 `guide_markdown`；缺省为空。`list_resource_tools.details.guide_markdown` 与候选来自同一快照，普通使用者可读取。修改指引不终止仍被允许的实例；指引不扩大权限，不允许结果未知时自动重放。独立资源文件的 `ResourceIndex` 仍为 `schema_version` 与 `tools`；旧本地 `ResourceConfig` 不是集中工具中心入口。

## 完整 JSON 示例与替换要求

以下文件是 `manage_relay_tools` 的完整参数对象，覆盖外层请求、工具资料、版本、包、平台、启动与健康检查，可直接读取其 JSON 结构：

- [外部 HTTPS 包登记](examples/put-tool-external.json)
- [中继托管包登记](examples/put-tool-hosted.json)
- [追加平台测试记录](examples/record-test.json)

示例的 `.invalid` 域名、`123456` 字节、64 个 `a` 的 SHA-256、程序路径、发布者、许可证、用途、依赖与权限都是占位，不是可用包或实际验证事实。使用前必须替换为实际工具资料。`request_id` 使用本次操作唯一标识，`expected_revision` 使用最新快照。`id`、内外层 `version`、包的精确摘要及对应平台必须保持一致。

外部流程先调用 `import_external`（例如 `{"action":"import_external","url":"https://实际来源/实际包.zip","expected_sha256":"实际64位小写摘要"}`），再将回执的真实大小、摘要和已导入来源写入 external 示例。托管流程先把实际 ZIP 放入 MCP 本机 transfer-root，调用 `upload_relay_tool_package`，再将真实回执填入 hosted 示例，来源为 `{"kind":"hosted"}`。实际导入／上传完成后重新核对修订，再登记资料。

`record-test.json` 的 pending 只表示未完成测试，不宣称通过。使用时替换为实际环境、步骤、观察结果与时间；只有真实测试通过才可改为 passed，Windows 必须填真实 `windows_build`，需要桌面的工具还必须实际使用交互桌面。`recorded_by` 由服务填写。签名 URL、令牌等秘密不能复制到测试结果。

各阶段分别验收：导入／上传证明文件身份和 ZIP 校验；`put_tool` 登记说明及执行契约；`record_test` 记录精确环境的事实；单工具与全局启用分别控制运行；Agent 目录同步、初始化和实际调用还需单独核对。没有真实执行证据时，不把 Schema 通过、登记成功或上传成功写成可运行。

## 本地字段诊断

MCP `.22` 先按共享模型生成的输入 Schema 校验，再转为领域命令。它按 `action`、`source.kind` 等判别字段选择分支，避免只返回联合分支不匹配。输入校验失败保留 MCP `isError=true`，文本为含以下字段的 JSON，例如：

```json
{
  "code": "invalid_tool_center_command",
  "message": "工具管理命令不符合输入契约",
  "details": {
    "field": "command.tool.versions[0].packages[0].tool.startup.interface",
    "expected": "合法取值 [\"stdio_mcp\",\"agent_browser_cli\",\"cli\"]"
  }
}
```

`details.field` 只使用契约内已知字段与数组索引，`details.expected` 表达所需类型或合法取值；具体措辞以运行版本为准。缺少名称／版本／平台字段、嵌套类型错误、无效接口／来源／包格式和空 record 均应定位到对应字段。诊断不回显输入值、任意用户键名、签名 URL、令牌或原始异常。此校验不能代替后续服务对权限、已校验包、摘要、平台事实和启用状态的判断。
