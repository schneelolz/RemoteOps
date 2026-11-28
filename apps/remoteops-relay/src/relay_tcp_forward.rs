//! Relay 的 TCP v2 归属、授权、策略和有界路由。
use super::{Relay, RelayState, Sender, active_controller, append_audit, control};
use anyhow::{Context, bail};
use chrono::Utc;
use remoteops_domain::{
    AgentInstanceId, ControllerInstanceId, PermissionMode, RemoteOperation, RequestId, SessionId,
};
use remoteops_protocol::tcp_forward::{
    IDLE_TIMEOUT, MAX_CONNECTIONS, MAX_ENDPOINT_CONNECTIONS, MAX_FORWARDS, MAX_RELAY_CONNECTIONS,
    TOTAL_BYTES, TcpForwardFrame as Frame, TcpForwardPolicy, TcpForwardPolicySnapshot,
    TcpForwardRule, TcpForwardWindow,
};
use remoteops_protocol::{RemoteRequest, WireMessage};
use std::collections::{BTreeMap, BTreeSet};
use tokio::time;

#[derive(Default)]
pub(super) struct TcpState {
    pub(super) policy: TcpForwardPolicy,
    forwards: BTreeMap<RequestId, Forward>,
    rate_started: Option<time::Instant>,
    rate_bytes: [u64; 2],
}

struct Forward {
    session_id: SessionId,
    agent_id: AgentInstanceId,
    agent_generation: u64,
    controller_id: ControllerInstanceId,
    controller_generation: u64,
    installation_id: Option<String>,
    proof: remoteops_protocol::ControlProof,
    host: String,
    port: u16,
    bytes: u64,
    last_activity: time::Instant,
    streams: BTreeMap<RequestId, Flow>,
    seen_streams: BTreeSet<RequestId>,
}

#[derive(Default)]
struct Flow {
    connected: bool,
    upstream: TcpForwardWindow,
    downstream: TcpForwardWindow,
}

impl TcpState {
    fn connection_limit_reached(&self, forward: &Forward) -> bool {
        let total = self
            .forwards
            .values()
            .map(|entry| entry.streams.len())
            .sum::<usize>();
        let controller_total = self
            .forwards
            .values()
            .filter(|entry| entry.controller_id == forward.controller_id)
            .map(|entry| entry.streams.len())
            .sum::<usize>();
        let agent_total = self
            .forwards
            .values()
            .filter(|entry| entry.agent_id == forward.agent_id)
            .map(|entry| entry.streams.len())
            .sum::<usize>();
        forward.streams.len() >= MAX_CONNECTIONS
            || total >= MAX_RELAY_CONNECTIONS
            || controller_total >= MAX_ENDPOINT_CONNECTIONS
            || agent_total >= MAX_ENDPOINT_CONNECTIONS
    }
}

impl Relay {
    pub(crate) async fn tcp_policy(&self) -> TcpForwardPolicy {
        self.state.lock().await.tcp.policy.clone()
    }

    pub(crate) async fn update_tcp_policy(
        &self,
        actor: &str,
        expected: u64,
        mut policy: TcpForwardPolicy,
    ) -> anyhow::Result<TcpForwardPolicy> {
        policy.validate().map_err(anyhow::Error::msg)?;
        let mut state = self.state.lock().await;
        if expected != state.tcp.policy.revision {
            bail!("tcp_policy_conflict：策略已更新，请重新读取并合并");
        }
        policy.revision = expected.checked_add(1).context("TCP 策略修订号耗尽")?;
        policy.updated_at = Some(Utc::now());
        let old = std::mem::replace(&mut state.tcp.policy, policy.clone());
        let previous_audit = state.audit_log.clone();
        append_audit(
            &mut state,
            "tcp_policy_updated",
            None,
            true,
            actor,
            &format!("revision={} rules={}", policy.revision, policy.rules.len()),
        );
        if let Err(error) = self.persist_state_locked(&state) {
            state.tcp.policy = old;
            state.audit_log = previous_audit;
            return Err(error);
        }
        self.prune_tcp_locked(&mut state);
        self.persist_state_locked(&state)?;
        Ok(policy)
    }

    pub(super) fn register_tcp_forward(
        state: &mut RelayState,
        controller_id: ControllerInstanceId,
        generation: u64,
        request: &RemoteRequest,
    ) -> Result<Vec<TcpForwardRule>, String> {
        let RemoteOperation::CreateTcpForward {
            host,
            port,
            local_address,
        } = &request.operation
        else {
            return Err("不是 TCP 转发操作".into());
        };
        remoteops_protocol::tcp_forward::validate_target(host, *port)?;
        if host.contains('/')
            || !local_address
                .parse::<std::net::SocketAddr>()
                .is_ok_and(|address| address.ip().is_loopback() && address.port() > 0)
        {
            return Err("TCP 创建需要单个目标及实际本机回环监听地址".into());
        }
        let controller =
            active_controller(state, controller_id, generation).ok_or("Controller 已断线")?;
        let installation = controller.installation_id.map(|id| id.to_string());
        let agent = state
            .agents
            .values()
            .find(|agent| {
                agent.session_id == request.session_id && agent.ready && agent.sender.is_some()
            })
            .ok_or("Agent 已离线")?;
        if !agent
            .hello
            .capabilities
            .contains(remoteops_domain::Capability::TcpForward)
            || !agent.hello.target_context.as_ref().is_some_and(|context| {
                context.capabilities.iter().any(|status| {
                    status.capability == "tcp_forward_v2"
                        && status.status == remoteops_domain::CapabilityAvailability::Available
                })
            })
        {
            return Err("tcp_forward_unsupported：需要支持持续转发 v2 的 Agent".into());
        }
        if state.tcp.forwards.contains_key(&request.request_id)
            || state.tcp.forwards.len() >= 64
            || state
                .tcp
                .forwards
                .values()
                .filter(|f| f.controller_id == controller_id)
                .count()
                >= MAX_FORWARDS
        {
            return Err("TCP 转发标识重复或活动数量达到上限".into());
        }
        let rules = state
            .tcp
            .policy
            .rules
            .iter()
            .filter(|r| {
                r.allows(
                    host,
                    *port,
                    agent.hello.agent_instance_id,
                    installation.as_deref(),
                    Utc::now(),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        if rules.is_empty() {
            return Err(format!(
                "tcp_target_not_approved：目标尚未获准。{}。申请字段为目标主机、端口、用途、现场 Agent 身份和请求方安装身份；先调用 get_tcp_forward_policy 查看当前绑定策略。策略修改无需重启 Relay。",
                state.tcp.policy.approval_contact
            ));
        }
        let proof = request
            .control_proof
            .clone()
            .ok_or("TCP 创建缺少控制授权依据")?;
        state.tcp.forwards.insert(
            request.request_id,
            Forward {
                session_id: request.session_id,
                agent_id: agent.hello.agent_instance_id,
                agent_generation: agent.connection_generation,
                controller_id,
                controller_generation: generation,
                installation_id: installation,
                proof,
                host: host.clone(),
                port: *port,
                bytes: 0,
                last_activity: time::Instant::now(),
                streams: BTreeMap::new(),
                seen_streams: BTreeSet::new(),
            },
        );
        Ok(rules)
    }

    pub(super) async fn query_tcp_policy(
        &self,
        controller: ControllerInstanceId,
        generation: u64,
        request_id: RequestId,
        session_id: SessionId,
        sender: &Sender,
    ) {
        let state = self.state.lock().await;
        let snapshot = (|| {
            let record = active_controller(&state, controller, generation)?;
            if !record.sessions.contains(&session_id)
                || !self.managed_request_allowed(&state, session_id, controller, generation)
            {
                return None;
            }
            let agent = state
                .agents
                .values()
                .find(|a| a.session_id == session_id && a.ready)?;
            let installation = record.installation_id.map(|id| id.to_string());
            let mut policy = state.tcp.policy.clone();
            policy.rules.retain(|rule| {
                rule.agent_id
                    .is_none_or(|id| id == agent.hello.agent_instance_id)
                    && rule
                        .installation_id
                        .as_deref()
                        .is_none_or(|id| Some(id) == installation.as_deref())
                    && rule.expires_at.is_none_or(|at| at > Utc::now())
            });
            Some(TcpForwardPolicySnapshot {
                session_id,
                agent_id: agent.hello.agent_instance_id,
                requester_installation_id: installation,
                policy,
                application_fields: vec![
                    "host".into(),
                    "port".into(),
                    "purpose".into(),
                    "agent_id".into(),
                    "installation_id".into(),
                ],
                hot_reload: true,
            })
        })();
        let error = snapshot
            .is_none()
            .then(|| "当前绑定无权读取目标策略".into());
        let _ = sender.send(WireMessage::TcpForwardPolicyResponse {
            request_id,
            snapshot,
            error,
        });
    }

    pub(super) async fn tcp_from_controller(
        &self,
        id: ControllerInstanceId,
        generation: u64,
        frame: Frame,
    ) {
        let mut state = self.state.lock().await;
        let forward_id = frame.forward_id();
        let Some(forward) = state.tcp.forwards.get(&forward_id) else {
            return;
        };
        if forward.controller_id != id || forward.controller_generation != generation {
            return;
        }
        if !self.tcp_authorized(&state, forward) {
            close(&mut state, forward_id, "TCP 当前授权已撤销或到期");
            return;
        }
        Self::route_tcp_locked(&mut state, frame, true);
    }

    pub(super) async fn tcp_from_agent(&self, id: AgentInstanceId, generation: u64, frame: Frame) {
        let mut state = self.state.lock().await;
        let Some(forward) = state.tcp.forwards.get(&frame.forward_id()) else {
            return;
        };
        if forward.agent_id != id || forward.agent_generation != generation {
            return;
        }
        if !self.tcp_authorized(&state, forward) {
            let id = frame.forward_id();
            close(&mut state, id, "TCP 当前授权已撤销或到期");
            return;
        }
        Self::route_tcp_locked(&mut state, frame, false);
    }

    fn tcp_authorized(&self, state: &RelayState, forward: &Forward) -> bool {
        let Some(controller) =
            active_controller(state, forward.controller_id, forward.controller_generation)
        else {
            return false;
        };
        if !controller.sessions.contains(&forward.session_id)
            || !self.managed_request_allowed(
                state,
                forward.session_id,
                forward.controller_id,
                forward.controller_generation,
            )
        {
            return false;
        }
        if !state.agents.get(&forward.agent_id).is_some_and(|a| {
            a.ready
                && a.sender.is_some()
                && a.connection_generation == forward.agent_generation
                && a.lease.is_valid_at(Utc::now())
        }) {
            return false;
        }
        let Some(bindings) = state.session_bindings.get(&forward.session_id) else {
            return false;
        };
        if bindings.permission_mode_for(controller.kind) == PermissionMode::ReadOnly {
            return false;
        }
        let Some(current) = bindings.ai.as_ref().and_then(|b| b.control_state.as_ref()) else {
            return false;
        };
        if current.initialization_pending
            || !control::control_versions_match(current, &forward.proof.state)
            || current.expires_at.is_some_and(|at| at <= Utc::now())
        {
            return false;
        }
        state.tcp.policy.rules.iter().any(|rule| {
            rule.allows(
                &forward.host,
                forward.port,
                forward.agent_id,
                forward.installation_id.as_deref(),
                Utc::now(),
            )
        })
    }

    #[allow(clippy::too_many_lines)]
    fn route_tcp_locked(state: &mut RelayState, frame: Frame, from_controller: bool) {
        let forward_id = frame.forward_id();
        if matches!(frame, Frame::Stop { .. }) {
            close(state, forward_id, "TCP 转发已关闭");
            return;
        }
        // 两个方向可同时结束。已退休连接的在途帧不能影响仍存活的监听。
        if !matches!(frame, Frame::Connect { .. })
            && let Some(connection) = frame.connection_id()
            && state.tcp.forwards.get(&forward_id).is_some_and(|forward| {
                forward.seen_streams.contains(&connection)
                    && !forward.streams.contains_key(&connection)
            })
        {
            return;
        }
        if let Frame::Connect { connection_id, .. } = &frame
            && from_controller
            && let Some(forward) = state.tcp.forwards.get(&forward_id)
            && state.tcp.connection_limit_reached(forward)
        {
            // 配额只拒绝新连接，保留监听及已经建立的连接。
            if let Some(controller) =
                active_controller(state, forward.controller_id, forward.controller_generation)
            {
                let _ = controller.sender.channel.try_send(WireMessage::TcpForwardV2(Frame::Connected {
                        forward_id, connection_id: *connection_id,
                        error: Some(format!("TCP 连接配额已满，每条转发 {MAX_CONNECTIONS}，每端点 {MAX_ENDPOINT_CONNECTIONS}，Relay 总计 {MAX_RELAY_CONNECTIONS}")),
                    }));
            }
            return;
        }
        let result = (|| -> Result<(), String> {
            if let Frame::Data { data_base64, .. } = &frame {
                let now = time::Instant::now();
                if state.tcp.rate_started.is_none_or(|started| {
                    now.duration_since(started) >= time::Duration::from_secs(1)
                }) {
                    state.tcp.rate_started = Some(now);
                    state.tcp.rate_bytes = [0; 2];
                }
                let direction = usize::from(from_controller);
                state.tcp.rate_bytes[direction] = state.tcp.rate_bytes[direction]
                    .saturating_add((data_base64.len() / 4 * 3) as u64);
                if state.tcp.rate_bytes[direction]
                    > remoteops_protocol::tcp_forward::BYTES_PER_SECOND
                        + remoteops_protocol::tcp_forward::WINDOW_BYTES
                {
                    return Err("Relay TCP 单方向进程速率达到上限".into());
                }
            }
            let forward = state
                .tcp
                .forwards
                .get_mut(&forward_id)
                .ok_or("TCP 转发不存在")?;
            match &frame {
                Frame::Connect { connection_id, .. } if from_controller => {
                    if forward.streams.len() >= MAX_CONNECTIONS
                        || forward.seen_streams.len() >= 65_536
                        || !forward.seen_streams.insert(*connection_id)
                    {
                        return Err("TCP 连接数达到上限或连接标识重复".into());
                    }
                    forward.streams.insert(*connection_id, Flow::default());
                }
                Frame::Connected {
                    connection_id,
                    error,
                    ..
                } if !from_controller => {
                    let flow = forward
                        .streams
                        .get_mut(connection_id)
                        .ok_or("TCP 连接不存在")?;
                    if flow.connected {
                        return Err("TCP 重复连接回执".into());
                    }
                    if error.is_none() {
                        flow.connected = true;
                    } else {
                        forward.streams.remove(connection_id);
                    }
                }
                Frame::Data {
                    connection_id,
                    offset,
                    data_base64,
                    eof,
                    ..
                } => {
                    use base64::Engine as _;
                    if data_base64.len()
                        > remoteops_protocol::tcp_forward::FRAME_BYTES.div_ceil(3) * 4
                    {
                        return Err("TCP 数据帧过大".into());
                    }
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(data_base64)
                        .map_err(|_| "TCP 数据帧编码无效")?;
                    let flow = forward
                        .streams
                        .get_mut(connection_id)
                        .ok_or("TCP 连接不存在")?;
                    if !flow.connected {
                        return Err("TCP 目标尚未连接".into());
                    }
                    if from_controller {
                        &mut flow.upstream
                    } else {
                        &mut flow.downstream
                    }
                    .data(*offset, data.len(), *eof)?;
                    forward.bytes = forward
                        .bytes
                        .checked_add(data.len() as u64)
                        .filter(|bytes| *bytes <= TOTAL_BYTES)
                        .ok_or("TCP 累计流量达到上限")?;
                }
                Frame::Window {
                    connection_id,
                    received,
                    ..
                } => {
                    let flow = forward
                        .streams
                        .get_mut(connection_id)
                        .ok_or("TCP 连接不存在")?;
                    if from_controller {
                        &mut flow.downstream
                    } else {
                        &mut flow.upstream
                    }
                    .acknowledge(*received)?;
                }
                Frame::Closed { connection_id, .. } => {
                    forward.streams.remove(connection_id);
                }
                _ => return Err("TCP 帧方向无效".into()),
            }
            forward.last_activity = time::Instant::now();
            Ok(())
        })();
        if let Err(reason) = result {
            close(state, forward_id, &reason);
            return;
        }
        let forward = state.tcp.forwards.get(&forward_id).expect("当前转发存在");
        let sender = if from_controller {
            state
                .agents
                .get(&forward.agent_id)
                .and_then(|a| a.sender.as_ref())
        } else {
            active_controller(state, forward.controller_id, forward.controller_generation)
                .map(|c| &c.sender)
        };
        // 传输队列有界。单流拥塞只关闭该转发，不通过慢消费者通知中断无关会话。
        if sender.is_none_or(|s| {
            s.channel
                .try_send(WireMessage::TcpForwardV2(frame))
                .is_err()
        }) {
            close(state, forward_id, "TCP 转发出站队列已满或连接断开");
        }
    }

    pub(super) fn prune_tcp_locked(&self, state: &mut RelayState) {
        let invalid = state
            .tcp
            .forwards
            .iter()
            .filter(|(_, f)| {
                !self.tcp_authorized(state, f) || f.last_activity.elapsed() >= IDLE_TIMEOUT
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        let changed = !invalid.is_empty();
        for id in invalid {
            close(state, id, "TCP 策略、会话、连接代次或控制授权已失效");
        }
        if changed && let Err(error) = self.persist_state_locked(state) {
            tracing::warn!(%error,"TCP 清理审计持久化失败");
        }
    }
}

pub(super) fn close(state: &mut RelayState, id: RequestId, reason: &str) {
    let Some(forward) = state.tcp.forwards.remove(&id) else {
        return;
    };
    let frame = WireMessage::TcpForwardV2(Frame::Stop {
        forward_id: id,
        reason: reason.into(),
    });
    if let Some(agent) = state
        .agents
        .get(&forward.agent_id)
        .filter(|a| a.connection_generation == forward.agent_generation)
        && let Some(sender) = &agent.sender
    {
        let _ = sender.send(frame.clone());
    }
    if let Some(controller) =
        active_controller(state, forward.controller_id, forward.controller_generation)
    {
        let _ = controller.sender.send(frame);
    }
    append_audit(
        state,
        "tcp_forward_closed",
        Some(forward.session_id.to_string()),
        true,
        "relay",
        &format!("{reason} bytes={}", forward.bytes),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use remoteops_protocol::{
        ControlBasis, ControlProof, ControllerControlMode, SessionControlState,
    };

    fn forward(
        agent_id: AgentInstanceId,
        controller_id: ControllerInstanceId,
        count: usize,
    ) -> Forward {
        let session_id = SessionId::new();
        Forward {
            session_id,
            agent_id,
            controller_id,
            agent_generation: 1,
            controller_generation: 1,
            installation_id: None,
            host: "127.0.0.1".into(),
            port: 80,
            bytes: 0,
            last_activity: time::Instant::now(),
            seen_streams: BTreeSet::new(),
            streams: (0..count)
                .map(|_| (RequestId::new(), Flow::default()))
                .collect(),
            proof: ControlProof {
                basis: ControlBasis::FullAccess,
                state: SessionControlState {
                    initialization_pending: false,
                    session_id,
                    agent_generation: 1,
                    controller_id,
                    controller_generation: 1,
                    revision: 1,
                    mode: ControllerControlMode::FullAccess,
                    source: None,
                    expires_at: None,
                },
            },
        }
    }

    #[test]
    fn relay_total_quota_rejects_only_new_connection_and_recovers_after_release() {
        let mut state = RelayState::default();
        for _ in 0..8 {
            state.tcp.forwards.insert(
                RequestId::new(),
                forward(
                    AgentInstanceId::new(),
                    ControllerInstanceId::new(),
                    MAX_CONNECTIONS,
                ),
            );
        }
        let id = RequestId::new();
        state.tcp.forwards.insert(
            id,
            forward(AgentInstanceId::new(), ControllerInstanceId::new(), 0),
        );
        assert!(state.tcp.connection_limit_reached(&state.tcp.forwards[&id]));
        Relay::route_tcp_locked(
            &mut state,
            Frame::Connect {
                forward_id: id,
                connection_id: RequestId::new(),
            },
            true,
        );
        assert_eq!(state.tcp.forwards.len(), 9);
        assert!(state.tcp.forwards[&id].streams.is_empty());
        let occupied = *state
            .tcp
            .forwards
            .iter()
            .find(|(_, entry)| !entry.streams.is_empty())
            .unwrap()
            .0;
        state
            .tcp
            .forwards
            .get_mut(&occupied)
            .unwrap()
            .streams
            .pop_first();
        assert!(!state.tcp.connection_limit_reached(&state.tcp.forwards[&id]));
    }

    #[test]
    fn relay_counts_controller_and_agent_limits_across_forwards() {
        for share_controller in [true, false] {
            let mut tcp = TcpState::default();
            let agent = AgentInstanceId::new();
            let controller = ControllerInstanceId::new();
            for _ in 0..2 {
                tcp.forwards.insert(
                    RequestId::new(),
                    forward(
                        if share_controller {
                            AgentInstanceId::new()
                        } else {
                            agent
                        },
                        if share_controller {
                            controller
                        } else {
                            ControllerInstanceId::new()
                        },
                        MAX_CONNECTIONS,
                    ),
                );
            }
            let id = RequestId::new();
            tcp.forwards.insert(id, forward(agent, controller, 0));
            assert!(tcp.connection_limit_reached(&tcp.forwards[&id]));
            tcp.forwards
                .values_mut()
                .find(|entry| !entry.streams.is_empty())
                .unwrap()
                .streams
                .pop_first();
            assert!(!tcp.connection_limit_reached(&tcp.forwards[&id]));
        }
    }

    #[test]
    fn retired_connection_frames_do_not_close_listener_or_other_connections() {
        let mut state = RelayState::default();
        let id = RequestId::new();
        let retired = RequestId::new();
        let mut entry = forward(AgentInstanceId::new(), ControllerInstanceId::new(), 1);
        let active = *entry.streams.keys().next().unwrap();
        entry.seen_streams.insert(retired);
        state.tcp.forwards.insert(id, entry);
        for frame in [
            Frame::Data {
                forward_id: id,
                connection_id: retired,
                offset: 0,
                data_base64: String::new(),
                eof: true,
            },
            Frame::Window {
                forward_id: id,
                connection_id: retired,
                received: 0,
            },
            Frame::Connected {
                forward_id: id,
                connection_id: retired,
                error: None,
            },
            Frame::Closed {
                forward_id: id,
                connection_id: retired,
                reason: "对端同时关闭".into(),
            },
        ] {
            Relay::route_tcp_locked(&mut state, frame, false);
            assert!(state.tcp.forwards.contains_key(&id), "迟到帧不能关闭监听");
            assert!(state.tcp.forwards[&id].streams.contains_key(&active));
        }
    }

    #[test]
    fn unknown_connection_data_is_still_rejected() {
        let mut state = RelayState::default();
        let id = RequestId::new();
        state.tcp.forwards.insert(
            id,
            forward(AgentInstanceId::new(), ControllerInstanceId::new(), 1),
        );
        Relay::route_tcp_locked(
            &mut state,
            Frame::Window {
                forward_id: id,
                connection_id: RequestId::new(),
                received: 0,
            },
            false,
        );
        assert!(!state.tcp.forwards.contains_key(&id));
    }
}
