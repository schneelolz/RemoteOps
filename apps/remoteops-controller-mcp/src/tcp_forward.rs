//! MCP 本机回环监听器及每条转发的独立流生命周期。
use super::{WriteAuthorization, application_error, control_snapshot_matches};
use remoteops_application::RelayClient;
use remoteops_domain::{EventSource, RemoteOperation, RequestId, SessionId};
use remoteops_protocol::tcp_forward::{self as tcp, TcpForwardBudget, TcpForwardFrame as Frame};
use remoteops_protocol::{ControlProof, WireMessage};
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::{Mutex as StdMutex, Weak};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio::{net::TcpListener, task::JoinHandle, time::timeout};

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct ForwardStatus {
    pub forward_id: String,
    pub session_id: String,
    pub local_address: String,
    pub target_host: String,
    pub target_port: u16,
    pub status: String,
    pub active_connections: usize,
    pub transferred_bytes: u64,
    pub message: String,
}
struct Stream {
    input: mpsc::Sender<Frame>,
    connected: Option<oneshot::Sender<Result<(), String>>>,
    task: JoinHandle<()>,
}
struct Forward {
    status: ForwardStatus,
    session_id: SessionId,
    proof: ControlProof,
    budget: Arc<TcpForwardBudget>,
    listener: Option<JoinHandle<()>>,
    streams: BTreeMap<RequestId, Stream>,
    // 连接标识在本次监听内不复用，供关闭后过滤迟到帧。
    seen: BTreeSet<RequestId>,
}
#[derive(Default)]
struct State {
    active: BTreeMap<RequestId, Forward>,
    closed: std::collections::VecDeque<ForwardStatus>,
}
pub(super) struct Manager {
    client: RelayClient,
    state: StdMutex<State>,
    output: mpsc::Sender<Frame>,
    rate: Arc<TcpForwardBudget>,
    tasks: StdMutex<Vec<JoinHandle<()>>>,
}

struct PendingForwardGuard {
    manager: Weak<Manager>,
    id: RequestId,
    armed: bool,
}
impl Drop for PendingForwardGuard {
    fn drop(&mut self) {
        if self.armed
            && let Some(manager) = self.manager.upgrade()
        {
            manager.close(self.id, "TCP 创建调用已取消，已关闭监听及目标授权");
        }
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        for task in self
            .tasks
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            task.abort();
        }
        for forward in self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .values()
        {
            if let Some(task) = &forward.listener {
                task.abort();
            }
            for stream in forward.streams.values() {
                stream.task.abort();
            }
        }
    }
}
impl Manager {
    pub(super) fn new(client: RelayClient) -> Arc<Self> {
        let mut events = client.subscribe_tcp_events();
        let (output, mut receiver) = mpsc::channel(32);
        let manager = Arc::new(Self {
            client: client.clone(),
            state: StdMutex::default(),
            output,
            rate: Arc::default(),
            tasks: StdMutex::default(),
        });
        let weak = Arc::downgrade(&manager);
        let writer = client.clone();
        let send_task = tokio::spawn(async move {
            while let Some(frame) = receiver.recv().await {
                let id = frame.forward_id();
                if writer.send_tcp_frame(frame).await.is_err()
                    && let Some(manager) = weak.upgrade()
                {
                    manager.close(id, "Relay 发送连接断开");
                }
            }
        });
        let weak = Arc::downgrade(&manager);
        let event_task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! {
                    item = events.recv() => {
                        let Some(manager) = weak.upgrade() else { break; };
                        match item {
                            Ok(WireMessage::TcpForwardV2(frame)) => manager.handle(frame),
                            Ok(_) => {},
                            Err(_) => {
                                manager.close_all("TCP 数据事件通道中断或超出窗口");
                                break;
                            }
                        }
                    }
                    _ = tick.tick() => {
                        let snapshots = if let Some(manager) = weak.upgrade() {
                            let state = manager.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            state.active.iter().map(|(id, forward)| {
                                (*id, forward.session_id, forward.proof.state.clone())
                            }).collect::<Vec<_>>()
                        } else {
                            break;
                        };
                        for (id, session, expected) in snapshots {
                            let valid = client.is_connected()
                                && client.cached_control_state(session).await.is_some_and(|state| {
                                    !state.initialization_pending
                                        && control_snapshot_matches(&state, &expected)
                                        && state.expires_at.is_none_or(|at| at > chrono::Utc::now())
                                });
                            if !valid && let Some(manager) = weak.upgrade() {
                                manager.close(id, "会话、Relay 连接或控制授权已失效");
                            }
                        }
                    }
                }
            }
        });
        manager
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend([send_task, event_task]);
        manager
    }

    pub(super) fn statuses(&self) -> Vec<ForwardStatus> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .active
            .values()
            .map(|f| {
                let mut s = f.status.clone();
                s.active_connections = f.streams.len();
                s.transferred_bytes = f.budget.bytes();
                s
            })
            .chain(state.closed.iter().cloned())
            .collect()
    }

    pub(super) fn close(&self, id: RequestId, reason: &str) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(forward) = state.active.remove(&id) else {
            return false;
        };
        if let Some(task) = forward.listener {
            task.abort();
        }
        for stream in forward.streams.values() {
            stream.task.abort();
        }
        let mut status = forward.status;
        status.status = "closed".into();
        status.message = reason.into();
        status.active_connections = 0;
        status.transferred_bytes = forward.budget.bytes();
        state.closed.push_back(status);
        while state.closed.len() > 32 {
            state.closed.pop_front();
        }
        let _ = self.output.try_send(Frame::Stop {
            forward_id: id,
            reason: reason.into(),
        });
        true
    }
    fn close_all(&self, reason: &str) {
        let ids = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for id in ids {
            self.close(id, reason);
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) async fn create(
        self: &Arc<Self>,
        session: SessionId,
        host: String,
        port: u16,
        listener: TcpListener,
        authorization: WriteAuthorization,
    ) -> Result<ForwardStatus, String> {
        if !self.client.supports_tcp_forward() {
            return Err("Relay 不支持授权 TCP 转发 v2，请更新 Relay".into());
        }
        if self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .len()
            >= tcp::MAX_FORWARDS
        {
            return Err("MCP 活动转发已达到八条上限".into());
        }
        let local_address = listener
            .local_addr()
            .map_err(|_| "本机监听地址读取失败")?
            .to_string();
        let proof = authorization.proof.clone().ok_or("TCP 创建缺少授权依据")?;
        let pending = self
            .client
            .start_execute_with_control(
                &session.to_string(),
                EventSource::Ai,
                RemoteOperation::CreateTcpForward {
                    host: host.clone(),
                    port,
                    local_address: local_address.clone(),
                },
                authorization.approval_id,
                None,
                None,
                authorization.proof,
            )
            .await
            .map_err(application_error)?;
        let id = pending.request_id;
        let status = ForwardStatus {
            forward_id: id.to_string(),
            session_id: session.to_string(),
            local_address,
            target_host: host,
            target_port: port,
            status: "opening".into(),
            active_connections: 0,
            transferred_bytes: 0,
            message: "等待三端授权确认".into(),
        };
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.active.len() >= tcp::MAX_FORWARDS {
                let _ = self.output.try_send(Frame::Stop {
                    forward_id: id,
                    reason: "MCP 并发创建达到活动转发上限".into(),
                });
                return Err("MCP 活动转发已达到八条上限".into());
            }
            state.active.insert(
                id,
                Forward {
                    status: status.clone(),
                    session_id: session,
                    proof,
                    budget: Arc::default(),
                    listener: None,
                    streams: BTreeMap::new(),
                    seen: BTreeSet::new(),
                },
            );
        }
        let mut creation = PendingForwardGuard {
            manager: Arc::downgrade(self),
            id,
            armed: true,
        };
        let response = match pending.wait_for(Duration::from_secs(15)).await {
            Ok(result)
                if result.response.error_code.is_none() && result.response.exit_code == Some(0) =>
            {
                result.response
            }
            Ok(result) => {
                self.close(id, &result.response.summary);
                return Err(format!(
                    "{}：{}",
                    result
                        .response
                        .error_code
                        .unwrap_or_else(|| "tcp_forward_rejected".into()),
                    result.response.summary
                ));
            }
            Err(error) => {
                self.close(id, "TCP 创建结果未确认，已发送关闭请求");
                return Err(application_error(error));
            }
        };
        let _ = response;
        let weak = Arc::downgrade(self);
        let budget = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let f = state.active.get_mut(&id).ok_or("授权确认期间转发已关闭")?;
            f.status.status = "listening".into();
            f.status.message = format!(
                "仅回环监听，每条转发最多 {} 个并发连接，MCP 总计最多 {} 个",
                tcp::MAX_CONNECTIONS,
                tcp::MAX_ENDPOINT_CONNECTIONS
            );
            f.budget.clone()
        };
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    if let Some(manager) = weak.upgrade() {
                        manager.close(id, "本机监听接受失败");
                    }
                    break;
                };
                let Some(manager) = weak.upgrade() else {
                    break;
                };
                manager.start_stream(id, stream, budget.clone());
            }
        });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let f = state.active.get_mut(&id).ok_or_else(|| {
            task.abort();
            "监听启动期间转发已关闭".to_owned()
        })?;
        f.listener = Some(task);
        creation.armed = false;
        Ok(f.status.clone())
    }

    fn start_stream(
        self: &Arc<Self>,
        id: RequestId,
        stream: tokio::net::TcpStream,
        budget: Arc<TcpForwardBudget>,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let total_connections = state
            .active
            .values()
            .map(|forward| forward.streams.len())
            .sum::<usize>();
        let Some(forward) = state.active.get_mut(&id) else {
            return;
        };
        if forward.streams.len() >= tcp::MAX_CONNECTIONS
            || total_connections >= tcp::MAX_ENDPOINT_CONNECTIONS
        {
            forward.status.message = format!(
                "本次本地连接被拒绝，每条转发最多 {} 个连接，MCP 总计最多 {} 个",
                tcp::MAX_CONNECTIONS,
                tcp::MAX_ENDPOINT_CONNECTIONS
            );
            return;
        }
        if forward.seen.len() >= 65_536 {
            drop(state);
            self.close(id, "TCP 监听连接标识预算已耗尽");
            return;
        }
        let connection_id = RequestId::new();
        forward.seen.insert(connection_id);
        let (input, incoming) = mpsc::channel(16);
        let (ready, waiting) = oneshot::channel();
        let weak = Arc::downgrade(self);
        let output = self.output.clone();
        let rate = self.rate.clone();
        let task = tokio::spawn(async move {
            run_stream(
                weak,
                id,
                connection_id,
                stream,
                incoming,
                waiting,
                output,
                budget,
                rate,
            )
            .await;
        });
        forward.streams.insert(
            connection_id,
            Stream {
                input,
                connected: Some(ready),
                task,
            },
        );
    }

    fn handle(&self, frame: Frame) {
        let id = frame.forward_id();
        if let Frame::Stop { reason, .. } = &frame {
            self.close(id, reason);
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(forward) = state.active.get_mut(&id) else {
            return;
        };
        if let Some(connection) = frame.connection_id()
            && forward.seen.contains(&connection)
            && !forward.streams.contains_key(&connection)
        {
            return;
        }
        match frame {
            Frame::Connected {
                connection_id,
                error,
                ..
            } => {
                if let Some(stream) = forward.streams.get_mut(&connection_id)
                    && let Some(ready) = stream.connected.take()
                {
                    let _ = ready.send(error.map_or(Ok(()), Err));
                }
            }
            Frame::Data { connection_id, .. } | Frame::Window { connection_id, .. } => {
                let Some(stream) = forward.streams.get(&connection_id) else {
                    drop(state);
                    self.close(id, "TCP 收到未知连接的数据");
                    return;
                };
                if stream.input.try_send(frame).is_err() {
                    // 消费任务可能刚结束并等待状态锁，或单连接已超出帧信用。
                    let stream = forward.streams.remove(&connection_id).expect("连接已核对");
                    stream.task.abort();
                    forward.status.message = "TCP 单连接接收已结束或队列超限，监听继续运行".into();
                    if self
                        .output
                        .try_send(Frame::Closed {
                            forward_id: id,
                            connection_id,
                            reason: forward.status.message.clone(),
                        })
                        .is_err()
                    {
                        drop(state);
                        self.close(id, "TCP 单连接关闭通知无法发送，传输队列已断开或拥塞");
                    }
                }
            }
            Frame::Closed {
                connection_id,
                reason,
                ..
            } => {
                if let Some(stream) = forward.streams.remove(&connection_id) {
                    stream.task.abort();
                }
                forward.status.message = reason;
            }
            _ => {
                drop(state);
                self.close(id, "TCP 帧方向无效");
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_stream(
    manager: Weak<Manager>,
    id: RequestId,
    connection: RequestId,
    stream: tokio::net::TcpStream,
    incoming: mpsc::Receiver<Frame>,
    waiting: oneshot::Receiver<Result<(), String>>,
    output: mpsc::Sender<Frame>,
    budget: Arc<TcpForwardBudget>,
    rate: Arc<TcpForwardBudget>,
) {
    let result = async {
        output
            .send(Frame::Connect {
                forward_id: id,
                connection_id: connection,
            })
            .await
            .map_err(|_| "TCP 连接发送失败".to_owned())?;
        timeout(Duration::from_secs(15), waiting)
            .await
            .map_err(|_| "TCP 目标连接确认超时".to_owned())?
            .map_err(|_| "TCP 连接已取消".to_owned())??;
        tcp::bridge(
            stream,
            id,
            connection,
            incoming,
            output.clone(),
            budget.clone(),
            rate,
        )
        .await
    }
    .await;
    if let Some(manager) = manager.upgrade() {
        if budget.exceeded() {
            manager.close(id, "TCP 累计流量达到 512 MiB 上限");
            return;
        }
        if let Some(forward) = manager
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .get_mut(&id)
        {
            forward.streams.remove(&connection);
            forward.status.message = result
                .as_ref()
                .err()
                .cloned()
                .unwrap_or_else(|| "TCP 单连接双向已结束，监听继续接受连接".into());
        }
    }
    let _ = output
        .send(Frame::Closed {
            forward_id: id,
            connection_id: connection,
            reason: result.err().unwrap_or_else(|| "TCP 双向已结束".into()),
        })
        .await;
}
