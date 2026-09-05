use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, anyhow, bail};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use remoteops_domain::{
    AgentInstanceId, ApprovalId, ApprovalState, ConnectionDescriptor, ConnectionState,
    ControllerInstanceId, ControllerOwnerId, EventSource, PairingCode, PairingLease,
    PermissionMode, RemoteOperation, RequestId, SessionId, SessionRole,
};
use remoteops_policy::{DefaultPolicy, PolicyDecision, RiskLevel, approval_operations_match};
use remoteops_protocol::{
    AgentHello, AgentLeaseRenewed, AgentResumeCommitted, AgentWelcome, ApprovalDecision,
    ApprovalRequest, ApprovalResult, AuthorizedRemoteRequest, ClientHello, ControllerBinding,
    ControllerHello, ControllerKind, PROTOCOL_VERSION, PairRequest, PairResult, RelayAuthorization,
    ReleaseSessionRequest, ReleaseSessionResult, RemoteRequest, WireMessage, read_frame,
    write_frame,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Mutex, Notify, mpsc},
    time,
};
use tracing::{info, warn};
use uuid::Uuid;

use crate::admin::{
    AdminActionOutcome, AdminAgent, AdminController, AdminControllerBinding, AdminIdentity,
    AdminOverview, AdminSession, AdminSnapshot,
};

const OUTBOUND_QUEUE_CAPACITY: usize = 256;
const CLIENT_HELLO_TIMEOUT: time::Duration = time::Duration::from_secs(10);
const MAX_REGISTERED_AGENTS: usize = 1024;
const UNPAIRED_AGENT_RETENTION: Duration = Duration::hours(24);
const MAX_AUDIT_EVENTS: usize = 1_000;

/// 有界出站队列。队列耗尽表示客户端持续消费过慢，连接任务会主动结束。
#[derive(Clone)]
struct Sender {
    channel: mpsc::Sender<WireMessage>,
    slow_consumer: Arc<Notify>,
}

impl Sender {
    fn send(&self, message: WireMessage) -> Result<(), ()> {
        match self.channel.try_send(message) {
            Ok(()) => Ok(()),
            Err(error) => {
                if matches!(error, mpsc::error::TrySendError::Full(_)) {
                    self.slow_consumer.notify_one();
                }
                Err(())
            }
        }
    }
}

fn outbound_channel() -> (Sender, mpsc::Receiver<WireMessage>, Arc<Notify>) {
    let (channel, receiver) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
    let slow_consumer = Arc::new(Notify::new());
    (
        Sender {
            channel,
            slow_consumer: slow_consumer.clone(),
        },
        receiver,
        slow_consumer,
    )
}

async fn read_client_hello<R>(
    reader: &mut R,
    timeout: time::Duration,
) -> anyhow::Result<WireMessage>
where
    R: AsyncRead + Unpin,
{
    time::timeout(timeout, read_frame(reader))
        .await
        .context("等待客户端 Hello 超时")?
        .context("读取客户端 Hello 失败")
}

#[derive(Clone)]
struct AgentRecord {
    hello: AgentHello,
    lease: PairingLease,
    resume_token: Option<String>,
    previous_resume_token: Option<PreviousResumeToken>,
    pending_resume: Option<PendingResume>,
    connection_generation: u64,
    ready: bool,
    session_id: SessionId,
    sender: Option<Sender>,
    last_seen: DateTime<Utc>,
    /// 最近一次确认写入状态文件的租约截止时间。
    persisted_lease_expires_at: DateTime<Utc>,
    /// 是否至少成功建立过一次 Controller 配对。
    ever_paired: bool,
    /// 在线租约是否已经过期；恢复身份仍保留等待有效令牌重连。
    lease_expired: bool,
    /// Agent 本地用户选择的会话级权限，不持久化。
    permission_mode: PermissionMode,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
enum PreviousResumeToken {
    Absent,
    Token(String),
}

#[derive(Clone)]
struct PendingResume {
    token: String,
    authenticated_token: Option<String>,
}

struct ControllerRecord {
    sender: Sender,
    sessions: BTreeSet<SessionId>,
    owner_id: ControllerOwnerId,
    kind: ControllerKind,
    connection_generation: u64,
    hostname: Option<String>,
    mac_address: Option<String>,
}

#[derive(Clone)]
struct SessionBinding {
    controller_id: ControllerInstanceId,
    owner_id: ControllerOwnerId,
    controller_kind: ControllerKind,
    controller_generation: u64,
    /// 当前角色请求的最高权限，由 Agent 权限上限进一步裁剪。
    permission_mode: PermissionMode,
    binding_token: String,
}

#[derive(Default)]
struct SessionBindings {
    owner_id: Option<ControllerOwnerId>,
    /// Agent 本地用户选择的权限上限。
    agent_permission_mode: PermissionMode,
    human: Option<SessionBinding>,
    ai: Option<SessionBinding>,
    human_takeover: bool,
}

impl SessionBindings {
    fn permission_mode(&self) -> PermissionMode {
        self.active_controller_kind()
            .map_or(self.agent_permission_mode, |kind| {
                self.permission_mode_for(kind)
            })
    }

    fn permission_mode_for(&self, kind: ControllerKind) -> PermissionMode {
        let permission_mode = self
            .get(kind)
            .map_or(PermissionMode::ApprovalRequired, |binding| {
                minimum_permission_mode(binding.permission_mode, self.agent_permission_mode)
            });
        if kind == ControllerKind::Ai && self.human_takeover {
            PermissionMode::ReadOnly
        } else {
            permission_mode
        }
    }

    fn active_controller_kind(&self) -> Option<ControllerKind> {
        if self.human_takeover && self.human.is_some() {
            Some(ControllerKind::Human)
        } else if self.ai.is_some() {
            Some(ControllerKind::Ai)
        } else if self.human.is_some() {
            Some(ControllerKind::Human)
        } else {
            None
        }
    }

    fn role(&self) -> SessionRole {
        match self.active_controller_kind() {
            Some(ControllerKind::Ai)
                if self.permission_mode_for(ControllerKind::Ai) == PermissionMode::ReadOnly =>
            {
                SessionRole::AiReadOnly
            }
            Some(ControllerKind::Ai) => SessionRole::AiControl,
            Some(ControllerKind::Human) | None => SessionRole::HumanControl,
        }
    }

    fn get(&self, kind: ControllerKind) -> Option<&SessionBinding> {
        match kind {
            ControllerKind::Human => self.human.as_ref(),
            ControllerKind::Ai => self.ai.as_ref(),
        }
    }

    fn get_mut(&mut self, kind: ControllerKind) -> Option<&mut SessionBinding> {
        match kind {
            ControllerKind::Human => self.human.as_mut(),
            ControllerKind::Ai => self.ai.as_mut(),
        }
    }

    fn insert(&mut self, binding: SessionBinding) {
        match binding.controller_kind {
            ControllerKind::Human => self.human = Some(binding),
            ControllerKind::Ai => self.ai = Some(binding),
        }
    }

    fn remove_owned(
        &mut self,
        kind: ControllerKind,
        controller_id: ControllerInstanceId,
        controller_generation: u64,
    ) -> Option<SessionBinding> {
        let slot = match kind {
            ControllerKind::Human => &mut self.human,
            ControllerKind::Ai => &mut self.ai,
        };
        if slot.as_ref().is_some_and(|binding| {
            binding.controller_id == controller_id
                && binding.controller_generation == controller_generation
        }) {
            slot.take()
        } else {
            None
        }
    }

    fn is_empty(&self) -> bool {
        self.human.is_none() && self.ai.is_none()
    }

    fn all(&self) -> impl Iterator<Item = &SessionBinding> {
        self.human.iter().chain(self.ai.iter())
    }
}

fn minimum_permission_mode(left: PermissionMode, right: PermissionMode) -> PermissionMode {
    match (left, right) {
        (PermissionMode::ReadOnly, _) | (_, PermissionMode::ReadOnly) => PermissionMode::ReadOnly,
        (PermissionMode::ControllerApproved, _) | (_, PermissionMode::ControllerApproved) => {
            PermissionMode::ControllerApproved
        }
        (PermissionMode::ApprovalRequired, _) | (_, PermissionMode::ApprovalRequired) => {
            PermissionMode::ApprovalRequired
        }
        (PermissionMode::FullAccess, PermissionMode::FullAccess) => PermissionMode::FullAccess,
    }
}

struct RelayApprovalRecord {
    session_id: SessionId,
    owner_id: ControllerOwnerId,
    operation: RemoteOperation,
    approval_id: ApprovalId,
    state: ApprovalState,
    expires_at: DateTime<Utc>,
    controller_id: ControllerInstanceId,
    controller_generation: u64,
    controller_kind: ControllerKind,
}

#[derive(Clone)]
struct InFlightRequest {
    agent_id: AgentInstanceId,
    session_id: SessionId,
    controller_id: ControllerInstanceId,
    owner_id: ControllerOwnerId,
    controller_generation: u64,
    controller_kind: ControllerKind,
    source: EventSource,
    approval: ApprovalState,
    previous_takeover: Option<bool>,
}

struct AgentRegistration {
    welcome: AgentWelcome,
    connection_generation: u64,
}

#[derive(Default)]
struct RelayState {
    agents: BTreeMap<AgentInstanceId, AgentRecord>,
    pairing_index: BTreeMap<PairingCode, AgentInstanceId>,
    controllers: BTreeMap<ControllerInstanceId, ControllerRecord>,
    session_bindings: BTreeMap<SessionId, SessionBindings>,
    approvals: BTreeMap<ApprovalId, RelayApprovalRecord>,
    in_flight: BTreeMap<RequestId, InFlightRequest>,
    next_controller_generation: u64,
    audit_log: Vec<AdminAuditEvent>,
    admin_password_hash: Option<AdminPasswordHash>,
}

/// 管理页面密码哈希。`algorithm` 缺省时表示可迁移的旧 SHA-256 格式；新值使用 Argon2id。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AdminPasswordHash {
    #[serde(default)]
    algorithm: Option<String>,
    salt: String,
    digest: String,
}

/// 管理审计事件。仅保存操作摘要，不保存认证令牌或绑定令牌。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdminAuditEvent {
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    pub action: String,
    pub target: Option<String>,
    pub success: bool,
    pub source: String,
    pub summary: String,
}

/// Relay 重启后需要恢复的最小 Agent 租约状态。
#[derive(Debug, Deserialize, Serialize)]
struct PersistedAgent {
    /// Agent 身份和能力快照。
    hello: AgentHello,
    /// 控制码和逻辑会话的租约。
    lease: PairingLease,
    /// 最近一次已经确认的 Agent 恢复令牌。
    resume_token: Option<String>,
    /// 恢复令牌两阶段确认期间仍可接受的上一个令牌。
    #[serde(default)]
    previous_resume_token: Option<PreviousResumeToken>,
    /// 跨 Relay 重启保持不变的逻辑会话标识。
    session_id: SessionId,
    /// 传输连接代次。
    connection_generation: u64,
    /// 是否至少成功建立过一次 Controller 配对。
    #[serde(default = "legacy_agent_was_paired")]
    ever_paired: bool,
}

/// Relay 状态文件格式。
#[derive(Debug, Deserialize, Serialize)]
struct PersistedRelayState {
    /// 当前状态文件格式版本。
    version: u16,
    /// 已登记的 Agent。
    agents: Vec<PersistedAgent>,
    #[serde(default)]
    audit_log: Vec<AdminAuditEvent>,
    #[serde(default)]
    admin_password_hash: Option<AdminPasswordHash>,
}

/// Relay 的并发安全业务入口。
pub struct Relay {
    state: Arc<Mutex<RelayState>>,
    state_path: Option<PathBuf>,
    lease_lifetime: Duration,
    approval_lifetime: Duration,
    heartbeat_seconds: u64,
    controller_owner_id: ControllerOwnerId,
    human_controller_token: String,
    ai_controller_token: String,
    policy: DefaultPolicy,
    started_at: DateTime<Utc>,
}

impl Relay {
    /// 创建 Relay。
    #[must_use]
    pub fn new(
        lease_lifetime: Duration,
        heartbeat_seconds: u64,
        controller_owner_id: ControllerOwnerId,
        human_controller_token: String,
        ai_controller_token: String,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(RelayState::default())),
            state_path: None,
            lease_lifetime: if lease_lifetime <= Duration::zero() {
                Duration::seconds(1)
            } else {
                lease_lifetime
            },
            approval_lifetime: Duration::minutes(5),
            heartbeat_seconds: heartbeat_seconds.max(1),
            controller_owner_id,
            human_controller_token,
            ai_controller_token,
            policy: DefaultPolicy::default(),
            started_at: Utc::now(),
        }
    }

    /// 返回管理页面使用的脱敏快照。
    #[allow(clippy::too_many_lines)]
    pub async fn admin_snapshot(&self) -> AdminSnapshot {
        let state = self.state.lock().await;
        let now = Utc::now();
        let agents = state
            .agents
            .values()
            .map(|agent| AdminAgent {
                agent_instance_id: agent.hello.agent_instance_id,
                session_id: agent.session_id,
                hostname: agent.hello.hostname.clone(),
                mac_address: agent.hello.mac_address.clone(),
                operating_system: agent.hello.operating_system.clone(),
                state: if agent.ready && agent.sender.is_some() {
                    "online"
                } else if agent.lease.is_valid_at(now) {
                    "reconnecting"
                } else {
                    "offline"
                }
                .to_owned(),
                pairing_code_configured: agent.lease.is_valid_at(now),
                lease_expires_at: agent.lease.expires_at,
                last_seen: agent.last_seen,
                connection_generation: agent.connection_generation,
                ready: agent.ready,
                ever_paired: agent.ever_paired,
                permission_mode: agent.permission_mode,
            })
            .collect::<Vec<_>>();
        let controllers = state
            .controllers
            .iter()
            .map(|(id, controller)| AdminController {
                controller_instance_id: *id,
                kind: format!("{:?}", controller.kind).to_lowercase(),
                owner_id: controller.owner_id,
                connection_generation: controller.connection_generation,
                session_ids: controller.sessions.iter().copied().collect(),
                hostname: controller.hostname.clone(),
                mac_address: controller.mac_address.clone(),
            })
            .collect::<Vec<_>>();
        let sessions = state
            .agents
            .values()
            .map(|agent| {
                let bindings = state.session_bindings.get(&agent.session_id);
                let controller_bindings = bindings
                    .map(|bindings| {
                        bindings
                            .all()
                            .map(|binding| AdminControllerBinding {
                                controller_instance_id: binding.controller_id,
                                kind: format!("{:?}", binding.controller_kind).to_lowercase(),
                                owner_id: binding.owner_id,
                                permission_mode: bindings
                                    .permission_mode_for(binding.controller_kind),
                                controller_hostname: state
                                    .controllers
                                    .get(&binding.controller_id)
                                    .and_then(|controller| controller.hostname.clone()),
                                controller_mac_address: state
                                    .controllers
                                    .get(&binding.controller_id)
                                    .and_then(|controller| controller.mac_address.clone()),
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                AdminSession {
                    session_id: agent.session_id,
                    agent_instance_id: agent.hello.agent_instance_id,
                    hostname: agent.hello.hostname.clone(),
                    mac_address: agent.hello.mac_address.clone(),
                    operating_system: agent.hello.operating_system.clone(),
                    state: if agent.ready && agent.sender.is_some() {
                        "online"
                    } else if agent.lease.is_valid_at(now) {
                        "reconnecting"
                    } else {
                        "offline"
                    }
                    .to_owned(),
                    role: bindings.map_or("unbound".to_owned(), |value| {
                        format!("{:?}", value.role()).to_lowercase()
                    }),
                    permission_mode: bindings
                        .map_or(agent.permission_mode, SessionBindings::permission_mode),
                    owner_id: bindings.and_then(|value| value.owner_id),
                    controller_bindings,
                    pending_approvals: state
                        .approvals
                        .values()
                        .filter(|approval| {
                            approval.session_id == agent.session_id
                                && approval.state == ApprovalState::Pending
                        })
                        .count(),
                    in_flight_requests: state
                        .in_flight
                        .values()
                        .filter(|request| request.session_id == agent.session_id)
                        .count(),
                    lease_expires_at: agent.lease.expires_at,
                    last_seen: agent.last_seen,
                    connection_generation: agent.connection_generation,
                }
            })
            .collect::<Vec<_>>();
        let identity = AdminIdentity {
            owner_id: self.controller_owner_id,
            human_token_configured: !self.human_controller_token.is_empty(),
            ai_token_configured: !self.ai_controller_token.is_empty(),
            ai_token_fingerprint: token_fingerprint(&self.ai_controller_token),
        };
        let overview = AdminOverview {
            owner_id: self.controller_owner_id,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            uptime_seconds: u64::try_from((now - self.started_at).num_seconds().max(0))
                .unwrap_or_default(),
            online_agents: agents
                .iter()
                .filter(|agent| agent.state == "online")
                .count(),
            active_sessions: sessions
                .iter()
                .filter(|session| {
                    session.state == "online" && !session.controller_bindings.is_empty()
                })
                .count(),
            connected_controllers: controllers.len(),
            pending_approvals: state
                .approvals
                .values()
                .filter(|approval| approval.state == ApprovalState::Pending)
                .count(),
            in_flight_requests: state.in_flight.len(),
        };
        AdminSnapshot {
            overview,
            identity,
            agents,
            controllers,
            sessions,
            audit: state.audit_log.clone(),
        }
    }

    /// 记录管理 API 认证失败，不保存提交的 Token。
    pub async fn admin_auth_failure(&self, source: &str) {
        let mut state = self.state.lock().await;
        append_audit(
            &mut state,
            "admin_login",
            Some("relay_admin".to_owned()),
            false,
            source,
            "管理凭据校验失败",
        );
        // 失败请求来自未认证的调用方；保留有界内存审计，避免每次尝试同步重写完整状态文件。
    }

    /// 记录管理页面登录成功，不保存密码或 Session Cookie。
    pub async fn admin_auth_success(&self, username: &str) {
        let mut state = self.state.lock().await;
        append_audit(
            &mut state,
            "admin_login",
            Some("relay_admin".to_owned()),
            true,
            username,
            "管理页面登录成功",
        );
        if let Err(error) = self.persist_state_locked(&state) {
            warn!(error = %error, "无法持久化管理登录成功审计事件");
        }
    }

    /// 管理员关闭 Session，释放全部 Controller 绑定并清理相关状态。
    pub async fn admin_close_session(
        &self,
        session_id: SessionId,
        source: &str,
    ) -> AdminActionOutcome {
        let (revocations, removals, failures, found) = {
            let mut state = self.state.lock().await;
            let Some(bindings) = state.session_bindings.remove(&session_id) else {
                let exists = state
                    .agents
                    .values()
                    .any(|agent| agent.session_id == session_id);
                append_audit(
                    &mut state,
                    "session_close",
                    Some(session_id.to_string()),
                    exists,
                    source,
                    if exists {
                        "Session 已经没有活动绑定"
                    } else {
                        "Session 不存在"
                    },
                );
                let _ = self.persist_state_locked(&state);
                return AdminActionOutcome {
                    success: exists,
                    changed: false,
                    message: if exists {
                        "Session 已经关闭".to_owned()
                    } else {
                        "Session 不存在".to_owned()
                    },
                };
            };
            let mut revocations = Vec::new();
            let mut removals = Vec::new();
            for binding in bindings.all() {
                if let Some(agent_sender) = state
                    .agents
                    .values()
                    .find(|agent| agent.session_id == session_id && agent.ready)
                    .and_then(|agent| agent.sender.clone())
                {
                    revocations.push((agent_sender, session_id, binding.binding_token.clone()));
                }
                if let Some(controller) = state.controllers.get_mut(&binding.controller_id) {
                    controller.sessions.remove(&session_id);
                    removals.push((controller.sender.clone(), session_id));
                }
            }
            let abandoned = state
                .in_flight
                .iter()
                .filter(|(_, request)| request.session_id == session_id)
                .map(|(id, request)| (*id, request.clone()))
                .collect::<Vec<_>>();
            let mut failures = Vec::new();
            for (request_id, request) in abandoned {
                state.in_flight.remove(&request_id);
                if let Some(controller) = state.controllers.get(&request.controller_id) {
                    failures.push((controller.sender.clone(), request_id));
                }
            }
            state
                .approvals
                .retain(|_, approval| approval.session_id != session_id);
            append_audit(
                &mut state,
                "session_close",
                Some(session_id.to_string()),
                true,
                source,
                "管理员关闭 Session",
            );
            let _ = self.persist_state_locked(&state);
            (revocations, removals, failures, true)
        };
        for (sender, session_id, binding_token) in revocations {
            let _ = sender.send(WireMessage::ControllerBindingRevoked {
                session_id,
                binding_token,
            });
        }
        for (sender, session_id) in removals {
            let _ = sender.send(WireMessage::ConnectionRemoved {
                session_id,
                reason: "管理员关闭 Session".to_owned(),
            });
        }
        for (sender, request_id) in failures {
            let _ = sender.send(WireMessage::Error {
                code: "session_closed".to_owned(),
                message: "Session 已被管理员关闭".to_owned(),
                request_id: Some(request_id),
            });
        }
        AdminActionOutcome {
            success: found,
            changed: true,
            message: "Session 已关闭".to_owned(),
        }
    }

    /// 清理已关闭且长期离线的 Agent 身份记录；在线 Agent 身份始终保留。
    pub async fn admin_purge_closed_sessions(&self, source: &str) -> AdminActionOutcome {
        let mut state = self.state.lock().await;
        let removable_ids = state
            .agents
            .iter()
            .filter(|(_, agent)| {
                agent.lease_expired
                    && agent.sender.is_none()
                    && !state.session_bindings.contains_key(&agent.session_id)
            })
            .map(|(agent_id, _)| *agent_id)
            .collect::<Vec<_>>();
        let removed = removable_ids.len();
        for agent_id in removable_ids {
            if let Some(agent) = state.agents.remove(&agent_id) {
                state.pairing_index.remove(&agent.lease.pairing_code);
            }
        }
        append_audit(
            &mut state,
            "session_purge_closed",
            None,
            true,
            source,
            &format!("清理 {removed} 条已关闭且离线的 Session 记录"),
        );
        if removed > 0 {
            if let Err(error) = self.persist_state_locked(&state) {
                warn!(error = %error, "无法持久化已关闭 Session 清理结果");
            }
        }
        AdminActionOutcome {
            success: true,
            changed: removed > 0,
            message: format!("已清理 {removed} 条已关闭 Session 记录"),
        }
    }

    /// 管理员请求 Agent 紧急停止当前任务。
    #[allow(clippy::too_many_lines)]
    pub async fn admin_emergency_stop(
        &self,
        session_id: SessionId,
        source: &str,
    ) -> AdminActionOutcome {
        let (sender, authorized) = {
            let mut state = self.state.lock().await;
            let Some(agent) = state
                .agents
                .values()
                .find(|agent| {
                    agent.session_id == session_id && agent.ready && agent.sender.is_some()
                })
                .cloned()
            else {
                append_audit(
                    &mut state,
                    "emergency_stop",
                    Some(session_id.to_string()),
                    false,
                    source,
                    "Agent 不在线",
                );
                let _ = self.persist_state_locked(&state);
                return AdminActionOutcome {
                    success: false,
                    changed: false,
                    message: "Agent 不在线".to_owned(),
                };
            };
            let Some(bindings) = state.session_bindings.get(&session_id) else {
                append_audit(
                    &mut state,
                    "emergency_stop",
                    Some(session_id.to_string()),
                    false,
                    source,
                    "Session 没有活动 Controller 绑定",
                );
                let _ = self.persist_state_locked(&state);
                return AdminActionOutcome {
                    success: false,
                    changed: false,
                    message: "Session 没有活动 Controller 绑定".to_owned(),
                };
            };
            let Some(binding) = bindings.all().next().cloned() else {
                return AdminActionOutcome {
                    success: false,
                    changed: false,
                    message: "Session 没有活动 Controller 绑定".to_owned(),
                };
            };
            let request_id = RequestId::new();
            let request = RemoteRequest {
                request_id,
                session_id,
                source: binding.controller_kind.event_source(),
                operation: RemoteOperation::EmergencyStop,
                approval_id: None,
                payload_base64: None,
            };
            let authorized = AuthorizedRemoteRequest {
                request,
                authorization: RelayAuthorization {
                    controller_instance_id: binding.controller_id,
                    owner_id: binding.owner_id,
                    controller_kind: binding.controller_kind,
                    permission_mode: bindings.permission_mode_for(binding.controller_kind),
                    binding_token: binding.binding_token.clone(),
                    approval: ApprovalState::NotRequired,
                },
            };
            let sender = agent.sender.clone().expect("在线 Agent 必须有发送器");
            let stopped = state
                .in_flight
                .iter()
                .filter(|(_, request)| request.session_id == session_id)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            for id in stopped {
                state.in_flight.remove(&id);
            }
            state.in_flight.insert(
                request_id,
                InFlightRequest {
                    agent_id: agent.hello.agent_instance_id,
                    session_id,
                    controller_id: binding.controller_id,
                    owner_id: binding.owner_id,
                    controller_generation: binding.controller_generation,
                    controller_kind: binding.controller_kind,
                    source: binding.controller_kind.event_source(),
                    approval: ApprovalState::NotRequired,
                    previous_takeover: None,
                },
            );
            append_audit(
                &mut state,
                "emergency_stop",
                Some(session_id.to_string()),
                true,
                source,
                "已向 Agent 发送紧急停止请求",
            );
            let _ = self.persist_state_locked(&state);
            (sender, authorized)
        };
        let _ = sender.send(WireMessage::AuthorizedRemoteRequest(authorized));
        AdminActionOutcome {
            success: true,
            changed: true,
            message: "已向 Agent 发送紧急停止请求".to_owned(),
        }
    }

    /// 创建带有持久化状态文件的 Relay。
    ///
    /// 只持久化 Agent 身份、控制码租约、逻辑 `session_id` 和恢复令牌；
    /// Controller 连接、在途请求和审批不会跨进程恢复。
    ///
    /// # Errors
    ///
    /// 当状态文件无法读取、解析或包含无效租约时返回错误。
    pub fn new_with_state_path(
        lease_lifetime: Duration,
        heartbeat_seconds: u64,
        controller_owner_id: ControllerOwnerId,
        human_controller_token: String,
        ai_controller_token: String,
        state_path: impl Into<PathBuf>,
    ) -> anyhow::Result<Self> {
        let state_path = state_path.into();
        let mut relay = Self::new(
            lease_lifetime,
            heartbeat_seconds,
            controller_owner_id,
            human_controller_token,
            ai_controller_token,
        );
        relay.state_path = Some(state_path.clone());
        let mut restored = RelayState::default();
        load_persisted_state(&state_path, &mut restored)?;
        save_persisted_state(&state_path, &restored)?;
        relay.state = Arc::new(Mutex::new(restored));
        Ok(relay)
    }

    fn persist_state_locked(&self, state: &RelayState) -> anyhow::Result<()> {
        let Some(path) = self.state_path.as_deref() else {
            return Ok(());
        };
        save_persisted_state(path, state)
    }

    /// 使用环境变量初始化或强制重置管理页面密码。
    pub async fn initialize_admin_password(
        &self,
        configured_password: Option<&str>,
        force_reset: bool,
    ) -> anyhow::Result<()> {
        let Some(password) = configured_password.filter(|value| !value.is_empty()) else {
            return Ok(());
        };
        validate_admin_password(password).map_err(anyhow::Error::msg)?;
        let mut state = self.state.lock().await;
        if force_reset || state.admin_password_hash.is_none() {
            state.admin_password_hash = Some(hash_admin_password(password));
            self.persist_state_locked(&state)?;
        }
        Ok(())
    }

    /// 验证管理页面密码。
    pub async fn admin_password_matches(&self, password: &str) -> bool {
        let mut state = self.state.lock().await;
        let Some(stored) = state.admin_password_hash.as_ref() else {
            return false;
        };
        let valid = verify_admin_password(password, stored);
        if valid && stored.algorithm.is_none() {
            state.admin_password_hash = Some(hash_admin_password(password));
            if let Err(error) = self.persist_state_locked(&state) {
                warn!(error = %error, "无法持久化管理密码 Argon2id 迁移");
            }
        }
        valid
    }

    /// 判断是否已经存在持久化的管理密码哈希。
    pub async fn admin_password_configured(&self) -> bool {
        self.state.lock().await.admin_password_hash.is_some()
    }

    /// 更新管理页面密码并持久化哈希。
    pub async fn admin_change_password(
        &self,
        current_password: &str,
        new_password: &str,
        source: &str,
        fallback_password: Option<&str>,
    ) -> AdminActionOutcome {
        let mut state = self.state.lock().await;
        let valid = state
            .admin_password_hash
            .as_ref()
            .is_some_and(|stored| verify_admin_password(current_password, stored))
            || (state.admin_password_hash.is_none()
                && fallback_password.is_some_and(|configured| {
                    constant_time_bytes_eq(current_password.as_bytes(), configured.as_bytes())
                }));
        if !valid {
            append_audit(
                &mut state,
                "admin_password_change",
                Some("relay_admin".to_owned()),
                false,
                source,
                "当前管理密码校验失败",
            );
            let _ = self.persist_state_locked(&state);
            return AdminActionOutcome {
                success: false,
                changed: false,
                message: "当前密码不正确".to_owned(),
            };
        }
        if let Err(error) = validate_admin_password(new_password) {
            append_audit(
                &mut state,
                "admin_password_change",
                Some("relay_admin".to_owned()),
                false,
                source,
                &error,
            );
            let _ = self.persist_state_locked(&state);
            return AdminActionOutcome {
                success: false,
                changed: false,
                message: error,
            };
        }
        state.admin_password_hash = Some(hash_admin_password(new_password));
        append_audit(
            &mut state,
            "admin_password_change",
            Some("relay_admin".to_owned()),
            true,
            source,
            "管理页面密码已修改，所有登录 Session 已失效",
        );
        if let Err(error) = self.persist_state_locked(&state) {
            warn!(error = %error, "无法持久化管理密码修改");
            return AdminActionOutcome {
                success: false,
                changed: false,
                message: "密码修改失败，无法写入 Relay 状态文件".to_owned(),
            };
        }
        AdminActionOutcome {
            success: true,
            changed: true,
            message: "密码修改成功，请使用新密码重新登录".to_owned(),
        }
    }

    /// 启动过期租约清理任务。
    pub fn spawn_cleanup(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut ticker = time::interval(time::Duration::from_secs(1));
            loop {
                ticker.tick().await;
                self.cleanup_expired().await;
            }
        });
    }

    /// 处理一个已完成 TLS 握手的客户端。
    pub async fn handle_client<S>(&self, stream: S) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let hello = read_client_hello(&mut reader, CLIENT_HELLO_TIMEOUT).await?;
        let (sender, mut receiver, connection_closed) = outbound_channel();
        let writer_closed = connection_closed.clone();
        let writer_task = tokio::spawn(async move {
            while let Some(message) = receiver.recv().await {
                if write_frame(&mut writer, &message).await.is_err() {
                    break;
                }
            }
            writer_closed.notify_one();
        });

        match hello {
            WireMessage::Hello(ClientHello::Agent(hello)) => {
                self.handle_agent(hello, sender, connection_closed, &mut reader)
                    .await?;
            }
            WireMessage::Hello(ClientHello::Controller(hello)) => {
                self.handle_controller(hello, sender, connection_closed, &mut reader)
                    .await?;
            }
            _ => bail!("第一条消息必须是 Agent 或 Controller Hello"),
        }
        writer_task.abort();
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_agent<R>(
        &self,
        hello: AgentHello,
        sender: Sender,
        connection_closed: Arc<Notify>,
        reader: &mut R,
    ) -> anyhow::Result<()>
    where
        R: AsyncRead + Unpin,
    {
        if hello.protocol_version != PROTOCOL_VERSION {
            bail!("Agent 协议版本不兼容：{}", hello.protocol_version);
        }
        let agent_id = hello.agent_instance_id;
        let registration = self.register_agent(hello, sender.clone()).await?;
        let connection_generation = registration.connection_generation;
        sender
            .send(WireMessage::AgentWelcome(registration.welcome))
            .map_err(|()| anyhow!("Agent 连接已关闭"))?;
        info!(
            agent_instance_id = %agent_id,
            connection_generation,
            "Agent 已注册，等待恢复令牌确认"
        );

        loop {
            let message = tokio::select! {
                biased;
                () = connection_closed.notified() => {
                    warn!(agent_instance_id = %agent_id, "Agent 写连接已关闭或消费过慢");
                    break;
                }
                result = read_frame::<WireMessage, _>(reader) => result,
            };
            match message {
                Ok(WireMessage::AgentWelcomeAck(ack))
                    if ack.connection_generation == connection_generation =>
                {
                    if self
                        .commit_agent_resume(agent_id, connection_generation)
                        .await
                    {
                        let _ =
                            sender.send(WireMessage::AgentResumeCommitted(AgentResumeCommitted {
                                connection_generation,
                            }));
                    }
                }
                Ok(WireMessage::AgentResumeCommitAck(ack))
                    if ack.connection_generation == connection_generation =>
                {
                    if let Some((update, bindings)) = self
                        .finalize_agent_resume(agent_id, connection_generation)
                        .await
                    {
                        for binding in bindings {
                            let _ = sender.send(WireMessage::ControllerBinding(binding));
                        }
                        self.forward_connection_update(update).await;
                        info!(
                            agent_instance_id = %agent_id,
                            connection_generation,
                            "Agent 恢复令牌已确认，连接可用"
                        );
                    }
                }
                Ok(WireMessage::Heartbeat { .. }) => {
                    if let Some(lease_expires_at) =
                        self.renew_agent(agent_id, connection_generation).await
                    {
                        let _ = sender.send(WireMessage::AgentLeaseRenewed(AgentLeaseRenewed {
                            lease_expires_at,
                        }));
                        let _ = sender.send(WireMessage::HeartbeatAck {
                            received_at: Utc::now(),
                        });
                    }
                }
                Ok(WireMessage::AgentPermissionModeChanged(update)) => {
                    self.update_agent_permission_mode(
                        agent_id,
                        connection_generation,
                        update.permission_mode,
                    )
                    .await;
                }
                Ok(message @ (WireMessage::RemoteEvent(_) | WireMessage::RemoteResponse(_))) => {
                    self.forward_agent_message(agent_id, connection_generation, message)
                        .await;
                }
                Ok(WireMessage::Error {
                    code,
                    message,
                    request_id,
                }) => {
                    self.forward_agent_message(
                        agent_id,
                        connection_generation,
                        WireMessage::Error {
                            code,
                            message,
                            request_id,
                        },
                    )
                    .await;
                }
                Ok(_) => {
                    warn!(agent_instance_id = %agent_id, "忽略 Agent 不允许发送的消息");
                }
                Err(_) => break,
            }
        }
        self.mark_agent_disconnected(agent_id, connection_generation)
            .await;
        info!(
            agent_instance_id = %agent_id,
            connection_generation,
            "Agent 传输连接已断开，等待租约内恢复"
        );
        Ok(())
    }

    async fn handle_controller<R>(
        &self,
        hello: ControllerHello,
        sender: Sender,
        connection_closed: Arc<Notify>,
        reader: &mut R,
    ) -> anyhow::Result<()>
    where
        R: AsyncRead + Unpin,
    {
        if hello.protocol_version != PROTOCOL_VERSION {
            bail!("Controller 协议版本不兼容：{}", hello.protocol_version);
        }
        self.authenticate_controller(&hello)?;
        let controller_id = hello.controller_instance_id;
        let owner_id = hello.owner_id;
        let controller_kind = hello.kind;
        let connection_generation = self
            .register_controller(
                controller_id,
                owner_id,
                controller_kind,
                hello.hostname.clone(),
                hello.mac_address.clone(),
                sender.clone(),
            )
            .await?;
        sender
            .send(WireMessage::ControllerWelcome {
                protocol_version: PROTOCOL_VERSION,
            })
            .map_err(|()| anyhow!("Controller 连接已关闭"))?;
        info!(
            controller_instance_id = %controller_id,
            owner_id = %owner_id,
            ?controller_kind,
            connection_generation,
            "Controller 已认证并连接"
        );

        loop {
            let message = tokio::select! {
                biased;
                () = connection_closed.notified() => {
                    warn!(controller_instance_id = %controller_id, "Controller 写连接已关闭或消费过慢");
                    break;
                }
                result = read_frame::<WireMessage, _>(reader) => result,
            };
            match message {
                Ok(WireMessage::PairRequest(request)) => {
                    let result = self
                        .pair_controller(controller_id, connection_generation, request)
                        .await;
                    let _ = sender.send(WireMessage::PairResult(result));
                }
                Ok(WireMessage::ReleaseSessionRequest(request)) => {
                    let result = self
                        .release_controller_session(controller_id, connection_generation, request)
                        .await;
                    let _ = sender.send(WireMessage::ReleaseSessionResult(result));
                }
                Ok(WireMessage::ApprovalRequest(request)) => {
                    let result = self
                        .request_approval(controller_id, connection_generation, request)
                        .await;
                    let _ = sender.send(WireMessage::ApprovalResult(result));
                }
                Ok(WireMessage::ApprovalDecision(decision)) => {
                    let result = self
                        .decide_approval(controller_id, connection_generation, decision)
                        .await;
                    let _ = sender.send(WireMessage::ApprovalResult(result));
                }
                Ok(WireMessage::RemoteRequest(request)) => {
                    self.forward_controller_request(
                        controller_id,
                        connection_generation,
                        request,
                        &sender,
                    )
                    .await;
                }
                Ok(WireMessage::Heartbeat { .. }) => {
                    let _ = sender.send(WireMessage::HeartbeatAck {
                        received_at: Utc::now(),
                    });
                }
                Ok(_) => {
                    warn!(controller_instance_id = %controller_id, "忽略 Controller 不允许发送的消息");
                }
                Err(_) => break,
            }
        }
        self.remove_controller(controller_id, connection_generation)
            .await;
        info!(
            controller_instance_id = %controller_id,
            connection_generation,
            "Controller 已断开"
        );
        Ok(())
    }

    fn authenticate_controller(&self, hello: &ControllerHello) -> anyhow::Result<()> {
        if hello.owner_id != self.controller_owner_id {
            bail!("Controller Owner 身份不匹配");
        }
        let expected = match hello.kind {
            ControllerKind::Human => &self.human_controller_token,
            ControllerKind::Ai => &self.ai_controller_token,
        };
        if !constant_time_eq(hello.auth_token.as_bytes(), expected.as_bytes()) {
            bail!("Controller 认证失败");
        }
        Ok(())
    }

    async fn register_controller(
        &self,
        controller_id: ControllerInstanceId,
        owner_id: ControllerOwnerId,
        kind: ControllerKind,
        hostname: Option<String>,
        mac_address: Option<String>,
        sender: Sender,
    ) -> anyhow::Result<u64> {
        let mut state = self.state.lock().await;
        if state.controllers.contains_key(&controller_id) {
            bail!("相同 controller_instance_id 已有活动连接");
        }
        state.next_controller_generation = state
            .next_controller_generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("Controller 连接代次已经耗尽"))?;
        let connection_generation = state.next_controller_generation;
        state.controllers.insert(
            controller_id,
            ControllerRecord {
                sender,
                sessions: BTreeSet::new(),
                owner_id,
                kind,
                connection_generation,
                hostname,
                mac_address,
            },
        );
        Ok(connection_generation)
    }

    #[allow(clippy::too_many_lines)]
    async fn request_approval(
        &self,
        controller_id: ControllerInstanceId,
        controller_generation: u64,
        request: ApprovalRequest,
    ) -> ApprovalResult {
        let mut state = self.state.lock().await;
        let Some(controller) = active_controller(&state, controller_id, controller_generation)
        else {
            return approval_failure(
                request.request_id,
                request.session_id,
                request.operation,
                "Controller 连接已经关闭或被替换",
            );
        };
        if !controller.sessions.contains(&request.session_id) {
            return approval_failure(
                request.request_id,
                request.session_id,
                request.operation,
                "Controller 未绑定该 session_id",
            );
        }
        let controller_kind = controller.kind;
        let controller_owner_id = controller.owner_id;
        let Some(binding) = state
            .session_bindings
            .get(&request.session_id)
            .and_then(|bindings| bindings.get(controller_kind))
        else {
            return approval_failure(
                request.request_id,
                request.session_id,
                request.operation,
                "Controller 的角色绑定不存在",
            );
        };
        if binding.controller_id != controller_id
            || binding.controller_generation != controller_generation
            || binding.owner_id != controller_owner_id
        {
            return approval_failure(
                request.request_id,
                request.session_id,
                request.operation,
                "Controller 的角色绑定已经被替换",
            );
        }
        let operation = normalize_operation(&self.policy, request.operation);
        let permission_mode = state
            .session_bindings
            .get(&request.session_id)
            .map_or(PermissionMode::ApprovalRequired, |bindings| {
                bindings.permission_mode_for(controller_kind)
            });
        if controller_kind == ControllerKind::Ai
            && state
                .session_bindings
                .get(&request.session_id)
                .is_some_and(|bindings| bindings.human_takeover)
            && !ai_operation_allowed_after_takeover(&self.policy, &operation)
        {
            return approval_failure(
                request.request_id,
                request.session_id,
                operation,
                "人工已接管该会话，AI 只能继续执行只读操作",
            );
        }
        if controller_kind == ControllerKind::Ai
            && matches!(operation, RemoteOperation::HumanTakeover)
        {
            return approval_failure(
                request.request_id,
                request.session_id,
                operation,
                "只有人工 Controller 可以接管会话",
            );
        }
        match self.policy.evaluate_with_mode(
            permission_mode,
            controller_kind.event_source(),
            &operation,
        ) {
            PolicyDecision::Allow => ApprovalResult {
                request_id: request.request_id,
                session_id: request.session_id,
                operation,
                approval_id: None,
                state: ApprovalState::NotRequired,
                reason: "该操作不需要人工审批".to_owned(),
                expires_at: None,
            },
            PolicyDecision::Deny { reason } => ApprovalResult {
                request_id: request.request_id,
                session_id: request.session_id,
                operation,
                approval_id: None,
                state: ApprovalState::Rejected,
                reason,
                expires_at: None,
            },
            PolicyDecision::RequireApproval { reason, .. } => {
                let approval_id = ApprovalId::new();
                let expires_at = Utc::now() + self.approval_lifetime;
                state.approvals.insert(
                    approval_id,
                    RelayApprovalRecord {
                        session_id: request.session_id,
                        owner_id: controller_owner_id,
                        operation: operation.clone(),
                        approval_id,
                        state: ApprovalState::Pending,
                        expires_at,
                        controller_id,
                        controller_generation,
                        controller_kind,
                    },
                );
                ApprovalResult {
                    request_id: request.request_id,
                    session_id: request.session_id,
                    operation,
                    approval_id: Some(approval_id),
                    state: ApprovalState::Pending,
                    reason,
                    expires_at: Some(expires_at),
                }
            }
        }
    }

    async fn decide_approval(
        &self,
        controller_id: ControllerInstanceId,
        controller_generation: u64,
        decision: ApprovalDecision,
    ) -> ApprovalResult {
        let mut state = self.state.lock().await;
        let Some(controller) = active_controller(&state, controller_id, controller_generation)
        else {
            return approval_failure(
                decision.request_id,
                decision.session_id,
                decision.operation,
                "Controller 连接已经关闭或被替换",
            );
        };
        if controller.kind != ControllerKind::Human {
            return approval_failure(
                decision.request_id,
                decision.session_id,
                decision.operation,
                "只有已认证的人工 Controller 可以作出审批决定",
            );
        }
        let controller_owner_id = controller.owner_id;
        if !controller.sessions.contains(&decision.session_id) {
            return approval_failure(
                decision.request_id,
                decision.session_id,
                decision.operation,
                "人工 Controller 未绑定该 session_id",
            );
        }
        let human_binding_matches = state
            .session_bindings
            .get(&decision.session_id)
            .and_then(|bindings| bindings.get(ControllerKind::Human))
            .is_some_and(|binding| {
                binding.controller_id == controller_id
                    && binding.controller_generation == controller_generation
                    && binding.owner_id == controller_owner_id
            });
        if !human_binding_matches {
            return approval_failure(
                decision.request_id,
                decision.session_id,
                decision.operation,
                "人工 Controller 的角色绑定不存在或已经被替换",
            );
        }
        let normalized_operation = normalize_operation(&self.policy, decision.operation);
        let Some(record) = state.approvals.get_mut(&decision.approval_id) else {
            return approval_failure(
                decision.request_id,
                decision.session_id,
                normalized_operation,
                "审批不存在、已过期或已经消费",
            );
        };
        if record.session_id != decision.session_id || record.operation != normalized_operation {
            return approval_failure(
                decision.request_id,
                decision.session_id,
                normalized_operation,
                "审批与 session_id 或操作不匹配",
            );
        }
        if record.owner_id != controller_owner_id {
            return approval_failure(
                decision.request_id,
                decision.session_id,
                normalized_operation,
                "审批与当前 Controller Owner 不匹配",
            );
        }
        let now = Utc::now();
        if record.expires_at <= now {
            record.state = ApprovalState::Expired;
        } else if record.state == ApprovalState::Pending {
            record.state = if decision.approved {
                ApprovalState::Approved
            } else {
                ApprovalState::Rejected
            };
        }
        ApprovalResult {
            request_id: decision.request_id,
            session_id: record.session_id,
            operation: record.operation.clone(),
            approval_id: Some(record.approval_id),
            state: record.state,
            reason: match record.state {
                ApprovalState::Approved => "人工已批准该操作".to_owned(),
                ApprovalState::Rejected => "人工已拒绝该操作".to_owned(),
                ApprovalState::Expired => "审批已经过期".to_owned(),
                ApprovalState::Pending => "审批仍在等待人工决定".to_owned(),
                ApprovalState::NotRequired => "该操作不需要审批".to_owned(),
            },
            expires_at: Some(record.expires_at),
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn register_agent(
        &self,
        hello: AgentHello,
        sender: Sender,
    ) -> anyhow::Result<AgentRegistration> {
        let now = Utc::now();
        let mut state = self.state.lock().await;
        let agent_id = hello.agent_instance_id;
        if state.agents.contains_key(&agent_id) {
            let original = state
                .agents
                .get(&agent_id)
                .expect("Agent 已在同一锁内确认存在")
                .clone();
            let (welcome, connection_generation, previous_sender) = {
                let existing = state
                    .agents
                    .get_mut(&agent_id)
                    .expect("Agent 已在同一锁内确认存在");
                if !resume_token_matches(existing, hello.resume_token.as_ref()) {
                    return Err(anyhow!("Agent 恢复令牌无效"));
                }
                if hello.resume_token == existing.resume_token {
                    existing.previous_resume_token = None;
                }
                let previous_sender = existing.sender.replace(sender);
                existing.connection_generation = existing
                    .connection_generation
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("Agent 连接代次已经耗尽"))?;
                existing.hello = hello;
                existing.last_seen = now;
                existing.lease.renew(now, self.lease_lifetime);
                existing.lease_expired = false;
                existing.ready = false;
                let proposed_resume_token = new_secret_token();
                existing.pending_resume = Some(PendingResume {
                    token: proposed_resume_token.clone(),
                    authenticated_token: existing.hello.resume_token.clone(),
                });
                let welcome = AgentWelcome {
                    pairing_code: existing.lease.pairing_code.clone(),
                    lease_expires_at: existing.lease.expires_at,
                    resume_token: proposed_resume_token,
                    connection_generation: existing.connection_generation,
                    heartbeat_interval_seconds: self.heartbeat_seconds,
                };
                (welcome, existing.connection_generation, previous_sender)
            };
            if let Err(error) = self.persist_state_locked(&state) {
                state.agents.insert(agent_id, original);
                return Err(error.context("保存 Agent 重连状态失败"));
            }
            if let Some(existing) = state.agents.get_mut(&agent_id) {
                existing.persisted_lease_expires_at = existing.lease.expires_at;
            }
            if let Some(previous_sender) = previous_sender {
                let _ = previous_sender.send(WireMessage::Error {
                    code: "connection_superseded".to_owned(),
                    message: "Agent 已建立更新的传输连接".to_owned(),
                    request_id: None,
                });
            }
            return Ok(AgentRegistration {
                welcome,
                connection_generation,
            });
        }

        ensure_agent_capacity(state.agents.len())?;
        let pairing_code = unique_pairing_code(&state)?;
        let resume_token = new_secret_token();
        let session_id = SessionId::new();
        let lease = PairingLease::new(agent_id, pairing_code.clone(), now, self.lease_lifetime);
        let welcome = AgentWelcome {
            pairing_code: pairing_code.clone(),
            lease_expires_at: lease.expires_at,
            resume_token: resume_token.clone(),
            connection_generation: 1,
            heartbeat_interval_seconds: self.heartbeat_seconds,
        };
        let persisted_lease_expires_at = lease.expires_at;
        state.pairing_index.insert(pairing_code.clone(), agent_id);
        state.agents.insert(
            agent_id,
            AgentRecord {
                hello,
                lease,
                resume_token: None,
                previous_resume_token: None,
                pending_resume: Some(PendingResume {
                    token: resume_token,
                    authenticated_token: None,
                }),
                connection_generation: 1,
                ready: false,
                session_id,
                sender: Some(sender),
                last_seen: now,
                persisted_lease_expires_at,
                ever_paired: false,
                lease_expired: false,
                permission_mode: PermissionMode::ApprovalRequired,
            },
        );
        if let Err(error) = self.persist_state_locked(&state) {
            state.agents.remove(&agent_id);
            state.pairing_index.remove(&pairing_code);
            return Err(error.context("保存新 Agent 注册状态失败"));
        }
        Ok(AgentRegistration {
            welcome,
            connection_generation: 1,
        })
    }

    async fn commit_agent_resume(
        &self,
        agent_id: AgentInstanceId,
        connection_generation: u64,
    ) -> bool {
        let mut state = self.state.lock().await;
        let (original_previous, original_resume) = {
            let Some(agent) = state.agents.get_mut(&agent_id) else {
                return false;
            };
            if agent.connection_generation != connection_generation || agent.ready {
                return false;
            }
            let Some(pending) = agent.pending_resume.as_ref() else {
                return false;
            };
            let original_previous = agent.previous_resume_token.clone();
            let original_resume = agent.resume_token.clone();
            agent.previous_resume_token = Some(match pending.authenticated_token.clone() {
                Some(token) => PreviousResumeToken::Token(token),
                None => PreviousResumeToken::Absent,
            });
            agent.resume_token = Some(pending.token.clone());
            (original_previous, original_resume)
        };
        if let Err(error) = self.persist_state_locked(&state) {
            if let Some(agent) = state.agents.get_mut(&agent_id)
                && agent.connection_generation == connection_generation
            {
                agent.previous_resume_token = original_previous;
                agent.resume_token = original_resume;
            }
            warn!(
                agent_instance_id = %agent_id,
                connection_generation,
                error = %error,
                "Relay 无法持久化恢复令牌，拒绝确认 Agent 恢复"
            );
            return false;
        }
        true
    }

    async fn finalize_agent_resume(
        &self,
        agent_id: AgentInstanceId,
        connection_generation: u64,
    ) -> Option<(ConnectionDescriptor, Vec<ControllerBinding>)> {
        let mut state = self.state.lock().await;
        let (session_id, mut update) = {
            let agent = state.agents.get_mut(&agent_id)?;
            if agent.connection_generation != connection_generation
                || agent.pending_resume.is_none()
                || agent.resume_token.as_ref()
                    != agent.pending_resume.as_ref().map(|pending| &pending.token)
            {
                return None;
            }
            agent.previous_resume_token = None;
            agent.pending_resume = None;
            agent.ready = true;
            (agent.session_id, descriptor(agent, ConnectionState::Online))
        };
        if let Err(error) = self.persist_state_locked(&state) {
            warn!(
                agent_instance_id = %agent_id,
                connection_generation,
                error = %error,
                "Relay 无法清理已完成恢复握手的旧令牌；当前连接仍保持可用"
            );
        }
        let bindings = state
            .session_bindings
            .get(&session_id)
            .map(|bindings| {
                bindings
                    .all()
                    .map(|binding| {
                        controller_binding(
                            session_id,
                            binding,
                            bindings.permission_mode_for(binding.controller_kind),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(bindings) = state.session_bindings.get(&session_id) {
            apply_session_bindings(&mut update, bindings);
        }
        Some((update, bindings))
    }

    async fn renew_agent(
        &self,
        agent_id: AgentInstanceId,
        connection_generation: u64,
    ) -> Option<DateTime<Utc>> {
        let now = Utc::now();
        let mut state = self.state.lock().await;
        let minimum_interval = Duration::seconds(
            i64::try_from((self.heartbeat_seconds / 2).max(1)).unwrap_or(i64::MAX),
        );
        let (
            original_last_seen,
            original_lease,
            original_lease_expired,
            should_persist,
            lease_expires_at,
        ) = {
            let agent = state.agents.get_mut(&agent_id)?;
            if agent.connection_generation != connection_generation || !agent.ready {
                return None;
            }
            if now - agent.last_seen < minimum_interval {
                return None;
            }
            let original_last_seen = agent.last_seen;
            let original_lease = agent.lease.clone();
            let original_lease_expired = agent.lease_expired;
            agent.last_seen = now;
            agent.lease.renew(now, self.lease_lifetime);
            agent.lease_expired = false;
            let lease_expires_at = agent.lease.expires_at;
            let persistence_reserve = self.lease_lifetime / 2;
            let should_persist = agent.persisted_lease_expires_at - now <= persistence_reserve;
            (
                original_last_seen,
                original_lease,
                original_lease_expired,
                should_persist,
                lease_expires_at,
            )
        };
        if !should_persist {
            return Some(lease_expires_at);
        }
        if let Err(error) = self.persist_state_locked(&state) {
            if let Some(agent) = state.agents.get_mut(&agent_id)
                && agent.connection_generation == connection_generation
            {
                agent.last_seen = original_last_seen;
                agent.lease = original_lease;
                agent.lease_expired = original_lease_expired;
            }
            warn!(
                agent_instance_id = %agent_id,
                connection_generation,
                error = %error,
                "Relay 无法持久化心跳租约，拒绝确认续租"
            );
            return None;
        }
        if let Some(agent) = state.agents.get_mut(&agent_id)
            && agent.connection_generation == connection_generation
        {
            agent.persisted_lease_expires_at = agent.lease.expires_at;
        }
        Some(lease_expires_at)
    }

    #[allow(clippy::too_many_lines)]
    async fn pair_controller(
        &self,
        controller_id: ControllerInstanceId,
        controller_generation: u64,
        request: PairRequest,
    ) -> PairResult {
        let now = Utc::now();
        let mut state = self.state.lock().await;
        let Some(agent_id) = state.pairing_index.get(&request.pairing_code).copied() else {
            return PairResult {
                request_id: request.request_id,
                connection: None,
                error: Some("控制码不存在或已经过期".to_owned()),
            };
        };
        let Some(agent) = state.agents.get(&agent_id) else {
            return PairResult {
                request_id: request.request_id,
                connection: None,
                error: Some("Agent 不存在".to_owned()),
            };
        };
        if !agent.lease.is_valid_at(now) {
            return PairResult {
                request_id: request.request_id,
                connection: None,
                error: Some("控制码租约已经过期".to_owned()),
            };
        }
        if agent.sender.is_none() || !agent.ready {
            return PairResult {
                request_id: request.request_id,
                connection: None,
                error: Some("Agent 当前离线，仍在等待恢复".to_owned()),
            };
        }
        let session_id = agent.session_id;
        let agent_permission_mode = agent.permission_mode;
        let mut connection = descriptor(agent, ConnectionState::Online);
        let agent_sender = agent.sender.clone();
        let Some(controller) = state.controllers.get(&controller_id) else {
            return PairResult {
                request_id: request.request_id,
                connection: None,
                error: Some("Controller 连接已经关闭".to_owned()),
            };
        };
        if controller.connection_generation != controller_generation {
            return PairResult {
                request_id: request.request_id,
                connection: None,
                error: Some("Controller 连接已经被替换".to_owned()),
            };
        }
        let controller_kind = controller.kind;
        let owner_id = controller.owner_id;
        if request.permission_mode == PermissionMode::ControllerApproved
            && controller_kind != ControllerKind::Ai
        {
            return PairResult {
                request_id: request.request_id,
                connection: None,
                error: Some("ControllerApproved 只允许已认证 AI Controller 使用".to_owned()),
            };
        }
        let (binding_messages, replaced_binding) = {
            let bindings = state.session_bindings.entry(session_id).or_default();
            if bindings
                .owner_id
                .is_some_and(|bound_owner_id| bound_owner_id != owner_id)
            {
                return PairResult {
                    request_id: request.request_id,
                    connection: None,
                    error: Some("该 Agent Session 已绑定其他 Controller Owner".to_owned()),
                };
            }
            if bindings.owner_id.is_none() {
                bindings.owner_id = Some(owner_id);
                bindings.agent_permission_mode = agent_permission_mode;
            }

            let replaced_binding = bindings
                .get(controller_kind)
                .filter(|existing| {
                    existing.controller_id != controller_id
                        || existing.controller_generation != controller_generation
                        || existing.owner_id != owner_id
                })
                .cloned();
            if let Some(existing) = &replaced_binding {
                let _ = bindings.remove_owned(
                    controller_kind,
                    existing.controller_id,
                    existing.controller_generation,
                );
            }
            if bindings.get(controller_kind).is_none() {
                bindings.insert(SessionBinding {
                    controller_id,
                    owner_id,
                    controller_kind,
                    controller_generation,
                    permission_mode: request.permission_mode,
                    binding_token: new_secret_token(),
                });
            } else if let Some(binding) = bindings.get_mut(controller_kind) {
                binding.permission_mode = request.permission_mode;
            }

            (
                bindings
                    .all()
                    .map(|binding| {
                        WireMessage::ControllerBinding(controller_binding(
                            session_id,
                            binding,
                            bindings.permission_mode_for(binding.controller_kind),
                        ))
                    })
                    .collect::<Vec<_>>(),
                replaced_binding,
            )
        };
        let replaced_controller_sender = replaced_binding.as_ref().and_then(|binding| {
            let controller = state.controllers.get_mut(&binding.controller_id)?;
            if controller.connection_generation != binding.controller_generation {
                return None;
            }
            controller.sessions.remove(&session_id);
            Some(controller.sender.clone())
        });
        if let Some(binding) = &replaced_binding {
            state.approvals.retain(|_, approval| {
                approval.session_id != session_id
                    || approval.controller_id != binding.controller_id
                    || approval.controller_generation != binding.controller_generation
            });
            state.in_flight.retain(|_, in_flight| {
                in_flight.session_id != session_id
                    || in_flight.controller_id != binding.controller_id
                    || in_flight.controller_generation != binding.controller_generation
            });
        }
        if let Some(bindings) = state.session_bindings.get(&session_id) {
            apply_session_bindings(&mut connection, bindings);
        }
        state
            .controllers
            .get_mut(&controller_id)
            .expect("Controller 已在同一锁内验证")
            .sessions
            .insert(session_id);
        send_session_connection_update(
            &state,
            session_id,
            Some((controller_id, controller_generation)),
        );
        if let Some(agent) = state.agents.get_mut(&agent_id) {
            agent.ever_paired = true;
        }
        if let Err(error) = self.persist_state_locked(&state) {
            warn!(agent_instance_id = %agent_id, error = %error, "Relay 无法持久化 Agent 已配对标记");
        }
        if let Some(sender) = agent_sender {
            if let Some(binding) = &replaced_binding {
                let _ = sender.send(WireMessage::ControllerBindingRevoked {
                    session_id,
                    binding_token: binding.binding_token.clone(),
                });
            }
            for message in binding_messages {
                let _ = sender.send(message);
            }
        }
        if let Some(sender) = replaced_controller_sender {
            let _ = sender.send(WireMessage::ConnectionRemoved {
                session_id,
                reason: "连接已由同一 Owner 的新 Controller 实例接管".to_owned(),
            });
        }
        PairResult {
            request_id: request.request_id,
            connection: Some(connection),
            error: None,
        }
    }

    async fn release_controller_session(
        &self,
        controller_id: ControllerInstanceId,
        controller_generation: u64,
        request: ReleaseSessionRequest,
    ) -> ReleaseSessionResult {
        let mut state = self.state.lock().await;
        let Some(controller) = active_controller(&state, controller_id, controller_generation)
        else {
            return ReleaseSessionResult {
                request_id: request.request_id,
                session_id: request.session_id,
                released: false,
                error: Some("Controller 连接已经关闭或被替换".to_owned()),
            };
        };
        let controller_kind = controller.kind;
        let removed_binding = state
            .session_bindings
            .get_mut(&request.session_id)
            .and_then(|bindings| {
                bindings.remove_owned(controller_kind, controller_id, controller_generation)
            });
        if let Some(controller) = state.controllers.get_mut(&controller_id)
            && controller.connection_generation == controller_generation
        {
            controller.sessions.remove(&request.session_id);
        }
        if let Some(bindings) = state.session_bindings.get_mut(&request.session_id) {
            if controller_kind == ControllerKind::Human {
                bindings.human_takeover = false;
            }
            if bindings.is_empty() {
                state.session_bindings.remove(&request.session_id);
            }
        }
        state.approvals.retain(|_, approval| {
            approval.session_id != request.session_id
                || approval.controller_id != controller_id
                || approval.controller_generation != controller_generation
        });
        state.in_flight.retain(|_, in_flight| {
            in_flight.session_id != request.session_id
                || in_flight.controller_id != controller_id
                || in_flight.controller_generation != controller_generation
        });
        send_session_connection_update(&state, request.session_id, None);
        let agent_sender = removed_binding.as_ref().and_then(|_| {
            state
                .agents
                .values()
                .find(|agent| agent.session_id == request.session_id && agent.ready)
                .and_then(|agent| agent.sender.clone())
        });
        drop(state);
        if let (Some(sender), Some(binding)) = (agent_sender, removed_binding) {
            let _ = sender.send(WireMessage::ControllerBindingRevoked {
                session_id: request.session_id,
                binding_token: binding.binding_token,
            });
        }
        ReleaseSessionResult {
            request_id: request.request_id,
            session_id: request.session_id,
            released: true,
            error: None,
        }
    }

    async fn update_agent_permission_mode(
        &self,
        agent_id: AgentInstanceId,
        connection_generation: u64,
        permission_mode: PermissionMode,
    ) {
        let (agent_sender, binding_messages, controller_senders, update) = {
            let mut state = self.state.lock().await;
            let (session_id, agent_sender) = {
                let Some(agent) = state.agents.get_mut(&agent_id) else {
                    return;
                };
                if agent.connection_generation != connection_generation || !agent.ready {
                    return;
                }
                agent.permission_mode = permission_mode;
                (agent.session_id, agent.sender.clone())
            };
            if let Some(bindings) = state.session_bindings.get_mut(&session_id) {
                bindings.agent_permission_mode = permission_mode;
            }
            let binding_messages = state
                .session_bindings
                .get(&session_id)
                .map(|bindings| {
                    bindings
                        .all()
                        .map(|binding| {
                            WireMessage::ControllerBinding(controller_binding(
                                session_id,
                                binding,
                                bindings.permission_mode_for(binding.controller_kind),
                            ))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let controller_senders = state
                .session_bindings
                .get(&session_id)
                .map(|bindings| {
                    [bindings.human.as_ref(), bindings.ai.as_ref()]
                        .into_iter()
                        .flatten()
                        .filter_map(|binding| {
                            state
                                .controllers
                                .get(&binding.controller_id)
                                .filter(|controller| {
                                    controller.connection_generation
                                        == binding.controller_generation
                                })
                                .map(|controller| controller.sender.clone())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut update = descriptor(
                state.agents.get(&agent_id).expect("Agent 应在同一锁内存在"),
                ConnectionState::Online,
            );
            if let Some(bindings) = state.session_bindings.get(&session_id) {
                apply_session_bindings(&mut update, bindings);
            }
            (agent_sender, binding_messages, controller_senders, update)
        };
        if let Some(agent_sender) = agent_sender {
            for message in binding_messages {
                let _ = agent_sender.send(message);
            }
        }
        for controller_sender in controller_senders {
            let _ = controller_sender.send(WireMessage::ConnectionUpdated(update.clone()));
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn forward_controller_request(
        &self,
        controller_id: ControllerInstanceId,
        controller_generation: u64,
        mut request: RemoteRequest,
        controller_sender: &Sender,
    ) {
        let mut state = self.state.lock().await;
        let Some(controller) = active_controller(&state, controller_id, controller_generation)
        else {
            send_request_error(
                controller_sender,
                &request,
                "controller_connection_replaced",
                "Controller 连接已经关闭或被替换",
            );
            return;
        };
        if !controller.sessions.contains(&request.session_id) {
            send_request_error(
                controller_sender,
                &request,
                "session_not_owned",
                "Controller 未绑定该 session_id",
            );
            return;
        }
        let controller_kind = controller.kind;
        let controller_owner_id = controller.owner_id;
        let Some(binding) = state
            .session_bindings
            .get(&request.session_id)
            .and_then(|bindings| bindings.get(controller_kind))
            .cloned()
        else {
            send_request_error(
                controller_sender,
                &request,
                "binding_missing",
                "session_id 当前没有对应角色的 Controller 授权绑定",
            );
            return;
        };
        if binding.controller_id != controller_id
            || binding.controller_generation != controller_generation
            || binding.owner_id != controller_owner_id
        {
            send_request_error(
                controller_sender,
                &request,
                "binding_replaced",
                "Controller 授权绑定已经被替换",
            );
            return;
        }
        let Some(session_bindings) = state.session_bindings.get(&request.session_id) else {
            send_request_error(
                controller_sender,
                &request,
                "binding_missing",
                "session_id 当前没有 Controller Owner 绑定",
            );
            return;
        };
        if session_bindings.owner_id != Some(controller_owner_id) {
            send_request_error(
                controller_sender,
                &request,
                "owner_mismatch",
                "Controller Owner 与 Agent Session 绑定不匹配",
            );
            return;
        }
        let mut permission_mode = session_bindings.permission_mode_for(controller_kind);
        if state.in_flight.contains_key(&request.request_id) {
            send_request_error(
                controller_sender,
                &request,
                "request_id_in_use",
                "request_id 已经存在尚未完成的请求",
            );
            return;
        }

        if matches!(request.operation, RemoteOperation::EmergencyStop)
            && controller_kind == ControllerKind::Ai
        {
            send_request_error(
                controller_sender,
                &request,
                "human_only",
                "只有人工 Controller 可以接管、释放或紧急停止会话",
            );
            return;
        }

        if matches!(request.operation, RemoteOperation::EmergencyStop) {
            let stopped_request_ids: Vec<_> = state
                .in_flight
                .iter()
                .filter(|(_, in_flight)| {
                    in_flight.session_id == request.session_id
                        && in_flight.owner_id == controller_owner_id
                })
                .map(|(request_id, _)| *request_id)
                .collect();
            for stopped_request_id in stopped_request_ids {
                if let Some(in_flight) = state.in_flight.remove(&stopped_request_id)
                    && let Some(target_controller) = state.controllers.get(&in_flight.controller_id)
                {
                    let _ = target_controller.sender.send(WireMessage::Error {
                        code: "emergency_stopped".to_owned(),
                        message: "该请求已被 Human Owner 的紧急停止中断".to_owned(),
                        request_id: Some(stopped_request_id),
                    });
                }
            }
            let bindings = state
                .session_bindings
                .get_mut(&request.session_id)
                .expect("Controller Owner 绑定已在同一锁内验证");
            for binding in [&mut bindings.human, &mut bindings.ai]
                .into_iter()
                .flatten()
            {
                binding.permission_mode = PermissionMode::ReadOnly;
            }
            bindings.human_takeover = true;
            permission_mode = PermissionMode::ReadOnly;
        }

        request.source = controller_kind.event_source();
        request.operation = normalize_operation(&self.policy, request.operation);
        if controller_kind == ControllerKind::Ai
            && matches!(
                &request.operation,
                RemoteOperation::HumanTakeover | RemoteOperation::ReleaseHumanTakeover
            )
        {
            send_request_error(
                controller_sender,
                &request,
                "human_only",
                "只有人工 Controller 可以接管、释放或紧急停止会话",
            );
            return;
        }
        let human_takeover = state
            .session_bindings
            .get(&request.session_id)
            .is_some_and(|bindings| bindings.human_takeover);
        if controller_kind == ControllerKind::Ai
            && human_takeover
            && !ai_operation_allowed_after_takeover(&self.policy, &request.operation)
        {
            send_request_error(
                controller_sender,
                &request,
                "ai_write_suspended",
                "人工已接管该会话，AI 只能继续执行只读操作",
            );
            return;
        }
        if let RemoteOperation::CancelRequest {
            request_id: target_request_id,
        } = &request.operation
        {
            let Some(target) = state.in_flight.get(target_request_id) else {
                send_request_error(
                    controller_sender,
                    &request,
                    "cancel_target_not_found",
                    "目标请求不存在或已经完成",
                );
                return;
            };
            if target.session_id != request.session_id {
                send_request_error(
                    controller_sender,
                    &request,
                    "cancel_target_session_mismatch",
                    "不能取消其他 session_id 的请求",
                );
                return;
            }
            let owns_target = target.controller_id == controller_id
                && target.controller_generation == controller_generation;
            if target.owner_id != controller_owner_id {
                send_request_error(
                    controller_sender,
                    &request,
                    "cancel_target_owner_mismatch",
                    "不能取消其他 Controller Owner 的请求",
                );
                return;
            }
            if controller_kind == ControllerKind::Ai && !owns_target {
                send_request_error(
                    controller_sender,
                    &request,
                    "cancel_target_not_owned",
                    "AI 只能取消自己发起的请求",
                );
                return;
            }
            if controller_kind == ControllerKind::Human
                && target.controller_kind != ControllerKind::Ai
                && !owns_target
            {
                send_request_error(
                    controller_sender,
                    &request,
                    "cancel_target_not_owned",
                    "人工 Controller 只能取消自己的请求或同会话 AI 请求",
                );
                return;
            }
        }

        let approval = match self.policy.evaluate_with_mode(
            permission_mode,
            request.source,
            &request.operation,
        ) {
            PolicyDecision::Allow => {
                request.approval_id = None;
                ApprovalState::NotRequired
            }
            PolicyDecision::Deny { reason } => {
                send_request_error(controller_sender, &request, "policy_denied", &reason);
                return;
            }
            PolicyDecision::RequireApproval { reason, .. } => {
                let Some(approval_id) = request.approval_id else {
                    send_request_error(controller_sender, &request, "approval_required", &reason);
                    return;
                };
                let Some(record) = state.approvals.get(&approval_id) else {
                    send_request_error(
                        controller_sender,
                        &request,
                        "approval_not_granted",
                        "审批不存在、已过期或已经消费",
                    );
                    return;
                };
                if record.state != ApprovalState::Approved
                    || record.expires_at <= Utc::now()
                    || record.session_id != request.session_id
                    || !approval_operations_match(&record.operation, &request.operation)
                    || record.controller_id != controller_id
                    || record.owner_id != controller_owner_id
                    || record.controller_generation != controller_generation
                    || record.controller_kind != controller_kind
                {
                    send_request_error(
                        controller_sender,
                        &request,
                        "approval_not_granted",
                        "审批未批准、已过期或与请求及 Controller 身份不匹配",
                    );
                    return;
                }
                state.approvals.remove(&approval_id);
                ApprovalState::Approved
            }
        };

        let Some((agent_id, agent_sender)) = state
            .agents
            .iter()
            .find(|(_, agent)| agent.session_id == request.session_id && agent.ready)
            .and_then(|(agent_id, agent)| agent.sender.clone().map(|sender| (*agent_id, sender)))
        else {
            send_request_error(
                controller_sender,
                &request,
                "agent_offline",
                "Agent 当前离线",
            );
            return;
        };

        let takeover_state = match &request.operation {
            RemoteOperation::HumanTakeover => Some(true),
            RemoteOperation::ReleaseHumanTakeover => Some(false),
            _ => None,
        };
        let connection_state_changed = takeover_state.is_some()
            || matches!(&request.operation, RemoteOperation::EmergencyStop);
        let previous_takeover = if let Some(next_state) = takeover_state {
            let bindings = state
                .session_bindings
                .get_mut(&request.session_id)
                .expect("Controller 角色绑定已在同一锁内验证");
            let previous = bindings.human_takeover;
            bindings.human_takeover = next_state;
            Some(previous)
        } else {
            None
        };
        state.in_flight.insert(
            request.request_id,
            InFlightRequest {
                agent_id,
                session_id: request.session_id,
                controller_id,
                owner_id: controller_owner_id,
                controller_generation,
                controller_kind,
                source: request.source,
                approval,
                previous_takeover,
            },
        );
        let binding_messages = if matches!(request.operation, RemoteOperation::EmergencyStop) {
            state
                .session_bindings
                .get(&request.session_id)
                .map(|bindings| {
                    bindings
                        .all()
                        .map(|binding| {
                            WireMessage::ControllerBinding(controller_binding(
                                request.session_id,
                                binding,
                                bindings.permission_mode_for(binding.controller_kind),
                            ))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        } else {
            vec![WireMessage::ControllerBinding(controller_binding(
                request.session_id,
                &binding,
                permission_mode,
            ))]
        };
        let request_message = WireMessage::AuthorizedRemoteRequest(AuthorizedRemoteRequest {
            request: request.clone(),
            authorization: RelayAuthorization {
                controller_instance_id: controller_id,
                owner_id: controller_owner_id,
                controller_kind,
                permission_mode,
                binding_token: binding.binding_token,
                approval,
            },
        });
        let binding_failed = binding_messages
            .into_iter()
            .any(|message| agent_sender.send(message).is_err());
        if binding_failed || agent_sender.send(request_message).is_err() {
            state.in_flight.remove(&request.request_id);
            if let Some(previous_takeover) = previous_takeover {
                state
                    .session_bindings
                    .get_mut(&request.session_id)
                    .expect("Controller 角色绑定已在同一锁内验证")
                    .human_takeover = previous_takeover;
            }
            send_request_error(
                controller_sender,
                &request,
                "agent_disconnected",
                "Agent 传输连接已经断开",
            );
            return;
        }
        if connection_state_changed {
            send_session_connection_update(&state, request.session_id, None);
        }
        info!(
            request_id = %request.request_id,
            session_id = %request.session_id,
            ?controller_kind,
            "Relay 已记录请求归属并转发授权请求"
        );
    }

    async fn forward_agent_message(
        &self,
        agent_id: AgentInstanceId,
        connection_generation: u64,
        message: WireMessage,
    ) {
        let senders = {
            let mut state = self.state.lock().await;
            let Some(agent) = state.agents.get(&agent_id) else {
                return;
            };
            if agent.connection_generation != connection_generation || !agent.ready {
                return;
            }
            let session_id = agent.session_id;
            let Some((request_id, terminal)) = agent_message_request(&message) else {
                warn!(agent_instance_id = %agent_id, "丢弃没有 request_id 的 Agent 业务消息");
                return;
            };
            let Some(in_flight) = state.in_flight.get(&request_id).cloned() else {
                warn!(
                    agent_instance_id = %agent_id,
                    %request_id,
                    "丢弃未知或已完成请求的 Agent 消息"
                );
                return;
            };
            if in_flight.agent_id != agent_id || in_flight.session_id != session_id {
                warn!(
                    agent_instance_id = %agent_id,
                    %request_id,
                    "丢弃跨 Agent 或跨 session_id 的 Agent 消息"
                );
                return;
            }
            if let WireMessage::RemoteEvent(event) = &message
                && (event.session_id != session_id
                    || event.source != in_flight.source
                    || event.approval != in_flight.approval)
            {
                warn!(
                    agent_instance_id = %agent_id,
                    %request_id,
                    "丢弃来源、审批或 session_id 不匹配的 Agent 事件"
                );
                return;
            }
            if let WireMessage::RemoteResponse(response) = &message
                && response.session_id != session_id
            {
                warn!(
                    agent_instance_id = %agent_id,
                    %request_id,
                    "丢弃跨 session_id 的 Agent 响应"
                );
                return;
            }
            let Some(controller) = state.controllers.get(&in_flight.controller_id) else {
                state.in_flight.remove(&request_id);
                return;
            };
            if controller.connection_generation != in_flight.controller_generation
                || controller.kind != in_flight.controller_kind
            {
                state.in_flight.remove(&request_id);
                return;
            }
            let mut senders = vec![controller.sender.clone()];
            if in_flight.controller_kind == ControllerKind::Ai
                && let Some(human_binding) = state
                    .session_bindings
                    .get(&session_id)
                    .and_then(|bindings| bindings.get(ControllerKind::Human))
                && human_binding.controller_id != in_flight.controller_id
                && let Some(human_controller) = state
                    .controllers
                    .get(&human_binding.controller_id)
                    .filter(|controller| {
                        controller.connection_generation == human_binding.controller_generation
                    })
            {
                senders.push(human_controller.sender.clone());
            }
            if terminal && let Some(completed) = state.in_flight.remove(&request_id) {
                let failed = matches!(
                    &message,
                    WireMessage::RemoteResponse(response)
                        if response.error_code.is_some()
                );
                if failed
                    && let Some(previous_takeover) = completed.previous_takeover
                    && let Some(bindings) = state.session_bindings.get_mut(&session_id)
                {
                    bindings.human_takeover = previous_takeover;
                    send_session_connection_update(&state, session_id, None);
                }
            }
            senders
        };
        for sender in senders {
            let _ = sender.send(message.clone());
        }
    }

    async fn forward_connection_update(&self, mut update: ConnectionDescriptor) {
        let senders = {
            let state = self.state.lock().await;
            let Some(bindings) = state.session_bindings.get(&update.session_id) else {
                return;
            };
            apply_session_bindings(&mut update, bindings);
            [bindings.human.as_ref(), bindings.ai.as_ref()]
                .into_iter()
                .flatten()
                .filter_map(|binding| {
                    state
                        .controllers
                        .get(&binding.controller_id)
                        .filter(|controller| {
                            controller.connection_generation == binding.controller_generation
                        })
                        .map(|controller| controller.sender.clone())
                })
                .collect::<Vec<_>>()
        };
        for sender in senders {
            let _ = sender.send(WireMessage::ConnectionUpdated(update.clone()));
        }
    }

    async fn mark_agent_disconnected(&self, agent_id: AgentInstanceId, connection_generation: u64) {
        let (update, failures) = {
            let mut state = self.state.lock().await;
            let update = state.agents.get_mut(&agent_id).and_then(|agent| {
                if agent.connection_generation != connection_generation {
                    return None;
                }
                agent.sender = None;
                agent.ready = false;
                Some((
                    agent.session_id,
                    descriptor(agent, ConnectionState::Reconnecting),
                ))
            });
            let update = update.map(|(session_id, mut update)| {
                if let Some(bindings) = state.session_bindings.get(&session_id) {
                    apply_session_bindings(&mut update, bindings);
                }
                update
            });
            let mut failures = Vec::new();
            if update.is_some() {
                let abandoned: Vec<_> = state
                    .in_flight
                    .iter()
                    .filter(|(_, request)| request.agent_id == agent_id)
                    .map(|(request_id, request)| (*request_id, request.clone()))
                    .collect();
                for (request_id, request) in abandoned {
                    state.in_flight.remove(&request_id);
                    if let Some(controller) = state.controllers.get(&request.controller_id)
                        && controller.connection_generation == request.controller_generation
                        && controller.kind == request.controller_kind
                    {
                        failures.push((controller.sender.clone(), request_id));
                    }
                }
            }
            (update, failures)
        };
        if let Some(update) = update {
            self.forward_connection_update(update).await;
        }
        for (sender, request_id) in failures {
            let _ = sender.send(WireMessage::Error {
                code: "agent_disconnected".to_owned(),
                message: "Agent 传输连接已经断开，尚未完成的请求已终止".to_owned(),
                request_id: Some(request_id),
            });
        }
    }

    async fn remove_controller(
        &self,
        controller_id: ControllerInstanceId,
        connection_generation: u64,
    ) {
        let revocations = {
            let mut state = self.state.lock().await;
            if state
                .controllers
                .get(&controller_id)
                .is_none_or(|controller| controller.connection_generation != connection_generation)
            {
                return;
            }
            let controller = state
                .controllers
                .remove(&controller_id)
                .expect("Controller 已在同一锁内验证");
            let mut revocations = Vec::new();
            for session_id in controller.sessions {
                let removed_binding =
                    state
                        .session_bindings
                        .get_mut(&session_id)
                        .and_then(|bindings| {
                            bindings.remove_owned(
                                controller.kind,
                                controller_id,
                                connection_generation,
                            )
                        });
                if let Some(binding) = removed_binding
                    && let Some(agent_sender) = state
                        .agents
                        .values()
                        .find(|agent| agent.session_id == session_id && agent.ready)
                        .and_then(|agent| agent.sender.clone())
                {
                    revocations.push((agent_sender, session_id, binding.binding_token));
                }
                if let Some(bindings) = state.session_bindings.get_mut(&session_id) {
                    if controller.kind == ControllerKind::Human {
                        bindings.human_takeover = false;
                    }
                    if bindings.is_empty() {
                        state.session_bindings.remove(&session_id);
                    }
                }
                send_session_connection_update(&state, session_id, None);
            }
            state.approvals.retain(|_, approval| {
                approval.controller_id != controller_id
                    || approval.controller_generation != connection_generation
            });
            state.in_flight.retain(|_, request| {
                request.controller_id != controller_id
                    || request.controller_generation != connection_generation
            });
            revocations
        };
        for (sender, session_id, binding_token) in revocations {
            let _ = sender.send(WireMessage::ControllerBindingRevoked {
                session_id,
                binding_token,
            });
        }
    }

    async fn cleanup_expired(&self) {
        let now = Utc::now();
        let (notifications, failures) = {
            let mut state = self.state.lock().await;
            state
                .approvals
                .retain(|_, approval| approval.expires_at > now);
            let expired_ids: Vec<_> = state
                .agents
                .iter()
                .filter(|(_, agent)| !agent.lease.is_valid_at(now) && !agent.lease_expired)
                .map(|(agent_id, _)| *agent_id)
                .collect();
            let mut notifications = Vec::new();
            let mut failures = Vec::new();
            for agent_id in expired_ids {
                let Some((session_id, mut update)) = state.agents.get_mut(&agent_id).map(|agent| {
                    agent.sender = None;
                    agent.ready = false;
                    agent.lease_expired = true;
                    (
                        agent.session_id,
                        descriptor(agent, ConnectionState::Offline),
                    )
                }) else {
                    continue;
                };
                if let Some(bindings) = state.session_bindings.get(&session_id) {
                    apply_session_bindings(&mut update, bindings);
                    for binding in bindings.all() {
                        if let Some(controller) = state.controllers.get(&binding.controller_id)
                            && controller.connection_generation == binding.controller_generation
                        {
                            notifications.push((controller.sender.clone(), update.clone()));
                        }
                    }
                }
                state
                    .approvals
                    .retain(|_, approval| approval.session_id != session_id);
                let abandoned = state
                    .in_flight
                    .iter()
                    .filter(|(_, request)| request.session_id == session_id)
                    .map(|(request_id, request)| (*request_id, request.clone()))
                    .collect::<Vec<_>>();
                for (request_id, request) in abandoned {
                    state.in_flight.remove(&request_id);
                    if let Some(controller) = state.controllers.get(&request.controller_id)
                        && controller.connection_generation == request.controller_generation
                    {
                        failures.push((controller.sender.clone(), request_id));
                    }
                }
                info!(
                    agent_instance_id = %agent_id,
                    session_id = %session_id,
                    "Agent 在线租约已过期，恢复身份继续保留"
                );
            }
            let removable_ids = state
                .agents
                .iter()
                .filter(|(_, agent)| {
                    agent.lease_expired
                        && agent.sender.is_none()
                        && !agent.ever_paired
                        && !state.session_bindings.contains_key(&agent.session_id)
                        && now - agent.lease.expires_at >= UNPAIRED_AGENT_RETENTION
                })
                .map(|(agent_id, _)| *agent_id)
                .collect::<Vec<_>>();
            let removed_any = !removable_ids.is_empty();
            for agent_id in removable_ids {
                if let Some(agent) = state.agents.remove(&agent_id) {
                    state.pairing_index.remove(&agent.lease.pairing_code);
                    info!(agent_instance_id = %agent_id, "已回收长期过期且未配对的 Agent 身份");
                }
            }
            if removed_any && let Err(error) = self.persist_state_locked(&state) {
                warn!(error = %error, "Relay 无法持久化租约清理结果");
            }
            (notifications, failures)
        };
        for (sender, descriptor) in notifications {
            let _ = sender.send(WireMessage::ConnectionUpdated(descriptor));
        }
        for (sender, request_id) in failures {
            let _ = sender.send(WireMessage::Error {
                code: "agent_lease_expired".to_owned(),
                message: "Agent 在线租约已过期，尚未完成的请求已终止".to_owned(),
                request_id: Some(request_id),
            });
        }
    }
}

const PERSISTED_STATE_VERSION: u16 = 1;

fn append_audit(
    state: &mut RelayState,
    action: &str,
    target: Option<String>,
    success: bool,
    source: &str,
    summary: &str,
) {
    let id = state
        .audit_log
        .last()
        .map_or(1, |event| event.id.saturating_add(1));
    state.audit_log.push(AdminAuditEvent {
        id,
        timestamp: Utc::now(),
        action: action.to_owned(),
        target,
        success,
        source: source.to_owned(),
        summary: summary.to_owned(),
    });
    if state.audit_log.len() > MAX_AUDIT_EVENTS {
        let remove = state.audit_log.len() - MAX_AUDIT_EVENTS;
        state.audit_log.drain(0..remove);
    }
}

fn token_fingerprint(token: &str) -> Option<String> {
    if token.is_empty() {
        return None;
    }
    Some(hex::encode(Sha256::digest(token.as_bytes())))
}

const MIN_ADMIN_PASSWORD_CHARS: usize = 12;

fn validate_admin_password(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_ADMIN_PASSWORD_CHARS {
        return Err(format!(
            "管理密码至少需要 {MIN_ADMIN_PASSWORD_CHARS} 个字符"
        ));
    }
    if password.len() > 1024 {
        return Err("管理密码不能超过 1024 字节".to_owned());
    }
    Ok(())
}

fn hash_admin_password(password: &str) -> AdminPasswordHash {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    let digest = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("Argon2id 密码哈希生成不应失败")
        .to_string();
    AdminPasswordHash {
        algorithm: Some("argon2id".to_owned()),
        salt: salt.to_string(),
        digest,
    }
}

fn verify_admin_password(password: &str, stored: &AdminPasswordHash) -> bool {
    match stored.algorithm.as_deref() {
        Some("argon2id") => PasswordHash::new(&stored.digest).is_ok_and(|hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
        }),
        None => constant_time_bytes_eq(
            password_digest(password, &stored.salt).as_bytes(),
            stored.digest.as_bytes(),
        ),
        _ => false,
    }
}

fn password_digest(password: &str, salt: &str) -> String {
    hex::encode(Sha256::digest(
        format!("remoteops-admin-password-v1:{salt}:{password}").as_bytes(),
    ))
}

fn constant_time_bytes_eq(left: &[u8], right: &[u8]) -> bool {
    let maximum = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..maximum {
        difference |= usize::from(left.get(index).copied().unwrap_or_default())
            ^ usize::from(right.get(index).copied().unwrap_or_default());
    }
    difference == 0
}

fn legacy_agent_was_paired() -> bool {
    true
}

fn load_persisted_state(path: &Path, state: &mut RelayState) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("无法读取 Relay 状态文件 {}", path.display()))?;
    let persisted: PersistedRelayState = serde_json::from_str(&contents)
        .with_context(|| format!("Relay 状态文件格式无效：{}", path.display()))?;
    if persisted
        .admin_password_hash
        .as_ref()
        .and_then(|hash| hash.algorithm.as_deref())
        .is_some_and(|algorithm| algorithm != "argon2id")
    {
        bail!("Relay 状态文件包含无法识别的管理密码哈希格式，请重置密码");
    }
    if persisted.version != PERSISTED_STATE_VERSION {
        bail!(
            "Relay 状态文件版本 {} 不受支持，当前版本为 {}",
            persisted.version,
            PERSISTED_STATE_VERSION
        );
    }
    let now = Utc::now();
    state.audit_log = persisted.audit_log;
    state.admin_password_hash = persisted.admin_password_hash;
    let mut session_ids = BTreeSet::new();
    ensure_agent_capacity(persisted.agents.len().saturating_sub(1))?;
    for persisted_agent in persisted.agents {
        if persisted_agent.hello.agent_instance_id != persisted_agent.lease.agent_instance_id {
            bail!("Relay 状态文件中的 Agent 身份与租约不一致");
        }
        if persisted_agent
            .resume_token
            .as_deref()
            .is_some_and(str::is_empty)
        {
            bail!("Relay 状态文件中的恢复令牌不能为空字符串");
        }
        if persisted_agent.connection_generation == 0 {
            bail!("Relay 状态文件中的连接代次必须大于零");
        }
        let agent_id = persisted_agent.hello.agent_instance_id;
        let pairing_code = persisted_agent.lease.pairing_code.clone();
        if state.agents.contains_key(&agent_id) || state.pairing_index.contains_key(&pairing_code) {
            bail!("Relay 状态文件包含重复的 Agent 或控制码");
        }
        if !session_ids.insert(persisted_agent.session_id) {
            bail!("Relay 状态文件包含重复的 session_id");
        }
        let mut hello = persisted_agent.hello;
        // 恢复令牌只用于当前握手，不重复写回内存快照。
        hello.resume_token = None;
        state.pairing_index.insert(pairing_code, agent_id);
        let lease_expired = !persisted_agent.lease.is_valid_at(now);
        let persisted_lease_expires_at = persisted_agent.lease.expires_at;
        state.agents.insert(
            agent_id,
            AgentRecord {
                hello,
                lease: persisted_agent.lease,
                resume_token: persisted_agent.resume_token,
                previous_resume_token: persisted_agent.previous_resume_token,
                pending_resume: None,
                connection_generation: persisted_agent.connection_generation,
                ready: false,
                session_id: persisted_agent.session_id,
                sender: None,
                last_seen: now,
                persisted_lease_expires_at,
                ever_paired: persisted_agent.ever_paired,
                lease_expired,
                permission_mode: PermissionMode::ApprovalRequired,
            },
        );
    }
    Ok(())
}

fn save_persisted_state(path: &Path, state: &RelayState) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("无法创建 Relay 状态目录 {}", parent.display()))?;
    let agents = state
        .agents
        .values()
        .map(|agent| {
            let mut hello = agent.hello.clone();
            hello.resume_token = None;
            PersistedAgent {
                hello,
                lease: agent.lease.clone(),
                resume_token: agent.resume_token.clone(),
                previous_resume_token: agent.previous_resume_token.clone(),
                session_id: agent.session_id,
                connection_generation: agent.connection_generation,
                ever_paired: agent.ever_paired,
            }
        })
        .collect::<Vec<_>>();
    let contents = serde_json::to_vec_pretty(&PersistedRelayState {
        version: PERSISTED_STATE_VERSION,
        agents,
        audit_log: state.audit_log.clone(),
        admin_password_hash: state.admin_password_hash.clone(),
    })?;
    let file_name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .context("Relay 状态文件名无效")?;
    let temporary_path = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary_path)
            .with_context(|| format!("无法创建 Relay 状态临时文件 {}", temporary_path.display()))?;
        file.write_all(&contents)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary_path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("无法替换 Relay 状态文件 {}", path.display()))?;
    }
    std::fs::rename(&temporary_path, path)
        .with_context(|| format!("无法提交 Relay 状态文件 {}", path.display()))?;
    #[cfg(unix)]
    OpenOptions::new()
        .read(true)
        .open(parent)
        .with_context(|| format!("无法打开 Relay 状态目录 {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("无法同步 Relay 状态目录 {}", parent.display()))?;
    Ok(())
}

fn active_controller(
    state: &RelayState,
    controller_id: ControllerInstanceId,
    connection_generation: u64,
) -> Option<&ControllerRecord> {
    state
        .controllers
        .get(&controller_id)
        .filter(|controller| controller.connection_generation == connection_generation)
}

fn approval_failure(
    request_id: remoteops_domain::RequestId,
    session_id: SessionId,
    operation: RemoteOperation,
    reason: &str,
) -> ApprovalResult {
    ApprovalResult {
        request_id,
        session_id,
        operation,
        approval_id: None,
        state: ApprovalState::Rejected,
        reason: reason.to_owned(),
        expires_at: None,
    }
}

fn normalize_operation(policy: &DefaultPolicy, mut operation: RemoteOperation) -> RemoteOperation {
    set_operation_readonly(&mut operation, false);
    let readonly = policy.classify(&operation) == RiskLevel::ReadOnly;
    set_operation_readonly(&mut operation, readonly);
    operation
}

fn ai_operation_allowed_after_takeover(
    policy: &DefaultPolicy,
    operation: &RemoteOperation,
) -> bool {
    !matches!(
        operation,
        RemoteOperation::HumanTakeover | RemoteOperation::ReleaseHumanTakeover
    ) && policy.classify(operation) == RiskLevel::ReadOnly
}

fn agent_message_request(message: &WireMessage) -> Option<(RequestId, bool)> {
    match message {
        WireMessage::RemoteResponse(response) => Some((response.request_id, true)),
        WireMessage::RemoteEvent(event) => event.request_id.map(|request_id| (request_id, false)),
        WireMessage::Error {
            request_id: Some(request_id),
            ..
        } => Some((*request_id, true)),
        _ => None,
    }
}

fn set_operation_readonly(operation: &mut RemoteOperation, readonly: bool) {
    match operation {
        RemoteOperation::RunCommand {
            readonly: declared, ..
        }
        | RemoteOperation::RunShellCommand {
            readonly: declared, ..
        }
        | RemoteOperation::RunSsh {
            readonly: declared, ..
        }
        | RemoteOperation::RunSerialQuery {
            readonly: declared, ..
        } => *declared = readonly,
        _ => {}
    }
}

fn send_request_error(sender: &Sender, request: &RemoteRequest, code: &str, message: &str) {
    let _ = sender.send(WireMessage::Error {
        code: code.to_owned(),
        message: message.to_owned(),
        request_id: Some(request.request_id),
    });
}

fn resume_token_matches(agent: &AgentRecord, supplied: Option<&String>) -> bool {
    agent.resume_token.as_ref() == supplied
        || agent
            .previous_resume_token
            .as_ref()
            .is_some_and(|previous| match previous {
                PreviousResumeToken::Absent => supplied.is_none(),
                PreviousResumeToken::Token(token) => Some(token) == supplied,
            })
}

fn controller_binding(
    session_id: SessionId,
    binding: &SessionBinding,
    permission_mode: PermissionMode,
) -> ControllerBinding {
    ControllerBinding {
        session_id,
        controller_instance_id: binding.controller_id,
        owner_id: binding.owner_id,
        controller_kind: binding.controller_kind,
        permission_mode,
        binding_token: binding.binding_token.clone(),
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let maximum = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..maximum {
        difference |= usize::from(left.get(index).copied().unwrap_or_default())
            ^ usize::from(right.get(index).copied().unwrap_or_default());
    }
    difference == 0
}

fn descriptor(agent: &AgentRecord, state: ConnectionState) -> ConnectionDescriptor {
    ConnectionDescriptor {
        session_id: agent.session_id,
        agent_instance_id: agent.hello.agent_instance_id,
        display_index: 0,
        alias: None,
        hostname: agent.hello.hostname.clone(),
        operating_system: agent.hello.operating_system.clone(),
        capabilities: agent.hello.capabilities.clone(),
        environment: agent.hello.environment.clone(),
        credential_encryption_public_key: agent.hello.credential_encryption_public_key.clone(),
        credential_encryption_key_id: agent.hello.credential_encryption_key_id.clone(),
        mac_address: agent.hello.mac_address.clone(),
        state,
        role: SessionRole::HumanControl,
        permission_mode: agent.permission_mode,
        updated_at: Utc::now(),
    }
}

fn apply_session_bindings(descriptor: &mut ConnectionDescriptor, bindings: &SessionBindings) {
    descriptor.role = bindings.role();
    descriptor.permission_mode = bindings.permission_mode();
}

fn send_session_connection_update(
    state: &RelayState,
    session_id: SessionId,
    excluded_controller: Option<(ControllerInstanceId, u64)>,
) {
    let Some(agent) = state
        .agents
        .values()
        .find(|agent| agent.session_id == session_id)
    else {
        return;
    };
    let Some(bindings) = state.session_bindings.get(&session_id) else {
        return;
    };
    let connection_state = if agent.ready && agent.sender.is_some() {
        ConnectionState::Online
    } else if agent.lease.is_valid_at(Utc::now()) {
        ConnectionState::Reconnecting
    } else {
        ConnectionState::Offline
    };
    let mut update = descriptor(agent, connection_state);
    apply_session_bindings(&mut update, bindings);
    for binding in bindings.all() {
        if excluded_controller.is_some_and(|(controller_id, generation)| {
            binding.controller_id == controller_id && binding.controller_generation == generation
        }) {
            continue;
        }
        if let Some(controller) = state.controllers.get(&binding.controller_id)
            && controller.connection_generation == binding.controller_generation
        {
            let _ = controller
                .sender
                .send(WireMessage::ConnectionUpdated(update.clone()));
        }
    }
}

fn unique_pairing_code(state: &RelayState) -> anyhow::Result<PairingCode> {
    let mut rng = rand::thread_rng();
    for _ in 0..100 {
        let value = format!("{:09}", rng.gen_range(0_u32..1_000_000_000));
        let code = PairingCode::parse(value).map_err(|error| anyhow!(error))?;
        if !state.pairing_index.contains_key(&code) {
            return Ok(code);
        }
    }
    Err(anyhow!("无法生成唯一配对码"))
}

fn ensure_agent_capacity(current_count: usize) -> anyhow::Result<()> {
    if current_count >= MAX_REGISTERED_AGENTS {
        bail!("Relay 已达到 Agent 登记上限 {MAX_REGISTERED_AGENTS}");
    }
    Ok(())
}

fn new_secret_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use remoteops_domain::{
        Capability, CapabilitySet, EventPayload, EventSource, RequestId, ShellKind,
    };
    use remoteops_protocol::{AgentHello, AuthorizedRemoteRequest, ControllerHello};

    use super::*;

    const HUMAN_TOKEN: &str = "human-controller-token-32-bytes-minimum";
    const AI_TOKEN: &str = "ai-controller-token-value-32-bytes-min";

    fn test_channel() -> (Sender, mpsc::Receiver<WireMessage>) {
        let (sender, receiver, _) = outbound_channel();
        (sender, receiver)
    }

    struct TestStateFile {
        root: PathBuf,
        path: PathBuf,
    }

    impl TestStateFile {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "remoteops-relay-{name}-{}",
                Uuid::new_v4().simple()
            ));
            Self {
                path: root.join("relay-state.json"),
                root,
            }
        }
    }

    impl Drop for TestStateFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn relay(lease_lifetime: Duration) -> Relay {
        Relay::new(
            lease_lifetime,
            15,
            test_owner_id(),
            HUMAN_TOKEN.to_owned(),
            AI_TOKEN.to_owned(),
        )
    }

    fn persisted_relay(path: &Path, lease_lifetime: Duration) -> anyhow::Result<Relay> {
        Relay::new_with_state_path(
            lease_lifetime,
            15,
            test_owner_id(),
            HUMAN_TOKEN.to_owned(),
            AI_TOKEN.to_owned(),
            path,
        )
    }

    fn test_owner_id() -> ControllerOwnerId {
        ControllerOwnerId::from_uuid(Uuid::from_u128(1))
    }

    fn other_owner_id() -> ControllerOwnerId {
        ControllerOwnerId::from_uuid(Uuid::from_u128(2))
    }

    fn hello(agent_instance_id: AgentInstanceId, resume_token: Option<String>) -> AgentHello {
        AgentHello {
            protocol_version: PROTOCOL_VERSION,
            agent_instance_id,
            resume_token,
            hostname: "LAB-WIN-A".to_owned(),
            operating_system: "Windows 11".to_owned(),
            capabilities: CapabilitySet::new([Capability::Cmd]),
            environment: remoteops_domain::EnvironmentProfile::empty(),
            credential_encryption_public_key: "test-public-key".to_owned(),
            credential_encryption_key_id: "test-key-id".to_owned(),
            mac_address: Some("00:11:22:33:44:55".to_owned()),
        }
    }

    async fn ready_agent(
        relay: &Relay,
        agent_id: AgentInstanceId,
        resume_token: Option<String>,
    ) -> (AgentRegistration, mpsc::Receiver<WireMessage>, SessionId) {
        let (sender, receiver) = test_channel();
        let registration = relay
            .register_agent(hello(agent_id, resume_token), sender)
            .await
            .expect("Agent 注册应成功");
        assert!(
            relay
                .commit_agent_resume(agent_id, registration.connection_generation)
                .await
        );
        let (descriptor, _) = relay
            .finalize_agent_resume(agent_id, registration.connection_generation)
            .await
            .expect("Agent 恢复令牌应完成确认");
        (registration, receiver, descriptor.session_id)
    }

    #[tokio::test]
    async fn new_agent_connects_without_pre_shared_secret_but_resume_still_requires_token() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (registration, _receiver, _session_id) = ready_agent(&relay, agent_id, None).await;
        let resumed = hello(agent_id, Some(registration.welcome.resume_token));
        let (resume_sender, _resume_receiver) = test_channel();
        assert!(relay.register_agent(resumed, resume_sender).await.is_ok());

        let invalid = hello(agent_id, Some("invalid-resume-token".to_owned()));
        let (invalid_sender, _invalid_receiver) = test_channel();
        assert!(relay.register_agent(invalid, invalid_sender).await.is_err());
    }

    #[tokio::test]
    async fn admin_snapshot_includes_agent_and_controller_host_identity() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (_registration, _receiver, _session_id) = ready_agent(&relay, agent_id, None).await;
        let controller_id = ControllerInstanceId::new();
        let (sender, _receiver) = test_channel();
        relay
            .register_controller(
                controller_id,
                test_owner_id(),
                ControllerKind::Ai,
                Some("MCP-CONSOLE-01".to_owned()),
                Some("00:AA:BB:CC:DD:EE".to_owned()),
                sender,
            )
            .await
            .expect("Controller 注册应成功");

        let snapshot = relay.admin_snapshot().await;
        assert_eq!(
            snapshot.agents[0].mac_address.as_deref(),
            Some("00:11:22:33:44:55")
        );
        assert_eq!(
            snapshot.controllers[0].hostname.as_deref(),
            Some("MCP-CONSOLE-01")
        );
        assert_eq!(
            snapshot.controllers[0].mac_address.as_deref(),
            Some("00:AA:BB:CC:DD:EE")
        );
    }

    async fn register_controller(
        relay: &Relay,
        kind: ControllerKind,
    ) -> (ControllerInstanceId, u64, mpsc::Receiver<WireMessage>) {
        register_controller_for_owner(relay, test_owner_id(), kind).await
    }

    async fn register_controller_for_owner(
        relay: &Relay,
        owner_id: ControllerOwnerId,
        kind: ControllerKind,
    ) -> (ControllerInstanceId, u64, mpsc::Receiver<WireMessage>) {
        let controller_id = ControllerInstanceId::new();
        let (sender, receiver) = test_channel();
        let generation = relay
            .register_controller(controller_id, owner_id, kind, None, None, sender)
            .await
            .expect("Controller 注册应成功");
        (controller_id, generation, receiver)
    }

    async fn controller_sender(relay: &Relay, controller_id: ControllerInstanceId) -> Sender {
        relay
            .state
            .lock()
            .await
            .controllers
            .get(&controller_id)
            .expect("Controller 应存在")
            .sender
            .clone()
    }

    async fn ai_sender(relay: &Relay, controller_id: ControllerInstanceId) -> Sender {
        controller_sender(relay, controller_id).await
    }

    async fn human_sender(relay: &Relay, controller_id: ControllerInstanceId) -> Sender {
        controller_sender(relay, controller_id).await
    }

    fn run_command(command: &str, readonly: bool) -> RemoteOperation {
        RemoteOperation::RunCommand {
            shell: ShellKind::WindowsPowerShell,
            command: command.to_owned(),
            readonly,
        }
    }

    fn serial_query(command: &str, readonly: bool) -> RemoteOperation {
        RemoteOperation::RunSerialQuery {
            serial_session_id: "serial-1".to_owned(),
            command: command.to_owned(),
            line_ending: remoteops_domain::SerialLineEnding::Cr,
            profile: remoteops_domain::SerialTerminalProfile::HuaweiVrp,
            overall_timeout_millis: 30_000,
            idle_timeout_millis: 1_200,
            max_bytes: 128 * 1024,
            max_pages: 50,
            readonly,
        }
    }

    fn ssh_command(command: &str, readonly: bool) -> RemoteOperation {
        RemoteOperation::RunSsh {
            host: "192.0.2.10".to_owned(),
            port: 22,
            username: "operator".to_owned(),
            identity_file: None,
            known_hosts_file: None,
            command: command.to_owned(),
            readonly,
        }
    }

    #[test]
    fn serial_query_readonly_is_recomputed_from_the_command() {
        let policy = DefaultPolicy::default();

        let normalized = normalize_operation(&policy, serial_query("display version", false));
        assert!(matches!(
            normalized,
            RemoteOperation::RunSerialQuery { readonly: true, .. }
        ));

        let normalized = normalize_operation(&policy, serial_query("system-view", true));
        assert!(matches!(
            normalized,
            RemoteOperation::RunSerialQuery {
                readonly: false,
                ..
            }
        ));
        assert!(matches!(
            policy.evaluate(EventSource::Ai, &normalized),
            PolicyDecision::RequireApproval { .. }
        ));
    }

    #[test]
    fn ssh_readonly_is_recomputed_from_the_command() {
        let policy = DefaultPolicy::default();

        let normalized = normalize_operation(&policy, ssh_command("display version", false));
        assert!(matches!(
            normalized,
            RemoteOperation::RunSsh { readonly: true, .. }
        ));

        let normalized = normalize_operation(&policy, ssh_command("system-view", true));
        assert!(matches!(
            normalized,
            RemoteOperation::RunSsh {
                readonly: false,
                ..
            }
        ));
        assert!(matches!(
            policy.evaluate(EventSource::Ai, &normalized),
            PolicyDecision::RequireApproval {
                risk: RiskLevel::High,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn outbound_queue_is_bounded_and_signals_slow_consumer() {
        let (sender, _receiver, slow_consumer) = outbound_channel();
        for _ in 0..OUTBOUND_QUEUE_CAPACITY {
            sender
                .send(WireMessage::Heartbeat {
                    sent_at: Utc::now(),
                })
                .expect("容量内消息应入队");
        }
        assert!(
            sender
                .send(WireMessage::Heartbeat {
                    sent_at: Utc::now()
                })
                .is_err(),
            "队列满时必须拒绝继续增长"
        );
        time::timeout(time::Duration::from_millis(50), slow_consumer.notified())
            .await
            .expect("队列满时必须唤醒连接关闭任务");
    }

    #[tokio::test]
    async fn client_hello_has_a_deadline() {
        let (_client, mut server) = tokio::io::duplex(64);
        let result = read_client_hello(&mut server, time::Duration::from_millis(10)).await;
        assert!(
            result
                .expect_err("没有首帧的客户端必须超时")
                .to_string()
                .contains("等待客户端 Hello 超时")
        );
    }

    #[test]
    fn agent_registry_has_a_hard_capacity() {
        assert!(ensure_agent_capacity(MAX_REGISTERED_AGENTS - 1).is_ok());
        assert!(ensure_agent_capacity(MAX_REGISTERED_AGENTS).is_err());
    }

    #[tokio::test]
    async fn rapid_agent_heartbeat_does_not_extend_lease() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (registration, _, _) = ready_agent(&relay, agent_id, None).await;
        {
            let mut state = relay.state.lock().await;
            state
                .agents
                .get_mut(&agent_id)
                .expect("Agent 应存在")
                .last_seen = Utc::now() - Duration::seconds(30);
        }
        assert!(
            relay
                .renew_agent(agent_id, registration.connection_generation)
                .await
                .is_some()
        );
        let renewed_expiry = relay
            .state
            .lock()
            .await
            .agents
            .get(&agent_id)
            .expect("Agent 应存在")
            .lease
            .expires_at;
        assert!(
            relay
                .renew_agent(agent_id, registration.connection_generation)
                .await
                .is_none(),
            "小于最小心跳间隔的请求不得再次续租"
        );
        assert_eq!(
            relay
                .state
                .lock()
                .await
                .agents
                .get(&agent_id)
                .expect("Agent 应存在")
                .lease
                .expires_at,
            renewed_expiry
        );
    }

    #[tokio::test]
    async fn accepted_heartbeat_does_not_always_advance_persisted_lease() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (registration, _, _) = ready_agent(&relay, agent_id, None).await;
        let persisted_expiry = {
            let mut state = relay.state.lock().await;
            let agent = state.agents.get_mut(&agent_id).expect("Agent 应存在");
            agent.last_seen = Utc::now() - Duration::seconds(30);
            agent.persisted_lease_expires_at
        };
        assert!(
            relay
                .renew_agent(agent_id, registration.connection_generation)
                .await
                .is_some()
        );
        let state = relay.state.lock().await;
        let agent = state.agents.get(&agent_id).expect("Agent 应存在");
        assert!(agent.lease.expires_at > persisted_expiry);
        assert_eq!(agent.persisted_lease_expires_at, persisted_expiry);
    }

    #[tokio::test]
    async fn accepted_heartbeat_returns_current_lease_expiry_for_agent_notification() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (registration, _, _) = ready_agent(&relay, agent_id, None).await;
        {
            let mut state = relay.state.lock().await;
            state
                .agents
                .get_mut(&agent_id)
                .expect("Agent 应存在")
                .last_seen = Utc::now() - Duration::seconds(30);
        }
        let renewed_expiry = relay
            .renew_agent(agent_id, registration.connection_generation)
            .await
            .expect("接受的心跳必须返回新的租约到期时间");
        let state = relay.state.lock().await;
        assert_eq!(
            renewed_expiry,
            state
                .agents
                .get(&agent_id)
                .expect("Agent 应存在")
                .lease
                .expires_at
        );
    }

    #[tokio::test]
    async fn cleanup_reclaims_only_long_expired_unpaired_agents() {
        let relay = relay(Duration::minutes(10));
        let unpaired_id = AgentInstanceId::new();
        let paired_id = AgentInstanceId::new();
        let (unpaired, _, _) = ready_agent(&relay, unpaired_id, None).await;
        let (paired, _, _) = ready_agent(&relay, paired_id, None).await;
        {
            let mut state = relay.state.lock().await;
            for (agent_id, generation, ever_paired) in [
                (unpaired_id, unpaired.connection_generation, false),
                (paired_id, paired.connection_generation, true),
            ] {
                let agent = state.agents.get_mut(&agent_id).expect("Agent 应存在");
                assert_eq!(agent.connection_generation, generation);
                agent.sender = None;
                agent.ready = false;
                agent.lease_expired = true;
                agent.ever_paired = ever_paired;
                agent.lease.expires_at = Utc::now() - Duration::hours(25);
            }
        }
        relay.cleanup_expired().await;
        let state = relay.state.lock().await;
        assert!(!state.agents.contains_key(&unpaired_id));
        assert!(state.agents.contains_key(&paired_id));
    }

    async fn recv_authorized(
        receiver: &mut mpsc::Receiver<WireMessage>,
    ) -> AuthorizedRemoteRequest {
        loop {
            match receiver.recv().await.expect("Agent 应收到 Relay 消息") {
                WireMessage::AuthorizedRemoteRequest(request) => return request,
                WireMessage::ControllerBinding(_) => {}
                other => panic!("Agent 应收到授权请求，实际为 {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn reconnect_keeps_pairing_code_and_session() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (first, _, first_session) = ready_agent(&relay, agent_id, None).await;
        let first_token = first.welcome.resume_token.clone();
        let (second, _, second_session) =
            ready_agent(&relay, agent_id, Some(first_token.clone())).await;

        assert_eq!(first.welcome.pairing_code, second.welcome.pairing_code);
        assert_eq!(first_session, second_session);
        assert_ne!(first_token, second.welcome.resume_token);

        let (sender, _receiver) = test_channel();
        assert!(
            relay
                .register_agent(hello(agent_id, Some(first_token)), sender)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn relay_restart_preserves_pairing_code_session_and_resume_token() {
        let state_file = TestStateFile::new("restart");
        let agent_id = AgentInstanceId::new();
        let (first_pairing_code, first_session_id, first_resume_token) = {
            let relay =
                persisted_relay(&state_file.path, Duration::minutes(10)).expect("Relay 应启动");
            let (registration, _, session_id) = ready_agent(&relay, agent_id, None).await;
            let persisted: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(&state_file.path).expect("状态文件应存在"),
            )
            .expect("状态文件应为 JSON");
            assert!(
                persisted["agents"][0]["hello"]["resume_token"].is_null(),
                "握手中的恢复令牌不得重复保存在 AgentHello 快照"
            );
            assert_eq!(
                persisted["agents"][0]["resume_token"].as_str(),
                Some(registration.welcome.resume_token.as_str())
            );
            (
                registration.welcome.pairing_code,
                session_id,
                registration.welcome.resume_token,
            )
        };

        let relay =
            persisted_relay(&state_file.path, Duration::minutes(10)).expect("Relay 应恢复状态");
        {
            let state = relay.state.lock().await;
            let agent = state.agents.get(&agent_id).expect("Agent 应从状态恢复");
            assert_eq!(agent.lease.pairing_code, first_pairing_code);
            assert_eq!(agent.session_id, first_session_id);
            assert_eq!(
                agent.resume_token.as_deref(),
                Some(first_resume_token.as_str())
            );
            assert!(!agent.ready);
            assert!(agent.sender.is_none());
        }

        let (sender, _receiver) = test_channel();
        let resumed = relay
            .register_agent(hello(agent_id, Some(first_resume_token)), sender)
            .await
            .expect("Agent 应使用已确认令牌恢复");
        assert_eq!(resumed.welcome.pairing_code, first_pairing_code);
        assert!(
            relay
                .commit_agent_resume(agent_id, resumed.connection_generation)
                .await
        );
        let (descriptor, _) = relay
            .finalize_agent_resume(agent_id, resumed.connection_generation)
            .await
            .expect("恢复握手应完成");
        assert_eq!(descriptor.session_id, first_session_id);
    }

    #[tokio::test]
    async fn relay_restart_during_resume_commit_accepts_previous_agent_token() {
        let state_file = TestStateFile::new("resume-commit");
        let agent_id = AgentInstanceId::new();
        let (pairing_code, session_id) = {
            let relay =
                persisted_relay(&state_file.path, Duration::minutes(10)).expect("Relay 应启动");
            let (sender, _receiver) = test_channel();
            let registration = relay
                .register_agent(hello(agent_id, None), sender)
                .await
                .expect("首次注册应成功");
            assert!(
                relay
                    .commit_agent_resume(agent_id, registration.connection_generation)
                    .await
            );
            let state = relay.state.lock().await;
            let agent = state.agents.get(&agent_id).expect("Agent 应存在");
            assert!(matches!(
                agent.previous_resume_token,
                Some(PreviousResumeToken::Absent)
            ));
            (registration.welcome.pairing_code, agent.session_id)
        };

        let relay =
            persisted_relay(&state_file.path, Duration::minutes(10)).expect("Relay 应恢复状态");
        let (sender, _receiver) = test_channel();
        let registration = relay
            .register_agent(hello(agent_id, None), sender)
            .await
            .expect("Relay 崩溃在确认消息发送前时仍应接受上一个令牌");
        assert_eq!(registration.welcome.pairing_code, pairing_code);
        assert!(
            relay
                .commit_agent_resume(agent_id, registration.connection_generation)
                .await
        );
        let (descriptor, _) = relay
            .finalize_agent_resume(agent_id, registration.connection_generation)
            .await
            .expect("恢复握手应完成");
        assert_eq!(descriptor.session_id, session_id);
    }

    #[tokio::test]
    async fn expired_persisted_lease_remains_recoverable() {
        let state_file = TestStateFile::new("expired");
        let agent_id = AgentInstanceId::new();
        let (pairing_code, session_id, resume_token) = {
            let relay = persisted_relay(&state_file.path, Duration::milliseconds(20))
                .expect("Relay 应启动");
            let (registration, _, session_id) = ready_agent(&relay, agent_id, None).await;
            (
                registration.welcome.pairing_code,
                session_id,
                registration.welcome.resume_token,
            )
        };
        time::sleep(time::Duration::from_millis(40)).await;

        let relay =
            persisted_relay(&state_file.path, Duration::minutes(10)).expect("Relay 应恢复过期身份");
        {
            let state = relay.state.lock().await;
            let agent = state.agents.get(&agent_id).expect("过期身份仍应保留");
            assert!(agent.lease_expired);
            assert_eq!(agent.session_id, session_id);
            assert_eq!(agent.lease.pairing_code, pairing_code);
            assert!(!agent.ready);
            assert!(agent.sender.is_none());
        }
        let persisted: PersistedRelayState = serde_json::from_str(
            &std::fs::read_to_string(&state_file.path).expect("状态文件应存在"),
        )
        .expect("状态文件应为 JSON");
        assert_eq!(persisted.agents.len(), 1);

        let (_, _, resumed_session_id) = ready_agent(&relay, agent_id, Some(resume_token)).await;
        assert_eq!(resumed_session_id, session_id);
        let state = relay.state.lock().await;
        let agent = state.agents.get(&agent_id).expect("Agent 应恢复在线");
        assert!(!agent.lease_expired);
        assert!(agent.ready);
    }

    #[test]
    fn corrupted_persisted_state_refuses_startup() {
        let state_file = TestStateFile::new("corrupted");
        std::fs::create_dir_all(&state_file.root).expect("应创建测试目录");
        std::fs::write(&state_file.path, b"{not-json").expect("应写入损坏状态");

        assert!(
            persisted_relay(&state_file.path, Duration::minutes(10)).is_err(),
            "损坏状态文件必须拒绝启动"
        );
    }

    #[tokio::test]
    async fn duplicate_agent_or_pairing_code_in_state_refuses_startup() {
        let state_file = TestStateFile::new("duplicate");
        {
            let relay =
                persisted_relay(&state_file.path, Duration::minutes(10)).expect("Relay 应启动");
            let _ = ready_agent(&relay, AgentInstanceId::new(), None).await;
        }
        let mut persisted: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&state_file.path).expect("状态文件应存在"),
        )
        .expect("状态文件应为 JSON");
        let duplicate = persisted["agents"][0].clone();
        persisted["agents"]
            .as_array_mut()
            .expect("agents 应为数组")
            .push(duplicate);
        std::fs::write(
            &state_file.path,
            serde_json::to_vec_pretty(&persisted).expect("应编码重复状态"),
        )
        .expect("应写入重复状态");

        assert!(
            persisted_relay(&state_file.path, Duration::minutes(10)).is_err(),
            "重复 Agent 或控制码必须拒绝启动"
        );
    }

    #[tokio::test]
    async fn resume_token_is_committed_only_after_confirmation() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (sender, _receiver) = test_channel();
        let first = relay
            .register_agent(hello(agent_id, None), sender)
            .await
            .expect("首次注册应成功");
        assert!(
            relay
                .state
                .lock()
                .await
                .agents
                .get(&agent_id)
                .expect("Agent 应存在")
                .resume_token
                .is_none()
        );

        let (sender, _receiver) = test_channel();
        let second = relay
            .register_agent(hello(agent_id, None), sender)
            .await
            .expect("Welcome 未确认时仍应接受原始空令牌");
        assert_ne!(first.welcome.resume_token, second.welcome.resume_token);
        assert!(
            relay
                .commit_agent_resume(agent_id, second.connection_generation)
                .await
        );
        assert_eq!(
            relay
                .state
                .lock()
                .await
                .agents
                .get(&agent_id)
                .expect("Agent 应存在")
                .resume_token
                .as_deref(),
            Some(second.welcome.resume_token.as_str())
        );
        relay
            .finalize_agent_resume(agent_id, second.connection_generation)
            .await
            .expect("最终确认应成功");

        let (sender, _receiver) = test_channel();
        assert!(
            relay
                .register_agent(hello(agent_id, Some(first.welcome.resume_token)), sender,)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn stale_agent_disconnect_cannot_clear_new_connection() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (first_sender, _first_receiver) = test_channel();
        let first = relay
            .register_agent(hello(agent_id, None), first_sender)
            .await
            .expect("首次注册应成功");
        assert!(
            relay
                .commit_agent_resume(agent_id, first.connection_generation)
                .await
        );
        relay
            .finalize_agent_resume(agent_id, first.connection_generation)
            .await
            .expect("首次确认应成功");

        let (invalid_sender, _invalid_receiver) = test_channel();
        assert!(
            relay
                .register_agent(hello(agent_id, None), invalid_sender)
                .await
                .is_err(),
            "首次确认后空恢复令牌必须失效"
        );

        let current_token = first.welcome.resume_token.clone();
        let (second_sender, _second_receiver) = test_channel();
        let second = relay
            .register_agent(hello(agent_id, Some(current_token)), second_sender)
            .await
            .expect("新连接应恢复成功");
        assert!(
            relay
                .commit_agent_resume(agent_id, second.connection_generation)
                .await
        );
        relay
            .finalize_agent_resume(agent_id, second.connection_generation)
            .await
            .expect("新连接确认应成功");

        relay
            .mark_agent_disconnected(agent_id, first.connection_generation)
            .await;
        let state = relay.state.lock().await;
        let agent = state.agents.get(&agent_id).expect("Agent 应存在");
        assert_eq!(agent.connection_generation, second.connection_generation);
        assert!(agent.sender.is_some());
        assert!(agent.ready);
    }

    #[test]
    fn controller_authentication_requires_role_specific_token() {
        let relay = relay(Duration::minutes(10));
        let controller_id = ControllerInstanceId::new();
        assert!(
            relay
                .authenticate_controller(&ControllerHello {
                    protocol_version: PROTOCOL_VERSION,
                    controller_instance_id: controller_id,
                    owner_id: test_owner_id(),
                    kind: ControllerKind::Human,
                    auth_token: HUMAN_TOKEN.to_owned(),
                    hostname: None,
                    mac_address: None,
                })
                .is_ok()
        );
        assert!(
            relay
                .authenticate_controller(&ControllerHello {
                    protocol_version: PROTOCOL_VERSION,
                    controller_instance_id: controller_id,
                    owner_id: test_owner_id(),
                    kind: ControllerKind::Ai,
                    auth_token: HUMAN_TOKEN.to_owned(),
                    hostname: None,
                    mac_address: None,
                })
                .is_err()
        );
        assert!(
            relay
                .authenticate_controller(&ControllerHello {
                    protocol_version: PROTOCOL_VERSION,
                    controller_instance_id: controller_id,
                    owner_id: test_owner_id(),
                    kind: ControllerKind::Human,
                    auth_token: AI_TOKEN.to_owned(),
                    hostname: None,
                    mac_address: None,
                })
                .is_err()
        );
        assert!(
            relay
                .authenticate_controller(&ControllerHello {
                    protocol_version: PROTOCOL_VERSION,
                    controller_instance_id: controller_id,
                    owner_id: test_owner_id(),
                    kind: ControllerKind::Ai,
                    auth_token: AI_TOKEN.to_owned(),
                    hostname: None,
                    mac_address: None,
                })
                .is_ok()
        );
        assert!(
            relay
                .authenticate_controller(&ControllerHello {
                    protocol_version: PROTOCOL_VERSION,
                    controller_instance_id: controller_id,
                    owner_id: other_owner_id(),
                    kind: ControllerKind::Human,
                    auth_token: HUMAN_TOKEN.to_owned(),
                    hostname: None,
                    mac_address: None,
                })
                .is_err()
        );
    }

    #[tokio::test]
    async fn session_accepts_same_owner_human_and_ai_but_rejects_other_owner() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, _agent_receiver, session_id) = ready_agent(&relay, agent_id, None).await;
        relay
            .update_agent_permission_mode(
                agent_id,
                agent.connection_generation,
                PermissionMode::FullAccess,
            )
            .await;
        let pairing_code = agent.welcome.pairing_code.clone();
        let (human_id, human_generation, _) =
            register_controller(&relay, ControllerKind::Human).await;
        let human_result = relay
            .pair_controller(
                human_id,
                human_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: pairing_code.clone(),
                    permission_mode: PermissionMode::FullAccess,
                },
            )
            .await;
        assert!(human_result.error.is_none());
        assert_eq!(
            human_result.connection.expect("Human 配对应返回连接").role,
            SessionRole::HumanControl
        );

        let (ai_id, ai_generation, _) = register_controller(&relay, ControllerKind::Ai).await;
        let ai_result = relay
            .pair_controller(
                ai_id,
                ai_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: pairing_code.clone(),
                    permission_mode: PermissionMode::ReadOnly,
                },
            )
            .await;
        assert!(ai_result.error.is_none());
        let ai_connection = ai_result.connection.expect("AI 配对应返回连接");
        assert_eq!(ai_connection.role, SessionRole::AiReadOnly);
        assert_eq!(ai_connection.permission_mode, PermissionMode::ReadOnly);

        let (other_ai_id, other_ai_generation, _) =
            register_controller_for_owner(&relay, other_owner_id(), ControllerKind::Ai).await;
        let other_result = relay
            .pair_controller(
                other_ai_id,
                other_ai_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code,
                    permission_mode: PermissionMode::FullAccess,
                },
            )
            .await;
        assert!(other_result.error.is_some());

        let state = relay.state.lock().await;
        let bindings = state
            .session_bindings
            .get(&session_id)
            .expect("Session 应保存 Owner 绑定");
        assert_eq!(bindings.owner_id, Some(test_owner_id()));
        assert_eq!(
            bindings.permission_mode_for(ControllerKind::Human),
            PermissionMode::FullAccess
        );
        assert_eq!(
            bindings.permission_mode_for(ControllerKind::Ai),
            PermissionMode::ReadOnly
        );
        assert_eq!(bindings.permission_mode(), PermissionMode::ReadOnly);
        assert!(bindings.human.is_some());
        assert!(bindings.ai.is_some());
    }

    #[tokio::test]
    async fn agent_permission_mode_controls_effective_session_access() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, mut agent_receiver, session_id) = ready_agent(&relay, agent_id, None).await;
        let (ai_id, ai_generation, mut ai_receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        let result = relay
            .pair_controller(
                ai_id,
                ai_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: agent.welcome.pairing_code,
                    permission_mode: PermissionMode::FullAccess,
                },
            )
            .await;
        assert_eq!(
            result
                .connection
                .as_ref()
                .expect("配对应返回连接")
                .permission_mode,
            PermissionMode::ApprovalRequired
        );
        assert!(matches!(
            agent_receiver.recv().await,
            Some(WireMessage::ControllerBinding(binding))
                if binding.session_id == session_id
                    && binding.permission_mode == PermissionMode::ApprovalRequired
        ));

        relay
            .update_agent_permission_mode(
                agent_id,
                agent.connection_generation,
                PermissionMode::FullAccess,
            )
            .await;

        assert!(matches!(
            agent_receiver.recv().await,
            Some(WireMessage::ControllerBinding(binding))
                if binding.session_id == session_id
                    && binding.permission_mode == PermissionMode::FullAccess
        ));
        assert!(matches!(
            ai_receiver.recv().await,
            Some(WireMessage::ConnectionUpdated(connection))
                if connection.session_id == session_id
                    && connection.permission_mode == PermissionMode::FullAccess
        ));
        assert_eq!(
            relay
                .state
                .lock()
                .await
                .session_bindings
                .get(&session_id)
                .expect("Session 绑定应存在")
                .permission_mode(),
            PermissionMode::FullAccess
        );

        relay
            .update_agent_permission_mode(
                agent_id,
                agent.connection_generation,
                PermissionMode::ApprovalRequired,
            )
            .await;

        assert!(matches!(
            agent_receiver.recv().await,
            Some(WireMessage::ControllerBinding(binding))
                if binding.session_id == session_id
                    && binding.permission_mode == PermissionMode::ApprovalRequired
        ));
        assert!(matches!(
            ai_receiver.recv().await,
            Some(WireMessage::ConnectionUpdated(connection))
                if connection.session_id == session_id
                    && connection.permission_mode == PermissionMode::ApprovalRequired
        ));
        assert_eq!(
            relay
                .state
                .lock()
                .await
                .session_bindings
                .get(&session_id)
                .expect("Session 绑定应存在")
                .permission_mode(),
            PermissionMode::ApprovalRequired
        );

        relay
            .mark_agent_disconnected(agent_id, agent.connection_generation)
            .await;
        assert!(matches!(
            ai_receiver.recv().await,
            Some(WireMessage::ConnectionUpdated(connection))
                if connection.session_id == session_id
                    && connection.state == ConnectionState::Reconnecting
                    && connection.permission_mode == PermissionMode::ApprovalRequired
        ));
    }

    #[tokio::test]
    async fn ai_controller_approved_mode_is_not_downgraded_by_agent_approval_default() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, mut agent_receiver, _) = ready_agent(&relay, agent_id, None).await;
        let (ai_id, ai_generation, _) = register_controller(&relay, ControllerKind::Ai).await;
        let result = relay
            .pair_controller(
                ai_id,
                ai_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: agent.welcome.pairing_code,
                    permission_mode: PermissionMode::ControllerApproved,
                },
            )
            .await;

        assert_eq!(
            result.connection.expect("配对应成功").permission_mode,
            PermissionMode::ControllerApproved
        );
        assert!(matches!(
            agent_receiver.recv().await,
            Some(WireMessage::ControllerBinding(binding))
                if binding.permission_mode == PermissionMode::ControllerApproved
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn ai_permission_survives_later_human_pairing_takeover_and_release() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, mut agent_receiver, session_id) = ready_agent(&relay, agent_id, None).await;
        let pairing_code = agent.welcome.pairing_code.clone();
        let (ai_id, ai_generation, mut ai_receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        let ai_result = relay
            .pair_controller(
                ai_id,
                ai_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: pairing_code.clone(),
                    permission_mode: PermissionMode::ControllerApproved,
                },
            )
            .await;
        let ai_connection = ai_result.connection.expect("AI 配对应成功");
        assert_eq!(ai_connection.role, SessionRole::AiControl);
        assert_eq!(
            ai_connection.permission_mode,
            PermissionMode::ControllerApproved
        );
        assert!(matches!(
            agent_receiver.recv().await,
            Some(WireMessage::ControllerBinding(binding))
                if binding.controller_kind == ControllerKind::Ai
                    && binding.permission_mode == PermissionMode::ControllerApproved
        ));

        let (human_id, human_generation, mut human_receiver) =
            register_controller(&relay, ControllerKind::Human).await;
        let human_result = relay
            .pair_controller(
                human_id,
                human_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code,
                    permission_mode: PermissionMode::ApprovalRequired,
                },
            )
            .await;
        let human_connection = human_result.connection.expect("Human 配对应成功");
        assert_eq!(human_connection.role, SessionRole::AiControl);
        assert_eq!(
            human_connection.permission_mode,
            PermissionMode::ControllerApproved
        );
        let mut ai_binding_permission = None;
        let mut human_binding_permission = None;
        for _ in 0..2 {
            let Some(WireMessage::ControllerBinding(binding)) = agent_receiver.recv().await else {
                panic!("Agent 应收到两个角色的独立绑定");
            };
            match binding.controller_kind {
                ControllerKind::Ai => ai_binding_permission = Some(binding.permission_mode),
                ControllerKind::Human => human_binding_permission = Some(binding.permission_mode),
            }
        }
        assert_eq!(
            ai_binding_permission,
            Some(PermissionMode::ControllerApproved)
        );
        assert_eq!(
            human_binding_permission,
            Some(PermissionMode::ApprovalRequired)
        );
        assert!(matches!(
            ai_receiver.recv().await,
            Some(WireMessage::ConnectionUpdated(connection))
                if connection.role == SessionRole::AiControl
                    && connection.permission_mode == PermissionMode::ControllerApproved
        ));

        let takeover_request = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Ai,
            operation: RemoteOperation::HumanTakeover,
            approval_id: None,
            payload_base64: None,
        };
        relay
            .forward_controller_request(
                human_id,
                human_generation,
                takeover_request,
                &human_sender(&relay, human_id).await,
            )
            .await;
        let takeover = recv_authorized(&mut agent_receiver).await;
        assert_eq!(
            takeover.authorization.permission_mode,
            PermissionMode::ApprovalRequired
        );
        for receiver in [&mut human_receiver, &mut ai_receiver] {
            assert!(matches!(
                receiver.recv().await,
                Some(WireMessage::ConnectionUpdated(connection))
                    if connection.role == SessionRole::HumanControl
                        && connection.permission_mode == PermissionMode::ApprovalRequired
            ));
        }

        let ai_read_request = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Human,
            operation: run_command("Get-Host", true),
            approval_id: None,
            payload_base64: None,
        };
        relay
            .forward_controller_request(
                ai_id,
                ai_generation,
                ai_read_request,
                &ai_sender(&relay, ai_id).await,
            )
            .await;
        assert_eq!(
            recv_authorized(&mut agent_receiver)
                .await
                .authorization
                .permission_mode,
            PermissionMode::ReadOnly
        );

        let release_request = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Ai,
            operation: RemoteOperation::ReleaseHumanTakeover,
            approval_id: None,
            payload_base64: None,
        };
        relay
            .forward_controller_request(
                human_id,
                human_generation,
                release_request,
                &human_sender(&relay, human_id).await,
            )
            .await;
        assert_eq!(
            recv_authorized(&mut agent_receiver)
                .await
                .authorization
                .permission_mode,
            PermissionMode::ApprovalRequired
        );
        for receiver in [&mut human_receiver, &mut ai_receiver] {
            assert!(matches!(
                receiver.recv().await,
                Some(WireMessage::ConnectionUpdated(connection))
                    if connection.role == SessionRole::AiControl
                        && connection.permission_mode == PermissionMode::ControllerApproved
            ));
        }

        let ai_write_request = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Human,
            operation: run_command("Set-Content C:\\temp\\relay-test.txt ok", false),
            approval_id: None,
            payload_base64: None,
        };
        relay
            .forward_controller_request(
                ai_id,
                ai_generation,
                ai_write_request,
                &ai_sender(&relay, ai_id).await,
            )
            .await;
        assert_eq!(
            recv_authorized(&mut agent_receiver)
                .await
                .authorization
                .permission_mode,
            PermissionMode::ControllerApproved
        );

        let state = relay.state.lock().await;
        let bindings = state
            .session_bindings
            .get(&session_id)
            .expect("Session 绑定应存在");
        assert_eq!(
            bindings.permission_mode_for(ControllerKind::Ai),
            PermissionMode::ControllerApproved
        );
        assert_eq!(
            bindings.permission_mode_for(ControllerKind::Human),
            PermissionMode::ApprovalRequired
        );
        assert!(!bindings.human_takeover);
    }

    #[test]
    fn controller_approved_overrides_agent_default_but_not_readonly() {
        assert_eq!(
            minimum_permission_mode(
                PermissionMode::ControllerApproved,
                PermissionMode::ApprovalRequired
            ),
            PermissionMode::ControllerApproved
        );
        assert_eq!(
            minimum_permission_mode(PermissionMode::ControllerApproved, PermissionMode::ReadOnly),
            PermissionMode::ReadOnly
        );
    }

    #[tokio::test]
    async fn human_controller_cannot_request_controller_approved_mode() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, _, _) = ready_agent(&relay, agent_id, None).await;
        let (human_id, human_generation, _) =
            register_controller(&relay, ControllerKind::Human).await;
        let result = relay
            .pair_controller(
                human_id,
                human_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: agent.welcome.pairing_code,
                    permission_mode: PermissionMode::ControllerApproved,
                },
            )
            .await;

        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn human_emergency_stop_aborts_owner_requests_and_downgrades_session() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, mut agent_receiver, session_id) = ready_agent(&relay, agent_id, None).await;
        relay
            .update_agent_permission_mode(
                agent_id,
                agent.connection_generation,
                PermissionMode::FullAccess,
            )
            .await;
        let (human_id, human_generation, _) =
            register_controller(&relay, ControllerKind::Human).await;
        relay
            .pair_controller(
                human_id,
                human_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: agent.welcome.pairing_code.clone(),
                    permission_mode: PermissionMode::FullAccess,
                },
            )
            .await;
        let (ai_id, ai_generation, mut ai_receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        relay
            .pair_controller(
                ai_id,
                ai_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: agent.welcome.pairing_code,
                    permission_mode: PermissionMode::FullAccess,
                },
            )
            .await;

        let stopped_request_id = RequestId::new();
        relay.state.lock().await.in_flight.insert(
            stopped_request_id,
            InFlightRequest {
                agent_id,
                session_id,
                controller_id: ai_id,
                owner_id: test_owner_id(),
                controller_generation: ai_generation,
                controller_kind: ControllerKind::Ai,
                source: EventSource::Ai,
                approval: ApprovalState::NotRequired,
                previous_takeover: None,
            },
        );

        let emergency_request_id = RequestId::new();
        let human_sender = human_sender(&relay, human_id).await;
        relay
            .forward_controller_request(
                human_id,
                human_generation,
                RemoteRequest {
                    request_id: emergency_request_id,
                    session_id,
                    source: EventSource::Ai,
                    operation: RemoteOperation::EmergencyStop,
                    approval_id: None,
                    payload_base64: None,
                },
                &human_sender,
            )
            .await;

        assert!(matches!(
            ai_receiver.recv().await,
            Some(WireMessage::Error {
                ref code,
                request_id: Some(request_id),
                ..
            }) if code == "emergency_stopped" && request_id == stopped_request_id
        ));
        let authorized = recv_authorized(&mut agent_receiver).await;
        assert_eq!(authorized.request.request_id, emergency_request_id);
        assert_eq!(authorized.request.source, EventSource::Human);
        assert_eq!(
            authorized.authorization.permission_mode,
            PermissionMode::ReadOnly
        );

        let state = relay.state.lock().await;
        assert!(!state.in_flight.contains_key(&stopped_request_id));
        let bindings = state
            .session_bindings
            .get(&session_id)
            .expect("Session 绑定应存在");
        assert_eq!(bindings.permission_mode(), PermissionMode::ReadOnly);
        assert!(bindings.human_takeover);
    }

    #[tokio::test]
    async fn ai_emergency_stop_is_rejected_without_side_effects() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, mut agent_receiver, session_id) = ready_agent(&relay, agent_id, None).await;
        let (ai_id, ai_generation, mut ai_receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        relay
            .pair_controller(
                ai_id,
                ai_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: agent.welcome.pairing_code,
                    permission_mode: PermissionMode::FullAccess,
                },
            )
            .await;
        let stopped_request_id = RequestId::new();
        relay.state.lock().await.in_flight.insert(
            stopped_request_id,
            InFlightRequest {
                agent_id,
                session_id,
                controller_id: ai_id,
                owner_id: test_owner_id(),
                controller_generation: ai_generation,
                controller_kind: ControllerKind::Ai,
                source: EventSource::Ai,
                approval: ApprovalState::NotRequired,
                previous_takeover: None,
            },
        );
        let before = relay.state.lock().await;
        let before_in_flight = before.in_flight.len();
        let before_mode = before
            .session_bindings
            .get(&session_id)
            .expect("Session 绑定应存在")
            .permission_mode();
        let before_takeover = before
            .session_bindings
            .get(&session_id)
            .expect("Session 绑定应存在")
            .human_takeover;
        drop(before);
        while agent_receiver.try_recv().is_ok() {}

        relay
            .forward_controller_request(
                ai_id,
                ai_generation,
                RemoteRequest {
                    request_id: RequestId::new(),
                    session_id,
                    source: EventSource::Ai,
                    operation: RemoteOperation::EmergencyStop,
                    approval_id: None,
                    payload_base64: None,
                },
                &ai_sender(&relay, ai_id).await,
            )
            .await;

        assert!(
            matches!(ai_receiver.recv().await, Some(WireMessage::Error { ref code, .. }) if code == "human_only")
        );
        assert!(
            agent_receiver.try_recv().is_err(),
            "拒绝的 AI 请求不得转发到 Agent"
        );
        let state = relay.state.lock().await;
        assert_eq!(state.in_flight.len(), before_in_flight);
        assert!(state.in_flight.contains_key(&stopped_request_id));
        assert_eq!(
            state
                .session_bindings
                .get(&session_id)
                .unwrap()
                .permission_mode(),
            before_mode
        );
        assert_eq!(
            state
                .session_bindings
                .get(&session_id)
                .unwrap()
                .human_takeover,
            before_takeover
        );
    }

    #[tokio::test]
    async fn duplicate_controller_instance_is_rejected() {
        let relay = relay(Duration::minutes(10));
        let controller_id = ControllerInstanceId::new();
        let (sender, _receiver) = test_channel();
        relay
            .register_controller(
                controller_id,
                test_owner_id(),
                ControllerKind::Human,
                None,
                None,
                sender,
            )
            .await
            .expect("首次 Controller 连接应成功");
        let (sender, _receiver) = test_channel();

        assert!(
            relay
                .register_controller(
                    controller_id,
                    test_owner_id(),
                    ControllerKind::Human,
                    None,
                    None,
                    sender
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn stale_controller_disconnect_cannot_remove_new_generation() {
        let relay = relay(Duration::minutes(10));
        let controller_id = ControllerInstanceId::new();
        let (sender, _receiver) = test_channel();
        let first_generation = relay
            .register_controller(
                controller_id,
                test_owner_id(),
                ControllerKind::Human,
                None,
                None,
                sender,
            )
            .await
            .expect("首次 Controller 连接应成功");
        relay
            .remove_controller(controller_id, first_generation)
            .await;
        let (sender, _receiver) = test_channel();
        let second_generation = relay
            .register_controller(
                controller_id,
                test_owner_id(),
                ControllerKind::Human,
                None,
                None,
                sender,
            )
            .await
            .expect("旧连接移除后应允许重新连接");

        relay
            .remove_controller(controller_id, first_generation)
            .await;

        assert!(
            relay
                .state
                .lock()
                .await
                .controllers
                .get(&controller_id)
                .is_some_and(|controller| {
                    controller.connection_generation == second_generation
                })
        );
    }

    #[tokio::test]
    async fn pairing_with_same_owner_replaces_stale_same_role_controller() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, mut agent_receiver, _) = ready_agent(&relay, agent_id, None).await;
        let pairing_code = agent.welcome.pairing_code.clone();
        let (first_id, first_generation, mut first_receiver) =
            register_controller(&relay, ControllerKind::Human).await;
        let (second_id, second_generation, _) =
            register_controller(&relay, ControllerKind::Human).await;

        let first = relay
            .pair_controller(
                first_id,
                first_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: pairing_code.clone(),
                    permission_mode: PermissionMode::ApprovalRequired,
                },
            )
            .await;
        let second = relay
            .pair_controller(
                second_id,
                second_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: pairing_code.clone(),
                    permission_mode: PermissionMode::ApprovalRequired,
                },
            )
            .await;

        assert!(first.connection.is_some());
        let first_binding = match agent_receiver.recv().await {
            Some(WireMessage::ControllerBinding(binding)) => binding,
            other => panic!("Agent 应收到首次绑定，实际为 {other:?}"),
        };
        assert!(second.connection.is_some());
        assert!(matches!(
            agent_receiver.recv().await,
            Some(WireMessage::ControllerBindingRevoked {
                session_id: _,
                ref binding_token,
            }) if binding_token == &first_binding.binding_token
        ));
        let second_binding = match agent_receiver.recv().await {
            Some(WireMessage::ControllerBinding(binding)) => binding,
            other => panic!("Agent 应收到新绑定，实际为 {other:?}"),
        };
        assert_ne!(first_binding.binding_token, second_binding.binding_token);
        assert!(matches!(
            first_receiver.recv().await,
            Some(WireMessage::ConnectionRemoved { session_id: removed, .. })
                if removed == second_binding.session_id
        ));
        assert!(
            relay
                .state
                .lock()
                .await
                .controllers
                .get(&first_id)
                .is_some_and(|controller| !controller
                    .sessions
                    .contains(&second_binding.session_id))
        );
    }

    #[tokio::test]
    async fn explicit_session_release_is_confirmed_and_allows_immediate_repair() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, mut agent_receiver, session_id) = ready_agent(&relay, agent_id, None).await;
        let pairing_code = agent.welcome.pairing_code.clone();
        let (first_id, first_generation, _) = register_controller(&relay, ControllerKind::Ai).await;
        let first = relay
            .pair_controller(
                first_id,
                first_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: pairing_code.clone(),
                    permission_mode: PermissionMode::ApprovalRequired,
                },
            )
            .await;
        assert!(first.connection.is_some());
        let first_binding = match agent_receiver.recv().await {
            Some(WireMessage::ControllerBinding(binding)) => binding,
            other => panic!("Agent 应收到首次绑定，实际为 {other:?}"),
        };

        let release = relay
            .release_controller_session(
                first_id,
                first_generation,
                ReleaseSessionRequest {
                    request_id: RequestId::new(),
                    session_id,
                },
            )
            .await;
        assert!(release.released);
        assert!(release.error.is_none());
        assert!(matches!(
            agent_receiver.recv().await,
            Some(WireMessage::ControllerBindingRevoked {
                session_id: revoked,
                ref binding_token,
            }) if revoked == session_id && binding_token == &first_binding.binding_token
        ));

        let (second_id, second_generation, _) =
            register_controller(&relay, ControllerKind::Ai).await;
        let second = relay
            .pair_controller(
                second_id,
                second_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code,
                    permission_mode: PermissionMode::ApprovalRequired,
                },
            )
            .await;
        assert!(second.connection.is_some());
        assert!(matches!(
            agent_receiver.recv().await,
            Some(WireMessage::ControllerBinding(_))
        ));
    }

    #[tokio::test]
    async fn relay_overrides_source_and_readonly_before_forwarding() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, mut agent_receiver, session_id) = ready_agent(&relay, agent_id, None).await;
        let (controller_id, generation, _controller_receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        relay
            .pair_controller(
                controller_id,
                generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: agent.welcome.pairing_code.clone(),
                    permission_mode: PermissionMode::ApprovalRequired,
                },
            )
            .await;
        assert!(matches!(
            agent_receiver.recv().await,
            Some(WireMessage::ControllerBinding(_))
        ));
        let (error_sender, mut error_receiver) = test_channel();
        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Human,
            operation: run_command("Get-Process", false),
            approval_id: Some(ApprovalId::new()),
            payload_base64: Some("opaque-hpke-envelope".to_owned()),
        };
        relay
            .forward_controller_request(controller_id, generation, request, &error_sender)
            .await;

        let authorized = recv_authorized(&mut agent_receiver).await;
        assert_eq!(authorized.request.source, EventSource::Ai);
        assert!(matches!(
            authorized.request.operation,
            RemoteOperation::RunCommand { readonly: true, .. }
        ));
        assert!(authorized.request.approval_id.is_none());
        assert_eq!(
            authorized.request.payload_base64.as_deref(),
            Some("opaque-hpke-envelope")
        );
        assert_eq!(
            authorized.authorization.approval,
            ApprovalState::NotRequired
        );
        assert!(error_receiver.try_recv().is_err());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn approval_is_human_only_exact_and_single_use() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, mut agent_receiver, session_id) = ready_agent(&relay, agent_id, None).await;
        let (ai_id, ai_generation, mut ai_receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        relay
            .pair_controller(
                ai_id,
                ai_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: agent.welcome.pairing_code.clone(),
                    permission_mode: PermissionMode::ApprovalRequired,
                },
            )
            .await;
        let _ = agent_receiver.recv().await;
        let operation = run_command("Remove-Item C:\\temp\\a.txt", true);
        let approval = relay
            .request_approval(
                ai_id,
                ai_generation,
                ApprovalRequest {
                    request_id: RequestId::new(),
                    session_id,
                    operation: operation.clone(),
                },
            )
            .await;
        assert_eq!(approval.state, ApprovalState::Pending);
        assert!(matches!(
            approval.operation,
            RemoteOperation::RunCommand {
                readonly: false,
                ..
            }
        ));
        let approval_id = approval.approval_id.expect("应签发审批标识");

        let denied = relay
            .decide_approval(
                ai_id,
                ai_generation,
                ApprovalDecision {
                    request_id: RequestId::new(),
                    session_id,
                    operation: operation.clone(),
                    approval_id,
                    approved: true,
                },
            )
            .await;
        assert_eq!(denied.state, ApprovalState::Rejected);

        let (human_id, human_generation, _) =
            register_controller(&relay, ControllerKind::Human).await;
        relay
            .pair_controller(
                human_id,
                human_generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: agent.welcome.pairing_code.clone(),
                    permission_mode: PermissionMode::ApprovalRequired,
                },
            )
            .await;
        let _ = agent_receiver.recv().await;
        assert!(matches!(
            ai_receiver.recv().await,
            Some(WireMessage::ConnectionUpdated(connection))
                if connection.role == SessionRole::AiControl
                    && connection.permission_mode == PermissionMode::ApprovalRequired
        ));
        let approved = relay
            .decide_approval(
                human_id,
                human_generation,
                ApprovalDecision {
                    request_id: RequestId::new(),
                    session_id,
                    operation: operation.clone(),
                    approval_id,
                    approved: true,
                },
            )
            .await;
        assert_eq!(approved.state, ApprovalState::Approved);

        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Human,
            operation: operation.clone(),
            approval_id: Some(approval_id),
            payload_base64: None,
        };
        let ai_sender = relay
            .state
            .lock()
            .await
            .controllers
            .get(&ai_id)
            .expect("AI Controller 应存在")
            .sender
            .clone();
        relay
            .forward_controller_request(ai_id, ai_generation, request.clone(), &ai_sender)
            .await;
        let authorized = recv_authorized(&mut agent_receiver).await;
        assert_eq!(authorized.request.source, EventSource::Ai);
        assert_eq!(authorized.authorization.approval, ApprovalState::Approved);

        relay
            .forward_agent_message(
                agent_id,
                agent.connection_generation,
                WireMessage::RemoteResponse(remoteops_protocol::RemoteResponse {
                    request_id: request.request_id,
                    session_id,
                    exit_code: Some(0),
                    summary: "done".to_owned(),
                    error_code: None,
                    payload_base64: None,
                    sha256: None,
                    details: None,
                }),
            )
            .await;
        assert!(matches!(
            ai_receiver.recv().await,
            Some(WireMessage::RemoteResponse(_))
        ));
        relay
            .forward_controller_request(ai_id, ai_generation, request, &ai_sender)
            .await;
        let replay_error = ai_receiver.recv().await.expect("重放必须返回错误");
        assert!(matches!(
            replay_error,
            WireMessage::Error { ref code, .. } if code == "approval_not_granted"
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn human_and_ai_share_session_and_human_takeover_blocks_ai_writes() {
        let relay = relay(Duration::minutes(10));
        let agent_id = AgentInstanceId::new();
        let (agent, mut agent_receiver, session_id) = ready_agent(&relay, agent_id, None).await;
        let (human_id, human_generation, mut human_receiver) =
            register_controller(&relay, ControllerKind::Human).await;
        let (ai_id, ai_generation, mut ai_receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        for (controller_id, generation) in [(human_id, human_generation), (ai_id, ai_generation)] {
            let result = relay
                .pair_controller(
                    controller_id,
                    generation,
                    PairRequest {
                        request_id: RequestId::new(),
                        pairing_code: agent.welcome.pairing_code.clone(),
                        permission_mode: PermissionMode::ApprovalRequired,
                    },
                )
                .await;
            assert!(result.connection.is_some());
            assert!(matches!(
                agent_receiver.recv().await,
                Some(WireMessage::ControllerBinding(_))
            ));
        }
        assert!(matches!(
            human_receiver.recv().await,
            Some(WireMessage::ConnectionUpdated(connection))
                if connection.role == SessionRole::AiControl
        ));

        let ai_read_request = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Human,
            operation: run_command("Get-Process", false),
            approval_id: None,
            payload_base64: None,
        };
        let ai_read_request_id = ai_read_request.request_id;
        relay
            .forward_controller_request(
                ai_id,
                ai_generation,
                ai_read_request,
                &ai_sender(&relay, ai_id).await,
            )
            .await;
        let authorized = recv_authorized(&mut agent_receiver).await;
        assert_eq!(authorized.authorization.controller_kind, ControllerKind::Ai);
        let event = WireMessage::RemoteEvent(remoteops_domain::RemoteEvent {
            sequence: 1,
            session_id,
            request_id: Some(ai_read_request_id),
            source: EventSource::Ai,
            approval: ApprovalState::NotRequired,
            payload: EventPayload::OperationStarted,
            occurred_at: Utc::now(),
        });
        relay
            .forward_agent_message(agent_id, agent.connection_generation, event)
            .await;
        assert!(matches!(
            ai_receiver.recv().await,
            Some(WireMessage::RemoteEvent(_))
        ));
        assert!(matches!(
            human_receiver.recv().await,
            Some(WireMessage::RemoteEvent(_))
        ));

        let human_sender = human_sender(&relay, human_id).await;
        let takeover_request = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Ai,
            operation: RemoteOperation::HumanTakeover,
            approval_id: None,
            payload_base64: None,
        };
        relay
            .forward_controller_request(human_id, human_generation, takeover_request, &human_sender)
            .await;
        let takeover = recv_authorized(&mut agent_receiver).await;
        assert_eq!(
            takeover.authorization.controller_kind,
            ControllerKind::Human
        );
        for receiver in [&mut human_receiver, &mut ai_receiver] {
            assert!(matches!(
                receiver.recv().await,
                Some(WireMessage::ConnectionUpdated(connection))
                    if connection.role == SessionRole::HumanControl
            ));
        }

        let ai_write = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Human,
            operation: run_command("Remove-Item C:\\temp\\a.txt", false),
            approval_id: None,
            payload_base64: None,
        };
        relay
            .forward_controller_request(
                ai_id,
                ai_generation,
                ai_write,
                &ai_sender(&relay, ai_id).await,
            )
            .await;
        assert!(matches!(
            ai_receiver.recv().await,
            Some(WireMessage::Error { ref code, .. }) if code == "ai_write_suspended"
        ));

        let release_request = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Ai,
            operation: RemoteOperation::ReleaseHumanTakeover,
            approval_id: None,
            payload_base64: None,
        };
        relay
            .forward_controller_request(human_id, human_generation, release_request, &human_sender)
            .await;
        let release = recv_authorized(&mut agent_receiver).await;
        assert_eq!(release.authorization.controller_kind, ControllerKind::Human);
        for receiver in [&mut human_receiver, &mut ai_receiver] {
            assert!(matches!(
                receiver.recv().await,
                Some(WireMessage::ConnectionUpdated(connection))
                    if connection.role == SessionRole::AiControl
            ));
        }

        let ai_write_after_release = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Human,
            operation: run_command("Remove-Item C:\\temp\\a.txt", false),
            approval_id: None,
            payload_base64: None,
        };
        relay
            .forward_controller_request(
                ai_id,
                ai_generation,
                ai_write_after_release,
                &ai_sender(&relay, ai_id).await,
            )
            .await;
        assert!(matches!(
            ai_receiver.recv().await,
            Some(WireMessage::Error { ref code, .. }) if code == "approval_required"
        ));

        let ai_read_after_takeover = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Human,
            operation: run_command("Get-Host", false),
            approval_id: None,
            payload_base64: None,
        };
        let ai_read_after_takeover_id = ai_read_after_takeover.request_id;
        relay
            .forward_controller_request(
                ai_id,
                ai_generation,
                ai_read_after_takeover,
                &ai_sender(&relay, ai_id).await,
            )
            .await;
        let _ = recv_authorized(&mut agent_receiver).await;

        let cancel = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Ai,
            operation: RemoteOperation::CancelRequest {
                request_id: ai_read_after_takeover_id,
            },
            approval_id: None,
            payload_base64: None,
        };
        relay
            .forward_controller_request(human_id, human_generation, cancel, &human_sender)
            .await;
        let cancel_authorized = recv_authorized(&mut agent_receiver).await;
        assert_eq!(
            cancel_authorized.authorization.controller_kind,
            ControllerKind::Human
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn agent_messages_are_bound_to_original_agent_session_source_and_approval() {
        let relay = relay(Duration::minutes(10));
        let first_agent_id = AgentInstanceId::new();
        let second_agent_id = AgentInstanceId::new();
        let (first_agent, mut first_agent_receiver, first_session) =
            ready_agent(&relay, first_agent_id, None).await;
        let (second_agent, mut second_agent_receiver, second_session) =
            ready_agent(&relay, second_agent_id, None).await;
        let (controller_id, generation, mut controller_receiver) =
            register_controller(&relay, ControllerKind::Human).await;
        for pairing_code in [
            first_agent.welcome.pairing_code.clone(),
            second_agent.welcome.pairing_code.clone(),
        ] {
            let result = relay
                .pair_controller(
                    controller_id,
                    generation,
                    PairRequest {
                        request_id: RequestId::new(),
                        pairing_code,
                        permission_mode: PermissionMode::ApprovalRequired,
                    },
                )
                .await;
            assert!(result.connection.is_some());
        }
        assert!(matches!(
            first_agent_receiver.recv().await,
            Some(WireMessage::ControllerBinding(_))
        ));
        assert!(matches!(
            second_agent_receiver.recv().await,
            Some(WireMessage::ControllerBinding(_))
        ));
        let sender = relay
            .state
            .lock()
            .await
            .controllers
            .get(&controller_id)
            .expect("Controller 应存在")
            .sender
            .clone();
        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id: first_session,
            source: EventSource::Human,
            operation: run_command("hostname", false),
            approval_id: None,
            payload_base64: None,
        };
        let request_id = request.request_id;
        relay
            .forward_controller_request(controller_id, generation, request, &sender)
            .await;
        let _ = recv_authorized(&mut first_agent_receiver).await;

        relay
            .forward_agent_message(
                second_agent_id,
                second_agent.connection_generation,
                WireMessage::RemoteResponse(remoteops_protocol::RemoteResponse {
                    request_id,
                    session_id: first_session,
                    exit_code: Some(0),
                    summary: "forged cross-agent response".to_owned(),
                    error_code: None,
                    payload_base64: None,
                    sha256: None,
                    details: None,
                }),
            )
            .await;
        assert!(controller_receiver.try_recv().is_err());

        for (source, approval) in [
            (EventSource::System, ApprovalState::NotRequired),
            (EventSource::Human, ApprovalState::Approved),
        ] {
            relay
                .forward_agent_message(
                    first_agent_id,
                    first_agent.connection_generation,
                    WireMessage::RemoteEvent(remoteops_domain::RemoteEvent {
                        sequence: 1,
                        session_id: first_session,
                        request_id: Some(request_id),
                        source,
                        approval,
                        payload: EventPayload::OperationStarted,
                        occurred_at: Utc::now(),
                    }),
                )
                .await;
            assert!(controller_receiver.try_recv().is_err());
        }

        relay
            .forward_agent_message(
                first_agent_id,
                first_agent.connection_generation,
                WireMessage::RemoteResponse(remoteops_protocol::RemoteResponse {
                    request_id,
                    session_id: first_session,
                    exit_code: Some(0),
                    summary: "valid response".to_owned(),
                    error_code: None,
                    payload_base64: None,
                    sha256: None,
                    details: None,
                }),
            )
            .await;
        assert!(matches!(
            controller_receiver.recv().await,
            Some(WireMessage::RemoteResponse(response))
                if response.request_id == request_id && response.session_id == first_session
        ));
        assert_ne!(first_session, second_session);
    }

    #[tokio::test]
    async fn expired_pairing_code_is_retained_for_authenticated_recovery() {
        let relay = relay(Duration::milliseconds(20));
        let agent_id = AgentInstanceId::new();
        let (registration, _, session_id) = ready_agent(&relay, agent_id, None).await;
        let resume_token = registration.welcome.resume_token.clone();
        relay
            .mark_agent_disconnected(agent_id, registration.connection_generation)
            .await;
        time::sleep(time::Duration::from_millis(30)).await;
        relay.cleanup_expired().await;

        {
            let state = relay.state.lock().await;
            let agent = state.agents.get(&agent_id).expect("恢复身份应保留");
            assert!(agent.lease_expired);
            assert!(
                state
                    .pairing_index
                    .contains_key(&registration.welcome.pairing_code)
            );
        }

        let (_, _, resumed_session_id) = ready_agent(&relay, agent_id, Some(resume_token)).await;
        assert_eq!(resumed_session_id, session_id);
    }

    #[tokio::test]
    async fn admin_password_hash_persists_and_can_be_changed() {
        let state_file = TestStateFile::new("admin-password");
        let initial = "initial-admin-password-123";
        let replacement = "replacement-admin-password-456";
        let relay = persisted_relay(&state_file.path, Duration::minutes(10)).expect("Relay 应启动");
        relay
            .initialize_admin_password(Some(initial), false)
            .await
            .expect("应初始化管理密码");
        assert!(relay.admin_password_matches(initial).await);
        assert!(!relay.admin_password_matches(replacement).await);

        let outcome = relay
            .admin_change_password(initial, replacement, "test", None)
            .await;
        assert!(outcome.success);
        assert!(!relay.admin_password_matches(initial).await);
        assert!(relay.admin_password_matches(replacement).await);
        drop(relay);

        let restored =
            persisted_relay(&state_file.path, Duration::minutes(10)).expect("Relay 应恢复");
        assert!(restored.admin_password_matches(replacement).await);
        assert!(!restored.admin_password_matches(initial).await);
    }

    #[tokio::test]
    async fn legacy_sha256_password_is_migrated_after_successful_login() {
        let state_file = TestStateFile::new("legacy-admin-password");
        let password = "legacy-admin-password-123";
        let relay = persisted_relay(&state_file.path, Duration::minutes(10)).expect("Relay 应启动");
        {
            let mut state = relay.state.lock().await;
            let salt = "legacy-salt".to_owned();
            let digest = password_digest(password, &salt);
            state.admin_password_hash = Some(AdminPasswordHash {
                algorithm: None,
                salt,
                digest,
            });
            relay
                .persist_state_locked(&state)
                .expect("旧密码应能写入状态");
        }
        assert!(relay.admin_password_matches(password).await);
        let state = relay.state.lock().await;
        assert_eq!(
            state
                .admin_password_hash
                .as_ref()
                .unwrap()
                .algorithm
                .as_deref(),
            Some("argon2id")
        );
        drop(state);
        assert!(!relay.admin_password_matches("wrong-password-123").await);
    }

    #[test]
    fn admin_password_requires_twelve_characters() {
        assert!(validate_admin_password("123456789012").is_ok());
        assert!(validate_admin_password("12345678901").is_err());
    }
}
