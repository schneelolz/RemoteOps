use chrono::{DateTime, Utc};
use remoteops_domain::{
    AgentInstanceId, ApprovalId, ApprovalState, CapabilitySet, ConnectionDescriptor,
    ControllerInstanceId, ControllerOwnerId, EnvironmentProfile, EventSource, PairingCode,
    PermissionMode, RemoteEvent, RemoteOperation, RequestId, SessionId,
};
use serde::{Deserialize, Serialize};

/// 当前线协议版本。
pub const PROTOCOL_VERSION: u16 = 14;

/// Controller 的受信任调用身份。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerKind {
    /// 由人工直接操作的控制端。
    Human,
    /// 由 AI 工具调用的控制端。
    Ai,
}

impl ControllerKind {
    /// 返回进入策略和审计层的可信来源。
    #[must_use]
    pub const fn event_source(self) -> EventSource {
        match self {
            Self::Human => EventSource::Human,
            Self::Ai => EventSource::Ai,
        }
    }
}

/// Agent 建立传输连接时的身份和能力声明。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentHello {
    /// 协议版本。
    pub protocol_version: u16,
    /// 进程实例标识。
    pub agent_instance_id: AgentInstanceId,
    /// Relay 上次签发的恢复令牌。
    pub resume_token: Option<String>,
    /// Agent 主机名。
    pub hostname: String,
    /// 操作系统说明。
    pub operating_system: String,
    /// 当前启用能力。
    pub capabilities: CapabilitySet,
    /// 启动后自动采集的脱敏环境画像。
    pub environment: EnvironmentProfile,
    /// 当前 Agent 进程用于 SSH 密码端到端加密的 HPKE 公钥。
    pub credential_encryption_public_key: String,
    /// 当前 HPKE 公钥的 SHA-256 标识。
    pub credential_encryption_key_id: String,
    /// Agent 主机的高置信度 MAC 地址；无法可靠判断时为空。
    #[serde(default)]
    pub mac_address: Option<String>,
}

/// Controller 建立传输连接时的身份声明。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ControllerHello {
    /// 协议版本。
    pub protocol_version: u16,
    /// 控制端实例标识。
    pub controller_instance_id: ControllerInstanceId,
    /// Human 与 AI Controller 共同使用的稳定 Owner 标识。
    pub owner_id: ControllerOwnerId,
    /// Relay 配置中对应的控制端身份。
    pub kind: ControllerKind,
    /// Relay 独立配置的控制端认证令牌。
    pub auth_token: String,
    /// Controller 所在计算机名；旧客户端未提供时为空。
    #[serde(default)]
    pub hostname: Option<String>,
    /// Controller 主机的高置信度 MAC 地址；旧客户端未提供时为空。
    #[serde(default)]
    pub mac_address: Option<String>,
}

/// 客户端发出的第一条消息。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum ClientHello {
    /// Agent 客户端。
    Agent(AgentHello),
    /// Controller 客户端。
    Controller(ControllerHello),
}

/// Relay 接受 Agent 后颁发的信息。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentWelcome {
    /// 租约有效期内不变的配对码。
    pub pairing_code: PairingCode,
    /// 租约到期时间。
    pub lease_expires_at: DateTime<Utc>,
    /// 只用于恢复和传输鉴权的随机令牌。
    pub resume_token: String,
    /// 当前 Agent 传输连接的单调递增代次。
    pub connection_generation: u64,
    /// 心跳建议间隔。
    pub heartbeat_interval_seconds: u64,
}

/// Agent 确认已经安全收到新的恢复令牌。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentWelcomeAck {
    /// 当前 Agent 传输连接代次。
    pub connection_generation: u64,
}

/// Relay 确认恢复令牌已经提交。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentResumeCommitted {
    /// 当前 Agent 传输连接代次。
    pub connection_generation: u64,
}

/// Agent 确认已经切换到 Relay 提交的恢复令牌。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentResumeCommitAck {
    /// 当前 Agent 传输连接代次。
    pub connection_generation: u64,
}

/// Relay 接受 Agent 心跳后通知新的租约到期时间。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentLeaseRenewed {
    /// 当前控制码租约到期时间。
    pub lease_expires_at: DateTime<Utc>,
}

/// Agent 本地用户更新当前进程的权限上限。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentPermissionModeChanged {
    /// Agent 本地选择的权限模式。
    pub permission_mode: PermissionMode,
}

/// Controller 请求配对 Agent。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PairRequest {
    /// 请求标识。
    pub request_id: RequestId,
    /// 人工输入的控制码。
    pub pairing_code: PairingCode,
    /// Human 首次绑定或重新配对时请求的会话权限。
    pub permission_mode: PermissionMode,
}

/// Relay 返回配对结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PairResult {
    /// 对应请求。
    pub request_id: RequestId,
    /// 配对成功后的连接；失败时为空。
    pub connection: Option<ConnectionDescriptor>,
    /// 失败时可展示的信息。
    pub error: Option<String>,
}

/// Controller 请求 Relay 释放当前实例持有的会话绑定。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseSessionRequest {
    /// 请求标识。
    pub request_id: RequestId,
    /// 要释放的逻辑会话。
    pub session_id: SessionId,
}

/// Relay 返回会话释放结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseSessionResult {
    /// 对应请求。
    pub request_id: RequestId,
    /// 请求释放的逻辑会话。
    pub session_id: SessionId,
    /// Relay 是否已经确认当前 Controller 不再持有该会话绑定。
    pub released: bool,
    /// 释放失败时可安全展示的信息。
    pub error: Option<String>,
}

/// Relay 向 Agent 声明当前唯一有权发送请求的 Controller 绑定。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ControllerBinding {
    /// 绑定的逻辑会话。
    pub session_id: SessionId,
    /// Controller 实例标识。
    pub controller_instance_id: ControllerInstanceId,
    /// Relay 认证后的 Owner 标识。
    pub owner_id: ControllerOwnerId,
    /// Relay 已认证的 Controller 身份。
    pub controller_kind: ControllerKind,
    /// Relay 当前强制执行的会话权限。
    pub permission_mode: PermissionMode,
    /// 只在 Relay 与 Agent 之间传输的随机绑定令牌。
    pub binding_token: String,
}

/// Relay 对单次远程请求给出的可信授权声明。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RelayAuthorization {
    /// Controller 实例标识。
    pub controller_instance_id: ControllerInstanceId,
    /// Relay 认证后的 Owner 标识。
    pub owner_id: ControllerOwnerId,
    /// Relay 已认证的 Controller 身份。
    pub controller_kind: ControllerKind,
    /// Relay 当前强制执行的会话权限。
    pub permission_mode: PermissionMode,
    /// 与 Agent 当前绑定完全匹配的随机令牌。
    pub binding_token: String,
    /// Relay 策略层确认的审批状态。
    pub approval: ApprovalState,
}

/// 只有 Relay 可以发往 Agent 的已授权远程请求。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthorizedRemoteRequest {
    /// 已由 Relay 覆盖来源并规范化只读声明的请求。
    pub request: RemoteRequest,
    /// Relay 生成的可信授权声明。
    pub authorization: RelayAuthorization,
}

/// Controller 为一项精确操作申请人工审批。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// 本次申请的请求标识。
    pub request_id: RequestId,
    /// 目标会话。
    pub session_id: SessionId,
    /// 需要审批的完整操作。
    pub operation: RemoteOperation,
}

/// 已认证人工 Controller 对审批作出决定。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalDecision {
    /// 本次决定的请求标识。
    pub request_id: RequestId,
    /// 目标会话。
    pub session_id: SessionId,
    /// 必须与申请完全一致的操作。
    pub operation: RemoteOperation,
    /// Relay 签发的审批标识。
    pub approval_id: ApprovalId,
    /// 是否批准。
    pub approved: bool,
}

/// Relay 返回审批申请或决定的当前结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalResult {
    /// 对应申请或决定请求。
    pub request_id: RequestId,
    /// 目标会话。
    pub session_id: SessionId,
    /// 审批绑定的完整操作。
    pub operation: RemoteOperation,
    /// 不需要审批或申请失败时为空。
    pub approval_id: Option<ApprovalId>,
    /// Relay 保存的可信审批状态。
    pub state: ApprovalState,
    /// 可安全展示的原因。
    pub reason: String,
    /// 审批到期时间；不需要审批时为空。
    pub expires_at: Option<DateTime<Utc>>,
}

/// Controller 发往 Agent 的受控请求。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteRequest {
    /// 请求标识。
    pub request_id: RequestId,
    /// 不可变目标会话。
    pub session_id: SessionId,
    /// 来源。
    pub source: EventSource,
    /// 受策略约束的操作。
    pub operation: RemoteOperation,
    /// 已由人工批准的审批标识。
    pub approval_id: Option<ApprovalId>,
    /// 文件等二进制内容使用的 Base64 负载；Relay 不解析。
    pub payload_base64: Option<String>,
}

/// Agent 完成一次请求后的结构化结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteResponse {
    /// 请求标识。
    pub request_id: RequestId,
    /// 目标会话。
    pub session_id: SessionId,
    /// 进程退出码。
    pub exit_code: Option<i32>,
    /// 成功摘要。
    pub summary: String,
    /// 稳定错误码。
    pub error_code: Option<String>,
    /// 下载文件等二进制结果使用的 Base64 负载。
    pub payload_base64: Option<String>,
    /// 结果文件的 SHA-256。
    pub sha256: Option<String>,
    /// 可选结构化明细。
    pub details: Option<serde_json::Value>,
}

/// TLS 连接上的完整消息集合。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum WireMessage {
    /// 第一条角色声明消息。
    Hello(ClientHello),
    /// Agent 注册成功。
    AgentWelcome(AgentWelcome),
    /// Agent 确认收到新的恢复令牌。
    AgentWelcomeAck(AgentWelcomeAck),
    /// Relay 确认恢复令牌已经提交。
    AgentResumeCommitted(AgentResumeCommitted),
    /// Agent 确认已切换到提交后的恢复令牌。
    AgentResumeCommitAck(AgentResumeCommitAck),
    /// Relay 通知 Agent 心跳续租后的新到期时间。
    AgentLeaseRenewed(AgentLeaseRenewed),
    /// Controller 连接就绪。
    ControllerWelcome {
        /// Relay 当前协议版本。
        protocol_version: u16,
    },
    /// Controller 请求配对。
    PairRequest(PairRequest),
    /// Relay 返回配对结果。
    PairResult(PairResult),
    /// Controller 请求释放当前实例持有的会话绑定。
    ReleaseSessionRequest(ReleaseSessionRequest),
    /// Relay 确认会话绑定已经释放。
    ReleaseSessionResult(ReleaseSessionResult),
    /// Controller 申请精确操作审批。
    ApprovalRequest(ApprovalRequest),
    /// 已认证人工 Controller 作出审批决定。
    ApprovalDecision(ApprovalDecision),
    /// Relay 返回审批状态。
    ApprovalResult(ApprovalResult),
    /// Agent 或 Controller 心跳。
    Heartbeat {
        /// 发送时间。
        sent_at: DateTime<Utc>,
    },
    /// Agent 通知 Relay 本地用户选择的权限模式。
    AgentPermissionModeChanged(AgentPermissionModeChanged),
    /// Relay 确认心跳。
    HeartbeatAck {
        /// Relay 时间。
        received_at: DateTime<Utc>,
    },
    /// Controller 发出的远程请求。
    RemoteRequest(RemoteRequest),
    /// Relay 验证并授权后发往 Agent 的远程请求。
    AuthorizedRemoteRequest(AuthorizedRemoteRequest),
    /// Agent 返回的结构化结果。
    RemoteResponse(RemoteResponse),
    /// Agent、Relay 或 Controller 发出的统一事件。
    RemoteEvent(RemoteEvent),
    /// Relay 通知连接状态变化。
    ConnectionUpdated(ConnectionDescriptor),
    /// Relay 通知 Controller 该会话已被释放或由同 Owner 的新实例接管。
    ConnectionRemoved {
        /// 已移除的逻辑会话。
        session_id: SessionId,
        /// 可安全展示的移除原因。
        reason: String,
    },
    /// Relay 向 Agent 声明当前唯一 Controller 绑定。
    ControllerBinding(ControllerBinding),
    /// Relay 向 Agent 撤销 Controller 绑定。
    ControllerBindingRevoked {
        /// 被撤销的逻辑会话。
        session_id: SessionId,
        /// 被撤销的绑定令牌。
        binding_token: String,
    },
    /// 对端发生协议或业务错误。
    Error {
        /// 稳定错误码。
        code: String,
        /// 可安全展示的信息。
        message: String,
        /// 可选关联请求。
        request_id: Option<RequestId>,
    },
}

#[cfg(test)]
mod tests {
    use remoteops_domain::{ApprovalState, SessionId, ShellKind};

    use super::*;

    #[test]
    fn remote_request_round_trips_json() {
        let request = WireMessage::RemoteRequest(RemoteRequest {
            request_id: RequestId::new(),
            session_id: SessionId::new(),
            source: EventSource::Ai,
            operation: RemoteOperation::RunCommand {
                shell: ShellKind::WindowsPowerShell,
                command: "Get-ComputerInfo".to_owned(),
                readonly: true,
            },
            approval_id: None,
            payload_base64: None,
        });

        let json = serde_json::to_vec(&request).expect("协议消息应可编码");
        let decoded: WireMessage = serde_json::from_slice(&json).expect("协议消息应可解码");

        assert_eq!(decoded, request);
    }

    #[test]
    fn authorized_request_round_trips_json() {
        let controller_instance_id = ControllerInstanceId::new();
        let owner_id = ControllerOwnerId::new();
        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id: SessionId::new(),
            source: EventSource::Ai,
            operation: RemoteOperation::TestPort {
                host: "127.0.0.1".to_owned(),
                port: 22,
            },
            approval_id: None,
            payload_base64: None,
        };
        let message = WireMessage::AuthorizedRemoteRequest(AuthorizedRemoteRequest {
            request,
            authorization: RelayAuthorization {
                controller_instance_id,
                owner_id,
                controller_kind: ControllerKind::Ai,
                permission_mode: PermissionMode::ApprovalRequired,
                binding_token: "binding-token".to_owned(),
                approval: ApprovalState::NotRequired,
            },
        });

        let json = serde_json::to_vec(&message).expect("授权消息应可编码");
        let decoded: WireMessage = serde_json::from_slice(&json).expect("授权消息应可解码");

        assert_eq!(decoded, message);
    }

    #[test]
    fn agent_lease_renewed_round_trips_json() {
        let message = WireMessage::AgentLeaseRenewed(AgentLeaseRenewed {
            lease_expires_at: Utc::now(),
        });
        let json = serde_json::to_vec(&message).expect("续租消息应可编码");
        let decoded: WireMessage = serde_json::from_slice(&json).expect("续租消息应可解码");
        assert_eq!(decoded, message);
    }

    #[test]
    fn legacy_secret_field_is_discarded_and_never_forwarded() {
        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id: SessionId::new(),
            source: EventSource::Ai,
            operation: RemoteOperation::ListProcesses,
            approval_id: None,
            payload_base64: None,
        };
        let mut value = serde_json::to_value(&request).expect("请求应可编码");
        value.as_object_mut().expect("请求应为 JSON 对象").insert(
            "secret".to_owned(),
            serde_json::Value::String("must-not-be-forwarded".to_owned()),
        );

        let decoded: RemoteRequest = serde_json::from_value(value).expect("旧请求应保持兼容");
        let forwarded = serde_json::to_string(&decoded).expect("请求应可重新编码");

        assert!(!forwarded.contains("secret"));
        assert!(!forwarded.contains("must-not-be-forwarded"));
    }
}
