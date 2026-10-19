//! 仅在专属回环测试中丢弃一次已成功消费邀请的响应，不记录任何帧或凭据。
use anyhow::{Result, ensure};
use remoteops_protocol::{
    WireMessage, accept_tls, connect_tls, load_client_config, load_server_config, read_frame,
    write_frame,
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    net::TcpListener,
    task::{JoinHandle, JoinSet},
};

pub(super) struct PairResultProxy {
    pub address: String,
    pub dropped: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}
impl Drop for PairResultProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) async fn start(address: &str, cert: &Path) -> Result<PairResultProxy> {
    let upstream: std::net::SocketAddr = address.parse()?;
    ensure!(
        upstream.ip().is_loopback(),
        "故障注入代理只允许连接专属回环Relay"
    );
    let server_config = load_server_config(cert, cert.with_file_name("key.pem"))?;
    let client_config = load_client_config(cert)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let proxy_address = listener.local_addr()?.to_string();
    let dropped = Arc::new(AtomicUsize::new(0));
    let count = dropped.clone();
    let address = address.to_owned();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                incoming = listener.accept() => {
                    let Ok((socket, peer)) = incoming else { break };
                    if !peer.ip().is_loopback() { continue; }
                    let server_config = server_config.clone();
                    let client_config = client_config.clone();
                    let count = count.clone();
                    let address = address.clone();
                    connections.spawn(async move {
                        let client = accept_tls(socket, &server_config).await?;
                        let relay = connect_tls(&address, "localhost", client_config).await?;
                        let (mut client_reader, mut client_writer) = tokio::io::split(client);
                        let (mut relay_reader, mut relay_writer) = tokio::io::split(relay);
                        let forward_requests = async {
                            loop {
                                let message: WireMessage = read_frame(&mut client_reader).await?;
                                write_frame(&mut relay_writer, &message).await?;
                            }
                            #[allow(unreachable_code)]
                            Ok::<(), anyhow::Error>(())
                        };
                        let forward_responses = async {
                            loop {
                                let message: WireMessage = read_frame(&mut relay_reader).await?;
                                if matches!(&message, WireMessage::PairResult(result) if result.connection.is_some() && result.connection_resume_token.is_some())
                                    && count.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                                    // Relay 已提交消费，但调用方没有见到连接或恢复令牌。
                                    return Ok::<(), anyhow::Error>(());
                                }
                                write_frame(&mut client_writer, &message).await?;
                            }
                        };
                        tokio::select! {
                            result = forward_requests => result,
                            result = forward_responses => result,
                        }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {},
            }
        }
    });
    Ok(PairResultProxy {
        address: proxy_address,
        dropped,
        task,
    })
}
