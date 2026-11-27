//! 已配对人工桌面的一次性双向 TLS 连接。每次开窗生成独立密钥，仅交换公有证书。
use super::Input;
use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName},
    server::WebPkiClientVerifier,
};
use std::{
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream},
    time::{Instant, timeout_at},
};
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};

const NAME: &str = "desktop.remoteops.invalid";
const TIMEOUT: Duration = Duration::from_secs(3);
/// 单次握手完成的双向桌面套接字。
pub type Socket = TlsStream<TcpStream>;

/// 返回单次开窗的短诊断编号，供双方关联日志。
#[must_use]
pub fn diagnostic_id(stream: remoteops_domain::RequestId) -> String {
    format!("{:08x}", stream.as_uuid().as_fields().0)
}

/// 本窗口专用证书及私钥，握手后立即释放配置。
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}
impl Identity {
    /// 在同一流的两种握手方向复用已通过控制链核对的身份。
    #[must_use]
    pub fn for_reverse(&self) -> Self {
        Self {
            cert: self.cert.clone(),
            key: self.key.clone_key(),
        }
    }
    /// 生成新密钥。
    ///
    /// # Errors
    /// 生成自签名证书或密钥失败时返回错误。
    pub fn new() -> io::Result<Self> {
        let signed =
            rcgen::generate_simple_self_signed(vec![NAME.into()]).map_err(io::Error::other)?;
        Ok(Self {
            cert: signed.cert.der().clone(),
            key: signed.signing_key.serialize_der().into(),
        })
    }
    /// 返回经控制链交换的公有证书。
    #[must_use]
    pub fn certificate(&self) -> Vec<u8> {
        self.cert.as_ref().to_vec()
    }
}
fn roots(cert: Vec<u8>) -> io::Result<RootCertStore> {
    if !(128..=2048).contains(&cert.len()) {
        return Err(io::Error::other("临时证书长度无效"));
    }
    let mut store = RootCertStore::empty();
    store
        .add(CertificateDer::from(cert))
        .map_err(io::Error::other)?;
    Ok(store)
}
fn interfaces() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    NetworkInterface::show()
        .unwrap_or_default()
        .into_iter()
        .filter(|nic| !nic.internal)
        .flat_map(|nic| nic.addr)
        .filter_map(|addr| match addr {
            Addr::V4(v) if v.ip.is_private() => v
                .netmask
                .filter(|m| u32::from(*m) != 0)
                .map(|mask| (v.ip, mask)),
            _ => None,
        })
        .take(8)
        .collect()
}
/// 当前可尝试直连的本机私网 IPv4 地址。
#[must_use]
pub fn addresses() -> Vec<Ipv4Addr> {
    interfaces().into_iter().map(|(ip, _)| ip).collect()
}
fn same_subnet(local: Ipv4Addr, mask: Ipv4Addr, peer: Ipv4Addr) -> bool {
    peer.is_private() && (u32::from(local) ^ u32::from(peer)) & u32::from(mask) == 0
}
/// 仅绑定与已配对控制端候选地址同网段的一张本地网卡。
///
/// # Errors
/// 找不到匹配的私网接口或监听端口绑定失败时返回错误。
pub async fn listen(peers: &[Ipv4Addr]) -> io::Result<(TcpListener, Ipv4Addr)> {
    let (ip, peer) = interfaces()
        .into_iter()
        .find_map(|(ip, mask)| {
            peers
                .iter()
                .find(|p| same_subnet(ip, mask, **p))
                .map(|p| (ip, *p))
        })
        .ok_or_else(|| io::Error::other("未发现共同私网"))?;
    Ok((TcpListener::bind((ip, 0)).await?, peer))
}
/// 核验来源地址与面板一次性客户端证书，只接受一个连接。
///
/// # Errors
/// 证书无效、握手失败或未能在期限内验证对端时返回错误。
pub async fn accept(
    listener: TcpListener,
    peer: Ipv4Addr,
    id: Identity,
    cert: Vec<u8>,
) -> io::Result<Socket> {
    accept_observed(listener, peer, id, cert, |_| {}).await
}

/// 对预期来源的首次 TCP 接入报告一次，认证失败仍可在原期限内重试。
///
/// # Errors
/// 与 `accept` 使用相同的证书、地址和超时检查。
pub async fn accept_observed(
    listener: TcpListener,
    peer: Ipv4Addr,
    id: Identity,
    cert: Vec<u8>,
    connected: impl FnOnce(SocketAddr),
) -> io::Result<Socket> {
    accept_with_deadline(listener, peer, id, cert, Duration::from_secs(6), connected).await
}

/// 在协商约定的期限内接入指定私网对端。
///
/// # Errors
/// 监听或证书配置失败，或在给定期限内没有通过双向验证的连接时返回错误。
pub async fn accept_with_deadline(
    listener: TcpListener,
    peer: Ipv4Addr,
    id: Identity,
    cert: Vec<u8>,
    wait: Duration,
    connected: impl FnOnce(SocketAddr),
) -> io::Result<Socket> {
    let expected = cert.clone();
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots(cert)?))
        .build()
        .map_err(|_| io::Error::other("TLS 客户端验证器初始化失败"))?;
    let cfg = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![id.cert], id.key.into())
        .map_err(|_| io::Error::other("TLS 服务端证书初始化失败"))?;
    let tls = TlsAcceptor::from(Arc::new(cfg));
    let deadline = Instant::now() + wait;
    let mut connected = Some(connected);
    let mut last_failure = io::Error::new(
        io::ErrorKind::TimedOut,
        "等待 TCP 超时，未收到预期控制端连接",
    );
    loop {
        if Instant::now() >= deadline {
            return Err(last_failure);
        }
        let Ok(incoming) = timeout_at(deadline, listener.accept()).await else {
            return Err(last_failure);
        };
        let (tcp, addr) = incoming.map_err(|e| io::Error::new(e.kind(), "接收 TCP 连接失败"))?;
        if addr.ip() != peer {
            if connected.is_some() {
                last_failure = io::Error::new(
                    io::ErrorKind::TimedOut,
                    "仅收到其他地址的 TCP 连接，未收到预期控制端连接",
                );
            }
            continue;
        }
        if let Some(report) = connected.take() {
            report(addr);
        }
        tcp.set_nodelay(true)
            .map_err(|e| io::Error::new(e.kind(), "TCP 套接字配置失败"))?;
        let handshake_deadline = deadline.min(Instant::now() + TIMEOUT);
        let mut socket = match timeout_at(handshake_deadline, tls.accept(tcp)).await {
            Err(_) => {
                last_failure = io::Error::new(io::ErrorKind::TimedOut, "TLS 握手超时");
                continue;
            }
            Ok(Err(e)) => {
                last_failure = io::Error::new(e.kind(), "TLS 握手或客户端证书验证失败");
                continue;
            }
            Ok(Ok(socket)) => socket,
        };
        if socket
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|p| p.first())
            .is_none_or(|p| p.as_ref() != expected)
        {
            last_failure = io::Error::new(
                io::ErrorKind::PermissionDenied,
                "控制端证书与本次配对不匹配",
            );
            continue;
        }
        timeout_at(deadline, async {
            socket.write_u8(1).await?;
            socket.flush().await
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS 端点确认发送超时"))?
        .map_err(|e| io::Error::new(e.kind(), "TLS 端点确认发送失败"))?;
        return Ok(socket.into());
    }
}

fn tcp_failure(kind: io::ErrorKind) -> io::Error {
    let message = match kind {
        io::ErrorKind::TimedOut => "TCP 连接超时",
        io::ErrorKind::ConnectionRefused => "TCP 连接被拒绝",
        io::ErrorKind::PermissionDenied => "本机拒绝 TCP 连接",
        io::ErrorKind::HostUnreachable | io::ErrorKind::NetworkUnreachable => {
            "TCP 目标或网络不可达"
        }
        _ => "TCP 连接失败",
    };
    io::Error::new(kind, message)
}

/// 绑定共同网段的出站网卡，验证 Agent 临时证书并完成双向认证。
///
/// # Errors
/// 地址不在可达的本机私网、TCP 连接失败或双向 TLS 认证失败时返回错误。
pub async fn connect(address: SocketAddrV4, id: Identity, cert: Vec<u8>) -> io::Result<Socket> {
    connect_observed(address, id, cert, |_| {}).await
}

/// 在 TCP 连接建立后、验证服务端证书之前报告实际本地端点。
///
/// # Errors
/// 地址、网卡绑定、TCP 连接、超时或证书校验失败时返回相应的 I/O 错误。
pub async fn connect_observed(
    address: SocketAddrV4,
    id: Identity,
    cert: Vec<u8>,
    connected: impl FnOnce(SocketAddr),
) -> io::Result<Socket> {
    if address.port() < 1024 {
        return Err(io::Error::other("直连端口无效"));
    }
    let (ip, _) = interfaces()
        .into_iter()
        .find(|(ip, mask)| same_subnet(*ip, *mask, *address.ip()))
        .ok_or_else(|| io::Error::other("目的地址不在本机私网"))?;
    let expected = cert.clone();
    let cfg = ClientConfig::builder()
        .with_root_certificates(roots(cert).map_err(|_| io::Error::other("TLS 服务端证书无效"))?)
        .with_client_auth_cert(vec![id.cert], id.key.into())
        .map_err(|_| io::Error::other("TLS 客户端证书初始化失败"))?;
    let deadline = Instant::now() + TIMEOUT;
    let tcp = TcpSocket::new_v4().map_err(|e| io::Error::new(e.kind(), "无法创建 TCP 套接字"))?;
    tcp.bind(SocketAddr::from((ip, 0)))
        .map_err(|e| io::Error::new(e.kind(), "无法绑定所选私网网卡"))?;
    let tcp = timeout_at(deadline, tcp.connect(address.into()))
        .await
        .map_err(|_| tcp_failure(io::ErrorKind::TimedOut))?
        .map_err(|e| tcp_failure(e.kind()))?;
    if let Ok(local) = tcp.local_addr() {
        connected(local);
    }
    tcp.set_nodelay(true)
        .map_err(|e| io::Error::new(e.kind(), "TCP 套接字配置失败"))?;
    let connector = TlsConnector::from(Arc::new(cfg));
    let mut socket = timeout_at(
        deadline,
        connector.connect(ServerName::try_from(NAME).map_err(io::Error::other)?, tcp),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS 握手超时"))?
    .map_err(|e| io::Error::new(e.kind(), "TLS 握手或服务端证书验证失败"))?;
    if socket
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|p| p.first())
        .is_none_or(|p| p.as_ref() != expected)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Agent 证书与本次配对不匹配",
        ));
    }
    let ack = timeout_at(deadline, socket.read_u8())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS 端点确认超时"))?
        .map_err(|e| io::Error::new(e.kind(), "TLS 认证未获 Agent 确认"))?;
    if ack != 1 {
        return Err(io::Error::other("TLS 端点确认无效"));
    }
    Ok(socket.into())
}

/// 直连下行零长度包是保活，其他包沿用原 H.264 访问单元。
///
/// # Errors
/// 读取长度或帧失败、或者对端声明的长度超过帧上限时返回错误。
pub async fn read_picture(r: &mut (impl AsyncRead + Unpin)) -> io::Result<Option<Vec<u8>>> {
    let size = r.read_u32().await? as usize;
    if size == 0 {
        return Ok(None);
    }
    if size > super::MAX_VIDEO_BYTES {
        return Err(io::Error::other("直连帧过大"));
    }
    let mut data = vec![0; size];
    r.read_exact(&mut data).await?;
    Ok(Some(data))
}

/// 直连键鼠只传输原有租约代次和有界输入事件。
///
/// # Errors
/// 序列化失败、消息超过单字节长度上限或写入失败时返回错误。
pub async fn write_input(
    w: &mut (impl AsyncWrite + Unpin),
    epoch: u64,
    event: Input,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(&(epoch, event))?;
    let size = u8::try_from(bytes.len()).map_err(io::Error::other)?;
    w.write_u8(size).await?;
    w.write_all(&bytes).await?;
    w.flush().await
}
/// 读取单个输入事件，输入通道不能传输开窗、授权或续租命令。
///
/// # Errors
/// 输入流提前结束或消息不能解析为键鼠事件时返回错误。
pub async fn read_input(r: &mut (impl AsyncRead + Unpin)) -> io::Result<(u64, Input)> {
    let size = usize::from(r.read_u8().await?);
    let mut buf = [0u8; 255];
    r.read_exact(&mut buf[..size]).await?;
    Ok(serde_json::from_slice(&buf[..size])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tcp_timeout_and_refusal_have_distinct_labels() {
        assert_eq!(
            tcp_failure(io::ErrorKind::TimedOut).to_string(),
            "TCP 连接超时"
        );
        assert_eq!(
            tcp_failure(io::ErrorKind::ConnectionRefused).to_string(),
            "TCP 连接被拒绝"
        );
    }

    #[tokio::test]
    #[ignore = "需要配置私有 IPv4 网卡"]
    async fn live_diagnostics_distinguish_refusal_from_tls_timeout() {
        let ip = addresses()[0];
        let bound = TcpSocket::new_v4().unwrap();
        bound.bind((ip, 0).into()).unwrap();
        let SocketAddr::V4(addr) = bound.local_addr().unwrap() else {
            panic!()
        };
        let cert = Identity::new().unwrap().certificate();
        let error = connect_observed(addr, Identity::new().unwrap(), cert.clone(), |_| {
            panic!("未建立 TCP")
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "TCP 连接被拒绝");
        let listener = bound.listen(1).unwrap();
        let (sent, received) = tokio::sync::oneshot::channel();
        let client = tokio::spawn(connect_observed(
            addr,
            Identity::new().unwrap(),
            cert,
            |local| {
                sent.send(local).unwrap();
            },
        ));
        let (_held_socket, source) = listener.accept().await.unwrap();
        assert_eq!(received.await.unwrap(), source);
        assert_eq!(
            client.await.unwrap().unwrap_err().to_string(),
            "TLS 握手超时"
        );
    }

    #[test]
    fn public_and_off_subnet_addresses_are_excluded() {
        let ip = Ipv4Addr::new(192, 168, 0, 2);
        let mask = Ipv4Addr::new(255, 255, 255, 0);
        assert!(same_subnet(ip, mask, Ipv4Addr::new(192, 168, 0, 3)));
        assert!(!same_subnet(ip, mask, Ipv4Addr::new(192, 168, 1, 3)));
        assert!(!same_subnet(ip, mask, Ipv4Addr::new(8, 8, 8, 8)));
    }
    #[tokio::test]
    #[ignore = "需要配置私有 IPv4 网卡"]
    async fn local_mutual_tls_transfers_video_and_input_and_rejects_other_identity() {
        let peers = addresses();
        let (l, p) = listen(&peers).await.unwrap();
        let SocketAddr::V4(addr) = l.local_addr().unwrap() else {
            panic!()
        };
        let server = Identity::new().unwrap();
        let client = Identity::new().unwrap();
        let cert = server.certificate();
        let allow = client.certificate();
        let server = tokio::spawn(async move {
            let mut s = accept(l, p, server, allow).await.unwrap();
            super::super::write_video(&mut s, b"frame").await.unwrap();
            assert_eq!(read_input(&mut s).await.unwrap(), (7, Input::Release));
        });
        assert!(
            connect(addr, Identity::new().unwrap(), cert.clone())
                .await
                .is_err()
        );
        assert!(
            connect(
                addr,
                Identity::new().unwrap(),
                Identity::new().unwrap().certificate()
            )
            .await
            .is_err()
        );
        let mut c = connect(addr, client, cert).await.unwrap();
        assert_eq!(super::super::read_video(&mut c).await.unwrap(), b"frame");
        write_input(&mut c, 7, Input::Release).await.unwrap();
        server.await.unwrap();
    }
}
