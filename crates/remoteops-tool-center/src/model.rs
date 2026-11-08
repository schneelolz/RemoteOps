use remoteops_resources::ResourceTool;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 工具中心一次传输的最大原始分块。
pub const PACKAGE_CHUNK_SIZE: usize = 256 * 1024;
/// ZIP 包压缩大小上限。
pub const MAX_PACKAGE_SIZE: u64 = 1024 * 1024 * 1024;

/// 可供后台和 MCP 共用的工具资料；版本包含独立平台包。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedTool {
    /// 稳定工具标识。
    pub id: String,
    /// 展示名称。
    pub name: String,
    /// 用途说明。
    pub purpose: String,
    /// 使用说明。
    pub instructions: String,
    /// 使用示例。
    #[serde(default)]
    pub examples: Vec<String>,
    /// 运行依赖说明，不自动安装依赖。
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// 独立运行开关；首次登记强制关闭。
    #[serde(default)]
    pub enabled: bool,
    /// 已登记的固定版本。
    pub versions: Vec<ToolVersion>,
}

/// 同一版本可登记多个平台包。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolVersion {
    /// 精确版本。
    pub version: String,
    /// 该版本的平台包。
    pub packages: Vec<ToolPackage>,
}

/// 包来源、执行契约和实际平台测试证据。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolPackage {
    /// 沿用 Agent 的固定包及执行定义。
    pub tool: ResourceTool,
    /// 托管包或已验证 HTTPS 外链。
    pub source: PackageSource,
    /// 文件校验事实，由服务计算，登记输入不能授予通过状态。
    #[serde(default)]
    pub file_verified: bool,
    /// 平台测试事实；上传文件不会自动新增记录。
    #[serde(default)]
    pub tests: Vec<PlatformTest>,
}

impl std::fmt::Debug for ToolPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolPackage")
            .field("id", &self.tool.id)
            .field("version", &self.tool.version)
            .field("sha256", &self.tool.package.sha256)
            .field("source", &self.source)
            .field("file_verified", &self.file_verified)
            .field("tests", &self.tests)
            .finish()
    }
}

/// 同一摘要可使用托管 ZIP 或具有相同内容的 HTTPS 地址。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageSource {
    /// 中继按 SHA-256 保存的 ZIP。
    Hosted,
    /// 不长期保存的外部 ZIP；查询参数只进入内部存储及实际下载目录。
    External {
        /// HTTPS 下载 URL。
        url: String,
    },
}

impl std::fmt::Debug for PackageSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hosted => f.write_str("Hosted"),
            Self::External { url } => f.debug_tuple("External").field(&redact_url(url)).finish(),
        }
    }
}

/// 实际环境的测试记录；headless 结果不能作为桌面平台通过。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlatformTest {
    /// 操作系统。
    pub os: String,
    /// 架构。
    pub arch: String,
    /// 实际系统版本及环境。
    pub environment: String,
    /// 是否使用真实交互桌面。
    pub desktop: bool,
    /// 实际 Windows build；Windows 通过记录必须提供。
    #[serde(default)]
    #[cfg_attr(
        feature = "json-schema",
        schemars(extend("maximum" = 4_294_967_295_u64))
    )]
    pub windows_build: Option<u32>,
    /// passed、failed 或 pending。
    #[cfg_attr(
        feature = "json-schema",
        schemars(extend("enum" = ["passed", "failed", "pending"]))
    )]
    pub status: String,
    /// 实际验证步骤及结果。
    pub result: String,
    /// 测试时间，RFC 3339。
    pub tested_at: String,
    /// 记录的真实操作者，由管理服务填写。
    #[serde(default)]
    pub recorded_by: String,
}

/// 后台普通列表；外链查询参数已移除。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCenterSnapshot {
    /// 全局目录修订号。
    pub revision: u64,
    /// 全局运行开关。
    pub enabled: bool,
    /// 全局工具选择指引，默认空，不授予任何执行权限。
    #[serde(default)]
    pub guide_markdown: String,
    /// 已登记工具。
    pub tools: Vec<ManagedTool>,
    /// 最近管理审计；不包含签名 URL 或包内容。
    pub audit: Vec<ToolAudit>,
}

/// 经认证连接发送给 Agent 的精确目录。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCatalog {
    /// 必须被 Agent 确认的修订号。
    pub revision: u64,
    /// 全局运行开关。
    pub enabled: bool,
    /// 随候选列表提供给 AI 的全局说明。
    #[serde(default)]
    pub guide_markdown: String,
    /// 已启用的工具及固定版本；包含实际外链地址。
    pub tools: Vec<ResourceTool>,
}

/// 不含包内容及签名参数的管理审计。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolAudit {
    /// 请求标识。
    pub request_id: String,
    /// 操作者。
    pub actor: String,
    /// 变更类型。
    pub action: String,
    /// 处理结果。
    pub result: String,
    /// 目录修订号。
    pub revision: u64,
    /// 处理时间。
    pub created_at: String,
}

/// 管理请求；修订号和请求标识防止丢失更新及重复登记。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCenterRequest {
    /// 每次管理操作的稳定唯一标识。
    pub request_id: String,
    /// 客户端最后看到的修订号。
    #[cfg_attr(
        feature = "json-schema",
        schemars(extend("maximum" = 18_446_744_073_709_551_615_u64))
    )]
    pub expected_revision: u64,
    /// 管理动作。
    pub command: ToolCenterCommand,
}

/// 与 Agent 会话无关的工具管理动作。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ToolCenterCommand {
    /// 读取策略及列表。
    Snapshot,
    /// 设置全局运行开关。
    SetPolicy {
        /// 是否允许全局运行。
        enabled: bool,
    },
    /// 更新全局说明，最多 32 KiB UTF-8。
    SetGuide {
        /// Markdown 源文，仅作为工具使用建议。
        guide_markdown: String,
    },
    /// 新增或更新资料；相同版本已有包不能替换或删除。
    PutTool {
        /// 完整工具资料和固定版本包。
        tool: ManagedTool,
    },
    /// 设置独立工具开关。
    SetToolEnabled {
        /// 已登记的稳定工具标识。
        tool_id: String,
        /// 是否允许该工具运行。
        enabled: bool,
    },
    /// 添加真实环境测试记录。
    RecordTest {
        /// 已登记的稳定工具标识。
        tool_id: String,
        /// 已登记的精确版本。
        version: String,
        /// 已登记包的小写 SHA-256。
        sha256: String,
        /// 实际环境的测试事实。
        record: PlatformTest,
    },
    /// 临时下载、校验并登记外链身份，不执行或长期保存 ZIP。
    ImportExternal {
        /// 临时下载并校验的 HTTPS ZIP 地址。
        url: String,
        /// 可选的预期小写 SHA-256；省略时由服务计算。
        #[serde(default)]
        expected_sha256: Option<String>,
    },
    /// 开始托管包上传。
    #[cfg_attr(feature = "json-schema", schemars(skip))]
    BeginUpload { size: u64, sha256: String },
    /// 按精确偏移上传一个分块，调用方必须等待该块确认。
    #[cfg_attr(feature = "json-schema", schemars(skip))]
    UploadChunk {
        upload_id: String,
        offset: u64,
        data_base64: String,
    },
    /// 校验完整 ZIP 并原子提交。
    #[cfg_attr(feature = "json-schema", schemars(skip))]
    FinishUpload { upload_id: String },
    /// 取消本操作者的上传。
    #[cfg_attr(feature = "json-schema", schemars(skip))]
    CancelUpload { upload_id: String },
}

impl ToolCenterCommand {
    /// 不含输入内容的稳定动作名称。
    #[must_use]
    pub const fn action(&self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::SetPolicy { .. } => "set_policy",
            Self::SetGuide { .. } => "set_guide",
            Self::PutTool { .. } => "put_tool",
            Self::SetToolEnabled { .. } => "set_tool_enabled",
            Self::RecordTest { .. } => "record_test",
            Self::ImportExternal { .. } => "import_external",
            Self::BeginUpload { .. } => "begin_upload",
            Self::UploadChunk { .. } => "upload_chunk",
            Self::FinishUpload { .. } => "finish_upload",
            Self::CancelUpload { .. } => "cancel_upload",
        }
    }
}

impl std::fmt::Debug for ToolCenterCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCenterCommand")
            .field("action", &self.action())
            .finish_non_exhaustive()
    }
}

/// 固定脱敏管理错误。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ToolCenterError {
    /// 稳定错误码。
    pub code: String,
    /// 不含路径、令牌或签名 URL 的说明。
    pub message: String,
}

/// 管理结果；上传成功只证明文件校验成功。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCenterResponse {
    /// 原请求标识。
    pub request_id: String,
    /// 当前修订号。
    pub revision: u64,
    /// 固定错误类别。
    pub error: Option<ToolCenterError>,
    /// 快照、上传偏移或文件校验回执。
    pub data: Value,
}

/// 已校验包的身份，不声明平台测试通过。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageReceipt {
    /// 精确 ZIP 字节数。
    pub size: u64,
    /// 小写 SHA-256。
    pub sha256: String,
    /// 校验成功的文件，不等于平台通过。
    pub file_verified: bool,
    /// 托管或脱敏外链来源。
    pub source: PackageSource,
}

pub(crate) fn redact_url(value: &str) -> String {
    url::Url::parse(value).map_or_else(
        |_| "[invalid URL]".into(),
        |mut url| {
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        },
    )
}

impl std::fmt::Debug for ToolCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCatalog")
            .field("revision", &self.revision)
            .field("enabled", &self.enabled)
            .field(
                "tools",
                &self
                    .tools
                    .iter()
                    .map(|t| (&t.id, &t.version, &t.package.sha256))
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}
