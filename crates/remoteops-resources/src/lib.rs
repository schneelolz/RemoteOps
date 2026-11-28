//! 显式信任的公开资源清单、固定摘要缓存和会话隔离工具运行时。

mod cache;
mod cleanup;
mod cli;
mod diagnostics;
mod manifest;
mod platform;
mod progress;
pub use platform::{PlatformAssessment, PlatformEvidence, PlatformState};
pub use progress::{PreparationProgress, ProgressReporter};
mod runtime;

pub use cache::validate_archive;
pub use diagnostics::{LaunchContext, StartupDiagnostics};
pub use manifest::{
    BrowserAdapter, CliArgument, CliOperation, CliResultFormat, ExecutionInterface, HealthCheck,
    Platform, ResourceConfig, ResourceIndex, ResourcePackage, ResourceTool, Startup,
    ToolDocumentation, validate_manifest,
};
pub use runtime::{CallToolSchema, PackageFetcher, ResourceManager, StartedTool};

use serde::{Deserialize, Serialize};

/// 资源协议主版本，独立于 `RemoteOps` 线协议版本。
pub const RESOURCE_TOOLS_SCHEMA_VERSION: u32 = 1;

/// 可供调用方安全分类的错误；消息不包含工具输出或本地私有路径。
#[derive(Clone, Debug, Serialize, Deserialize, thiserror::Error)]
#[error("resource tool [{code}]: {message}")]
pub struct ResourceError {
    /// 稳定错误类别。
    pub code: String,
    /// 已脱敏的固定说明。
    pub message: String,
    /// 仅当操作尚未提交时，允许 AI 选择其他候选。
    pub retry_safe: bool,
    /// 启动阶段的有界诊断；不含原始业务报文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Box<StartupDiagnostics>>,
}

impl ResourceError {
    fn before(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retry_safe: true,
            diagnostics: None,
        }
    }

    fn unknown() -> Self {
        Self {
            code: "result_unknown".into(),
            message: "操作已经提交，结果未知；请先核实状态，禁止自动重放或切换后端重试".into(),
            retry_safe: false,
            diagnostics: None,
        }
    }
}

type Result<T> = std::result::Result<T, ResourceError>;
