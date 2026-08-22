use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ShellKind;

/// Agent 向 Controller 公开的环境画像版本。
pub const ENVIRONMENT_PROFILE_SCHEMA_VERSION: u16 = 1;

/// 单个命令解释器的可用性和版本信息。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShellProfile {
    /// 解释器类型。
    pub kind: ShellKind,
    /// 是否可以启动。
    pub available: bool,
    /// 可执行文件名称；不包含本机完整路径。
    pub executable: Option<String>,
    /// 解释器报告的版本摘要。
    pub version: Option<String>,
}

/// 外部工具的可用性和版本信息。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolProfile {
    /// 稳定工具名称，例如 `ssh`。
    pub name: String,
    /// 是否可以启动。
    pub available: bool,
    /// 工具报告的版本摘要。
    pub version: Option<String>,
}

/// Agent 启动后自动采集的脱敏环境画像。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentProfile {
    /// 画像结构版本，用于未来兼容字段扩展。
    pub schema_version: u16,
    /// 画像首次采集时间。
    #[serde(default = "default_timestamp")]
    pub collected_at: DateTime<Utc>,
    /// 最近一次刷新时间。
    #[serde(default = "default_timestamp")]
    pub refreshed_at: DateTime<Utc>,
    /// Agent 版本。
    #[serde(default)]
    pub agent_version: String,
    /// 画像对应的 `RemoteOps` 协议版本。
    #[serde(default)]
    pub protocol_version: u16,
    /// 操作系统家族，例如 `windows`。
    pub os_family: String,
    /// 操作系统版本摘要，不包含用户名或安装路径。
    pub os_version: Option<String>,
    /// 运行时架构，例如 `x86_64`。
    pub architecture: String,
    /// 是否检测到提升权限；检测失败时为空。
    pub elevated: Option<bool>,
    /// 命令解释器探测结果。
    pub shells: Vec<ShellProfile>,
    /// 外部工具探测结果。
    pub tools: Vec<ToolProfile>,
}

impl EnvironmentProfile {
    /// 创建空的当前版本画像。
    #[must_use]
    pub fn empty() -> Self {
        Self {
            schema_version: ENVIRONMENT_PROFILE_SCHEMA_VERSION,
            collected_at: Utc::now(),
            refreshed_at: Utc::now(),
            ..Self::default()
        }
    }

    /// 查询某种 Shell 是否可用。
    #[must_use]
    pub fn has_shell(&self, kind: ShellKind) -> bool {
        self.shells
            .iter()
            .any(|shell| shell.kind == kind && shell.available)
    }
}

fn default_timestamp() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

