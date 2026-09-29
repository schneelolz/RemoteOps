use super::{
    AgentInstanceId, ControlBasis, ControlSource, ControlStateRequest, ControlStateResult,
    ControllerControlMode, ControllerInstanceId, ControllerKind, DefaultPolicy, Duration,
    InFlightRequest, PermissionMode, Relay, RelayState, RemoteOperation, RemoteRequest, RiskLevel,
    Sender, SessionBinding, SessionBindings, SessionControlState, SessionId, Utc, WireMessage,
    active_controller,
};
use std::sync::atomic::{AtomicU64, Ordering};

// 进程内唯一修订号避免同一连接解绑再配对后接受旧审批。
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

fn next_revision() -> u64 {
    NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
}

/// 比较审批绑定及修订号，续期不改变审批身份。
pub(super) fn control_versions_match(
    left: &SessionControlState,
    right: &SessionControlState,
) -> bool {
    left.session_id == right.session_id
        && left.agent_generation == right.agent_generation
        && left.controller_id == right.controller_id
        && left.controller_generation == right.controller_generation
        && left.revision == right.revision
        && left.mode == right.mode
        && left.source == right.source
}

/// 为当前绑定建立安全默认授权并应用上限和空闲失效。
pub(super) fn initialize_binding_state(
    bindings: &mut SessionBindings,
    session_id: SessionId,
    agent_generation: u64,
) {
    let permission = bindings.permission_mode_for(ControllerKind::Ai);
    let Some(binding) = bindings.ai.as_mut() else {
        return;
    };
    let stale = binding.control_state.as_ref().is_none_or(|current| {
        current.agent_generation != agent_generation
            || current.controller_id != binding.controller_id
            || current.controller_generation != binding.controller_generation
    });
    if stale {
        binding.control_state = Some(SessionControlState {
            session_id,
            agent_generation,
            controller_id: binding.controller_id,
            controller_generation: binding.controller_generation,
            revision: next_revision(),
            mode: match permission {
                PermissionMode::ReadOnly => ControllerControlMode::ReadOnly,
                PermissionMode::ApprovalRequired => ControllerControlMode::ExternalApproval,
                _ => ControllerControlMode::StepByStep,
            },
            source: None,
            expires_at: None,
        });
    }
    let current = binding.control_state.as_mut().expect("当前绑定已初始化");
    let limited = match permission {
        PermissionMode::ReadOnly => Some(ControllerControlMode::ReadOnly),
        PermissionMode::ApprovalRequired if current.mode != ControllerControlMode::ReadOnly => {
            Some(ControllerControlMode::ExternalApproval)
        }
        _ => None,
    };
    if let Some(mode) = limited.filter(|mode| *mode != current.mode) {
        current.mode = mode;
        current.source = None;
        current.expires_at = None;
        current.revision = next_revision();
    }
    if current.mode == ControllerControlMode::FullAccess
        && current
            .expires_at
            .is_some_and(|expires| expires <= Utc::now())
    {
        current.mode = ControllerControlMode::Expired;
        current.source = None;
        current.expires_at = None;
        current.revision = next_revision();
    }
}

/// 读取在线会话的最新权威授权。
pub(super) fn refresh_control_state(
    state: &mut RelayState,
    session_id: SessionId,
) -> Option<SessionControlState> {
    let generation = state
        .agents
        .values()
        .find(|agent| agent.session_id == session_id && agent.ready && agent.sender.is_some())?
        .connection_generation;
    let bindings = state.session_bindings.get_mut(&session_id)?;
    initialize_binding_state(bindings, session_id, generation);
    bindings.ai.as_ref()?.control_state.clone()
}

/// 在持有状态锁时向两端广播同一授权快照。
pub(super) fn broadcast_control_state(state: &RelayState, current: &SessionControlState) {
    if let Some(agent) = state.agents.values().find(|agent| {
        agent.session_id == current.session_id
            && agent.ready
            && agent.connection_generation == current.agent_generation
    }) && let Some(sender) = &agent.sender
    {
        let _ = sender.send(WireMessage::ControlStateUpdated(current.clone()));
    }
    if let Some(bindings) = state.session_bindings.get(&current.session_id) {
        for binding in bindings.all() {
            if let Some(controller) =
                active_controller(state, binding.controller_id, binding.controller_generation)
            {
                let _ = controller
                    .sender
                    .send(WireMessage::ControlStateUpdated(current.clone()));
            }
        }
    }
}

#[derive(Clone, Copy)]
enum RequestOrigin {
    Agent(AgentInstanceId, u64),
    Controller(ControllerInstanceId, u64),
}

impl Relay {
    /// 按当前 Agent 连接身份补发授权快照，不从心跳恢复授权。
    pub(super) async fn sync_agent_control_state(
        &self,
        agent_id: AgentInstanceId,
        generation: u64,
    ) {
        let mut state = self.state.lock().await;
        let Some(session_id) = state
            .agents
            .get(&agent_id)
            .filter(|agent| agent.ready && agent.connection_generation == generation)
            .map(|agent| agent.session_id)
        else {
            return;
        };
        if let Some(current) = refresh_control_state(&mut state, session_id) {
            broadcast_control_state(&state, &current);
        }
    }

    /// 处理现场 Agent 发起的授权查询或变更。
    pub(super) async fn handle_agent_control_state(
        &self,
        agent: AgentInstanceId,
        generation: u64,
        request: ControlStateRequest,
        sender: &Sender,
    ) {
        self.handle_control_state(RequestOrigin::Agent(agent, generation), request, sender)
            .await;
    }

    /// 处理已绑定控制端发起的授权查询或变更。
    pub(super) async fn handle_controller_control_state(
        &self,
        controller: ControllerInstanceId,
        generation: u64,
        request: ControlStateRequest,
        sender: &Sender,
    ) {
        self.handle_control_state(
            RequestOrigin::Controller(controller, generation),
            request,
            sender,
        )
        .await;
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_control_state(
        &self,
        origin: RequestOrigin,
        request: ControlStateRequest,
        sender: &Sender,
    ) {
        let mut state = self.state.lock().await;
        let authorized = match origin {
            RequestOrigin::Agent(id, generation) => state.agents.get(&id).is_some_and(|agent| {
                agent.session_id == request.session_id
                    && agent.connection_generation == generation
                    && agent.ready
            }),
            RequestOrigin::Controller(id, generation) => active_controller(&state, id, generation)
                .is_some_and(|controller| {
                    controller.sessions.contains(&request.session_id)
                        && state
                            .session_bindings
                            .get(&request.session_id)
                            .and_then(|bindings| bindings.get(controller.kind))
                            .is_some_and(|binding| {
                                binding.controller_id == id
                                    && binding.controller_generation == generation
                            })
                }),
        };
        if !authorized {
            let _ = sender.send(WireMessage::ControlStateResult(ControlStateResult {
                request_id: request.request_id,
                state: None,
                error: Some("当前连接未绑定目标会话".to_owned()),
            }));
            return;
        }
        let mut current = refresh_control_state(&mut state, request.session_id);
        let error = if let Some(mode) = request.mode {
            let valid_source = match origin {
                RequestOrigin::Agent(..) => matches!(
                    request.source,
                    Some(ControlSource::Local | ControlSource::LocalDefault)
                ),
                RequestOrigin::Controller(id, generation) => {
                    active_controller(&state, id, generation)
                        .is_some_and(|controller| controller.kind == ControllerKind::Ai)
                        && request.source == Some(ControlSource::Mcp)
                }
            };
            if !valid_source {
                Some("授权来源与发送方身份不匹配".to_owned())
            } else if !matches!(
                mode,
                ControllerControlMode::FullAccess
                    | ControllerControlMode::StepByStep
                    | ControllerControlMode::ReadOnly
                    | ControllerControlMode::ExternalApproval
            ) {
                Some("不允许主动设置该控制模式".to_owned())
            } else if !current
                .as_ref()
                .zip(request.expected.as_ref())
                .is_some_and(|(latest, expected)| control_versions_match(latest, expected))
            {
                Some("控制授权已经变化，请刷新后重新确认".to_owned())
            } else {
                let bindings = state
                    .session_bindings
                    .get_mut(&request.session_id)
                    .expect("已验证当前绑定");
                let limit = bindings.permission_mode_for(ControllerKind::Ai);
                if (limit == PermissionMode::ReadOnly && mode != ControllerControlMode::ReadOnly)
                    || (limit == PermissionMode::ApprovalRequired
                        && !matches!(
                            mode,
                            ControllerControlMode::ReadOnly
                                | ControllerControlMode::ExternalApproval
                        ))
                {
                    Some("请求超出会话只读或强制审批上限".to_owned())
                } else {
                    let latest = bindings
                        .ai
                        .as_mut()
                        .and_then(|binding| binding.control_state.as_mut())
                        .expect("已验证授权快照");
                    latest.mode = mode;
                    latest.revision = next_revision();
                    latest.source = (mode == ControllerControlMode::FullAccess)
                        .then_some(request.source)
                        .flatten();
                    latest.expires_at = (mode == ControllerControlMode::FullAccess
                        && request.source == Some(ControlSource::Mcp))
                    .then(|| Utc::now() + Duration::hours(1));
                    current = Some(latest.clone());
                    None
                }
            }
        } else {
            current.is_none().then(|| "当前没有在线 AI 绑定".to_owned())
        };
        let _ = sender.send(WireMessage::ControlStateResult(ControlStateResult {
            request_id: request.request_id,
            state: current.clone(),
            error,
        }));
        if let Some(current) = current {
            broadcast_control_state(&state, &current);
        }
    }
}

/// 上传续操作仍会写入文件，不能因策略归类为资源操作而跳过授权检查。
pub(super) fn requires_control_proof(policy: &DefaultPolicy, operation: &RemoteOperation) -> bool {
    policy.classify(operation) != RiskLevel::ReadOnly
        || matches!(
            operation,
            RemoteOperation::UploadFileChunk { .. } | RemoteOperation::CompleteUploadFile { .. }
        )
}

/// 验证 AI 修改请求的审批依据并返回派生权限。
pub(super) fn validate_control_proof(
    binding: &SessionBinding,
    limit: PermissionMode,
    request: &RemoteRequest,
) -> Result<PermissionMode, &'static str> {
    let proof = request
        .control_proof
        .as_ref()
        .ok_or("AI 修改请求缺少当前授权凭据，请升级控制端并重新确认")?;
    let current = binding
        .control_state
        .as_ref()
        .ok_or("当前连接没有有效授权状态")?;
    if !control_versions_match(current, &proof.state) {
        return Err("审批对应的控制授权已经变化，拒绝旧请求");
    }
    if limit == PermissionMode::ReadOnly || current.mode == ControllerControlMode::ReadOnly {
        return Err("当前会话仅允许只读操作");
    }
    match proof.basis {
        ControlBasis::FullAccess
            if current.mode == ControllerControlMode::FullAccess
                && current
                    .expires_at
                    .is_none_or(|expires| expires > Utc::now())
                && limit != PermissionMode::ApprovalRequired =>
        {
            Ok(PermissionMode::ControllerApproved)
        }
        ControlBasis::SingleApproval
            if matches!(
                current.mode,
                ControllerControlMode::StepByStep
                    | ControllerControlMode::Expired
                    | ControllerControlMode::FullAccess
            ) && limit != PermissionMode::ApprovalRequired =>
        {
            Ok(PermissionMode::ControllerApproved)
        }
        ControlBasis::ExternalApproval if request.approval_id.is_some() => {
            Ok(PermissionMode::ApprovalRequired)
        }
        _ => Err("当前授权模式不允许该审批依据"),
    }
}

/// 仅成功的同代次 AI 操作延长尚未失效的 MCP 授权。
pub(super) fn renew_after_success(
    state: &mut RelayState,
    completed: &InFlightRequest,
    message: &WireMessage,
) {
    if completed.controller_kind != ControllerKind::Ai {
        return;
    }
    let WireMessage::RemoteResponse(response) = message else {
        return;
    };
    if response.error_code.is_some() || response.exit_code.is_some_and(|code| code != 0) {
        return;
    }
    let Some(proof_state) = &completed.control_state else {
        return;
    };
    let Some(current) = state
        .session_bindings
        .get_mut(&completed.session_id)
        .and_then(|bindings| bindings.ai.as_mut())
        .and_then(|binding| binding.control_state.as_mut())
    else {
        return;
    };
    if control_versions_match(current, proof_state)
        && current.mode == ControllerControlMode::FullAccess
        && current.source == Some(ControlSource::Mcp)
        && current
            .expires_at
            .is_some_and(|expires| expires > Utc::now())
    {
        current.expires_at = Some(Utc::now() + Duration::hours(1));
        let current = current.clone();
        broadcast_control_state(state, &current);
    }
}
