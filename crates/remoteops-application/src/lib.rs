//! Controller、Agent 和 Relay 壳子共用的应用服务。
#![allow(clippy::missing_errors_doc)]

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use chrono::Utc;
use remoteops_audit::{JsonlAuditStore, sha256_bytes};
use remoteops_domain::{
    ApprovalId, ApprovalState, AuditEvent, ConnectionDescriptor, ConnectionState,
    ControllerOwnerId, DomainError, EventPayload, EventSource, PairingCode, PermissionMode,
    RemoteEvent, RemoteOperation, RequestId, SessionId,
};
use remoteops_policy::{DefaultPolicy, PolicyDecision};
pub use remoteops_protocol::ControllerKind;
use remoteops_protocol::{
    ApprovalDecision, ApprovalRequest, ApprovalResult, ClientHello, ControllerHello,
    PROTOCOL_VERSION, PairResult, ReleaseSessionRequest, ReleaseSessionResult, RemoteRequest,
    RemoteResponse, WireMessage, connect_tls, load_client_config, load_native_client_config,
    load_pinned_client_config, read_frame, write_frame,
};
use remoteops_session::ConnectionRegistry;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::{Mutex, broadcast, mpsc, oneshot, watch},
    time::{Duration as TokioDuration, interval, sleep, timeout},
};
use tokio_rustls::{client::TlsStream, rustls::ClientConfig};
use tracing::warn;

/// 应用服务错误。
#[derive(Debug, Error)]
pub enum ApplicationError {
    /// 领域规则拒绝操作。
    #[error(transparent)]
    Domain(#[from] DomainError),
    /// 操作需要人工审批。
    #[error("操作需要人工审批：{approval_id}")]
    ApprovalRequired {
        /// 新建的审批标识。
        approval_id: ApprovalId,
        /// 审批原因。
        reason: String,
    },
    /// 操作被策略拒绝。
    #[error("操作被策略拒绝：{0}")]
    PolicyDenied(String),
    /// 审批不存在或尚未批准。
    #[error("审批不存在或尚未批准")]
    ApprovalNotGranted,
    /// TLS、Relay 或本地传输失败。
    #[error("远程传输失败：{0}")]
    Transport(String),
    /// Agent 返回了远程执行错误。
    #[error("远程操作失败 [{code}]：{message}")]
    Remote {
        /// 稳定远程错误码。
        code: String,
        /// 远程安全摘要。
        message: String,
    },
    /// 等待远程响应超时。
    #[error("等待远程响应超时")]
    Timeout,
    /// 审计日志写入或导出失败。
    #[error("审计日志失败：{0}")]
    Audit(String),
}

/// Controller 壳子共用的连接、目标和审批服务。
pub struct ControllerCore {
    registry: ConnectionRegistry,
    policy: DefaultPolicy,
}

impl Default for ControllerCore {
    fn default() -> Self {
        Self {
            registry: ConnectionRegistry::new(),
            policy: DefaultPolicy::default(),
        }
    }
}

impl ControllerCore {
    /// 新增或更新 Relay 返回的连接。
    pub fn upsert_connection(&mut self, connection: ConnectionDescriptor) -> ConnectionDescriptor {
        self.registry.upsert(connection)
    }

    /// 返回全部连接。
    #[must_use]
    pub fn list_connections(&self) -> Vec<ConnectionDescriptor> {
        self.registry.list()
    }

    /// 按会话标识取得连接。
    #[must_use]
    pub fn get_connection(&self, session_id: SessionId) -> Option<&ConnectionDescriptor> {
        self.registry.get(session_id)
    }

    /// 解析会话 UUID、默认编号或别名。
    pub fn resolve_target(&self, target: &str) -> Result<SessionId, ApplicationError> {
        Ok(self.registry.resolve(target)?)
    }

    /// 修改连接别名。
    pub fn set_alias(
        &mut self,
        session_id: SessionId,
        alias: impl Into<String>,
    ) -> Result<(), ApplicationError> {
        Ok(self.registry.set_alias(session_id, alias)?)
    }

    /// 人工接管写入权。
    pub fn human_takeover(&mut self, session_id: SessionId) -> Result<(), ApplicationError> {
        Ok(self.registry.human_takeover(session_id)?)
    }

    /// 释放人工接管，使会话回到 AI 按策略工作的状态。
    pub fn release_human_takeover(
        &mut self,
        session_id: SessionId,
    ) -> Result<(), ApplicationError> {
        Ok(self.registry.release_human_takeover(session_id)?)
    }

    /// 从当前控制端移除连接。
    pub fn remove_connection(
        &mut self,
        session_id: SessionId,
    ) -> Result<ConnectionDescriptor, ApplicationError> {
        Ok(self.registry.remove(session_id)?)
    }

    /// 将当前所有连接标记为传输重连中。
    pub fn mark_all_reconnecting(&mut self) {
        self.registry.mark_all_reconnecting();
    }

    /// 根据策略创建可发送的远程请求，必要时生成审批。
    pub fn prepare_request(
        &mut self,
        session_id: SessionId,
        source: EventSource,
        operation: RemoteOperation,
        approval_id: Option<ApprovalId>,
    ) -> Result<RemoteRequest, ApplicationError> {
        if self.registry.get(session_id).is_none() {
            return Err(ApplicationError::Domain(DomainError::ConnectionNotFound));
        }

        match self.policy.evaluate(source, &operation) {
            PolicyDecision::Allow | PolicyDecision::RequireApproval { .. } => {}
            PolicyDecision::Deny { reason } => {
                return Err(ApplicationError::PolicyDenied(reason));
            }
        }

        Ok(RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source,
            operation,
            approval_id,
            payload_base64: None,
        })
    }
}

/// Controller 连接配置。
#[derive(Clone, Debug)]
pub struct RelayClientConfig {
    /// Relay 地址，例如 `127.0.0.1:7443`。
    pub relay_address: String,
    /// TLS 证书中的服务名或 IP。
    pub server_name: String,
    /// Relay 自签名 CA/证书路径；为空时使用操作系统可信根。
    pub ca_certificate: Option<PathBuf>,
    /// 用户确认并固定的 Relay 叶证书 SHA-256 指纹。
    pub tls_fingerprint: Option<String>,
    /// Controller 本地 JSON Lines 审计文件；为空时不持久化。
    pub audit_log: Option<PathBuf>,
    /// Relay 用于确定可信事件来源和审批权限的控制端类型。
    pub controller_kind: ControllerKind,
    /// Human 与 AI Controller 共同使用的稳定 Owner 标识。
    pub owner_id: ControllerOwnerId,
    /// Human 配对时请求的会话权限；AI 首次配对只能使用默认审批模式。
    pub permission_mode: PermissionMode,
    /// 与控制端类型对应的 Relay 认证令牌。
    pub authentication_token: String,
    /// 传输中断后的重连间隔。
    pub reconnect_delay: TokioDuration,
}

/// 一次远程执行的响应和可选事件。
#[derive(Clone, Debug)]
pub struct OperationResult {
    /// Relay/Agent 返回的结构化响应。
    pub response: RemoteResponse,
}

/// 已经发往 Agent、可等待或由同一 Controller 取消的远程操作。
pub struct PendingOperation {
    /// 原始远程请求标识。
    pub request_id: RequestId,
    /// Agent 最终响应通道。
    receiver: oneshot::Receiver<Result<RemoteResponse, ApplicationError>>,
}

impl PendingOperation {
    /// 等待远程操作完成。
    pub async fn wait(self) -> Result<OperationResult, ApplicationError> {
        let response = timeout(TokioDuration::from_mins(3), self.receiver)
            .await
            .map_err(|_| ApplicationError::Timeout)?
            .map_err(|_| ApplicationError::Transport("远程响应通道已关闭".to_owned()))??;
        Ok(OperationResult { response })
    }
}

struct ClientInner {
    config: RelayClientConfig,
    tls_config: Arc<ClientConfig>,
    controller_instance_id: remoteops_domain::ControllerInstanceId,
    transport: Mutex<Option<ActiveTransport>>,
    transport_generation: AtomicU64,
    connected: watch::Sender<bool>,
    core: Mutex<ControllerCore>,
    pending: Mutex<BTreeMap<RequestId, PendingRemoteResponse>>,
    pair_pending: Mutex<BTreeMap<RequestId, oneshot::Sender<PairResult>>>,
    release_pending: Mutex<BTreeMap<RequestId, oneshot::Sender<ReleaseSessionResult>>>,
    approval_pending: Mutex<BTreeMap<RequestId, oneshot::Sender<ApprovalResult>>>,
    known_pairings: Mutex<BTreeMap<PairingCode, Option<SessionId>>>,
    automatic_pair_pending: Mutex<BTreeMap<RequestId, PairingCode>>,
    events: broadcast::Sender<RemoteEvent>,
    event_history: Mutex<VecDeque<RemoteEvent>>,
    audit: Option<Arc<JsonlAuditStore>>,
}

struct PendingRemoteResponse {
    /// 请求发送时绑定的不可变目标会话。
    session_id: SessionId,
    /// 仅向原始等待者交付匹配的响应。
    sender: oneshot::Sender<Result<RemoteResponse, ApplicationError>>,
}

enum PendingResponseMatch {
    Matched(PendingRemoteResponse),
    SessionMismatch(SessionId),
    NotFound,
}

#[derive(Clone)]
struct ActiveTransport {
    generation: u64,
    sender: mpsc::UnboundedSender<WireMessage>,
}

/// Controller Core 的真实 Relay 客户端。
#[derive(Clone)]
pub struct RelayClient {
    inner: Arc<ClientInner>,
}

impl RelayClient {
    /// 连接 Relay 并启动可自动恢复的传输任务。
    pub async fn connect(
        config: RelayClientConfig,
        controller_instance_id: remoteops_domain::ControllerInstanceId,
    ) -> Result<Self, ApplicationError> {
        if config.authentication_token.len() < 32 {
            return Err(ApplicationError::Transport(
                "Controller 认证令牌必须至少包含 32 个字节".to_owned(),
            ));
        }
        let tls_config = if let Some(certificate_path) = config.ca_certificate.as_deref() {
            load_client_config(certificate_path)
        } else if let Some(fingerprint) = config.tls_fingerprint.as_deref() {
            load_pinned_client_config(&config.relay_address, &config.server_name, fingerprint).await
        } else {
            load_native_client_config()
        }
        .map_err(|error| ApplicationError::Transport(error.to_string()))?;
        let (events, _) = broadcast::channel(512);
        let (connected, _) = watch::channel(false);
        let audit = config
            .audit_log
            .clone()
            .map(JsonlAuditStore::new)
            .map(Arc::new);
        let inner = Arc::new(ClientInner {
            config,
            tls_config,
            controller_instance_id,
            transport: Mutex::new(None),
            transport_generation: AtomicU64::new(0),
            connected,
            core: Mutex::new(ControllerCore::default()),
            pending: Mutex::new(BTreeMap::new()),
            pair_pending: Mutex::new(BTreeMap::new()),
            release_pending: Mutex::new(BTreeMap::new()),
            approval_pending: Mutex::new(BTreeMap::new()),
            known_pairings: Mutex::new(BTreeMap::new()),
            automatic_pair_pending: Mutex::new(BTreeMap::new()),
            events,
            event_history: Mutex::new(VecDeque::with_capacity(2048)),
            audit,
        });

        let (initial_sender, initial_receiver) = oneshot::channel();
        tokio::spawn(connection_supervisor(inner.clone(), initial_sender));
        timeout(TokioDuration::from_secs(20), initial_receiver)
            .await
            .map_err(|_| ApplicationError::Timeout)?
            .map_err(|_| ApplicationError::Transport("Controller 连接任务意外退出".to_owned()))??;

        Ok(Self { inner })
    }

    /// 返回 Controller 当前是否已经连接 Relay。
    #[must_use]
    pub fn is_connected(&self) -> bool {
        *self.inner.connected.borrow()
    }

    /// 等待传输在指定时限内恢复。
    pub async fn wait_until_connected(
        &self,
        timeout_duration: TokioDuration,
    ) -> Result<(), ApplicationError> {
        if self.is_connected() {
            return Ok(());
        }
        let mut receiver = self.inner.connected.subscribe();
        timeout(timeout_duration, async move {
            loop {
                receiver.changed().await.map_err(|_| {
                    ApplicationError::Transport("Controller 连接任务已经退出".to_owned())
                })?;
                if *receiver.borrow() {
                    return Ok(());
                }
            }
        })
        .await
        .map_err(|_| ApplicationError::Timeout)?
    }

    /// 订阅人工、AI 和 Agent 的统一事件流。
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<RemoteEvent> {
        self.inner.events.subscribe()
    }

    /// 使用配对码绑定一个 Agent。
    pub async fn pair(
        &self,
        pairing_code: remoteops_domain::PairingCode,
    ) -> Result<ConnectionDescriptor, ApplicationError> {
        let request_id = RequestId::new();
        let (sender, receiver) = oneshot::channel();
        self.inner
            .pair_pending
            .lock()
            .await
            .insert(request_id, sender);
        if let Err(error) = self
            .send_wire(WireMessage::PairRequest(remoteops_protocol::PairRequest {
                request_id,
                pairing_code: pairing_code.clone(),
                permission_mode: self.inner.config.permission_mode,
            }))
            .await
        {
            self.inner.pair_pending.lock().await.remove(&request_id);
            return Err(error);
        }
        let result = receive_pending(
            &self.inner.pair_pending,
            request_id,
            receiver,
            TokioDuration::from_secs(20),
            "Relay 配对响应通道已关闭",
        )
        .await?;
        if let Some(error) = result.error {
            return Err(ApplicationError::Remote {
                code: "pairing_failed".to_owned(),
                message: error,
            });
        }
        let connection = result.connection.ok_or_else(|| ApplicationError::Remote {
            code: "pairing_failed".to_owned(),
            message: "Relay 未返回连接描述".to_owned(),
        })?;
        self.inner
            .known_pairings
            .lock()
            .await
            .insert(pairing_code, Some(connection.session_id));
        let mut core = self.inner.core.lock().await;
        let connection = core.upsert_connection(connection);
        drop(core);
        self.append_audit(AuditEvent {
            session_id: connection.session_id,
            request_id: Some(request_id),
            owner_id: Some(self.inner.config.owner_id),
            controller_instance_id: Some(self.inner.controller_instance_id),
            source: EventSource::Human,
            action: "pair_connection".to_owned(),
            result: format!(
                "paired agent_instance_id={} hostname={}",
                connection.agent_instance_id, connection.hostname
            ),
            approval: ApprovalState::NotRequired,
            permission_mode: self.inner.config.permission_mode,
            occurred_at: Utc::now(),
        })?;
        Ok(connection)
    }

    /// 向 Relay 申请一项与 `session_id` 和完整操作绑定的审批。
    pub async fn request_approval(
        &self,
        target: &str,
        operation: RemoteOperation,
    ) -> Result<ApprovalResult, ApplicationError> {
        let session_id = self.resolve_target(target).await?;
        let request_id = RequestId::new();
        let (sender, receiver) = oneshot::channel();
        self.inner
            .approval_pending
            .lock()
            .await
            .insert(request_id, sender);
        if let Err(error) = self
            .send_wire(WireMessage::ApprovalRequest(ApprovalRequest {
                request_id,
                session_id,
                operation,
            }))
            .await
        {
            self.inner.approval_pending.lock().await.remove(&request_id);
            return Err(error);
        }
        receive_pending(
            &self.inner.approval_pending,
            request_id,
            receiver,
            TokioDuration::from_secs(20),
            "Relay 审批响应通道已关闭",
        )
        .await
    }

    /// 由已认证的人工 Controller 批准或拒绝一项精确操作。
    pub async fn decide_approval(
        &self,
        session_id: SessionId,
        operation: RemoteOperation,
        approval_id: ApprovalId,
        approved: bool,
    ) -> Result<ApprovalResult, ApplicationError> {
        if self.inner.config.controller_kind != ControllerKind::Human {
            return Err(ApplicationError::PolicyDenied(
                "只有人工 Controller 可以作出审批决定".to_owned(),
            ));
        }
        let request_id = RequestId::new();
        let (sender, receiver) = oneshot::channel();
        self.inner
            .approval_pending
            .lock()
            .await
            .insert(request_id, sender);
        if let Err(error) = self
            .send_wire(WireMessage::ApprovalDecision(ApprovalDecision {
                request_id,
                session_id,
                operation,
                approval_id,
                approved,
            }))
            .await
        {
            self.inner.approval_pending.lock().await.remove(&request_id);
            return Err(error);
        }
        receive_pending(
            &self.inner.approval_pending,
            request_id,
            receiver,
            TokioDuration::from_secs(20),
            "Relay 审批响应通道已关闭",
        )
        .await
    }

    /// 返回当前连接列表。
    pub async fn list_connections(&self) -> Vec<ConnectionDescriptor> {
        self.inner.core.lock().await.list_connections()
    }

    /// 使用 `session_id`、默认连接编号或唯一别名解析目标。
    pub async fn resolve_target(&self, target: &str) -> Result<SessionId, ApplicationError> {
        self.inner.core.lock().await.resolve_target(target)
    }

    /// 读取指定会话的历史事件。
    pub async fn read_events(
        &self,
        session_id: SessionId,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Vec<RemoteEvent> {
        let history = self.inner.event_history.lock().await;
        history
            .iter()
            .filter(|event| {
                event.session_id == session_id
                    && after_sequence.is_none_or(|sequence| event.sequence > sequence)
            })
            .take(limit.clamp(1, 500))
            .cloned()
            .collect()
    }

    /// 修改连接别名。
    pub async fn set_alias(
        &self,
        target: &str,
        alias: impl Into<String>,
    ) -> Result<(), ApplicationError> {
        let mut core = self.inner.core.lock().await;
        let session_id = core.resolve_target(target)?;
        core.set_alias(session_id, alias)
    }

    /// 人工接管指定连接。
    pub async fn human_takeover(&self, target: &str) -> Result<OperationResult, ApplicationError> {
        let mut core = self.inner.core.lock().await;
        let session_id = core.resolve_target(target)?;
        let request = core.prepare_request(
            session_id,
            EventSource::Human,
            RemoteOperation::HumanTakeover,
            None,
        )?;
        drop(core);
        let result = self
            .queue_request(request, None, None)
            .await?
            .wait()
            .await?;
        self.inner.core.lock().await.human_takeover(session_id)?;
        Ok(result)
    }

    /// 释放人工接管，让 AI 恢复按 Relay 策略工作。
    pub async fn release_human_takeover(
        &self,
        target: &str,
    ) -> Result<OperationResult, ApplicationError> {
        let mut core = self.inner.core.lock().await;
        let session_id = core.resolve_target(target)?;
        let request = core.prepare_request(
            session_id,
            EventSource::Human,
            RemoteOperation::ReleaseHumanTakeover,
            None,
        )?;
        drop(core);
        let result = self
            .queue_request(request, None, None)
            .await?
            .wait()
            .await?;
        self.inner
            .core
            .lock()
            .await
            .release_human_takeover(session_id)?;
        Ok(result)
    }

    /// 根据目标字符串和策略执行远程操作。
    pub async fn execute(
        &self,
        target: &str,
        source: EventSource,
        operation: RemoteOperation,
        approval_id: Option<ApprovalId>,
        payload_base64: Option<String>,
        secret: Option<String>,
    ) -> Result<OperationResult, ApplicationError> {
        if secret.is_some() {
            return Err(ApplicationError::PolicyDenied(
                "RemoteOps 协议不允许传递远端秘密".to_owned(),
            ));
        }
        self.start_execute(
            target,
            source,
            operation,
            approval_id,
            payload_base64,
            secret,
        )
        .await?
        .wait()
        .await
    }

    /// 发送远程操作并立即返回可取消的请求句柄。
    pub async fn start_execute(
        &self,
        target: &str,
        source: EventSource,
        operation: RemoteOperation,
        approval_id: Option<ApprovalId>,
        payload_base64: Option<String>,
        secret: Option<String>,
    ) -> Result<PendingOperation, ApplicationError> {
        if secret.is_some() {
            return Err(ApplicationError::PolicyDenied(
                "RemoteOps 协议不允许传递远端秘密".to_owned(),
            ));
        }
        let mut core = self.inner.core.lock().await;
        let session_id = core.resolve_target(target)?;
        let request = match core.prepare_request(session_id, source, operation.clone(), approval_id)
        {
            Ok(request) => request,
            Err(error) => {
                let (approval, result) = match &error {
                    ApplicationError::ApprovalRequired { reason, .. } => {
                        (ApprovalState::Pending, reason.clone())
                    }
                    ApplicationError::PolicyDenied(reason) => {
                        (ApprovalState::Rejected, reason.clone())
                    }
                    _ => (ApprovalState::Rejected, error.to_string()),
                };
                drop(core);
                self.append_audit(AuditEvent {
                    session_id,
                    request_id: None,
                    owner_id: Some(self.inner.config.owner_id),
                    controller_instance_id: Some(self.inner.controller_instance_id),
                    source,
                    action: audit_operation(&operation),
                    result,
                    approval,
                    permission_mode: self.inner.config.permission_mode,
                    occurred_at: Utc::now(),
                })?;
                return Err(error);
            }
        };
        drop(core);
        self.queue_request(request, payload_base64, secret).await
    }

    /// 取消一个尚未完成的远程请求。
    pub async fn cancel(
        &self,
        target: &str,
        request_id: RequestId,
    ) -> Result<OperationResult, ApplicationError> {
        self.execute(
            target,
            EventSource::Human,
            RemoteOperation::CancelRequest { request_id },
            None,
            None,
            None,
        )
        .await
    }

    /// 立即中止指定会话中的全部在途任务并关闭交互资源。
    pub async fn emergency_stop(&self, target: &str) -> Result<OperationResult, ApplicationError> {
        self.execute(
            target,
            EventSource::Human,
            RemoteOperation::EmergencyStop,
            None,
            None,
            None,
        )
        .await
    }

    /// 关闭远程会话并从当前控制端列表移除。
    pub async fn close_connection(
        &self,
        target: &str,
    ) -> Result<OperationResult, ApplicationError> {
        let session_id = {
            let core = self.inner.core.lock().await;
            core.resolve_target(target)?
        };
        let result = self
            .execute(
                target,
                EventSource::Human,
                RemoteOperation::CloseConnection,
                None,
                None,
                None,
            )
            .await?;
        let request_id = RequestId::new();
        let (sender, receiver) = oneshot::channel();
        self.inner
            .release_pending
            .lock()
            .await
            .insert(request_id, sender);
        if let Err(error) = self
            .send_wire(WireMessage::ReleaseSessionRequest(ReleaseSessionRequest {
                request_id,
                session_id,
            }))
            .await
        {
            self.inner.release_pending.lock().await.remove(&request_id);
            return Err(error);
        }
        let release = receive_pending(
            &self.inner.release_pending,
            request_id,
            receiver,
            TokioDuration::from_secs(20),
            "Relay 会话释放响应通道已关闭",
        )
        .await?;
        if release.session_id != session_id {
            return Err(ApplicationError::Transport(
                "Relay 返回了不匹配的会话释放结果".to_owned(),
            ));
        }
        if !release.released {
            return Err(ApplicationError::Remote {
                code: "session_release_failed".to_owned(),
                message: release
                    .error
                    .unwrap_or_else(|| "Relay 未确认会话绑定已经释放".to_owned()),
            });
        }
        forget_session_pairing(&self.inner, session_id).await;
        self.inner.core.lock().await.remove_connection(session_id)?;
        Ok(result)
    }

    /// 将指定会话的脱敏审计事件导出为 JSON。
    pub fn export_audit(
        &self,
        session_id: SessionId,
        destination: impl AsRef<std::path::Path>,
    ) -> Result<usize, ApplicationError> {
        let store =
            self.inner.audit.as_ref().ok_or_else(|| {
                ApplicationError::Audit("当前 Controller 未启用审计文件".to_owned())
            })?;
        store
            .export_session(session_id, destination)
            .map_err(|error| ApplicationError::Audit(error.to_string()))
    }

    async fn queue_request(
        &self,
        mut request: RemoteRequest,
        payload_base64: Option<String>,
        _secret: Option<String>,
    ) -> Result<PendingOperation, ApplicationError> {
        request.payload_base64 = payload_base64;
        let request_id = request.request_id;
        self.append_audit(AuditEvent {
            session_id: request.session_id,
            request_id: Some(request.request_id),
            owner_id: Some(self.inner.config.owner_id),
            controller_instance_id: Some(self.inner.controller_instance_id),
            source: request.source,
            action: audit_operation(&request.operation),
            result: "requested".to_owned(),
            approval: outgoing_approval_state(&request),
            permission_mode: self.inner.config.permission_mode,
            occurred_at: Utc::now(),
        })?;
        let (sender, receiver) = oneshot::channel();
        self.inner.pending.lock().await.insert(
            request_id,
            PendingRemoteResponse {
                session_id: request.session_id,
                sender,
            },
        );
        if let Err(error) = self.send_wire(WireMessage::RemoteRequest(request)).await {
            self.inner.pending.lock().await.remove(&request_id);
            return Err(error);
        }
        Ok(PendingOperation {
            request_id,
            receiver,
        })
    }

    async fn send_wire(&self, message: WireMessage) -> Result<(), ApplicationError> {
        let sender = self
            .inner
            .transport
            .lock()
            .await
            .as_ref()
            .map(|transport| transport.sender.clone())
            .ok_or_else(|| {
                ApplicationError::Transport("Controller 正在重新连接 Relay".to_owned())
            })?;
        sender
            .send(message)
            .map_err(|_| ApplicationError::Transport("Controller 传输连接已经关闭".to_owned()))
    }

    #[allow(clippy::needless_pass_by_value)]
    fn append_audit(&self, event: AuditEvent) -> Result<(), ApplicationError> {
        if let Some(store) = &self.inner.audit {
            store
                .append(&event)
                .map_err(|error| ApplicationError::Audit(error.to_string()))?;
        }
        Ok(())
    }
}

async fn receive_pending<T>(
    pending: &Mutex<BTreeMap<RequestId, oneshot::Sender<T>>>,
    request_id: RequestId,
    receiver: oneshot::Receiver<T>,
    timeout_duration: TokioDuration,
    closed_message: &str,
) -> Result<T, ApplicationError> {
    match timeout(timeout_duration, receiver).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(_)) => {
            pending.lock().await.remove(&request_id);
            Err(ApplicationError::Transport(closed_message.to_owned()))
        }
        Err(_) => {
            pending.lock().await.remove(&request_id);
            Err(ApplicationError::Timeout)
        }
    }
}

async fn connection_supervisor(
    inner: Arc<ClientInner>,
    initial_sender: oneshot::Sender<Result<(), ApplicationError>>,
) {
    let mut initial_sender = Some(initial_sender);
    loop {
        let connection = open_controller_connection(&inner).await;
        let stream = match connection {
            Ok(stream) => stream,
            Err(error) => {
                if let Some(sender) = initial_sender.take() {
                    let _ = sender.send(Err(error));
                    return;
                }
                sleep(
                    inner
                        .config
                        .reconnect_delay
                        .max(TokioDuration::from_millis(250)),
                )
                .await;
                continue;
            }
        };

        let result = run_transport(inner.clone(), stream, initial_sender.take()).await;
        set_transport_disconnected(&inner, result.err()).await;
        sleep(
            inner
                .config
                .reconnect_delay
                .max(TokioDuration::from_millis(250)),
        )
        .await;
    }
}

async fn open_controller_connection(
    inner: &ClientInner,
) -> Result<TlsStream<TcpStream>, ApplicationError> {
    let mut stream = connect_tls(
        &inner.config.relay_address,
        &inner.config.server_name,
        inner.tls_config.clone(),
    )
    .await
    .map_err(|error| ApplicationError::Transport(error.to_string()))?;
    write_frame(
        &mut stream,
        &WireMessage::Hello(ClientHello::Controller(ControllerHello {
            protocol_version: PROTOCOL_VERSION,
            controller_instance_id: inner.controller_instance_id,
            owner_id: inner.config.owner_id,
            kind: inner.config.controller_kind,
            auth_token: inner.config.authentication_token.clone(),
        })),
    )
    .await
    .map_err(|error| ApplicationError::Transport(error.to_string()))?;
    match read_frame::<WireMessage, _>(&mut stream)
        .await
        .map_err(|error| ApplicationError::Transport(error.to_string()))?
    {
        WireMessage::ControllerWelcome { protocol_version }
            if protocol_version == PROTOCOL_VERSION =>
        {
            Ok(stream)
        }
        WireMessage::Error { code, message, .. } => Err(ApplicationError::Remote { code, message }),
        other => Err(ApplicationError::Transport(format!(
            "Relay 返回意外欢迎消息：{other:?}"
        ))),
    }
}

async fn run_transport(
    inner: Arc<ClientInner>,
    stream: TlsStream<TcpStream>,
    initial_sender: Option<oneshot::Sender<Result<(), ApplicationError>>>,
) -> Result<(), ApplicationError> {
    let generation = inner
        .transport_generation
        .fetch_add(1, Ordering::AcqRel)
        .saturating_add(1);
    let (sender, receiver) = mpsc::unbounded_channel();
    {
        let mut transport = inner.transport.lock().await;
        *transport = Some(ActiveTransport {
            generation,
            sender: sender.clone(),
        });
    }
    set_connected(&inner.connected, true);
    if let Some(sender) = initial_sender {
        let _ = sender.send(Ok(()));
    }

    let pairing_recovery_inner = inner.clone();
    let pairing_recovery_sender = sender.clone();
    let pairing_recovery_task = tokio::spawn(async move {
        let retry_delay = pairing_recovery_inner
            .config
            .reconnect_delay
            .max(TokioDuration::from_secs(1));
        let mut retry = interval(retry_delay);
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            retry.tick().await;
            if !send_recoverable_pairings(&pairing_recovery_inner, &pairing_recovery_sender).await {
                break;
            }
        }
    });

    let heartbeat_sender = sender.clone();
    let heartbeat_task = tokio::spawn(async move {
        let mut heartbeat = interval(TokioDuration::from_secs(15));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            heartbeat.tick().await;
            if heartbeat_sender
                .send(WireMessage::Heartbeat {
                    sent_at: Utc::now(),
                })
                .is_err()
            {
                break;
            }
        }
    });

    let (reader, writer) = tokio::io::split(stream);
    let result = run_transport_io(inner, reader, writer, receiver).await;
    heartbeat_task.abort();
    pairing_recovery_task.abort();
    result
}

async fn send_recoverable_pairings(
    inner: &ClientInner,
    sender: &mpsc::UnboundedSender<WireMessage>,
) -> bool {
    let known_pairings = inner.known_pairings.lock().await.clone();
    if known_pairings.is_empty() {
        return true;
    }
    let pending_codes = inner
        .automatic_pair_pending
        .lock()
        .await
        .values()
        .cloned()
        .collect::<BTreeSet<_>>();
    let pairing_codes = {
        let core = inner.core.lock().await;
        known_pairings
            .into_iter()
            .filter_map(|(pairing_code, session_id)| {
                if pending_codes.contains(&pairing_code) {
                    return None;
                }
                let online = session_id
                    .and_then(|session_id| core.get_connection(session_id))
                    .is_some_and(|connection| connection.state == ConnectionState::Online);
                (!online).then_some(pairing_code)
            })
            .collect::<Vec<_>>()
    };
    for pairing_code in pairing_codes {
        let request_id = RequestId::new();
        inner
            .automatic_pair_pending
            .lock()
            .await
            .insert(request_id, pairing_code.clone());
        if sender
            .send(WireMessage::PairRequest(remoteops_protocol::PairRequest {
                request_id,
                pairing_code,
                permission_mode: inner.config.permission_mode,
            }))
            .is_err()
        {
            inner
                .automatic_pair_pending
                .lock()
                .await
                .remove(&request_id);
            return false;
        }
    }
    true
}

async fn forget_session_pairing(inner: &ClientInner, session_id: SessionId) {
    let removed_codes = {
        let mut known_pairings = inner.known_pairings.lock().await;
        let removed_codes = known_pairings
            .iter()
            .filter_map(|(pairing_code, paired_session_id)| {
                (*paired_session_id == Some(session_id)).then_some(pairing_code.clone())
            })
            .collect::<BTreeSet<_>>();
        known_pairings.retain(|pairing_code, _| !removed_codes.contains(pairing_code));
        removed_codes
    };
    if !removed_codes.is_empty() {
        inner
            .automatic_pair_pending
            .lock()
            .await
            .retain(|_, pairing_code| !removed_codes.contains(pairing_code));
    }
}

async fn run_transport_io<R, W>(
    inner: Arc<ClientInner>,
    reader: R,
    writer: W,
    receiver: mpsc::UnboundedReceiver<WireMessage>,
) -> Result<(), ApplicationError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader_task = tokio::spawn(read_transport_messages(inner, reader));
    let mut writer_task = tokio::spawn(write_transport_messages(writer, receiver));

    tokio::select! {
        result = &mut reader_task => {
            writer_task.abort();
            flatten_transport_task("读取", result)
        }
        result = &mut writer_task => {
            reader_task.abort();
            flatten_transport_task("写入", result)
        }
    }
}

async fn read_transport_messages<R>(
    inner: Arc<ClientInner>,
    mut reader: R,
) -> Result<(), ApplicationError>
where
    R: AsyncRead + Unpin,
{
    loop {
        let message = read_frame::<WireMessage, _>(&mut reader)
            .await
            .map_err(|error| ApplicationError::Transport(error.to_string()))?;
        handle_wire_message(&inner, message).await;
    }
}

async fn write_transport_messages<W>(
    mut writer: W,
    mut receiver: mpsc::UnboundedReceiver<WireMessage>,
) -> Result<(), ApplicationError>
where
    W: AsyncWrite + Unpin,
{
    while let Some(message) = receiver.recv().await {
        write_frame(&mut writer, &message)
            .await
            .map_err(|error| ApplicationError::Transport(error.to_string()))?;
    }
    Err(ApplicationError::Transport(
        "Controller 发送通道已经关闭".to_owned(),
    ))
}

fn flatten_transport_task(
    task_name: &str,
    result: Result<Result<(), ApplicationError>, tokio::task::JoinError>,
) -> Result<(), ApplicationError> {
    result.map_err(|error| {
        ApplicationError::Transport(format!("Controller {task_name}任务意外退出：{error}"))
    })?
}

async fn set_transport_disconnected(inner: &ClientInner, cause: Option<ApplicationError>) {
    let active_generation = inner
        .transport
        .lock()
        .await
        .as_ref()
        .map(|transport| transport.generation);
    if active_generation.is_some() {
        *inner.transport.lock().await = None;
    }
    set_connected(&inner.connected, false);
    inner.core.lock().await.mark_all_reconnecting();
    let message = cause.map_or_else(|| "Relay 连接已关闭".to_owned(), |error| error.to_string());
    fail_pending_requests(inner, &message).await;
}

async fn fail_pending_requests(inner: &ClientInner, message: &str) {
    let pending = std::mem::take(&mut *inner.pending.lock().await);
    for (_, pending) in pending {
        let _ = pending
            .sender
            .send(Err(ApplicationError::Transport(message.to_owned())));
    }
    let pair_pending = std::mem::take(&mut *inner.pair_pending.lock().await);
    for (request_id, sender) in pair_pending {
        let _ = sender.send(PairResult {
            request_id,
            connection: None,
            error: Some(message.to_owned()),
        });
    }
    let release_pending = std::mem::take(&mut *inner.release_pending.lock().await);
    drop(release_pending);
    let approval_pending = std::mem::take(&mut *inner.approval_pending.lock().await);
    drop(approval_pending);
    inner.automatic_pair_pending.lock().await.clear();
}

/// 更新 Relay 连接状态，即使当前没有订阅者也要保留最新值。
fn set_connected(sender: &watch::Sender<bool>, connected: bool) {
    sender.send_replace(connected);
}

#[allow(clippy::too_many_lines)]
async fn handle_wire_message(inner: &ClientInner, message: WireMessage) {
    match message {
        WireMessage::PairResult(result) => {
            let automatic_pairing = inner
                .automatic_pair_pending
                .lock()
                .await
                .remove(&result.request_id);
            if let Some(connection) = result.connection.clone() {
                inner
                    .core
                    .lock()
                    .await
                    .upsert_connection(connection.clone());
                if let Some(pairing_code) = automatic_pairing {
                    inner
                        .known_pairings
                        .lock()
                        .await
                        .insert(pairing_code, Some(connection.session_id));
                }
            }
            if let Some(sender) = inner.pair_pending.lock().await.remove(&result.request_id) {
                let _ = sender.send(result);
            }
        }
        WireMessage::ReleaseSessionResult(result) => {
            if let Some(sender) = inner
                .release_pending
                .lock()
                .await
                .remove(&result.request_id)
            {
                let _ = sender.send(result);
            }
        }
        WireMessage::ApprovalResult(result) => {
            if let Some(sender) = inner
                .approval_pending
                .lock()
                .await
                .remove(&result.request_id)
            {
                let _ = sender.send(result);
            }
        }
        WireMessage::RemoteResponse(response) => {
            let pending = {
                let mut pending = inner.pending.lock().await;
                take_matching_pending(&mut pending, response.request_id, response.session_id)
            };
            let sender = match pending {
                PendingResponseMatch::Matched(pending) => pending.sender,
                PendingResponseMatch::SessionMismatch(expected_session_id) => {
                    warn!(
                        request_id = %response.request_id,
                        expected_session_id = %expected_session_id,
                        actual_session_id = %response.session_id,
                        "拒绝 session_id 与原请求不匹配的远程响应"
                    );
                    if let Some(store) = &inner.audit {
                        let _ = store.append(&AuditEvent {
                            session_id: expected_session_id,
                            request_id: Some(response.request_id),
                            owner_id: Some(inner.config.owner_id),
                            controller_instance_id: Some(inner.controller_instance_id),
                            source: EventSource::System,
                            action: "remote_response_rejected".to_owned(),
                            result: format!(
                                "session_mismatch expected={expected_session_id} actual={}",
                                response.session_id
                            ),
                            approval: ApprovalState::NotRequired,
                            permission_mode: inner.config.permission_mode,
                            occurred_at: Utc::now(),
                        });
                    }
                    return;
                }
                PendingResponseMatch::NotFound => return,
            };
            if let Some(store) = &inner.audit {
                let _ = store.append(&AuditEvent {
                    session_id: response.session_id,
                    request_id: Some(response.request_id),
                    owner_id: Some(inner.config.owner_id),
                    controller_instance_id: Some(inner.controller_instance_id),
                    source: EventSource::System,
                    action: "remote_response".to_owned(),
                    result: audit_response(&response),
                    approval: ApprovalState::NotRequired,
                    permission_mode: inner.config.permission_mode,
                    occurred_at: Utc::now(),
                });
            }
            let result = if let Some(code) = response.error_code.clone() {
                Err(ApplicationError::Remote {
                    code,
                    message: response.summary.clone(),
                })
            } else {
                Ok(response)
            };
            let _ = sender.send(result);
        }
        WireMessage::RemoteEvent(event) => {
            if let Some(store) = &inner.audit {
                let _ = store.append(&AuditEvent {
                    session_id: event.session_id,
                    request_id: event.request_id,
                    owner_id: Some(inner.config.owner_id),
                    controller_instance_id: Some(inner.controller_instance_id),
                    source: event.source,
                    action: "remote_event".to_owned(),
                    result: audit_event_payload(&event.payload),
                    approval: event.approval,
                    permission_mode: inner.config.permission_mode,
                    occurred_at: event.occurred_at,
                });
            }
            let mut history = inner.event_history.lock().await;
            if history.len() >= 2048 {
                history.pop_front();
            }
            history.push_back(event.clone());
            drop(history);
            let _ = inner.events.send(event);
        }
        WireMessage::ConnectionUpdated(connection) => {
            inner.core.lock().await.upsert_connection(connection);
        }
        WireMessage::ConnectionRemoved { session_id, reason } => {
            forget_session_pairing(inner, session_id).await;
            let _ = inner.core.lock().await.remove_connection(session_id);
            let pending = {
                let mut pending = inner.pending.lock().await;
                let request_ids = pending
                    .iter()
                    .filter_map(|(request_id, entry)| {
                        (entry.session_id == session_id).then_some(*request_id)
                    })
                    .collect::<Vec<_>>();
                request_ids
                    .into_iter()
                    .filter_map(|request_id| pending.remove(&request_id))
                    .collect::<Vec<_>>()
            };
            for pending in pending {
                let _ = pending.sender.send(Err(ApplicationError::Remote {
                    code: "connection_removed".to_owned(),
                    message: reason.clone(),
                }));
            }
        }
        WireMessage::Error {
            code,
            message,
            request_id,
        } => {
            if let Some(request_id) = request_id
                && let Some(pending) = inner.pending.lock().await.remove(&request_id)
            {
                let _ = pending
                    .sender
                    .send(Err(ApplicationError::Remote { code, message }));
            }
        }
        _ => {}
    }
}

fn outgoing_approval_state(request: &RemoteRequest) -> ApprovalState {
    if request.approval_id.is_some() {
        ApprovalState::Pending
    } else {
        ApprovalState::NotRequired
    }
}

fn take_matching_pending(
    pending: &mut BTreeMap<RequestId, PendingRemoteResponse>,
    request_id: RequestId,
    response_session_id: SessionId,
) -> PendingResponseMatch {
    match pending.get(&request_id) {
        Some(entry) if entry.session_id != response_session_id => {
            PendingResponseMatch::SessionMismatch(entry.session_id)
        }
        Some(_) => pending.remove(&request_id).map_or(
            PendingResponseMatch::NotFound,
            PendingResponseMatch::Matched,
        ),
        None => PendingResponseMatch::NotFound,
    }
}

fn audit_operation(operation: &RemoteOperation) -> String {
    match operation {
        RemoteOperation::RunCommand {
            shell,
            command,
            readonly,
        } => format!(
            "run_command shell={shell:?} readonly={readonly} command_bytes={} command_sha256={}",
            command.len(),
            sha256_bytes(command.as_bytes())
        ),
        RemoteOperation::RunShellCommand {
            shell_id,
            shell,
            command,
            readonly,
        } => format!(
            "run_shell_command shell_id={shell_id} shell={shell:?} readonly={readonly} command_bytes={} command_sha256={}",
            command.len(),
            sha256_bytes(command.as_bytes())
        ),
        RemoteOperation::RunSsh {
            host,
            port,
            username,
            command,
            readonly,
            ..
        } => format!(
            "run_ssh target={username}@{host}:{port} readonly={readonly} command_bytes={} command_sha256={}",
            command.len(),
            sha256_bytes(command.as_bytes())
        ),
        RemoteOperation::ProvisionSshCredential {
            host,
            port,
            username,
            credential_ref,
        } => format!(
            "provision_ssh_credential target={username}@{host}:{port} credential_ref={credential_ref}"
        ),
        RemoteOperation::RunSerialQuery {
            serial_session_id,
            command,
            profile,
            readonly,
            overall_timeout_millis,
            idle_timeout_millis,
            max_bytes,
            max_pages,
            ..
        } => format!(
            "run_serial_query serial_session_id={serial_session_id} profile={profile:?} readonly={readonly} command_bytes={} command_sha256={} overall_timeout_millis={overall_timeout_millis} idle_timeout_millis={idle_timeout_millis} max_bytes={max_bytes} max_pages={max_pages}",
            command.len(),
            sha256_bytes(command.as_bytes())
        ),
        RemoteOperation::GetFileMetadata {
            remote_path,
            include_sha256,
        } => {
            format!(
                "get_file_metadata path_sha256={} include_sha256={include_sha256}",
                sha256_bytes(remote_path.as_bytes())
            )
        }
        RemoteOperation::MoveFile {
            source_path,
            destination_path,
            overwrite,
        } => format!(
            "move_file source_sha256={} destination_sha256={} overwrite={overwrite}",
            sha256_bytes(source_path.as_bytes()),
            sha256_bytes(destination_path.as_bytes())
        ),
        RemoteOperation::DeleteFile { remote_path } => format!(
            "delete_file path_sha256={}",
            sha256_bytes(remote_path.as_bytes())
        ),
        RemoteOperation::TcpExchange {
            host,
            port,
            byte_count,
            max_response_bytes,
            timeout_millis,
            ..
        } => format!(
            "tcp_exchange host={host} port={port} bytes={byte_count} max_response_bytes={max_response_bytes} timeout_millis={timeout_millis}"
        ),
        RemoteOperation::TerminateProcess { process_id } => {
            format!("terminate_process process_id={process_id}")
        }
        RemoteOperation::ControlService {
            service_name,
            action,
        } => format!(
            "control_service name_sha256={} action={action:?}",
            sha256_bytes(service_name.as_bytes())
        ),
        _ => serde_json::to_string(operation).unwrap_or_else(|_| format!("{operation:?}")),
    }
}

fn audit_response(response: &RemoteResponse) -> String {
    format!(
        "exit_code={:?} error_code={:?} summary_bytes={} summary_sha256={} payload_bytes={} sha256={:?}",
        response.exit_code,
        response.error_code,
        response.summary.len(),
        sha256_bytes(response.summary.as_bytes()),
        response
            .payload_base64
            .as_ref()
            .map_or(0, std::string::String::len),
        response.sha256
    )
}

fn audit_event_payload(payload: &EventPayload) -> String {
    match payload {
        EventPayload::OperationRequested { operation } => {
            format!("operation_requested {}", audit_operation(operation))
        }
        EventPayload::OutputChunk { stderr, text } => format!(
            "output_chunk stderr={stderr} bytes={} sha256={}",
            text.len(),
            sha256_bytes(text.as_bytes())
        ),
        EventPayload::OperationCompleted { exit_code, summary } => format!(
            "operation_completed exit_code={exit_code:?} summary_bytes={} summary_sha256={}",
            summary.len(),
            sha256_bytes(summary.as_bytes())
        ),
        EventPayload::OperationFailed { code, message } => format!(
            "operation_failed code={code} message_bytes={} message_sha256={}",
            message.len(),
            sha256_bytes(message.as_bytes())
        ),
        other => {
            serde_json::to_string(other).unwrap_or_else(|_| "event_serialization_failed".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use remoteops_domain::{
        AgentInstanceId, CapabilitySet, ConnectionState, ControllerInstanceId, ControllerOwnerId,
        SessionRole, ShellKind,
    };
    use tokio::io::{AsyncWriteExt, duplex};

    use super::*;

    fn test_connection(session_id: SessionId, state: ConnectionState) -> ConnectionDescriptor {
        ConnectionDescriptor {
            session_id,
            agent_instance_id: AgentInstanceId::new(),
            display_index: 0,
            alias: None,
            hostname: "LAB-WIN-A".to_owned(),
            operating_system: "Windows 11".to_owned(),
            capabilities: CapabilitySet::default(),
            environment: remoteops_domain::EnvironmentProfile::empty(),
            state,
            role: SessionRole::HumanControl,
            permission_mode: PermissionMode::ApprovalRequired,
            updated_at: Utc::now(),
        }
    }

    fn core_with_connection() -> (ControllerCore, SessionId) {
        let mut core = ControllerCore::default();
        let session_id = SessionId::new();
        core.upsert_connection(test_connection(session_id, ConnectionState::Online));
        (core, session_id)
    }

    fn transport_test_inner() -> Arc<ClientInner> {
        let (connected, _) = watch::channel(false);
        let (events, _) = broadcast::channel(16);
        Arc::new(ClientInner {
            config: RelayClientConfig {
                relay_address: "127.0.0.1:7443".to_owned(),
                server_name: "localhost".to_owned(),
                ca_certificate: None,
                tls_fingerprint: None,
                audit_log: None,
                controller_kind: ControllerKind::Ai,
                owner_id: ControllerOwnerId::new(),
                permission_mode: PermissionMode::ApprovalRequired,
                authentication_token: "test-controller-token-with-32-bytes".to_owned(),
                reconnect_delay: TokioDuration::from_secs(1),
            },
            tls_config: load_native_client_config().expect("应加载系统可信根"),
            controller_instance_id: ControllerInstanceId::new(),
            transport: Mutex::new(None),
            transport_generation: AtomicU64::new(0),
            connected,
            core: Mutex::new(ControllerCore::default()),
            pending: Mutex::new(BTreeMap::new()),
            pair_pending: Mutex::new(BTreeMap::new()),
            release_pending: Mutex::new(BTreeMap::new()),
            approval_pending: Mutex::new(BTreeMap::new()),
            known_pairings: Mutex::new(BTreeMap::new()),
            automatic_pair_pending: Mutex::new(BTreeMap::new()),
            events,
            event_history: Mutex::new(VecDeque::new()),
            audit: None,
        })
    }

    #[test]
    fn prepares_ai_readonly_request() {
        let (mut core, session_id) = core_with_connection();
        let request = core
            .prepare_request(
                session_id,
                EventSource::Ai,
                RemoteOperation::RunCommand {
                    shell: ShellKind::WindowsPowerShell,
                    command: "Get-NetTCPConnection".to_owned(),
                    readonly: true,
                },
                None,
            )
            .expect("只读请求应允许");

        assert_eq!(request.session_id, session_id);
        assert_eq!(request.source, EventSource::Ai);
    }

    #[test]
    fn high_risk_request_is_forwarded_to_central_relay_approval() {
        let (mut core, session_id) = core_with_connection();
        let operation = RemoteOperation::RunCommand {
            shell: ShellKind::WindowsPowerShell,
            command: "Restart-Computer".to_owned(),
            readonly: false,
        };
        let approval_id = ApprovalId::new();
        let request = core
            .prepare_request(session_id, EventSource::Human, operation, Some(approval_id))
            .expect("本地核心应把审批交给 Relay 验证");

        assert_eq!(request.approval_id, Some(approval_id));
        assert_eq!(outgoing_approval_state(&request), ApprovalState::Pending);
    }

    #[test]
    fn mismatched_response_session_cannot_complete_pending_request() {
        let request_id = RequestId::new();
        let expected_session_id = SessionId::new();
        let wrong_session_id = SessionId::new();
        let (sender, _receiver) = oneshot::channel();
        let mut pending = BTreeMap::from([(
            request_id,
            PendingRemoteResponse {
                session_id: expected_session_id,
                sender,
            },
        )]);

        assert!(matches!(
            take_matching_pending(&mut pending, request_id, wrong_session_id),
            PendingResponseMatch::SessionMismatch(session_id)
                if session_id == expected_session_id
        ));
        assert!(pending.contains_key(&request_id));
        assert!(matches!(
            take_matching_pending(&mut pending, request_id, expected_session_id),
            PendingResponseMatch::Matched(_)
        ));
        assert!(!pending.contains_key(&request_id));
    }

    #[test]
    fn rejects_false_readonly_request_before_transport() {
        let (mut core, session_id) = core_with_connection();
        let operation = RemoteOperation::RunCommand {
            shell: ShellKind::WindowsPowerShell,
            command: "Set-ItemProperty HKCU:\\Software\\RemoteOps Enabled 1".to_owned(),
            readonly: true,
        };

        assert!(matches!(
            core.prepare_request(session_id, EventSource::Ai, operation, None),
            Err(ApplicationError::PolicyDenied(_))
        ));
    }

    #[test]
    fn connected_state_is_retained_without_subscribers() {
        let (sender, receiver) = watch::channel(false);
        drop(receiver);

        set_connected(&sender, true);
        assert!(*sender.borrow());

        set_connected(&sender, false);
        assert!(!*sender.borrow());
    }

    #[tokio::test]
    async fn recoverable_pairing_retries_until_target_is_online() {
        let inner = transport_test_inner();
        let pairing_code = PairingCode::parse("123-456-789").expect("测试控制码应有效");
        inner
            .known_pairings
            .lock()
            .await
            .insert(pairing_code.clone(), None);
        let (sender, mut receiver) = mpsc::unbounded_channel();

        assert!(send_recoverable_pairings(&inner, &sender).await);
        let first_request = match receiver.recv().await.expect("应发送首次恢复配对") {
            WireMessage::PairRequest(request) => request,
            other => panic!("应发送配对请求，实际为 {other:?}"),
        };
        assert_eq!(first_request.pairing_code, pairing_code);

        assert!(send_recoverable_pairings(&inner, &sender).await);
        assert!(receiver.try_recv().is_err(), "在途配对不应重复发送");

        handle_wire_message(
            &inner,
            WireMessage::PairResult(PairResult {
                request_id: first_request.request_id,
                connection: None,
                error: Some("Agent 当前离线".to_owned()),
            }),
        )
        .await;
        assert!(send_recoverable_pairings(&inner, &sender).await);
        let second_request = match receiver.recv().await.expect("离线后应继续恢复配对") {
            WireMessage::PairRequest(request) => request,
            other => panic!("应发送配对请求，实际为 {other:?}"),
        };

        let session_id = SessionId::new();
        handle_wire_message(
            &inner,
            WireMessage::PairResult(PairResult {
                request_id: second_request.request_id,
                connection: Some(test_connection(session_id, ConnectionState::Online)),
                error: None,
            }),
        )
        .await;
        assert_eq!(
            inner
                .known_pairings
                .lock()
                .await
                .get(&pairing_code)
                .copied(),
            Some(Some(session_id))
        );
        assert!(send_recoverable_pairings(&inner, &sender).await);
        assert!(receiver.try_recv().is_err(), "在线目标不应继续重复配对");
    }

    #[tokio::test]
    async fn explicit_close_forgets_automatic_recovery_pairing() {
        let inner = transport_test_inner();
        let session_id = SessionId::new();
        let pairing_code = PairingCode::parse("987-654-321").expect("测试控制码应有效");
        inner
            .known_pairings
            .lock()
            .await
            .insert(pairing_code.clone(), Some(session_id));
        let pending_request_id = RequestId::new();
        inner
            .automatic_pair_pending
            .lock()
            .await
            .insert(pending_request_id, pairing_code);

        forget_session_pairing(&inner, session_id).await;

        assert!(inner.known_pairings.lock().await.is_empty());
        assert!(inner.automatic_pair_pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn relay_takeover_notification_removes_connection_and_recovery_state() {
        let inner = transport_test_inner();
        let session_id = SessionId::new();
        let pairing_code = PairingCode::parse("456-789-123").expect("测试控制码应有效");
        inner
            .core
            .lock()
            .await
            .upsert_connection(test_connection(session_id, ConnectionState::Online));
        inner
            .known_pairings
            .lock()
            .await
            .insert(pairing_code.clone(), Some(session_id));
        inner
            .automatic_pair_pending
            .lock()
            .await
            .insert(RequestId::new(), pairing_code);
        let request_id = RequestId::new();
        let (response_sender, response_receiver) = oneshot::channel();
        inner.pending.lock().await.insert(
            request_id,
            PendingRemoteResponse {
                session_id,
                sender: response_sender,
            },
        );

        handle_wire_message(
            &inner,
            WireMessage::ConnectionRemoved {
                session_id,
                reason: "连接已由新 Controller 接管".to_owned(),
            },
        )
        .await;

        assert!(inner.core.lock().await.get_connection(session_id).is_none());
        assert!(inner.known_pairings.lock().await.is_empty());
        assert!(inner.automatic_pair_pending.lock().await.is_empty());
        assert!(matches!(
            response_receiver.await,
            Ok(Err(ApplicationError::Remote { code, .. })) if code == "connection_removed"
        ));
    }

    #[test]
    fn structured_serial_query_audit_never_records_command_plaintext() {
        let command = "display current-configuration";
        let operation = RemoteOperation::RunSerialQuery {
            serial_session_id: "serial-1".to_owned(),
            command: command.to_owned(),
            line_ending: remoteops_domain::SerialLineEnding::Cr,
            profile: remoteops_domain::SerialTerminalProfile::HuaweiVrp,
            overall_timeout_millis: 30_000,
            idle_timeout_millis: 1_200,
            max_bytes: 128 * 1024,
            max_pages: 50,
            readonly: true,
        };

        let request_audit = audit_operation(&operation);
        let event_audit = audit_event_payload(&EventPayload::OperationRequested { operation });

        for audit in [&request_audit, &event_audit] {
            assert!(!audit.contains(command));
            assert!(audit.contains("command_bytes=29"));
            assert!(audit.contains("command_sha256="));
        }
    }

    #[tokio::test]
    async fn concurrent_send_does_not_cancel_a_partial_incoming_frame() {
        let inner = transport_test_inner();
        let request_id = RequestId::new();
        let (pair_sender, pair_receiver) = oneshot::channel();
        inner
            .pair_pending
            .lock()
            .await
            .insert(request_id, pair_sender);

        let incoming = WireMessage::PairResult(PairResult {
            request_id,
            connection: None,
            error: Some("expected-test-error".to_owned()),
        });
        let payload = serde_json::to_vec(&incoming).expect("应序列化测试帧");
        let length = u32::try_from(payload.len()).expect("测试帧长度应有效");
        let mut frame = length.to_be_bytes().to_vec();
        frame.extend_from_slice(&payload);

        let (controller_stream, mut relay_stream) = duplex(4096);
        let (reader, writer) = tokio::io::split(controller_stream);
        let (outgoing_sender, outgoing_receiver) = mpsc::unbounded_channel();
        let transport_task =
            tokio::spawn(run_transport_io(inner, reader, writer, outgoing_receiver));

        relay_stream
            .write_all(&frame[..2])
            .await
            .expect("应写入部分长度前缀");
        relay_stream.flush().await.expect("应刷新部分帧");
        sleep(TokioDuration::from_millis(10)).await;

        outgoing_sender
            .send(WireMessage::Heartbeat {
                sent_at: Utc::now(),
            })
            .expect("并发发送应进入写队列");
        let outbound = timeout(
            TokioDuration::from_secs(1),
            read_frame::<WireMessage, _>(&mut relay_stream),
        )
        .await
        .expect("应及时收到并发发送的帧")
        .expect("并发发送帧应有效");
        assert!(matches!(outbound, WireMessage::Heartbeat { .. }));

        relay_stream
            .write_all(&frame[2..])
            .await
            .expect("应写完剩余协议帧");
        let pair_result = timeout(TokioDuration::from_secs(1), pair_receiver)
            .await
            .expect("分段帧应被完整读取")
            .expect("配对响应通道应保持打开");
        assert_eq!(pair_result.request_id, request_id);

        transport_task.abort();
    }

    #[tokio::test]
    async fn pending_entry_is_removed_after_timeout() {
        let request_id = RequestId::new();
        let pending = Mutex::new(BTreeMap::new());
        let (sender, receiver) = oneshot::channel::<PairResult>();
        pending.lock().await.insert(request_id, sender);

        let result = receive_pending(
            &pending,
            request_id,
            receiver,
            TokioDuration::from_millis(10),
            "测试通道已关闭",
        )
        .await;

        assert!(matches!(result, Err(ApplicationError::Timeout)));
        assert!(pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn controller_rejects_remote_secret_before_transport() {
        let client = RelayClient {
            inner: transport_test_inner(),
        };
        let result = client
            .execute(
                "missing-target",
                EventSource::Ai,
                RemoteOperation::ListProcesses,
                None,
                None,
                Some("must-not-enter-protocol".to_owned()),
            )
            .await;

        assert!(matches!(result, Err(ApplicationError::PolicyDenied(_))));
    }
}
