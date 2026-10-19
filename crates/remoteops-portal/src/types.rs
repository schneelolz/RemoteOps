use chrono::{DateTime, Utc};
use remoteops_domain::{
    AgentBootId, AgentInstanceId, ControllerInstanceId, ControllerOwnerId, SessionId,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 门户存储错误不包含任何调用方凭据。
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    /// 门户请求格式无效。
    #[error("门户请求格式无效")]
    InvalidInput,
    /// 门户身份无效或已经撤销。
    #[error("门户身份无效或已经撤销")]
    Unauthorized,
    /// 调用方无权访问目标。
    #[error("调用方无权访问目标")]
    Forbidden,
    /// 门户资源不存在。
    #[error("门户资源不存在")]
    NotFound,
    /// 门户资源状态或修订号冲突。
    #[error("门户资源状态或修订号冲突")]
    Conflict,
    /// 邀请已经过期。
    #[error("邀请已经过期")]
    Expired,
    /// 邀请或连接已撤销。
    #[error("邀请或连接已撤销")]
    Revoked,
    /// 目标身份、运行或会话不匹配。
    #[error("目标身份、运行或会话不匹配")]
    TargetMismatch,
    /// 门户存储不可用或状态损坏。
    #[error("门户存储不可用或状态损坏")]
    Storage,
}
impl From<rusqlite::Error> for PortalError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}

/// 经凭据认证的门户集成主体。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Principal {
    /// 所属集成主体的服务端标识。
    pub principal_id: Uuid,
    /// 集成主体的显示名称。
    pub name: String,
    /// Relay 认证的控制方归属。
    pub owner_id: ControllerOwnerId,
    /// 允许该集成主体访问的资产键白名单。
    pub allowed_assets: Vec<String>,
    /// 主体是否已被管理员撤销。
    pub revoked: bool,
}

/// 仅允许受控管理入口创建集成主体。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePrincipalRequest {
    /// 集成主体的显示名称。
    pub name: String,
    /// Relay 认证的控制方归属。
    pub owner_id: ControllerOwnerId,
    /// 允许该集成主体访问的资产键白名单。
    pub allowed_assets: Vec<String>,
}

/// 首次返回的主体凭据，禁止记录其序列化结果。
#[derive(Serialize)]
pub struct CreatedPrincipal {
    /// 已创建主体的非机密信息。
    pub principal: Principal,
    /// 仅首次返回的集成凭据，禁止记录到日志或交付前端。
    pub token: String,
}

/// 受控后台登记的资产请求。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAssetRequest {
    /// 集成主体范围内的稳定资产键。
    pub asset_id: String,
    /// 所属集成主体的服务端标识。
    pub principal_id: Uuid,
}

/// 资产与已认证 Agent 的稳定映射。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Asset {
    /// 服务端分配的稳定目标标识。
    pub target_id: String,
    /// 集成主体范围内的稳定资产键。
    pub asset_id: String,
    /// 所属集成主体的服务端标识。
    pub principal_id: Uuid,
    /// 部署证明锁定的准确 Agent 安装实例。
    pub agent_id: Option<AgentInstanceId>,
    /// 资产修订号，用于签发时乐观并发校验及显式替换。
    pub revision: u64,
}

/// 资产登记凭据只返回受控后台。
#[derive(Serialize)]
pub struct CreatedAsset {
    /// 当前资产登记与修订信息。
    pub asset: Asset,
    /// 仅首次返回的部署登记证明，禁止写入日志或公开上下文。
    pub enrollment_token: String,
}

/// Relay 在持有运行状态锁时取得的准确目标快照。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TargetSnapshot {
    /// 部署证明锁定的准确 Agent 安装实例。
    pub agent_id: AgentInstanceId,
    /// 准确 Agent 本次进程运行标识。
    pub boot_id: AgentBootId,
    /// 此次授权绑定的单目标逻辑会话。
    pub session_id: SessionId,
    /// 签发或兑换时的传输代次快照，不限制同运行重连。
    pub agent_generation: u64,
}

/// 邀请签发请求；同一幂等键不得用于不同参数。
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueInvitationRequest {
    /// 本次签发请求的唯一幂等键，重试必须保留。
    pub idempotency_key: Uuid,
    #[serde(default)]
    /// 是否明确替换当前活动授权连接。
    pub replace_active: bool,
    /// 替换时必须匹配状态接口返回的资产修订号。
    pub expected_revision: Option<u64>,
}

/// 不含短码的邀请状态。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvitationSummary {
    /// 邀请的服务端唯一标识。
    pub invitation_id: Uuid,
    /// 服务端分配的稳定目标标识。
    pub target_id: String,
    /// 授权绑定的准确目标、运行与会话。
    pub target: TargetSnapshot,
    /// 邀请修订号，反映该邀请的签发和撤销变化。
    pub revision: u64,
    /// 邀请首次兑换的截止时间，不是活动连接到期时间。
    pub expires_at: DateTime<Utc>,
    /// 该资源的持久生命周期状态。
    pub state: String,
}

/// 签发结果的短码为限时机密；相同有效请求可恢复原结果。
#[derive(Serialize)]
pub struct IssuedInvitation {
    /// 最近或本次邀请的非机密状态。
    pub invitation: InvitationSummary,
    /// 限时一次控制码，禁止记录到日志或普通状态接口。
    pub code: String,
    /// 本次签发原子关闭的旧活动连接；幂等重试为空。
    pub replaced_connection: Option<ConnectionSummary>,
}

/// 从已认证传输连接取得的消费者身份，不接受客户端自报归属。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConsumerIdentity {
    /// 已认证控制进程实例标识。
    pub controller_id: ControllerInstanceId,
    /// 可选的已认证控制端安装身份。
    pub installation_id: Option<Uuid>,
    /// Relay 认证的控制方归属。
    pub owner_id: ControllerOwnerId,
}

/// 不包含恢复凭据的单目标授权连接。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionSummary {
    /// 单目标持久授权连接的唯一标识。
    pub connection_id: Uuid,
    /// 邀请的服务端唯一标识。
    pub invitation_id: Uuid,
    /// 服务端分配的稳定目标标识。
    pub target_id: String,
    /// 所属集成主体的服务端标识。
    pub principal_id: Uuid,
    /// 授权绑定的准确目标、运行与会话。
    pub target: TargetSnapshot,
    /// 从已认证传输取得的控制消费者身份。
    pub consumer: ConsumerIdentity,
    /// 连接修订号，用于关闭该连接时的乐观并发校验。
    pub revision: u64,
    /// 该资源的持久生命周期状态。
    pub state: String,
}

/// 兑换与恢复的结果；恢复令牌仅允许当前单目标连接使用。
#[derive(Serialize)]
pub struct RedeemedConnection {
    /// 单目标持久授权连接的唯一标识。
    pub connection_id: Uuid,
    /// 授权绑定的准确目标、运行与会话。
    pub target: TargetSnapshot,
    /// 从已认证传输取得的控制消费者身份。
    pub consumer: ConsumerIdentity,
    /// 单目标授权连接的非机密状态。
    pub connection: ConnectionSummary,
    /// 绑定消费者与目标的高熵恢复凭据，禁止日志及前端持有。
    pub resume_token: String,
}

/// 资产状态不包含旧短码或任何凭据。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetStatus {
    /// 当前资产登记与修订信息。
    pub asset: Asset,
    /// 最近或本次邀请的非机密状态。
    pub invitation: Option<InvitationSummary>,
    /// 单目标授权连接的非机密状态。
    pub connection: Option<ConnectionSummary>,
}
