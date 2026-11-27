//! 同一流的两种数据路径共用解码队列。切换只从带参数集的关键帧恢复。
use super::{RequestId, Video};
use anyhow::{Result, anyhow};
use remoteops_protocol::desktop::{Input, lan};
use std::{
    net::SocketAddrV4,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::{interval, timeout},
};

/// MiniRTC 重组 NALU / FU-A 时会在完整 Annex B 访问单元前保留一字节传输头。
pub(super) fn normalize_rtc_frame(frame: &mut Vec<u8>) -> bool {
    fn annex_b(bytes: &[u8]) -> bool {
        bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1])
    }
    if annex_b(frame) { return true; }
    if frame.split_first().is_some_and(|(header, body)| matches!(header & 0x9f, 1 | 28) && annex_b(body)) {
        frame.remove(0);
        return true;
    }
    false
}

pub(super) fn is_idr(bytes: &[u8]) -> bool {
    bytes
        .windows(4)
        .any(|nal| nal[..3] == [0, 0, 1] && nal[3] & 31 == 5)
}

pub(super) struct Pictures {
    video: Video,
    stream: RequestId,
    source: u8,
    route: Arc<AtomicU8>,
    last: Option<u8>,
    need_idr: bool,
}
impl Pictures {
    pub(super) fn new(video: Video, stream: RequestId, direct: bool, route: Arc<AtomicU8>) -> Self {
        Self {
            video,
            stream,
            source: u8::from(direct),
            route,
            last: None,
            need_idr: true,
        }
    }
    pub(super) fn mini_rtc(video: Video, stream: RequestId, route: Arc<AtomicU8>) -> Self {
        Self { source: 3, ..Self::new(video,stream,false,route) }
    }
    pub(super) async fn put(&mut self, packet: Vec<u8>) -> Result<()> {
        let capture = remoteops_protocol::desktop::fps::capture_fps(&packet);
        let mut counted = false;
        let mut pending = Some(packet);
        loop {
            {
                let mut buffer = self.video.lock().map_err(|_| anyhow!("媒体队列不可用"))?;
                let generation = self.route.load(Ordering::Acquire);
                if buffer.stream != Some(self.stream) || (match generation { 1 => 1, 3 => 3, _ => 0 }) != self.source {
                    return Ok(());
                }
                if self.last != Some(generation) {
                    self.last = Some(generation);
                    self.need_idr = true;
                    buffer.packets.clear();
                }
                if !counted {
                    counted = true;
                    buffer.received = buffer.received.saturating_add(1);
                    if let Some(fps) = capture { buffer.capture = Some((fps, std::time::Instant::now())); }
                }
                if self.need_idr {
                    if !is_idr(pending.as_ref().unwrap()) {
                        return Ok(());
                    }
                    self.need_idr = false;
                }
                if buffer.packets.len() < 4 {
                    buffer.packets.extend(pending.take());
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PeerState {
    Relay,
    Direct,
    Fallback,
    Failed(String),
}

pub(super) struct Peer {
    pub keys: mpsc::Sender<(u64, Input)>,
    pub state: watch::Receiver<PeerState>,
    pub route: Arc<AtomicU8>,
    state_tx: watch::Sender<PeerState>,
    input: Option<mpsc::Receiver<(u64, Input)>>,
    tasks: JoinSet<()>,
}
impl Peer {
    pub(super) fn resume_tcp_after_rtc(&self) {
        // 即使此前从 LAN 回过 TCP，也要让解码器等这一轮 TCP 的首个关键帧。
        let _ = self.route.compare_exchange(3, 4, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn new() -> Self {
        let (keys, input) = mpsc::channel(64);
        let (state_tx, state) = watch::channel(PeerState::Relay);
        Self {
            keys,
            state,
            route: Arc::new(AtomicU8::new(0)),
            state_tx,
            input: Some(input),
            tasks: JoinSet::new(),
        }
    }
    pub fn connect(
        &mut self,
        address: SocketAddrV4,
        id: lan::Identity,
        cert: Vec<u8>,
        listener: Option<tokio::net::TcpListener>,
        video: Video,
        stream: RequestId,
        report: impl Fn(&str) + Send + Sync + 'static,
    ) {
        let Some(mut keys) = self.input.take() else {
            return;
        };
        let route = self.route.clone();
        let states = self.state_tx.clone();
        self.tasks.spawn(async move {
            report(&format!("正在向 {address} 发起 TCP 连接"));
            let reverse=listener.map(|listener|(listener,id.for_reverse()));
            let forward=lan::connect_observed(address, id, cert.clone(), |local| {
                report(&format!("TCP {local} → {address} 已连接，开始 TLS 认证"));
            }).await;
            let connection=match (forward,reverse) {
                (Err(error),Some((listener,identity))) if matches!(error.kind(),std::io::ErrorKind::TimedOut | std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NetworkUnreachable | std::io::ErrorKind::HostUnreachable) => {
                    report("正向 TCP 未接通，等待已配对 Agent 反向连接");
                    lan::accept_with_deadline(listener,*address.ip(),identity,cert,Duration::from_secs(10),|_|report("已收到反向 TCP，正在认证")).await
                }
                (other,_)=>other,
            };
            let socket=match connection {
                Ok(socket) => socket,
                Err(error) => {
                    states.send_replace(PeerState::Failed(error.to_string()));
                    return;
                }
            };
            let (mut reader,mut writer)=tokio::io::split(socket);
            let mut pictures=Pictures::new(video,stream,true,route.clone());
            route.store(1,Ordering::Release);states.send_replace(PeerState::Direct);
            let receive=async {
                loop {
                    if let Some(data)=timeout(Duration::from_secs(3),lan::read_picture(&mut reader)).await?? {
                        pictures.put(data).await?;
                    }
                }
                #[allow(unreachable_code)]
                Ok::<(),anyhow::Error>(())
            };
            let send=async {
                let mut tick=interval(Duration::from_secs(1));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    let (epoch,event)=tokio::select!{
                        _=tick.tick()=>(0,Input::Release),
                        key=keys.recv()=>match key {Some(i)=>i,None=>return Ok::<(),anyhow::Error>(())},
                    };
                    timeout(Duration::from_secs(1),lan::write_input(&mut writer,epoch,event)).await??;
                }
            };
            tokio::select! {_=receive=>{},_=send=>{}}
            route.store(2,Ordering::Release);states.send_replace(PeerState::Fallback);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtc_transport_headers_are_removed_only_before_annex_b() {
        let access = vec![0, 0, 0, 1, 0x65, 3];
        for header in [None, Some(0x01), Some(0x1c), Some(0x61)] {
            let mut framed: Vec<_> = header.into_iter().chain(access.iter().copied()).collect();
            assert!(normalize_rtc_frame(&mut framed));
            assert_eq!(framed, access);
        }
        for mut invalid in [vec![], vec![0x1c, 3], vec![0xff, 0, 0, 1, 0x65]] {
            assert!(!normalize_rtc_frame(&mut invalid));
        }
    }
    #[tokio::test]
    async fn route_changes_drop_old_pictures_and_wait_for_idr() {
        let video = Video::default();
        let stream = RequestId::new();
        video.lock().unwrap().reset(Some(stream), 1);
        let route = Arc::new(AtomicU8::new(0));
        let mut relay = Pictures::new(video.clone(), stream, false, route.clone());
        let mut lan = Pictures::new(video.clone(), stream, true, route.clone());
        let idr = vec![0, 0, 1, 0x65];
        let delta = vec![0, 0, 1, 0x41];
        relay.put(idr.clone()).await.unwrap();
        route.store(1, Ordering::Release);
        relay.put(delta.clone()).await.unwrap();
        lan.put(delta.clone()).await.unwrap();
        assert!(video.lock().unwrap().packets.is_empty());
        lan.put(idr.clone()).await.unwrap();
        route.store(2, Ordering::Release);
        relay.put(delta).await.unwrap();
        assert!(video.lock().unwrap().packets.is_empty());
        relay.put(idr).await.unwrap();
        assert_eq!(video.lock().unwrap().packets.len(), 1);
    }

    #[tokio::test]
    async fn tcp_needs_a_new_keyframe_after_lan_and_rtc_both_fall_back() {
        let video = Video::default();
        let stream = RequestId::new();
        video.lock().unwrap().reset(Some(stream), 1);
        let peer = Peer::new();
        let mut tcp = Pictures::new(video.clone(), stream, false, peer.route.clone());
        let mut rtc = Pictures::mini_rtc(video.clone(), stream, peer.route.clone());
        let idr = vec![0, 0, 1, 0x65];
        let delta = vec![0, 0, 1, 0x41];
        peer.route.store(2, Ordering::Release);
        tcp.put(idr.clone()).await.unwrap();
        peer.resume_tcp_after_rtc();
        assert_eq!(peer.route.load(Ordering::Acquire), 2);
        peer.route.store(3, Ordering::Release);
        rtc.put(idr.clone()).await.unwrap();
        peer.resume_tcp_after_rtc();
        tcp.put(delta).await.unwrap();
        assert!(video.lock().unwrap().packets.is_empty());
        tcp.put(idr).await.unwrap();
        assert_eq!(video.lock().unwrap().packets.len(), 1);
    }
}
