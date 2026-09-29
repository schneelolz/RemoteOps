//! 两端共享的会话控制授权；与旧展示字段隔离。
use crate::ControllerControlMode;
use chrono::{DateTime, Utc};
use remoteops_domain::{ControllerInstanceId, RequestId, SessionId};
use serde::{Deserialize, Serialize};

/// 完全控制的授权来源。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlSource {
    /// 控制端用户明确授权。
    Mcp,
    /// 现场人员为本连接临时授权。
    Local,
    /// 现场人员已保存的自动授权偏好。
    LocalDefault,
}

/// Relay 确认的授权快照，修订号仅在相同绑定内比较。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionControlState {
    /// 目标会话。
    pub session_id: SessionId,
    /// Agent 传输连接代次。
    pub agent_generation: u64,
    /// 当前 AI 控制端实例。
    pub controller_id: ControllerInstanceId,
    /// 控制端传输连接代次。
    pub controller_generation: u64,
    /// 同一绑定内递增的修订号。
    pub revision: u64,
    /// 实际生效的模式。
    pub mode: ControllerControlMode,
    /// 完全控制来源；逐项确认时为空。
    pub source: Option<ControlSource>,
    /// MCP 空闲授权截止时间；现场授权为空。
    pub expires_at: Option<DateTime<Utc>>,
}

/// 查询或比较修订号后变更授权，空模式表示只查询。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ControlStateRequest {
    /// 用于关联确认结果。
    pub request_id: RequestId,
    /// 目标会话。
    pub session_id: SessionId,
    /// 变更时必须提供完整旧快照，防止跨代次重放。
    pub expected: Option<SessionControlState>,
    /// 请求的模式；为空时不更改授权。
    pub mode: Option<ControllerControlMode>,
    /// 请求来源，由 Relay 根据发送方角色验证。
    pub source: Option<ControlSource>,
}

/// Relay 对查询或变更的确认；失败仍可携带最新快照。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ControlStateResult {
    /// 原请求标识。
    pub request_id: RequestId,
    /// 当前有效快照。
    pub state: Option<SessionControlState>,
    /// 失败原因；非空时不得认为变更成功。
    pub error: Option<String>,
}

/// 本次修改操作使用的审批依据。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlBasis {
    /// 使用当前连接的完全控制授权。
    FullAccess,
    /// 控制端刚刚完成本次操作的逐项确认。
    SingleApproval,
    /// 独立人工控制端签发的审批。
    ExternalApproval,
}

/// 修改请求携带的绑定和授权版本，不得从最新缓存补写旧审批。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ControlProof {
    /// 审批时读取的有效快照。
    pub state: SessionControlState,
    /// 本次操作的授权依据。
    pub basis: ControlBasis,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WireMessage;

    #[test]
    fn control_messages_round_trip_with_binding_and_revision() {
        let state = SessionControlState {
            session_id: SessionId::new(),
            agent_generation: 2,
            controller_id: ControllerInstanceId::new(),
            controller_generation: 4,
            revision: 9,
            mode: ControllerControlMode::FullAccess,
            source: Some(ControlSource::LocalDefault),
            expires_at: None,
        };
        let request_id = RequestId::new();
        for message in [
            WireMessage::ControlStateUpdated(state.clone()),
            WireMessage::ControlStateRequest(ControlStateRequest {
                request_id,
                session_id: state.session_id,
                expected: Some(state.clone()),
                mode: Some(ControllerControlMode::StepByStep),
                source: Some(ControlSource::Local),
            }),
            WireMessage::ControlStateResult(ControlStateResult {
                request_id,
                state: Some(state),
                error: Some("conflict".into()),
            }),
        ] {
            let serialized = serde_json::to_string(&message).unwrap();
            assert_eq!(
                serde_json::from_str::<WireMessage>(&serialized).unwrap(),
                message
            );
        }
    }
}
