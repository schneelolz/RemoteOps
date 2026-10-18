//! 受管目标的运行态与持久授权在同一 Relay 状态锁下衔接。
use super::{
    AgentHello, AgentInstanceId, AgentRecord, BTreeMap, Context, ControllerInstanceId,
    ControllerKind, ControllerOwnerId, ControllerRecord, DateTime, PROTOCOL_VERSION, PairRequest,
    PairResult, PairingErrorCode, PairingMode, Path, PathBuf, PortalStore, Relay, RelayState,
    RequestId, SessionId, Utc, Uuid, WireMessage, active_controller, bail, end_session, time,
};
use remoteops_portal::{
    ConnectionSummary, ConsumerIdentity, CreateAssetRequest, CreatePrincipalRequest, CreatedAsset,
    CreatedPrincipal, InvitationSummary, IssueInvitationRequest, IssuedInvitation, PortalError,
    PortalResult, TargetSnapshot,
};

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
enum LimitKey {
    Global,
    Owner(ControllerOwnerId),
    Installation(Uuid),
    Peer(std::net::IpAddr),
}

#[derive(Default)]
pub(super) struct PairingLimits(BTreeMap<LimitKey, (time::Instant, u32)>);
type PairingResolution =
    Result<(AgentInstanceId, Option<String>, Option<Uuid>), (PairingErrorCode, String)>;
impl PairingLimits {
    fn keys(controller: &ControllerRecord) -> Vec<(LimitKey, u32)> {
        let mut keys = vec![
            (LimitKey::Global, 1000),
            (
                controller
                    .installation_id
                    .map_or(LimitKey::Owner(controller.owner_id), LimitKey::Installation),
                60,
            ),
        ];
        if let Some(peer) = controller.peer_ip {
            keys.push((LimitKey::Peer(peer), 120));
        }
        keys
    }
    pub(super) fn allows(&self, controller: &ControllerRecord) -> bool {
        Self::keys(controller).into_iter().all(|(key, limit)| {
            self.0.get(&key).is_none_or(|(start, count)| {
                start.elapsed() >= time::Duration::from_mins(1) || *count < limit
            })
        })
    }
    fn failure(&mut self, controller: &ControllerRecord) {
        self.0
            .retain(|_, (start, _)| start.elapsed() < time::Duration::from_mins(1));
        for (key, _) in Self::keys(controller) {
            if self.0.len() >= 8192 && !self.0.contains_key(&key) {
                continue;
            }
            let window = self.0.entry(key).or_insert((time::Instant::now(), 0));
            window.1 = window.1.saturating_add(1);
        }
    }
}

pub(super) fn portal_database_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".portal.sqlite3");
    PathBuf::from(value)
}

pub(super) fn target(agent: &AgentRecord) -> TargetSnapshot {
    TargetSnapshot {
        agent_id: agent.hello.agent_instance_id,
        boot_id: agent.hello.boot_id,
        session_id: agent.session_id,
        agent_generation: agent.connection_generation,
    }
}

fn consumer(
    controller_id: ControllerInstanceId,
    controller: &ControllerRecord,
) -> ConsumerIdentity {
    ConsumerIdentity {
        controller_id,
        installation_id: controller.installation_id,
        owner_id: controller.owner_id,
    }
}

pub(super) fn reject_pair(
    state: &mut RelayState,
    controller_id: ControllerInstanceId,
    request_id: RequestId,
    code: PairingErrorCode,
    message: impl Into<String>,
) -> PairResult {
    if code != PairingErrorCode::AgentOffline {
        let (controllers, limits) = (&state.controllers, &mut state.pairing_limits);
        if let Some(controller) = controllers.get(&controller_id) {
            limits.failure(controller);
        }
    }
    PairResult {
        request_id,
        connection: None,
        connection_resume_token: None,
        error_code: Some(code),
        error: Some(message.into()),
    }
}

// 作为 Result::map_err 的转换函数消费错误。
#[allow(clippy::needless_pass_by_value)]
fn portal_pair_error(error: PortalError) -> (PairingErrorCode, String) {
    let code = match error {
        PortalError::Expired => PairingErrorCode::InvitationExpired,
        PortalError::Conflict => PairingErrorCode::AlreadyRedeemed,
        PortalError::Revoked | PortalError::Unauthorized | PortalError::Forbidden => {
            PairingErrorCode::AccessRevoked
        }
        PortalError::TargetMismatch => PairingErrorCode::SessionMismatch,
        PortalError::NotFound => PairingErrorCode::UnknownOrExpired,
        _ => PairingErrorCode::Rejected,
    };
    (code, error.to_string())
}

impl Relay {
    // 新启动登记若曾在跨存储边界中断，同启动重试仍须收敛旧门户状态。
    pub(super) fn reconcile_managed_boot(
        &self,
        agent_id: AgentInstanceId,
        boot_id: super::AgentBootId,
    ) -> anyhow::Result<()> {
        let store = self.portal_store()?;
        let asset = store
            .asset_for_agent(agent_id)?
            .context("受管资产登记不存在")?;
        let status = store.status(asset.principal_id, &asset.asset_id, Utc::now())?;
        let stale = status
            .connection
            .is_some_and(|connection| connection.target.boot_id != boot_id)
            || status
                .invitation
                .is_some_and(|invitation| invitation.target.boot_id != boot_id);
        if stale {
            store.close_target(&asset.target_id)?;
        }
        Ok(())
    }

    /// 调用方先验证绑定归属；持久撤销成功后才清理运行态。
    pub(super) fn close_managed_target(
        &self,
        state: &mut RelayState,
        session: SessionId,
    ) -> PortalResult<()> {
        let target_id = state
            .agents
            .values()
            .find(|agent| agent.session_id == session)
            .and_then(|agent| agent.managed_target_id.clone())
            .ok_or(PortalError::NotFound)?;
        self.portal_store()?.close_target(&target_id)?;
        end_managed_session(state, session);
        if let Some(agent) = state
            .agents
            .values_mut()
            .find(|agent| agent.session_id == session)
        {
            agent.session_id = SessionId::new();
        }
        self.persist_state_locked(state)
            .map_err(|_| PortalError::Storage)
    }

    fn portal_store(&self) -> PortalResult<&PortalStore> {
        self.portal_store.as_deref().ok_or(PortalError::Storage)
    }

    pub(super) fn validate_managed_agent(
        &self,
        hello: &AgentHello,
    ) -> anyhow::Result<Option<String>> {
        if hello.pairing_mode == PairingMode::Unknown {
            bail!("Agent 配对模式未知");
        }
        if let Some(context) = &hello.target_context {
            context.validate().map_err(anyhow::Error::msg)?;
        }
        let registered = self
            .portal_store
            .as_deref()
            .map(|store| store.asset_for_agent(hello.agent_instance_id))
            .transpose()?
            .flatten();
        if hello.pairing_mode == PairingMode::ManagedOnDemand {
            if hello.protocol_version != PROTOCOL_VERSION || hello.target_context.is_none() {
                bail!("受管 Agent 需要新版协议及目标上下文");
            }
            let enrollment = hello
                .managed_enrollment
                .as_ref()
                .context("受管 Agent 缺少部署登记凭据")?;
            let asset = self.portal_store()?.validate_agent_enrollment(
                &enrollment.target_id,
                &enrollment.token,
                hello.agent_instance_id,
            )?;
            Ok(Some(asset.target_id))
        } else {
            if registered.is_some() || hello.managed_enrollment.is_some() {
                bail!("已登记的受管 Agent 不允许切换自动签码模式");
            }
            Ok(None)
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn resolve_pairing_target(
        &self,
        state: &RelayState,
        controller_id: ControllerInstanceId,
        request: &PairRequest,
        now: DateTime<Utc>,
    ) -> PairingResolution {
        let controller = active_controller(
            state,
            controller_id,
            state
                .controllers
                .get(&controller_id)
                .map_or(0, |controller| controller.connection_generation),
        )
        .ok_or((PairingErrorCode::Rejected, "控制端已经断开".to_owned()))?;
        if !state.pairing_limits.allows(controller) {
            return Err((
                PairingErrorCode::Rejected,
                "配对失败次数过多，请稍后再试".into(),
            ));
        }
        if request.connection_resume_token.is_some()
            && (request.pairing_code.is_some() || request.expected_session_id.is_none())
        {
            return Err((
                PairingErrorCode::Rejected,
                "受管恢复不能提交一次码，且必须指定原会话".into(),
            ));
        }
        if request.connection_resume_token.is_none()
            && let Some(code) = &request.pairing_code
            && let Some(agent_id) = state.pairing_index.get(code)
        {
            return Ok((*agent_id, None, None));
        }
        if controller.protocol_version != PROTOCOL_VERSION || controller.kind != ControllerKind::Ai
        {
            return Err((
                PairingErrorCode::UnsupportedMode,
                "受管配对需要兼容的新版通用 MCP".into(),
            ));
        }
        let store = self.portal_store().map_err(|_| {
            (
                if request.expected_session_id.is_some() {
                    PairingErrorCode::SessionEnded
                } else {
                    PairingErrorCode::UnknownOrExpired
                },
                "控制码不存在或原会话已结束".into(),
            )
        })?;
        let saved = if let Some(token) = &request.connection_resume_token {
            store.connection_target(token)
        } else if let Some(code) = &request.pairing_code {
            store
                .invitation_target(code.as_str(), now)
                .map_err(|error| {
                    if matches!(error, PortalError::Unauthorized) {
                        PortalError::NotFound
                    } else {
                        error
                    }
                })
        } else {
            return Err((
                PairingErrorCode::Rejected,
                "缺少控制码或连接恢复凭据".into(),
            ));
        }
        .map_err(|error| {
            if matches!(error, PortalError::NotFound) && request.expected_session_id.is_some() {
                (PairingErrorCode::SessionEnded, "原会话已经结束".into())
            } else {
                portal_pair_error(error)
            }
        })?;
        let agent = state
            .agents
            .get(&saved.agent_id)
            .ok_or((PairingErrorCode::SessionEnded, "受管目标当前不可用".into()))?;
        if agent.managed_target_id.is_none()
            || agent.hello.boot_id != saved.boot_id
            || agent.session_id != saved.session_id
            || request
                .expected_session_id
                .is_some_and(|session| session != saved.session_id)
        {
            return Err((
                PairingErrorCode::SessionMismatch,
                "原运行或会话已经结束".into(),
            ));
        }
        if !agent.ready || agent.sender.is_none() || !agent.lease.is_valid_at(now) {
            return Err((PairingErrorCode::AgentOffline, "受管 Agent 暂时离线".into()));
        }
        let identity = consumer(controller_id, controller);
        let access = if let Some(token) = &request.connection_resume_token {
            store.resume(token, identity, target(agent), now)
        } else {
            store.redeem(
                request
                    .pairing_code
                    .as_ref()
                    .expect("已经验证控制码")
                    .as_str(),
                identity,
                target(agent),
                request.request_id.as_uuid(),
                now,
            )
        }
        .map_err(portal_pair_error)?;
        Ok((
            saved.agent_id,
            Some(access.resume_token),
            Some(access.connection_id),
        ))
    }

    pub(super) fn managed_request_allowed(
        &self,
        state: &RelayState,
        session: SessionId,
        controller_id: ControllerInstanceId,
        generation: u64,
    ) -> bool {
        let Some(agent) = state
            .agents
            .values()
            .find(|agent| agent.session_id == session)
        else {
            return false;
        };
        if agent.managed_target_id.is_none() {
            return true;
        }
        let Some(controller) = active_controller(state, controller_id, generation) else {
            return false;
        };
        let Some(connection_id) = state
            .session_bindings
            .get(&session)
            .and_then(|bindings| bindings.managed_connection_id)
        else {
            return false;
        };
        self.portal_store()
            .and_then(|store| {
                store.connection_authorized(
                    connection_id,
                    consumer(controller_id, controller),
                    target(agent),
                )
            })
            .is_ok()
    }

    pub(crate) async fn portal_create_principal(
        &self,
        request: CreatePrincipalRequest,
    ) -> PortalResult<CreatedPrincipal> {
        let _state = self.state.lock().await;
        self.portal_store()?.create_principal(request)
    }
    pub(crate) async fn portal_create_asset(
        &self,
        request: CreateAssetRequest,
    ) -> PortalResult<CreatedAsset> {
        let _state = self.state.lock().await;
        self.portal_store()?.create_asset(request)
    }
    pub(crate) async fn portal_status(
        &self,
        bearer: &str,
        asset_id: &str,
    ) -> PortalResult<serde_json::Value> {
        let state = self.state.lock().await;
        let store = self.portal_store()?;
        let principal = store.authenticate_principal(bearer)?;
        let status = store.status(principal.principal_id, asset_id, Utc::now())?;
        let agent = status.asset.agent_id.and_then(|id| state.agents.get(&id));
        Ok(serde_json::json!({"schema_version":1,"asset":status.asset,
            "online":agent.is_some_and(|a| a.ready && a.sender.is_some() && a.lease.is_valid_at(Utc::now())),
            "target":agent.map(target),"invitation":status.invitation,"connection":status.connection}))
    }
    pub(crate) async fn portal_issue(
        &self,
        bearer: &str,
        asset_id: &str,
        request: IssueInvitationRequest,
    ) -> PortalResult<IssuedInvitation> {
        let mut state = self.state.lock().await;
        let store = self.portal_store()?;
        let principal = store.authenticate_principal(bearer)?;
        let asset = store.asset_for_principal(principal.principal_id, asset_id)?;
        let agent_id = asset.agent_id.ok_or(PortalError::Conflict)?;
        let agent = state.agents.get(&agent_id).ok_or(PortalError::Conflict)?;
        if agent.managed_target_id.as_deref() != Some(&asset.target_id)
            || !agent.ready
            || agent.sender.is_none()
            || !agent.lease.is_valid_at(Utc::now())
        {
            return Err(PortalError::Conflict);
        }
        let snapshot = target(agent);
        let reserved = state
            .pairing_index
            .keys()
            .map(|code| code.as_str().to_owned())
            .collect::<Vec<_>>();
        let issued = store.issue(
            principal.principal_id,
            asset_id,
            snapshot,
            request,
            &reserved,
            Utc::now(),
        )?;
        let previous = state
            .agents
            .get(&agent_id)
            .expect("持锁内已验证 Agent")
            .clone();
        if previous.session_id != issued.invitation.target.session_id {
            end_managed_session(&mut state, previous.session_id);
        }
        let agent = state.agents.get_mut(&agent_id).expect("持锁内已验证 Agent");
        agent.session_id = issued.invitation.target.session_id;
        if let Err(_error) = self.persist_state_locked(&state) {
            state.agents.insert(agent_id, previous.clone());
            end_managed_session(&mut state, previous.session_id);
            return Err(PortalError::Storage);
        }
        Ok(issued)
    }
    pub(crate) async fn portal_revoke_invitation(
        &self,
        bearer: &str,
        id: Uuid,
    ) -> PortalResult<InvitationSummary> {
        let _state = self.state.lock().await;
        let store = self.portal_store()?;
        let principal = store.authenticate_principal(bearer)?;
        store.revoke_invitation(principal.principal_id, id, Utc::now())
    }
    pub(crate) async fn portal_connection(
        &self,
        bearer: &str,
        id: Uuid,
    ) -> PortalResult<ConnectionSummary> {
        let _state = self.state.lock().await;
        let store = self.portal_store()?;
        let principal = store.authenticate_principal(bearer)?;
        store.connection(principal.principal_id, id)
    }
    pub(crate) async fn portal_close(
        &self,
        bearer: &str,
        id: Uuid,
        revision: Option<u64>,
    ) -> PortalResult<ConnectionSummary> {
        let mut state = self.state.lock().await;
        let store = self.portal_store()?;
        let principal = store.authenticate_principal(bearer)?;
        let connection =
            store.close_connection(principal.principal_id, id, revision, Utc::now())?;
        end_managed_connection(&mut state, &connection);
        self.persist_state_locked(&state)
            .map_err(|_| PortalError::Storage)?;
        Ok(connection)
    }
    pub(crate) async fn portal_revoke_principal(
        &self,
        id: Uuid,
    ) -> PortalResult<Vec<ConnectionSummary>> {
        let mut state = self.state.lock().await;
        let connections = self.portal_store()?.revoke_principal(id)?;
        for connection in &connections {
            end_managed_connection(&mut state, connection);
        }
        self.persist_state_locked(&state)
            .map_err(|_| PortalError::Storage)?;
        Ok(connections)
    }
}

// 先撤销全部运行态，再统一持久化；单个 JSON 故障不能中止其余目标的撤权。
fn end_managed_connection(state: &mut RelayState, connection: &ConnectionSummary) {
    let id = connection.target.session_id;
    end_managed_session(state, id);
    if let Some(agent) = state.agents.get_mut(&connection.target.agent_id)
        && agent.session_id == id
        && agent.hello.boot_id == connection.target.boot_id
    {
        agent.session_id = SessionId::new();
    }
}

pub(super) fn end_managed_session(state: &mut RelayState, session: SessionId) {
    if let Some(bindings) = state.session_bindings.get(&session)
        && let Some(agent) = state
            .agents
            .values()
            .find(|agent| agent.session_id == session)
        && let Some(sender) = &agent.sender
    {
        for binding in bindings.all() {
            let _ = sender.send(WireMessage::ControllerBindingRevoked {
                session_id: session,
                binding_token: binding.binding_token.clone(),
            });
        }
    }
    end_session(state, session, "受管远程访问已结束，请重新取得控制码");
}
