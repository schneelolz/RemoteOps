use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentInstanceId, CapabilitySet, EnvironmentProfile, PermissionMode, SessionId};

/// 控制端看到的连接状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    /// Agent 已连接且可执行操作。
    Online,
    /// 传输连接中断但租约仍可恢复。
    Reconnecting,
    /// 会话已离线。
    Offline,
    /// 会话已关闭。
    Closed,
}

/// 当前会话的控制角色。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRole {
    /// 人工拥有写入权。
    HumanControl,
    /// AI 拥有受策略约束的写入权。
    AiControl,
    /// AI 只能读取输出。
    AiReadOnly,
}

/// 控制端连接列表使用的稳定描述。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConnectionDescriptor {
    /// 逻辑会话标识。
    pub session_id: SessionId,
    /// Agent 实例标识。
    pub agent_instance_id: AgentInstanceId,
    /// 控制端分配的显示序号。
    pub display_index: u32,
    /// 用户可修改的别名。
    pub alias: Option<String>,
    /// Agent 主机名。
    pub hostname: String,
    /// Agent 操作系统描述。
    pub operating_system: String,
    /// Agent 当前公开的能力。
    pub capabilities: CapabilitySet,
    /// Agent 启动后自动采集的脱敏环境画像。
    pub environment: EnvironmentProfile,
    /// 当前 Agent 进程用于端到端凭据加密的 HPKE 公钥。
    #[serde(default)]
    pub credential_encryption_public_key: String,
    /// 当前 HPKE 公钥的 SHA-256 标识。
    #[serde(default)]
    pub credential_encryption_key_id: String,
    /// 当前连接状态。
    pub state: ConnectionState,
    /// 当前写入角色。
    pub role: SessionRole,
    /// Relay 根据 Controller 降权和 Agent 本地选择计算出的有效权限。
    #[serde(default)]
    pub permission_mode: PermissionMode,
    /// 最近一次状态更新时间。
    pub updated_at: DateTime<Utc>,
}

impl ConnectionDescriptor {
    /// 返回默认编号或用户别名。
    #[must_use]
    pub fn display_name(&self) -> String {
        self.alias
            .clone()
            .unwrap_or_else(|| format!("连接 {}", self.display_index))
    }
}
