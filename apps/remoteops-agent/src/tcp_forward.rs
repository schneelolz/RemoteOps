//! Agent 持续 TCP v2。创建、连接和数据均绑定同一份授权快照。
use super::{LocalPermissionPolicy, verify_authorized_request};
use chrono::Utc;
use remoteops_domain::{
    AgentInstanceId, ControllerInstanceId, PermissionMode, RemoteOperation, RequestId,
};
use remoteops_policy::DefaultPolicy;
use remoteops_protocol::tcp_forward::{
    self as tcp, TcpForwardBudget, TcpForwardFrame as Frame, TcpForwardRule,
};
use remoteops_protocol::{AuthorizedRemoteRequest, ControllerBinding, WireMessage};
use std::sync::{Mutex as StdMutex, Weak};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{net::TcpStream, sync::mpsc, task::JoinHandle, time::timeout};

struct Stream {
    input: mpsc::Sender<Frame>,
    task: JoinHandle<()>,
}
struct Forward {
    authorized: AuthorizedRemoteRequest,
    rules: Vec<TcpForwardRule>,
    budget: Arc<TcpForwardBudget>,
    streams: BTreeMap<RequestId, Stream>,
    seen: BTreeSet<RequestId>,
    last_activity: Instant,
}
pub(super) struct Manager {
    forwards: StdMutex<BTreeMap<RequestId, Forward>>,
    output: mpsc::Sender<Frame>,
    sender: mpsc::UnboundedSender<WireMessage>,
    process_rate: Arc<TcpForwardBudget>,
    policy: DefaultPolicy,
    agent_id: AgentInstanceId,
    enabled: bool,
}

impl Drop for Manager {
    fn drop(&mut self) {
        for forward in self
            .forwards
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            for stream in forward.streams.values() {
                stream.task.abort();
            }
        }
    }
}

impl Manager {
    pub(super) fn new(
        sender: mpsc::UnboundedSender<WireMessage>,
        output: mpsc::Sender<Frame>,
        agent_id: AgentInstanceId,
        enabled: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            forwards: StdMutex::default(),
            output,
            sender,
            process_rate: Arc::default(),
            policy: DefaultPolicy::default(),
            agent_id,
            enabled,
        })
    }

    pub(super) fn register(
        &self,
        authorized: AuthorizedRemoteRequest,
        mut rules: Vec<TcpForwardRule>,
    ) -> Result<(), String> {
        let RemoteOperation::CreateTcpForward { host, port, .. } = &authorized.request.operation
        else {
            return Err("TCP 转发操作无效".into());
        };
        tcp::validate_target(host, *port)?;
        if !self.enabled {
            return Err("现场产品未启用 TCP 转发能力".into());
        }
        rules.retain(|rule| {
            rule.allows(
                host,
                *port,
                self.agent_id,
                rule.installation_id.as_deref(),
                Utc::now(),
            )
        });
        if rules.is_empty() {
            return Err("Agent 未收到固定目标批准规则".into());
        }
        let mut forwards = self
            .forwards
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if forwards.len() >= tcp::MAX_FORWARDS
            || forwards.contains_key(&authorized.request.request_id)
        {
            return Err("Agent 转发达到上限或标识重复".into());
        }
        forwards.insert(
            authorized.request.request_id,
            Forward {
                authorized,
                rules,
                budget: Arc::default(),
                streams: BTreeMap::new(),
                seen: BTreeSet::new(),
                last_activity: Instant::now(),
            },
        );
        Ok(())
    }

    pub(super) fn prune(
        &self,
        local: &LocalPermissionPolicy,
        bindings: &BTreeMap<ControllerInstanceId, ControllerBinding>,
    ) {
        let mut forwards = self
            .forwards
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ids = forwards
            .iter()
            .filter(|(_, f)| {
                f.last_activity.elapsed() >= tcp::IDLE_TIMEOUT
                    || local.permission_control.permission_mode() == PermissionMode::ReadOnly
                    || !f
                        .rules
                        .iter()
                        .any(|rule| rule.expires_at.is_none_or(|at| at > Utc::now()))
                    || verify_authorized_request(
                        &self.policy,
                        local,
                        bindings,
                        f.authorized.clone(),
                    )
                    .is_err()
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in ids {
            self.close_locked(&mut forwards, id, "Agent 控制授权、绑定或空闲时限已失效");
        }
    }

    fn close_locked(
        &self,
        forwards: &mut BTreeMap<RequestId, Forward>,
        id: RequestId,
        reason: &str,
    ) {
        if let Some(forward) = forwards.remove(&id) {
            for stream in forward.streams.values() {
                stream.task.abort();
            }
            let _ = self.sender.send(WireMessage::TcpForwardV2(Frame::Stop {
                forward_id: id,
                reason: reason.into(),
            }));
        }
    }

    pub(super) fn handle(
        self: &Arc<Self>,
        frame: Frame,
        local: &LocalPermissionPolicy,
        bindings: &BTreeMap<ControllerInstanceId, ControllerBinding>,
    ) {
        self.prune(local, bindings);
        let forward_id = frame.forward_id();
        let mut forwards = self
            .forwards
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let total_connections = forwards
            .values()
            .map(|forward| forward.streams.len())
            .sum::<usize>();
        let Some(forward) = forwards.get_mut(&forward_id) else {
            return;
        };
        if !matches!(frame, Frame::Connect { .. })
            && let Some(connection) = frame.connection_id()
            && forward.seen.contains(&connection)
            && !forward.streams.contains_key(&connection)
        {
            return;
        }
        forward.last_activity = Instant::now();
        match frame {
            Frame::Connect { connection_id, .. } => {
                if forward.streams.len() >= tcp::MAX_CONNECTIONS
                    || total_connections >= tcp::MAX_ENDPOINT_CONNECTIONS
                    || forward.seen.len() >= 65_536
                    || !forward.seen.insert(connection_id)
                {
                    let _ = self
                        .sender
                        .send(WireMessage::TcpForwardV2(Frame::Connected {
                            forward_id,
                            connection_id,
                            error: Some(format!("Agent TCP 每条转发最多 {} 个连接，进程总计最多 {} 个，或连接标识重复", tcp::MAX_CONNECTIONS, tcp::MAX_ENDPOINT_CONNECTIONS)),
                        }));
                    return;
                }
                let (input, receiver) = mpsc::channel(16);
                let RemoteOperation::CreateTcpForward { host, port, .. } =
                    &forward.authorized.request.operation
                else {
                    return;
                };
                let host = host.clone();
                let port = *port;
                let rules = forward.rules.clone();
                let budget = forward.budget.clone();
                let output = self.output.clone();
                let rate = self.process_rate.clone();
                let manager = Arc::downgrade(self);
                let task = tokio::spawn(async move {
                    run_stream(
                        manager,
                        forward_id,
                        connection_id,
                        host,
                        port,
                        rules,
                        receiver,
                        output,
                        budget,
                        rate,
                    )
                    .await;
                });
                forward
                    .streams
                    .insert(connection_id, Stream { input, task });
            }
            Frame::Data { connection_id, .. } | Frame::Window { connection_id, .. } => {
                let Some(stream) = forward.streams.get(&connection_id) else {
                    self.close_locked(&mut forwards, forward_id, "Agent TCP 收到未知连接的数据");
                    return;
                };
                if stream.input.try_send(frame).is_err() {
                    let stream = forward.streams.remove(&connection_id).expect("连接已核对");
                    stream.task.abort();
                    let _ = self.sender.send(WireMessage::TcpForwardV2(Frame::Closed {
                        forward_id,
                        connection_id,
                        reason: "Agent TCP 单连接接收已结束或队列超限".into(),
                    }));
                }
            }
            Frame::Closed { connection_id, .. } => {
                if let Some(stream) = forward.streams.remove(&connection_id) {
                    stream.task.abort();
                }
            }
            Frame::Stop { .. } => {
                self.close_locked(&mut forwards, forward_id, "控制端已关闭 TCP 转发");
            }
            Frame::Connected { .. } => {
                self.close_locked(&mut forwards, forward_id, "TCP 帧方向无效");
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_stream(
    manager: Weak<Manager>,
    forward_id: RequestId,
    connection_id: RequestId,
    host: String,
    port: u16,
    rules: Vec<TcpForwardRule>,
    receiver: mpsc::Receiver<Frame>,
    output: mpsc::Sender<Frame>,
    budget: Arc<TcpForwardBudget>,
    rate: Arc<TcpForwardBudget>,
) {
    let result = timeout(Duration::from_secs(10), connect_target(&host, port, &rules))
        .await
        .unwrap_or_else(|_| Err("Agent TCP 解析或连接超时".into()));
    let error = result.as_ref().err().cloned();
    if output
        .send(Frame::Connected {
            forward_id,
            connection_id,
            error,
        })
        .await
        .is_err()
    {
        return;
    }
    if let Ok(stream) = result {
        let result = tcp::bridge(
            stream,
            forward_id,
            connection_id,
            receiver,
            output.clone(),
            budget.clone(),
            rate,
        )
        .await;
        if let Some(manager) = manager.upgrade() {
            let mut forwards = manager
                .forwards
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if budget.exceeded() {
                manager.close_locked(&mut forwards, forward_id, "TCP 累计流量达到上限");
                return;
            }
            if let Some(forward) = forwards.get_mut(&forward_id) {
                forward.streams.remove(&connection_id);
            }
        }
        let _ = output
            .send(Frame::Closed {
                forward_id,
                connection_id,
                reason: result.err().unwrap_or_else(|| "TCP 双向已结束".into()),
            })
            .await;
    } else if let Some(manager) = manager.upgrade()
        && let Some(forward) = manager
            .forwards
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&forward_id)
    {
        forward.streams.remove(&connection_id);
    }
}

async fn connect_target(
    host: &str,
    port: u16,
    rules: &[TcpForwardRule],
) -> Result<TcpStream, String> {
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| "TCP 目标 DNS 解析失败")?
        .collect::<Vec<_>>();
    if addresses.is_empty()
        || addresses.len() > 64
        || addresses.iter().any(|address| {
            !rules.iter().any(|rule| {
                rule.port == port
                    && rule.expires_at.is_none_or(|at| at > Utc::now())
                    && rule.allows_resolved(host, address.ip())
            })
        })
    {
        return Err("TCP 目标解析结果不符合已批准规则或属于禁止地址".into());
    }
    for address in addresses {
        if let Ok(stream) = TcpStream::connect(address).await {
            return Ok(stream);
        }
    }
    Err("Agent 无法连接已批准的 TCP 目标".into())
}
