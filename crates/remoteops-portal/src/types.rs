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
    /// 管理操作的资源修订号冲突。
    #[error("资源修订号冲突")]
    RevisionConflict,
    /// 管理操作预期的资产归属不匹配。
    #[error("资产归属冲突")]
    OwnershipConflict,
    /// 管理操作幂等键被用于不同请求。
    #[error("管理操作幂等键冲突")]
    IdempotencyConflict,
    /// 目标主体已存在同名资产。
    #[error("目标主体资产名称冲突")]
    NameConflict,
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
    /// 主体权限修订号，旧数据库记录默认从一开始。
    #[serde(default = "initial_revision")]
    pub revision: u64,
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

fn initial_revision() -> u64 {
    1
}

/// 管理员按修订号增删现有主体白名单，不更换主体凭据。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePrincipalAssetsRequest {
    /// 当前主体权限修订号。
    pub expected_revision: u64,
    /// 本次操作唯一幂等键，重试必须保持。
    pub idempotency_key: Uuid,
    /// 要新增的资产键。
    pub add: Vec<String>,
    /// 要移除的资产键，允许移除最后一项。
    pub remove: Vec<String>,
}

/// 管理员在同一个 Relay 所有者下迁移准确资产。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferAssetRequest {
    /// 必须匹配资产当前归属的主体。
    pub expected_principal_id: Uuid,
    /// 迁移后的目标主体，必须有效并允许该资产键。
    pub destination_principal_id: Uuid,
    /// 当前资产修订号。
    pub expected_revision: u64,
    /// 本次操作唯一幂等键，重试必须保持。
    pub idempotency_key: Uuid,
}

/// 管理变更的持久结果；资源字段是原操作快照，当前状态请另行查询。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdminOperation {
    /// 服务端生成的唯一操作标识。
    pub operation_id: Uuid,
    /// 管理变更类型。
    pub kind: String,
    /// pending 表示仍需同步 Relay 运行态，completed 表示同步已完成。
    pub state: String,
    /// 必须精确同步运行态的目标集合。
    pub affected_target_ids: Vec<String>,
    /// 白名单变更前的主体快照。
    pub previous_principal: Option<Principal>,
    /// 白名单变更当时的主体快照。
    pub principal: Option<Principal>,
    /// 迁移前的资产快照。
    pub previous_asset: Option<Asset>,
    /// 迁移当时的资产快照。
    pub asset: Option<Asset>,
    /// 持久变更提交时间。
    pub created_at: DateTime<Utc>,
    /// Relay 完成运行态同步的时间。
    pub completed_at: Option<DateTime<Utc>>,
}

/// 不包含任何凭据的管理操作审计。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdminAuditEvent {
    /// 每次成功或失败尝试的审计标识。
    pub event_id: Uuid,
    /// 对应的持久管理操作；失败尝试没有已提交操作。
    pub operation_id: Option<Uuid>,
    /// 操作的非机密主体或目标标识。
    pub resource: String,
    /// 失败时的脱敏错误分类。
    pub error_code: Option<String>,
    /// 管理认证上下文提供的操作来源，不能使用正文自报身份。
    pub actor: String,
    /// 管理变更类型。
    pub kind: String,
    /// 当前运行态同步结果。
    pub state: String,
    /// 受影响的准确目标集合。
    pub affected_target_ids: Vec<String>,
    /// 白名单变更前的主体快照。
    pub previous_principal: Option<Principal>,
    /// 白名单变更后的主体快照。
    pub principal: Option<Principal>,
    /// 迁移前的资产快照。
    pub previous_asset: Option<Asset>,
    /// 迁移后的资产快照。
    pub asset: Option<Asset>,
    /// 持久变更提交时间。
    pub created_at: DateTime<Utc>,
    /// Relay 完成运行态同步的时间。
    pub completed_at: Option<DateTime<Utc>>,
}
