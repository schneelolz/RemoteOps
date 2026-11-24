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
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};

const NAME: &str = "desktop.remoteops.invalid";
const TIMEOUT: Duration = Duration::from_secs(3);
/// 单次握手完成的双向桌面套接字。
pub type Socket = TlsStream<TcpStream>;

/// 本窗口专用证书及私钥，握手后立即释放配置。
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}
impl Identity {
    /// 生成新密钥。
    pub fn new() -> io::Result<Self> {
        let signed =
            rcgen::generate_simple_self_signed(vec![NAME.into()]).map_err(io::Error::other)?;
        Ok(Self {
            cert: signed.cert.der().clone(),
            key: signed.signing_key.serialize_der().into(),
        })
    }
    /// 返回经控制链交换的公有证书。
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
pub fn addresses() -> Vec<Ipv4Addr> {
    interfaces().into_iter().map(|(ip, _)| ip).collect()
}
fn same_subnet(local: Ipv4Addr, mask: Ipv4Addr, peer: Ipv4Addr) -> bool {
    peer.is_private() && (u32::from(local) ^ u32::from(peer)) & u32::from(mask) == 0
}
/// 仅绑定与已配对控制端候选地址同网段的一张本地网卡。
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
pub async fn accept(
    listener: TcpListener,
    peer: Ipv4Addr,
    id: Identity,
    cert: Vec<u8>,
) -> io::Result<Socket> {
    let expected = cert.clone();
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots(cert)?))
        .build()
        .map_err(io::Error::other)?;
    let cfg = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![id.cert], id.key.into())
        .map_err(io::Error::other)?;
    let tls = TlsAcceptor::from(Arc::new(cfg));
    timeout(Duration::from_secs(6), async move {
        loop {
            let (tcp, addr) = listener.accept().await?;
            if addr.ip() != peer {
                continue;
            }
            tcp.set_nodelay(true)?;
            if let Ok(Ok(mut socket)) = timeout(TIMEOUT, tls.accept(tcp)).await {
                if socket
                    .get_ref()
                    .1
                    .peer_certificates()
                    .and_then(|p| p.first())
                    .is_none_or(|p| p.as_ref() != expected)
                {
                    continue;
                }
                socket.write_u8(1).await?;
                socket.flush().await?;
                return Ok(socket.into());
            }
        }
    })
    .await?
}
/// 绑定共同网段的出站网卡，验证 Agent 临时证书并完成双向认证。
pub async fn connect(address: SocketAddrV4, id: Identity, cert: Vec<u8>) -> io::Result<Socket> {
    if address.port() < 1024 {
        return Err(io::Error::other("直连端口无效"));
    }
    let (ip, _) = interfaces()
        .into_iter()
        .find(|(ip, mask)| same_subnet(*ip, *mask, *address.ip()))
        .ok_or_else(|| io::Error::other("目的地址不在本机私网"))?;
    let expected = cert.clone();
    let cfg = ClientConfig::builder()
        .with_root_certificates(roots(cert)?)
        .with_client_auth_cert(vec![id.cert], id.key.into())
        .map_err(io::Error::other)?;
    timeout(TIMEOUT, async move {
        let tcp = TcpSocket::new_v4()?;
        tcp.bind(SocketAddr::from((ip, 0)))?;
        let tcp = tcp.connect(address.into()).await?;
        tcp.set_nodelay(true)?;
        let mut socket = TlsConnector::from(Arc::new(cfg))
            .connect(ServerName::try_from(NAME).unwrap(), tcp)
            .await?;
        if socket
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|p| p.first())
            .is_none_or(|p| p.as_ref() != expected)
            || socket.read_u8().await? != 1
        {
            return Err(io::Error::other("桌面端点认证失败"));
        }
        Ok(socket.into())
    })
    .await?
}

/// 直连下行零长度包是保活，其他包沿用原 H.264 访问单元。
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
