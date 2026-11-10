//! 持续 TCP 转发 v2 的固定目标策略、流控帧和端点流状态机。
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Utc};
use remoteops_domain::{AgentInstanceId, RequestId, SessionId};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{Mutex, mpsc},
    time::{Instant, timeout},
};

/// 本版转发扩展，旧帧不能用于创建转发。
pub const TCP_FORWARD_VERSION: u16 = 2;
/// 每个端点最多八条监听。
pub const MAX_FORWARDS: usize = 8;
/// 每个监听最多十六个并发目标连接。
pub const MAX_CONNECTIONS: usize = 16;
/// 单个 MCP 或 Agent 进程最多三十二个活动目标连接。
pub const MAX_ENDPOINT_CONNECTIONS: usize = 32;
/// Relay 进程最多一百二十八个活动目标连接。
pub const MAX_RELAY_CONNECTIONS: usize = 128;
/// 每帧最大原始字节数。
pub const FRAME_BYTES: usize = 64 * 1024;
/// 每方向最大未确认字节数。
pub const WINDOW_BYTES: u64 = 256 * 1024;
/// 同一转发双向累计上限。
pub const TOTAL_BYTES: u64 = 512 * 1024 * 1024;
/// 单个端点进程 TCP 数据速率上限。
pub const BYTES_PER_SECOND: u64 = 8 * 1024 * 1024;
/// 无数据或确认进展的关闭时限。
pub const IDLE_TIMEOUT: Duration = Duration::from_mins(10);

/// 管理员批准的固定目标或 CIDR，范围还可限制到现场和安装身份。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TcpForwardRule {
    /// 供审计和撤销使用的稳定规则名。
    pub id: String,
    /// 精确主机名、IP 或 CIDR。
    pub host: String,
    /// 单个目标端口。
    pub port: u16,
    /// 可选的固定 Agent 身份。
    #[serde(default)]
    pub agent_id: Option<AgentInstanceId>,
    /// 可选的持久 MCP 安装身份。
    #[serde(default)]
    pub installation_id: Option<String>,
    /// 使用目的，供管理员复核。
    pub purpose: String,
    /// 临时授权的到期时间。
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// 持久化的独立 TCP 目标策略，修订采用比较后更新。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TcpForwardPolicy {
    /// 策略修订。
    pub revision: u64,
    /// 已批准规则，空列表拒绝所有创建。
    pub rules: Vec<TcpForwardRule>,
    /// 管理员申请入口说明。
    #[serde(default = "default_contact")]
    pub approval_contact: String,
    /// 最后一次成功保存时间。
    pub updated_at: Option<DateTime<Utc>>,
}

impl Default for TcpForwardPolicy {
    fn default() -> Self {
        Self {
            revision: 0,
            rules: Vec::new(),
            approval_contact: default_contact(),
            updated_at: None,
        }
    }
}

fn default_contact() -> String {
    "请联系本 Relay 管理员，在管理后台的 TCP 转发策略中批准目标".into()
}

impl TcpForwardPolicy {
    /// 检查完整策略，未知和无效目标不会静默扩大访问范围。
    /// # Errors
    /// 规则数量、目标或范围字段无效时拒绝整份策略。
    pub fn validate(&self) -> Result<(), String> {
        if self.rules.len() > 256
            || self.approval_contact.len() > 1024
            || self.approval_contact.contains('\0')
        {
            return Err("TCP 转发策略过大或申请说明无效".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for rule in &self.rules {
            validate_target(&rule.host, rule.port)?;
            if rule.id.is_empty()
                || rule.id.len() > 128
                || !ids.insert(&rule.id)
                || rule.purpose.trim().is_empty()
                || rule.purpose.len() > 1024
                || rule
                    .installation_id
                    .as_ref()
                    .is_some_and(|id| id.len() > 128 || id.is_empty())
            {
                return Err("TCP 转发规则名、用途或身份范围无效".into());
            }
            if rule.host.contains('/') && parse_network(&rule.host).is_none() {
                return Err("TCP 目标 CIDR 无效".into());
            }
        }
        Ok(())
    }
}

impl TcpForwardRule {
    /// 在请求创建和每次目标连接时检查目标、现场、安装身份和有效期。
    #[must_use]
    pub fn allows(
        &self,
        host: &str,
        port: u16,
        agent: AgentInstanceId,
        installation: Option<&str>,
        now: DateTime<Utc>,
    ) -> bool {
        self.port == port
            && self.agent_id.is_none_or(|id| id == agent)
            && self
                .installation_id
                .as_deref()
                .is_none_or(|id| Some(id) == installation)
            && self.expires_at.is_none_or(|at| at > now)
            && host_matches(&self.host, host)
    }

    /// DNS 解析后对实际连接地址执行检查。
    #[must_use]
    pub fn allows_resolved(&self, requested: &str, address: IpAddr) -> bool {
        !forbidden_address(address)
            && if self.host.parse::<IpAddr>().is_ok() || self.host.contains('/') {
                host_matches(&self.host, &address.to_string())
            } else {
                self.host.eq_ignore_ascii_case(requested)
            }
    }
}

/// 固定目标输入校验，拒绝端口零及主机控制字符。
/// # Errors
/// 目标主机或端口不合法时返回诊断。
pub fn validate_target(host: &str, port: u16) -> Result<(), String> {
    if host.is_empty()
        || host.len() > 253
        || host.chars().any(|c| c.is_whitespace() || c.is_control())
        || port == 0
    {
        return Err("TCP 目标主机或端口无效".into());
    }
    Ok(())
}

/// 特殊地址总是拒绝，IPv4 映射地址使用相同约束。
#[must_use]
pub fn forbidden_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_link_local()
                || ip.octets()[0] == 0
                || ip.octets()[0] >= 240
                || ip.octets() == [100, 100, 100, 200]
        }
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or_else(
            || {
                ip.is_unspecified()
                    || ip.is_multicast()
                    || ip.is_unicast_link_local()
                    || ip.segments() == [0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254]
            },
            |ip| forbidden_address(IpAddr::V4(ip)),
        ),
    }
}

fn parse_network(value: &str) -> Option<(IpAddr, u8)> {
    let (address, prefix) = value.split_once('/')?;
    let address = address.parse::<IpAddr>().ok()?;
    let prefix = prefix.parse::<u8>().ok()?;
    (prefix <= if address.is_ipv4() { 32 } else { 128 }).then_some((address, prefix))
}

fn host_matches(rule: &str, target: &str) -> bool {
    if let Some((network, prefix)) = parse_network(rule) {
        match (network, target.parse::<IpAddr>()) {
            (IpAddr::V4(network), Ok(IpAddr::V4(target))) => {
                (u32::from(network) ^ u32::from(target)).leading_zeros() >= u32::from(prefix)
            }
            (IpAddr::V6(network), Ok(IpAddr::V6(target))) => {
                (u128::from(network) ^ u128::from(target)).leading_zeros() >= u32::from(prefix)
            }
            _ => false,
        }
    } else {
        rule.eq_ignore_ascii_case(target)
    }
}

/// 同一监听内复用的流帧，每个连接有不可复用标识。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TcpForwardFrame {
    /// 为一个已接受的本地连接创建独立目标连接。
    Connect {
        forward_id: RequestId,
        connection_id: RequestId,
    },
    /// 目标连接结果，数据只能在成功后发送。
    Connected {
        forward_id: RequestId,
        connection_id: RequestId,
        error: Option<String>,
    },
    /// 有序数据，EOF 只关闭写入方向。
    Data {
        forward_id: RequestId,
        connection_id: RequestId,
        offset: u64,
        data_base64: String,
        eof: bool,
    },
    /// 对端实际写入 TCP 后回收窗口。
    Window {
        forward_id: RequestId,
        connection_id: RequestId,
        received: u64,
    },
    /// 关闭单个连接，保留监听。
    Closed {
        forward_id: RequestId,
        connection_id: RequestId,
        reason: String,
    },
    /// 关闭监听和其中全部连接。
    Stop {
        forward_id: RequestId,
        reason: String,
    },
}

impl TcpForwardFrame {
    /// 返回单连接标识，整条转发的停止帧为空。
    #[must_use]
    pub fn connection_id(&self) -> Option<RequestId> {
        match self {
            Self::Connect { connection_id, .. }
            | Self::Connected { connection_id, .. }
            | Self::Data { connection_id, .. }
            | Self::Window { connection_id, .. }
            | Self::Closed { connection_id, .. } => Some(*connection_id),
            Self::Stop { .. } => None,
        }
    }

    /// 返回固定归属。
    #[must_use]
    pub fn forward_id(&self) -> RequestId {
        match self {
            Self::Connect { forward_id, .. }
            | Self::Connected { forward_id, .. }
            | Self::Data { forward_id, .. }
            | Self::Window { forward_id, .. }
            | Self::Closed { forward_id, .. }
            | Self::Stop { forward_id, .. } => *forward_id,
        }
    }
}

/// 转发字节预算和进程级发送节流器。
#[derive(Default)]
pub struct TcpForwardBudget {
    bytes: AtomicU64,
    exceeded: AtomicBool,
    // 同一进程的各转发共享节流时钟。
    next_send: Mutex<Option<Instant>>,
}

impl TcpForwardBudget {
    /// 记录双向累计字节，超限后停止整个转发。
    /// # Errors
    /// 双向累计字节达到上限时返回错误并锁定预算。
    pub fn account(&self, size: usize) -> Result<(), String> {
        let size = size as u64;
        self.bytes
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
                old.checked_add(size).filter(|total| *total <= TOTAL_BYTES)
            })
            .map(|_| ())
            .map_err(|_| {
                self.exceeded.store(true, Ordering::SeqCst);
                "TCP 转发累计流量达到 512 MiB 上限".into()
            })
    }
    /// 当前已处理字节数。
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::SeqCst)
    }
    /// 预算一旦耗尽，本次监听不能通过重连复用。
    #[must_use]
    pub fn exceeded(&self) -> bool {
        self.exceeded.load(Ordering::SeqCst) || self.bytes() >= TOTAL_BYTES
    }
    /// 将单个端点进程的 TCP 数据发送速率限制到 8 MiB/s。
    pub async fn pace(&self, size: usize) {
        let mut next = self.next_send.lock().await;
        let at = next.unwrap_or_else(Instant::now).max(Instant::now());
        let nanos = u64::try_from(size)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_000_000_000)
            / BYTES_PER_SECOND;
        *next = Some(at + Duration::from_nanos(nanos));
        drop(next);
        tokio::time::sleep_until(at).await;
    }
}

/// 严格的单方向滑动窗口，重复和跨窗口确认均被拒绝。
#[derive(Clone, Debug, Default)]
pub struct TcpForwardWindow {
    sent: u64,
    acknowledged: u64,
    eof: bool,
    // 短数据帧同样消耗信用，保证每流入站队列的帧数有界。
    pending_frames: VecDeque<u64>,
}
impl TcpForwardWindow {
    /// 未确认的原始字节数。
    #[must_use]
    pub fn remaining(&self) -> usize {
        if self.pending_frames.len() >= 4 {
            return 0;
        }
        usize::try_from(WINDOW_BYTES - (self.sent - self.acknowledged)).unwrap_or(0)
    }
    /// 接受有序有界数据。
    /// # Errors
    /// 帧超限、重复、越过窗口或发生在 EOF 后时拒绝。
    pub fn data(&mut self, offset: u64, size: usize, eof: bool) -> Result<(), String> {
        if self.eof
            || offset != self.sent
            || size > FRAME_BYTES
            || size > self.remaining()
            || (size == 0 && !eof)
        {
            return Err("TCP 数据次序、窗口或 EOF 无效".into());
        }
        self.sent += size as u64;
        if size > 0 {
            self.pending_frames.push_back(self.sent);
        }
        self.eof = eof;
        Ok(())
    }
    /// 只接受对已发送数据的累计确认。
    /// # Errors
    /// 确认倒退或超过已发送字节时拒绝。
    pub fn acknowledge(&mut self, received: u64) -> Result<(), String> {
        if received < self.acknowledged || received > self.sent {
            return Err("TCP 窗口确认超出已发送范围".into());
        }
        self.acknowledged = received;
        while self
            .pending_frames
            .front()
            .is_some_and(|offset| *offset <= received)
        {
            self.pending_frames.pop_front();
        }
        Ok(())
    }
}

/// 有界双向桥接，保留半关闭并等待对端剩余数据，发送信用由实际写入确认回收。
/// # Errors
/// 流量、帧、空闲时限或 TCP/传输连接异常时返回错误并释放套接字。
pub async fn bridge(
    stream: TcpStream,
    forward_id: RequestId,
    connection_id: RequestId,
    mut incoming: mpsc::Receiver<TcpForwardFrame>,
    outgoing: mpsc::Sender<TcpForwardFrame>,
    budget: Arc<TcpForwardBudget>,
    process_rate: Arc<TcpForwardBudget>,
) -> Result<(), String> {
    let (mut reader, mut writer) = stream.into_split();
    let mut window = TcpForwardWindow::default();
    let mut received = 0_u64;
    let mut local_eof = false;
    let mut remote_eof = false;
    let mut buffer = vec![0; FRAME_BYTES];
    let mut deadline = Instant::now() + IDLE_TIMEOUT;
    loop {
        if local_eof && remote_eof && window.sent == window.acknowledged {
            return Ok(());
        }
        let capacity = FRAME_BYTES.min(window.remaining());
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {
                return Err("TCP 连接空闲超时".into());
            }
            result = reader.read(&mut buffer[..capacity]), if !local_eof && capacity > 0 => {
                let size = result.map_err(|_| "本地 TCP 读取失败")?;
                let offset = window.sent;
                window.data(offset, size, size == 0)?;
                budget.account(size)?;
                process_rate.pace(size).await;
                local_eof = size == 0;
                outgoing.send(TcpForwardFrame::Data {
                    forward_id,
                    connection_id,
                    offset,
                    data_base64: BASE64.encode(&buffer[..size]),
                    eof: local_eof,
                }).await.map_err(|_| "TCP 传输通道已关闭")?;
                deadline = Instant::now() + IDLE_TIMEOUT;
            }
            item = incoming.recv() => match item {
                Some(TcpForwardFrame::Data { offset, data_base64, eof, .. }) => {
                    if data_base64.len() > FRAME_BYTES.div_ceil(3) * 4 {
                        return Err("TCP 编码数据帧过大".into());
                    }
                    let data = BASE64.decode(data_base64).map_err(|_| "TCP 数据编码无效")?;
                    if remote_eof || offset != received || data.len() > FRAME_BYTES
                        || (data.is_empty() && !eof) {
                        return Err("TCP 入站数据次序无效".into());
                    }
                    budget.account(data.len())?;
                    timeout(IDLE_TIMEOUT, writer.write_all(&data)).await
                        .map_err(|_| "TCP 写入空闲超时")?
                        .map_err(|_| "TCP 写入失败")?;
                    received += data.len() as u64;
                    if eof {
                        writer.shutdown().await.map_err(|_| "TCP 半关闭失败")?;
                        remote_eof = true;
                    }
                    if !data.is_empty() {
                        outgoing.send(TcpForwardFrame::Window {
                            forward_id,
                            connection_id,
                            received,
                        }).await.map_err(|_| "TCP 确认通道关闭")?;
                    }
                    deadline = Instant::now() + IDLE_TIMEOUT;
                }
                Some(TcpForwardFrame::Window { received, .. }) => {
                    let previous = window.acknowledged;
                    window.acknowledge(received)?;
                    if received > previous {
                        deadline = Instant::now() + IDLE_TIMEOUT;
                    }
                }
                Some(TcpForwardFrame::Closed { .. } | TcpForwardFrame::Stop { .. }) | None => {
                    return Ok(());
                }
                _ => return Err("TCP 流收到了无效状态帧".into()),
            }
        }
    }
}

/// 控制端可见的策略快照，只包含当前绑定可申请的规则。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TcpForwardPolicySnapshot {
    /// 当前会话。
    pub session_id: SessionId,
    /// 申请时使用的现场身份。
    pub agent_id: AgentInstanceId,
    /// 申请时使用的持久安装身份，旧共享令牌模式为空。
    pub requester_installation_id: Option<String>,
    /// 策略内容。
    pub policy: TcpForwardPolicy,
    /// 需要提交给管理员的请求字段说明。
    pub application_fields: Vec<String>,
    /// 更新策略后立即作用于后续连接，并清理撤销目标。
    pub hot_reload: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_frames_stop_reading_until_written_frames_are_acknowledged() {
        let mut window = TcpForwardWindow::default();
        for offset in 0..4 {
            window.data(offset, 1, false).unwrap();
        }
        assert_eq!(window.remaining(), 0);
        assert!(window.data(4, 1, false).is_err());
        window.acknowledge(1).unwrap();
        assert!(window.remaining() > 0);
        window.data(4, 1, false).unwrap();
        assert_eq!(window.remaining(), 0);
        window.acknowledge(5).unwrap();
        assert_eq!(window.remaining(), usize::try_from(WINDOW_BYTES).unwrap());
    }

    #[tokio::test]
    async fn bridge_pauses_small_writes_until_frame_credit_returns() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let stream = listener.accept().await.unwrap().0;
        let (input, incoming) = mpsc::channel(16);
        let (outgoing, mut output) = mpsc::channel(16);
        let forward = RequestId::new();
        let connection = RequestId::new();
        let task = tokio::spawn(bridge(
            stream,
            forward,
            connection,
            incoming,
            outgoing,
            Arc::default(),
            Arc::default(),
        ));
        for offset in 0..4 {
            client.write_all(&[1]).await.unwrap();
            let frame = timeout(Duration::from_secs(1), output.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(frame, TcpForwardFrame::Data { offset: actual, .. } if actual == offset)
            );
        }
        client.write_all(&[2]).await.unwrap();
        assert!(
            timeout(Duration::from_millis(100), output.recv())
                .await
                .is_err()
        );
        input
            .send(TcpForwardFrame::Window {
                forward_id: forward,
                connection_id: connection,
                received: 1,
            })
            .await
            .unwrap();
        let frame = timeout(Duration::from_secs(1), output.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(frame, TcpForwardFrame::Data { offset: 4, .. }));
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn bridge_preserves_large_half_closed_request_and_slow_response() {
        let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(first.local_addr().unwrap())
            .await
            .unwrap();
        let (bridge_a, _) = first.accept().await.unwrap();
        let second = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bridge_b = TcpStream::connect(second.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server, _) = second.accept().await.unwrap();
        let (a_tx, a_rx) = mpsc::channel(16);
        let (b_tx, b_rx) = mpsc::channel(16);
        let forward = RequestId::new();
        let connection = RequestId::new();
        let a = tokio::spawn(bridge(
            bridge_a,
            forward,
            connection,
            b_rx,
            a_tx,
            Arc::default(),
            Arc::default(),
        ));
        let b = tokio::spawn(bridge(
            bridge_b,
            forward,
            connection,
            a_rx,
            b_tx,
            Arc::default(),
            Arc::default(),
        ));
        let expected = vec![0x71; 2 * 1024 * 1024];
        let response = vec![0x93; 3 * 1024 * 1024];
        let received = expected.clone();
        let sent = response.clone();
        let endpoint = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut request = Vec::new();
            server.read_to_end(&mut request).await.unwrap();
            assert_eq!(request, received);
            server.write_all(&sent).await.unwrap();
            server.shutdown().await.unwrap();
        });
        timeout(Duration::from_secs(15), async {
            client.write_all(&expected).await.unwrap();
            client.shutdown().await.unwrap();
            let mut actual = Vec::new();
            client.read_to_end(&mut actual).await.unwrap();
            assert_eq!(actual, response);
            endpoint.await.unwrap();
            a.await.unwrap().unwrap();
            b.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }
    #[test]
    fn window_enforces_credit_and_eof() {
        let mut w = TcpForwardWindow::default();
        for n in 0..4 {
            w.data(n * FRAME_BYTES as u64, FRAME_BYTES, false).unwrap();
        }
        assert_eq!(w.remaining(), 0);
        assert!(w.data(WINDOW_BYTES, 1, false).is_err());
        assert!(w.acknowledge(WINDOW_BYTES + 1).is_err());
        w.acknowledge(FRAME_BYTES as u64).unwrap();
        w.data(WINDOW_BYTES, 1, true).unwrap();
        assert!(w.data(WINDOW_BYTES + 1, 1, false).is_err());
    }
    #[test]
    fn rules_bind_scope_and_resolved_addresses() {
        let agent = AgentInstanceId::new();
        let r = TcpForwardRule {
            id: "lan-web".into(),
            host: "192.168.10.0/24".into(),
            port: 80,
            agent_id: Some(agent),
            installation_id: Some("installation".into()),
            purpose: "诊断".into(),
            expires_at: None,
        };
        assert!(r.allows(
            "192.168.10.254",
            80,
            agent,
            Some("installation"),
            Utc::now()
        ));
        assert!(!r.allows(
            "192.168.10.254",
            80,
            AgentInstanceId::new(),
            Some("installation"),
            Utc::now()
        ));
        assert!(!r.allows_resolved("192.168.10.254", "169.254.169.254".parse().unwrap()));
        assert!(!r.allows_resolved("192.168.10.254", "192.168.11.2".parse().unwrap()));
        assert!(forbidden_address("::ffff:169.254.169.254".parse().unwrap()));
        assert!(forbidden_address("fd00:ec2::254".parse().unwrap()));
    }
}
