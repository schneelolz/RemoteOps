use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::{RESOURCE_TOOLS_SCHEMA_VERSION, ResourceError, Result};

/// 本地配置是信任根；清单不能扩大下载来源范围。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourceConfig {
    /// 完整 HTTPS 清单 URL；缺省关闭。
    pub index_url: Option<String>,
    /// 允许的 HTTPS 来源（协议、主机和端口，不含路径）。
    pub allowed_origins: Vec<String>,
}

/// 公开资源清单。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceIndex {
    /// 独立资源协议主版本。
    pub schema_version: u32,
    /// 由发布者维护的工具版本列表。
    pub tools: Vec<ResourceTool>,
}

/// 固定版本的资源工具及执行契约。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceTool {
    /// 稳定工具标识。
    pub id: String,
    /// 中继登记的用途与使用说明，不授予执行权限。
    #[serde(default)]
    pub documentation: ToolDocumentation,
    /// 精确版本，不支持 latest。
    pub version: String,
    /// 中文或英文用途说明。
    pub description: String,
    /// 能力标签，仅用于筛选，不授予权限。
    pub capabilities: Vec<String>,
    /// 使用说明 URL，可相对于清单。
    pub documentation_url: String,
    /// 发布者名称。
    pub publisher: String,
    /// SPDX 许可证标识或发布者组合说明。
    pub license: String,
    /// 已知限制。
    pub limitations: Vec<String>,
    /// 当前版本变更说明。
    pub release_notes: String,
    /// 所需权限的说明，不能绕过本地权限模式。
    pub permissions: Vec<String>,
    /// 是否需要交互桌面。
    pub requires_desktop: bool,
    /// 支持的平台及经验证状态。
    pub platforms: Vec<Platform>,
    /// 中继维护的精确平台测试证据；不由包声明授予通过状态。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platform_tests: Vec<crate::PlatformEvidence>,
    /// 固定包信息。
    pub package: ResourcePackage,
    /// 启动契约。
    pub startup: Startup,
    /// 启动健康检查。
    pub health_check: HealthCheck,
    /// 单次调用超时，单位秒。
    #[cfg_attr(
        feature = "json-schema",
        schemars(extend("maximum" = 18_446_744_073_709_551_615_u64))
    )]
    pub timeout_secs: u64,
}

/// 工具中心向 Agent 及 AI 展示的说明。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolDocumentation {
    /// 显示名称。
    pub name: String,
    /// 适用用途。
    pub purpose: String,
    /// 使用说明。
    pub instructions: String,
    /// 调用示例。
    pub examples: Vec<String>,
    /// 依赖与准备条件。
    pub dependencies: Vec<String>,
}

/// 精确平台组合；待测组合不会自动作为可执行候选。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Platform {
    /// Rust 平台名称，例如 windows、linux。
    pub os: String,
    /// Rust 架构名称，例如 `x86_64`。
    pub arch: String,
    /// 操作系统范围的人类可读说明。
    pub os_versions: String,
    /// passed、pending 或 unsupported。
    #[cfg_attr(
        feature = "json-schema",
        schemars(extend("enum" = ["passed", "pending", "unsupported"]))
    )]
    pub status: String,
    /// 已验证 Windows 最低内部版本；Windows 的通过组合必须提供。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "json-schema",
        schemars(extend("maximum" = 4_294_967_295_u64))
    )]
    pub min_windows_build: Option<u32>,
    /// 已验证 Windows 最高内部版本，禁止将未验证的新系统自动视为通过。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "json-schema",
        schemars(extend("maximum" = 4_294_967_295_u64))
    )]
    pub max_windows_build: Option<u32>,
}

/// 下载包身份；大小和摘要由发布脚本生成。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourcePackage {
    /// HTTPS 或相对下载地址。
    pub url: String,
    /// 精确压缩包字节数。
    #[cfg_attr(
        feature = "json-schema",
        schemars(extend("maximum" = 18_446_744_073_709_551_615_u64))
    )]
    pub size: u64,
    /// SHA-256 小写十六进制。
    pub sha256: String,
    /// 首版仅支持 zip。
    #[cfg_attr(feature = "json-schema", schemars(extend("enum" = ["zip"])))]
    pub format: String,
}

/// 受支持的固定调用接口。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionInterface {
    /// 通用逐行 JSON-RPC STDIO MCP；可选浏览器适配。
    StdioMcp,
    /// agent-browser 的 JSON CLI。
    AgentBrowserCli,
    /// 按登记操作逐次运行的原生 CLI。
    Cli,
}

/// 显式浏览器适配；省略时沿用旧清单的 Playwright 兼容行为。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAdapter {
    /// 标准 MCP，不注入浏览器配置。
    None,
    /// Playwright 参数与独立浏览器探针。
    Playwright,
    /// agent-browser MCP 的浏览器路径环境变量。
    AgentBrowser,
}

/// 不经 Shell 的可执行文件及参数数组。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Startup {
    /// 支持 `stdio_mcp`、`agent_browser_cli` 与 `cli`。
    pub interface: ExecutionInterface,
    /// 包内相对可执行文件路径。
    pub executable: String,
    /// 固定参数；只支持 ${package}、${run} 和 ${profile} 路径替换。
    pub args: Vec<String>,
    /// 包内浏览器路径，交给适配器而非用户覆盖。
    #[serde(default)]
    pub browser_executable: String,
    /// 显式 STDIO MCP 浏览器适配，需要新版 Agent 能力声明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_adapter: Option<BrowserAdapter>,
    /// 通用 CLI 的命名操作及参数映射。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<CliOperation>,
}

/// 通用 CLI 的单次操作定义。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliOperation {
    /// 对调用方公开的稳定名称。
    pub name: String,
    /// 操作用途。
    pub description: String,
    /// 可选包内原生程序；省略时继承工具启动文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    /// 调用参数 JSON Schema，不解析外部引用。
    #[cfg_attr(
        feature = "json-schema",
        schemars(schema_with = "input_schema_contract")
    )]
    pub input_schema: serde_json::Value,
    /// 逐项传递给原生进程的参数映射。
    pub args: Vec<CliArgument>,
    /// stdout 的解析方式。
    #[serde(default)]
    pub result_format: CliResultFormat,
}

/// 单个进程参数；用户值不会解释成 Shell 或占位模板。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CliArgument {
    /// 登记者提供的固定参数。
    Literal {
        /// 固定文本。
        value: String,
    },
    /// 取已通过 Schema 验证的顶层字段作为一个参数。
    Parameter {
        /// 字段名称。
        name: String,
    },
}

/// CLI 标准输出格式。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliResultFormat {
    /// UTF-8 文本。
    #[default]
    Text,
    /// 单个 JSON 值。
    Json,
}

/// 有界健康检查。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheck {
    /// `stdio_mcp` 使用 tools/list；agent-browser 使用 version；CLI 使用 schema。
    #[cfg_attr(
        feature = "json-schema",
        schemars(extend("enum" = ["tools/list", "version", "schema"]))
    )]
    pub method: String,
    /// 初始化和健康检查共享超时，单位秒。
    #[cfg_attr(
        feature = "json-schema",
        schemars(extend("maximum" = 18_446_744_073_709_551_615_u64))
    )]
    pub timeout_secs: u64,
}

// 任意 JSON Schema 使用对象形式表达，兼容 MCP SDK 的属性契约。
#[cfg(feature = "json-schema")]
fn input_schema_contract(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({})
}

impl ResourceConfig {
    /// 验证配置；关闭状态允许空白名单，开启时必须明确允许清单来源。
    ///
    /// # Errors
    /// 拒绝无效清单入口、非 HTTPS 及白名单外来源。
    pub fn validate(&self) -> Result<()> {
        if self
            .index_url
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            return Ok(());
        }
        if self
            .allowed_origins
            .iter()
            .any(|value| origin(value).is_none())
        {
            return Err(ResourceError::before(
                "invalid_config",
                "来源白名单必须使用不含路径、用户信息或查询的 HTTPS 来源",
            ));
        }
        self.index().map(|_| ())
    }

    pub(crate) fn index(&self) -> Result<Url> {
        let value = self
            .index_url
            .as_deref()
            .filter(|x| !x.trim().is_empty())
            .ok_or_else(|| ResourceError::before("disabled", "尚未配置资源清单地址"))?;
        let url = Url::parse(value)
            .map_err(|_| ResourceError::before("invalid_config", "清单 URL 无效"))?;
        if url.path() != "/remoteops/v1/index.json"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ResourceError::before(
                "invalid_config",
                "清单入口必须为 /remoteops/v1/index.json",
            ));
        }
        self.check_url(&url)?;
        Ok(url)
    }

    pub(crate) fn check_url(&self, url: &Url) -> Result<()> {
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(ResourceError::before(
                "source_denied",
                "仅允许无用户信息的 HTTPS 来源",
            ));
        }
        let allowed = self
            .allowed_origins
            .iter()
            .any(|value| origin(value).is_some_and(|origin| origin == url.origin()));
        if !allowed {
            return Err(ResourceError::before(
                "source_denied",
                "下载来源未在本地 HTTPS 白名单中",
            ));
        }
        Ok(())
    }
}

fn origin(value: &str) -> Option<url::Origin> {
    Url::parse(value)
        .ok()
        .filter(|url| {
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
                && url.username().is_empty()
                && url.password().is_none()
        })
        .map(|url| url.origin())
}

impl ResourceTool {
    /// 当前平台是否具备正常启动所需的验证资格，不代表实际兼容性结论。
    #[must_use]
    pub fn compatible(&self) -> bool {
        self.platform_assessment().status == crate::PlatformState::Verified
    }
}

pub(crate) fn current_windows_build() -> Option<u32> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        use std::sync::OnceLock;
        static BUILD: OnceLock<Option<u32>> = OnceLock::new();
        *BUILD.get_or_init(|| {
            let root = std::env::var_os("SystemRoot")?;
            let output =
                std::process::Command::new(std::path::PathBuf::from(root).join("System32/reg.exe"))
                    .args([
                        "query",
                        r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
                        "/v",
                        "CurrentBuildNumber",
                    ])
                    .creation_flags(0x0800_0000)
                    .output()
                    .ok()?;
            if !output.status.success() {
                return None;
            }
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .last()?
                .parse()
                .ok()
        })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// 校验独立资源协议及不依赖本机配置的清单约束。
///
/// # Errors
/// 拒绝未知主版本、未知调用接口、重复身份、越界限制及不安全包路径。
pub fn validate_manifest(bytes: &[u8]) -> Result<ResourceIndex> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(ResourceError::before(
            "manifest_limit",
            "资源清单超过 2 MiB",
        ));
    }
    let index: ResourceIndex = serde_json::from_slice(bytes)
        .map_err(|_| ResourceError::before("invalid_manifest", "资源清单字段或调用接口无效"))?;
    if index.schema_version != RESOURCE_TOOLS_SCHEMA_VERSION {
        return Err(ResourceError::before(
            "unsupported_schema",
            "不支持该资源协议主版本",
        ));
    }
    if index.tools.len() > 128 {
        return Err(ResourceError::before(
            "manifest_limit",
            "资源工具数量超过限制",
        ));
    }
    let mut identities = std::collections::HashSet::new();
    for tool in &index.tools {
        if !safe_id(&tool.id)
            || !safe_id(&tool.version)
            || tool.version == "latest"
            || !identities.insert((&tool.id, &tool.version, &tool.package.sha256))
            || tool.description.trim().is_empty()
            || tool.publisher.trim().is_empty()
            || tool.license.trim().is_empty()
            || tool.documentation_url.trim().is_empty()
            || tool.capabilities.is_empty()
            || tool.permissions.is_empty()
            || tool.platforms.is_empty()
            || !valid_platform_evidence(tool)
            || tool.package.size == 0
            || tool.package.size > 1024 * 1024 * 1024
            || !valid_sha256(&tool.package.sha256)
            || tool.package.format != "zip"
            || !(1..=300).contains(&tool.timeout_secs)
            || !(1..=120).contains(&tool.health_check.timeout_secs)
            || !safe_relative(&tool.startup.executable)
            || (!tool.startup.browser_executable.is_empty()
                && !safe_relative(&tool.startup.browser_executable))
            || (tool.startup.interface == ExecutionInterface::AgentBrowserCli
                && tool.startup.browser_executable.is_empty())
            || tool.startup.args.len() > 64
            || tool
                .startup
                .args
                .iter()
                .any(|arg| arg.len() > 8192 || arg.contains('\0'))
            || tool.platforms.iter().any(|p| {
                p.os_versions.is_empty()
                    || !matches!(p.status.as_str(), "passed" | "pending" | "unsupported")
            })
        {
            return Err(ResourceError::before(
                "invalid_manifest",
                "资源身份、平台、包或限制不符合协议",
            ));
        }
        validate_browser_adapter(&tool.startup)?;
        let expected = match tool.startup.interface {
            ExecutionInterface::StdioMcp => "tools/list",
            ExecutionInterface::AgentBrowserCli => "version",
            ExecutionInterface::Cli => "schema",
        };
        if tool
            .platforms
            .iter()
            .any(|platform| platform.os == "windows")
            && (!tool
                .startup
                .executable
                .to_ascii_lowercase()
                .ends_with(".exe")
                || (!tool.startup.browser_executable.is_empty()
                    && !tool
                        .startup
                        .browser_executable
                        .to_ascii_lowercase()
                        .ends_with(".exe")))
        {
            return Err(ResourceError::before(
                "invalid_manifest",
                "Windows 启动文件和浏览器必须为原生 EXE，禁止批处理的隐式 Shell",
            ));
        }
        validate_cli_platform(tool)?;
        if tool.health_check.method != expected {
            return Err(ResourceError::before(
                "invalid_manifest",
                "健康检查与调用接口不匹配",
            ));
        }
    }
    Ok(index)
}

fn validate_browser_adapter(startup: &Startup) -> Result<()> {
    if let Some(adapter) = startup.browser_adapter
        && (startup.interface != ExecutionInterface::StdioMcp
            || (adapter == BrowserAdapter::None && !startup.browser_executable.is_empty())
            || (adapter != BrowserAdapter::None && startup.browser_executable.is_empty()))
    {
        return Err(ResourceError::before(
            "invalid_manifest",
            "浏览器适配与接口或浏览器路径不匹配",
        ));
    }
    Ok(())
}

fn validate_cli_platform(tool: &ResourceTool) -> Result<()> {
    if tool.startup.interface == ExecutionInterface::Cli {
        crate::cli::validate_operations(&tool.startup.operations)?;
        if tool
            .platforms
            .iter()
            .any(|platform| platform.os == "windows")
            && tool.startup.operations.iter().any(|operation| {
                operation
                    .executable
                    .as_ref()
                    .is_some_and(|path| !path.to_ascii_lowercase().ends_with(".exe"))
            })
        {
            return Err(ResourceError::before(
                "invalid_manifest",
                "Windows CLI 操作必须使用原生 EXE",
            ));
        }
    }
    Ok(())
}

fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-+".contains(&c))
        && value != "."
        && value != ".."
}

pub(crate) fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub(crate) fn safe_relative(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.split('/').count() <= 32
        && !value
            .chars()
            .any(|c| c.is_control() || "<>\"|?*".contains(c))
        && !value.contains(['\\', ':', '\0'])
        && !value.starts_with('/')
        && value.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with([' ', '.'])
                && !matches!(
                    part.split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn valid_platform_evidence(tool: &ResourceTool) -> bool {
    tool.platform_tests.len() <= 256
        && tool.platform_tests.iter().all(|t| {
            matches!(t.status.as_str(), "passed" | "failed" | "pending")
                && t.os.len() <= 32
                && t.arch.len() <= 32
                && (t.os != "windows" || t.status == "pending" || t.windows_build.is_some())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_exact_and_https_only() {
        let config = ResourceConfig {
            index_url: Some("https://a.example/remoteops/v1/index.json".into()),
            allowed_origins: vec!["https://a.example".into()],
        };
        assert!(config.index().is_ok());
        let mut malformed = config.clone();
        malformed
            .allowed_origins
            .push("https://other.example/path".into());
        assert_eq!(malformed.validate().unwrap_err().code, "invalid_config");
        for candidate in [
            "http://a.example/x",
            "https://a.example.evil/x",
            "https://a.example:444/x",
            "https://user@a.example/x",
            "https://b.example/x",
        ] {
            assert!(
                config.check_url(&Url::parse(candidate).unwrap()).is_err(),
                "{candidate}"
            );
        }
    }

    #[test]
    fn portable_paths_reject_traversal_links_and_windows_aliases() {
        for path in [
            "../x", "/x", "C:/x", "a/../x", "a\\x", "a//x", "CON.txt", "foo/NUL", "a.", "a ", ".",
            "",
        ] {
            assert!(!safe_relative(path), "{path}");
        }
        assert!(safe_relative("node/bin/node.exe"));
    }

    #[test]
    fn unknown_major_is_rejected_and_unknown_fields_are_ignored() {
        assert_eq!(
            validate_manifest(br#"{"schema_version":2,"tools":[]}"#)
                .unwrap_err()
                .code,
            "unsupported_schema"
        );
        assert!(validate_manifest(br#"{"schema_version":1,"tools":[],"future":true}"#).is_ok());
        let mut sample: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../../examples/resource-site/remoteops/v1/index.json"
        ))
        .unwrap();
        sample["tools"][0]["startup"]["interface"] = serde_json::json!("future_interface");
        assert_eq!(
            validate_manifest(&serde_json::to_vec(&sample).unwrap())
                .unwrap_err()
                .code,
            "invalid_manifest"
        );
        sample["tools"][0]["startup"]["interface"] = serde_json::json!("stdio_mcp");
        sample["tools"][0]["startup"]["executable"] = serde_json::json!("tool.cmd");
        assert_eq!(
            validate_manifest(&serde_json::to_vec(&sample).unwrap())
                .unwrap_err()
                .code,
            "invalid_manifest"
        );
    }
}
