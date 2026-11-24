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

fn is_idr(bytes: &[u8]) -> bool {
    bytes
        .windows(4)
        .any(|nal| nal[..3] == [0, 0, 1] && nal[3] & 31 == 5)
}

pub(super) struct Pictures {
    video: Video,
    stream: RequestId,
    direct: bool,
    route: Arc<AtomicU8>,
    last: Option<u8>,
    need_idr: bool,
}
impl Pictures {
    pub(super) fn new(video: Video, stream: RequestId, direct: bool, route: Arc<AtomicU8>) -> Self {
        Self {
            video,
            stream,
            direct,
            route,
            last: None,
            need_idr: true,
        }
    }
    pub(super) async fn put(&mut self, packet: Vec<u8>) -> Result<()> {
        let mut pending = Some(packet);
        loop {
            {
                let mut buffer = self.video.lock().map_err(|_| anyhow!("媒体队列不可用"))?;
                let generation = self.route.load(Ordering::Acquire);
                if buffer.stream != Some(self.stream) || (generation == 1) != self.direct {
                    return Ok(());
                }
                if self.last != Some(generation) {
                    self.last = Some(generation);
                    self.need_idr = true;
                    buffer.packets.clear();
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

pub(super) struct Peer {
    pub keys: mpsc::Sender<(u64, Input)>,
    pub state: watch::Receiver<u8>,
    pub route: Arc<AtomicU8>,
    state_tx: watch::Sender<u8>,
    input: Option<mpsc::Receiver<(u64, Input)>>,
    tasks: JoinSet<()>,
}
impl Peer {
    pub fn new() -> Self {
        let (keys, input) = mpsc::channel(64);
        let (state_tx, state) = watch::channel(0);
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
        video: Video,
        stream: RequestId,
    ) {
        let Some(mut keys) = self.input.take() else {
            return;
        };
        let route = self.route.clone();
        let states = self.state_tx.clone();
        self.tasks.spawn(async move {
            let Ok(socket)=lan::connect(address,id,cert).await else {states.send_replace(3);return;};
            let (mut reader,mut writer)=tokio::io::split(socket);
            let mut pictures=Pictures::new(video,stream,true,route.clone());
            route.store(1,Ordering::Release);states.send_replace(1);
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
            route.store(2,Ordering::Release);states.send_replace(2);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
