//! 配对后的可选私网数据通道。控制链始终负责续租、权限和断开。
use remoteops_protocol::desktop::{Action, Input, LEASE, lan, write_video};
use std::{
    io,
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot},
    time::{interval, timeout},
};

pub(super) fn permits_input(
    route: u64,
    controlling: bool,
    current: u64,
    permit: Option<u64>,
    event_epoch: u64,
) -> bool {
    route == 1 && controlling && current != 0 && permit == Some(current) && event_epoch == current
}

async fn negotiate(
    request: oneshot::Receiver<Action>,
    notify: impl Fn(Action),
    report: &impl Fn(&str),
) -> io::Result<Option<lan::Socket>> {
    let Ok(Action::LanRequest {
        addresses,
        certificate,
        reverse,
    }) = request.await
    else {
        return Ok(None);
    };
    let (listener, peer) = lan::listen(&addresses).await?;
    let SocketAddr::V4(address) = listener.local_addr()? else {
        return Err(io::Error::other("无私网端点"));
    };
    let id = lan::Identity::new().map_err(|_| io::Error::other("TLS 临时身份创建失败"))?;
    report(&format!("监听 {address}，等待控制端 {peer}"));
    let reverse = reverse
        .into_iter()
        .find(|addr| addr.ip() == &peer && addr.port() >= 1024);
    let reverse_identity = reverse.map(|_| id.for_reverse());
    notify(Action::LanOffer {
        reverse,
        address,
        certificate: id.certificate(),
    });
    let attempt = lan::accept_observed(listener, peer, id, certificate.clone(), |source| {
        report(&format!("收到控制端 {source} 的 TCP 连接，开始 TLS 认证"));
    })
    .await;
    let socket = match (attempt, reverse.zip(reverse_identity)) {
        (Ok(socket), _) => socket,
        (Err(error), Some((address, identity))) if error.kind() == io::ErrorKind::TimedOut => {
            report("正向 TCP 超时，尝试已配对控制端的反向直连");
            lan::connect(address, identity, certificate).await?
        }
        (Err(error), _) => return Err(error),
    };
    report("TLS 双向认证完成，已建立局域网直连");
    Ok(Some(socket))
}

pub(super) async fn transfer(
    relay: &mut (impl AsyncWrite + Unpin),
    video: &mut mpsc::Receiver<Vec<u8>>,
    request: oneshot::Receiver<Action>,
    keys: mpsc::Sender<(u64, Input)>,
    route: &AtomicU64,
    notify: impl Fn(Action),
    report: impl Fn(&str),
) -> io::Result<()> {
    let attempt = negotiate(request, notify, &report);
    tokio::pin!(attempt);
    let socket = loop {
        tokio::select! {
            result=&mut attempt => break match result {
                Ok(socket) => socket,
                Err(error) => {
                    report(&format!("{error}；继续使用中继"));
                    None
                }
            },
            data=video.recv() => {
                let Some(data)=data else {return Ok(());};
                timeout(LEASE,write_video(relay,&data)).await??;
            }
        }
    };
    if let Some(socket) = socket {
        route.store(1, Ordering::Release);
        let (mut reader, mut writer) = tokio::io::split(socket);
        let input = async {
            loop {
                let (epoch, event) =
                    timeout(Duration::from_secs(3), lan::read_input(&mut reader)).await??;
                if epoch != 0 {
                    keys.try_send((epoch, event))
                        .map_err(|_| io::Error::other("键鼠队列不可用"))?;
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), io::Error>(())
        };
        let output = async {
            let mut tick = interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    data=video.recv()=>{
                        let Some(data)=data else {return Ok::<(),io::Error>(());};
                        timeout(Duration::from_secs(1),write_video(&mut writer,&data)).await??;
                    }
                    _=tick.tick()=>timeout(Duration::from_secs(1),writer.write_u32(0)).await??,
                }
            }
        };
        tokio::select! { _=input=>{}, _=output=>{} }
        route.store(2, Ordering::Release);
        report("局域网直连已断开，回退到中继");
    }
    while let Some(data) = video.recv().await {
        timeout(LEASE, write_video(relay, &data)).await??;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use remoteops_protocol::desktop::read_video;
    #[test]
    fn observation_stale_epoch_and_revoked_route_refuse_keys() {
        assert!(permits_input(1, true, 3, Some(3), 3));
        assert!(!permits_input(1, false, 3, Some(3), 3));
        assert!(!permits_input(1, true, 3, None, 3));
        assert!(!permits_input(1, true, 3, Some(3), 2));
        assert!(!permits_input(2, true, 3, Some(3), 3));
    }
    #[tokio::test]
    #[ignore = "需要配置私有 IPv4 网卡"]
    async fn reverse_connection_after_forward_timeout_authenticates_and_keeps_relay() {
        let addresses = lan::addresses();
        let (viewer, peer) = lan::listen(&addresses).await.unwrap();
        let SocketAddr::V4(endpoint) = viewer.local_addr().unwrap() else {
            panic!()
        };
        let id = lan::Identity::new().unwrap();
        let (req, request) = oneshot::channel();
        let (offer, mut offers) = mpsc::unbounded_channel();
        req.send(Action::LanRequest {
            addresses: vec![peer],
            certificate: id.certificate(),
            reverse: vec![endpoint],
        })
        .unwrap();
        let (mut relay, mut output) = tokio::io::duplex(128);
        let (frames, mut input) = mpsc::channel(2);
        let (keys, mut received) = mpsc::channel(2);
        let route = std::sync::Arc::new(AtomicU64::new(0));
        let active = route.clone();
        let worker = tokio::spawn(async move {
            transfer(
                &mut relay,
                &mut input,
                request,
                keys,
                &active,
                |a| {
                    offer.send(a).unwrap();
                },
                |_| {},
            )
            .await
            .unwrap();
        });
        let Some(Action::LanOffer {
            address,
            certificate,
            reverse,
        }) = offers.recv().await
        else {
            panic!()
        };
        assert_eq!(reverse, Some(endpoint));
        frames.send(b"relay".to_vec()).await.unwrap();
        assert_eq!(read_video(&mut output).await.unwrap(), b"relay");
        let mut socket = lan::accept_with_deadline(
            viewer,
            *address.ip(),
            id,
            certificate,
            Duration::from_secs(12),
            |_| {},
        )
        .await
        .unwrap();
        while route.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
        lan::write_input(&mut socket, 7, Input::Release)
            .await
            .unwrap();
        assert_eq!(received.recv().await.unwrap(), (7, Input::Release));
        drop(socket);
        drop(frames);
        worker.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "需要配置私有 IPv4 网卡"]
    async fn absent_tcp_peer_reports_timeout_and_keeps_relay_video() {
        timeout(Duration::from_secs(10), async {
            let (mut relay, mut viewer) = tokio::io::duplex(1024);
            let (send, mut frames) = mpsc::channel(2);
            let (req, request) = oneshot::channel();
            let (keys, _input) = mpsc::channel(2);
            let route = std::sync::Arc::new(AtomicU64::new(0));
            let active_route = route.clone();
            let (offers, mut offer) = mpsc::unbounded_channel();
            let (logs, mut log) = mpsc::unbounded_channel();
            let task = tokio::spawn(async move {
                transfer(
                    &mut relay,
                    &mut frames,
                    request,
                    keys,
                    &active_route,
                    |action| {
                        offers.send(action).unwrap();
                    },
                    |text| {
                        logs.send(text.to_owned()).unwrap();
                    },
                )
                .await
                .unwrap();
            });
            req.send(Action::LanRequest {
                reverse: Vec::new(),
                addresses: lan::addresses(),
                certificate: lan::Identity::new().unwrap().certificate(),
            })
            .unwrap();
            assert!(matches!(offer.recv().await, Some(Action::LanOffer { .. })));
            assert!(log.recv().await.unwrap().starts_with("监听 "));
            send.send(b"during".to_vec()).await.unwrap();
            assert_eq!(read_video(&mut viewer).await.unwrap(), b"during");
            assert!(log.recv().await.unwrap().contains("未收到预期控制端连接"));
            assert_eq!(route.load(Ordering::Acquire), 0);
            send.send(b"after".to_vec()).await.unwrap();
            assert_eq!(read_video(&mut viewer).await.unwrap(), b"after");
            drop(send);
            task.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "需要配置私有 IPv4 网卡"]
    async fn direct_video_and_keys_revert_to_live_relay_on_disconnect() {
        timeout(Duration::from_secs(10), async {
            let (mut relay, mut viewer) = tokio::io::duplex(1024);
            let (send, mut frames) = mpsc::channel(3);
            let (req, request) = oneshot::channel();
            let (keys, mut input) = mpsc::channel(3);
            let (offers, mut offers_rx) = mpsc::unbounded_channel();
            let route = std::sync::Arc::new(AtomicU64::new(0));
            let changed = route.clone();
            let task = tokio::spawn(async move {
                transfer(
                    &mut relay,
                    &mut frames,
                    request,
                    keys,
                    &changed,
                    |v| {
                        offers.send(v).unwrap();
                    },
                    |_| {},
                )
                .await
            });
            send.send(b"before".to_vec()).await.unwrap();
            assert_eq!(read_video(&mut viewer).await.unwrap(), b"before");
            let id = lan::Identity::new().unwrap();
            req.send(Action::LanRequest {
                reverse: Vec::new(),
                addresses: lan::addresses(),
                certificate: id.certificate(),
            })
            .unwrap();
            let Some(Action::LanOffer {
                address,
                certificate,
                ..
            }) = offers_rx.recv().await
            else {
                panic!()
            };
            let mut direct = lan::connect(address, id, certificate).await.unwrap();
            while route.load(Ordering::Acquire) != 1 {
                tokio::task::yield_now().await;
            }
            send.send(b"direct".to_vec()).await.unwrap();
            loop {
                if let Some(frame) = lan::read_picture(&mut direct).await.unwrap() {
                    assert_eq!(frame, b"direct");
                    break;
                }
            }
            lan::write_input(&mut direct, 9, Input::Release)
                .await
                .unwrap();
            assert_eq!(input.recv().await.unwrap(), (9, Input::Release));
            assert!(
                timeout(Duration::from_millis(80), read_video(&mut viewer))
                    .await
                    .is_err()
            );
            drop(direct);
            while route.load(Ordering::Acquire) != 2 {
                tokio::task::yield_now().await;
            }
            send.send(b"fallback".to_vec()).await.unwrap();
            assert_eq!(read_video(&mut viewer).await.unwrap(), b"fallback");
            drop(send);
            task.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }
}
