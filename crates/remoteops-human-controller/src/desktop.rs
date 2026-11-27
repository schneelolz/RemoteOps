//! 桌面心跳和媒体独立于命令工具的执行时间。
use super::{Events, Video};
use anyhow::{Context, Result, anyhow, bail};
use remoteops_application::RelayClient;
use remoteops_domain::{RequestId, SessionId};
use remoteops_protocol::desktop::{Action, DesktopEvent, Input, LEASE, Quality, read_video};
use serde_json::{Value, json};
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{Duration, Instant, interval, timeout},
};

#[path = "desktop_direct.rs"]
mod direct;
#[path = "desktop_route.rs"]
mod route;
use route::{RoutePlan, RtcBackend};

/// 每一档都使用现有 Relay 桌面设置消息，兼容既有 Agent。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum PictureMode {
    #[default]
    Auto,
    Smooth,
    Balanced,
    High,
}

impl PictureMode {
    fn settings(self) -> Action {
        let (quality, fps, bitrate) = match self {
            Self::Auto => (Quality::High, 30, 6_000_000),
            Self::Smooth => (Quality::Low, 8, 600_000),
            Self::Balanced => (Quality::Balanced, 15, 2_000_000),
            // 与自动档共用像素和带宽上限，减低帧率，给静态文字留更多编码预算。
            Self::High => (Quality::High, 15, 6_000_000),
        };
        Action::Settings {
            quality,
            fps,
            bitrate,
        }
    }
}

pub(super) struct Desktop {
    pub session: SessionId,
    pub view: u64,
    stop: tokio::sync::oneshot::Sender<()>,
    pub commands: mpsc::Sender<Value>,
    pub picture: tokio::sync::watch::Sender<PictureMode>,
    work: JoinHandle<()>,
}
impl Desktop {
    pub fn finished(&self) -> bool {
        self.work.is_finished()
    }
    pub async fn end(self) {
        let _ = self.stop.send(());
        drop(self.commands);
        let _ = self.work.await;
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "会话身份、媒体数据与释放信号属于不同通道"
    )]
    pub fn start(
        client: RelayClient,
        session: SessionId,
        view: u64,
        initial_picture: PictureMode,
        backend: &str,
        out: Events,
        video: Video,
        mut release: tokio::sync::watch::Receiver<Option<(SessionId, u64)>>,
    ) -> Self {
        let mut routes = RoutePlan::new(backend);
        #[cfg(windows)]
        let request_rtc = routes.upgrade == Some(RtcBackend::MiniRtc);
        #[cfg(windows)]
        let request_crossdesk = routes.upgrade == Some(RtcBackend::CrossDesk);
        let (commands, rx) = mpsc::channel::<Value>(128);
        let (picture, mut picture_rx) = tokio::sync::watch::channel(initial_picture);
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let work = tokio::spawn(async move {
            let stream = RequestId::new();
            let event = |action| DesktopEvent {
                session,
                stream,
                action,
            };
            let mut events = client.desktop_events();
            let status = |state: &str, text: &str, epoch: u64| -> Result<()> {
                out.try_send(json!({"kind":"desktop","session":session,"view":view,"state":state,"epoch":epoch,"text":text}))
                    .map_err(|_| anyhow!("控制端状态队列不可用"))
            };
            let send = |action| {
                let msg = event(action);
                let client = &client;
                async move {
                    timeout(Duration::from_secs(1), client.desktop_command(msg)).await??;
                    Ok::<(), anyhow::Error>(())
                }
            };
            let mut media: Option<JoinHandle<Result<()>>> = None;
            let mut opened = false;
            let result=async {
                if !client.supports_desktop(){bail!("当前连接不支持实时桌面")}
                let mut rx=rx;
                send(Action::Open).await?;
                opened=true;
                video.lock().map_err(|_|anyhow!("媒体队列不可用"))?.reset(Some(stream),view);
                status("waiting","正在连接远程画面",0)?;
                let mut heartbeat=interval(Duration::from_secs(1));
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut last=Instant::now();
                let mut metric_window=Instant::now();
                let mut geometry_ready=false;
                let mut peer=direct::Peer::new();
                #[cfg(windows)]
                let mut rtc = None::<remoteops_desktop_media::minirtc::Peer>;
                #[cfg(windows)]
                let mut rtc_pictures = direct::Pictures::mini_rtc(video.clone(),stream,peer.route.clone());
                let mut rtc_tick=interval(Duration::from_millis(4));
                rtc_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut rtc_deadline=None::<Instant>;
                #[cfg(windows)]
                let mut rtc_receiving=false;
                #[cfg(not(windows))]
                let rtc_receiving=false;
                #[cfg(windows)]
                let mut crossdesk_route=None::<bool>;
                #[cfg(windows)]
                let stop_rtc=|| if request_crossdesk {Action::CrossDeskStop} else {Action::RtcStop};
                let mut identity=None;
                let mut listeners=std::collections::BTreeMap::new();
                let mut route_seen=direct::PeerState::Relay;
                let attempt = remoteops_protocol::desktop::lan::diagnostic_id(stream);
                let route_status=|text:&str| {
                    out.try_send(json!({"kind":"route","session":session,"view":view,"text":text}))
                       .map_err(|_|anyhow!("控制端状态队列不可用"))
                };
                route_status("中继连接")?;
                let (mut epoch, mut controlled, mut requested)=(0,false,false);
                let mut releasing=false;
                let mut clip_supported=false;
                #[cfg(windows)]
                let mut clipboard=None::<(u64,RequestId,remoteops_desktop_media::clipboard::Clipboard)>;
                let mut clip_tick=interval(Duration::from_millis(15));
                clip_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut offer_due=None::<Instant>;
                let reason=loop {
                    if geometry_ready && let Some(backend)=routes.take_upgrade() {
                        let (action,wait)=match backend {
                            RtcBackend::MiniRtc => (Action::RtcRequest,15),
                            RtcBackend::CrossDesk => (Action::CrossDeskRequest,22),
                        };
                        rtc_deadline=Some(Instant::now()+Duration::from_secs(wait));
                        send(action).await?;
                        route_status(&format!("TCP 中继 · 正在连接 {}",backend.name()))?;
                    }
                    #[cfg(windows)]
                    if !controlled || releasing || clipboard.as_ref().is_some_and(|c|c.0!=epoch){clipboard=None;}
                    tokio::select! {
                        biased;
                        _=&mut stopped=>break "本机已关闭桌面窗口或断开连接",
                        _=rtc_tick.tick(), if rtc_deadline.is_some() || rtc_receiving => {
                            #[cfg(windows)]
                            {
                                use remoteops_desktop_media::minirtc::Event;
                                let mut fallback=false;
                                if rtc.is_some() {
                                    for _ in 0..8 {
                                        match rtc.as_ref().and_then(|worker|worker.poll()) {
                                            Some(Event::Ready)=>{},
                                            Some(Event::CrossDeskRoute(relayed))=>{
                                                if !request_crossdesk {fallback=true;break;}
                                                crossdesk_route=Some(relayed);
                                            },
                                            Some(Event::Failed)=>{fallback=true; break;},
                                            Some(Event::Signal(s))=>{
                                                if request_crossdesk {fallback=true;break;}
                                                send(Action::RtcSignal(remoteops_protocol::desktop::rtc::Signal(s))).await?;
                                            },
                                            Some(Event::Video(mut frame))=>{
                                                if !direct::normalize_rtc_frame(&mut frame) {fallback=true;break;}
                                                if !rtc_receiving {
                                                    if !direct::is_idr(&frame) {continue;}
                                                    peer.route.store(3,std::sync::atomic::Ordering::Release);
                                                    rtc_receiving=true; rtc_deadline=None;
                                                    if request_crossdesk {
                                                        let Some(relayed)=crossdesk_route else {fallback=true; break;};
                                                        send(Action::CrossDeskReady).await?;
                                                        route_status(if relayed {"CrossDesk · TURN 中继"} else {"CrossDesk · 点对点直连"})?;
                                                    } else {
                                                        send(Action::RtcReady).await?;
                                                        route_status("MiniRTC · TURN/UDP 中继")?;
                                                    }
                                                }
                                                if !matches!(timeout(Duration::from_millis(100),rtc_pictures.put(frame)).await, Ok(Ok(()))) {
                                                    fallback=true;break;
                                                }
                                                if request_rtc {
                                                    rtc_deadline=Some(Instant::now()+Duration::from_secs(10));
                                                }
                                            },
                                            None=>break,
                                        }
                                    }
                                }
                                if fallback || rtc_deadline.is_some_and(|t|Instant::now()>=t){
                                    rtc=None; rtc_deadline=None; rtc_receiving=false;
                                    peer.resume_tcp_after_rtc();
                                    send(stop_rtc()).await?;
                                    route_status(if request_crossdesk {"TCP 中继 · CrossDesk 未就绪或已中断"} else {"TCP 中继 · MiniRTC 未就绪或已中断"})?;
                                }
                            }
                        }
                        _=heartbeat.tick()=>{
                            if !client.supports_desktop(){bail!("Relay 控制连接已断开")}
                            if last.elapsed()>=LEASE{bail!("Agent 桌面心跳超时")}
                            if media.as_ref().is_some_and(|t| t.is_finished()) {
                                media.take().unwrap().await.context("视频接收任务已中止")?.context("中继视频连接已关闭")?;
                                bail!("中继视频连接已结束")
                            }
                            send(Action::Heartbeat).await?;
                            let elapsed = metric_window.elapsed();
                            if elapsed >= Duration::from_millis(850) {
                                if let Ok(mut buffer) = video.lock() && buffer.stream == Some(stream) {
                                    let received = f64::from(std::mem::take(&mut buffer.received)) / elapsed.as_secs_f64();
                                    let capture = buffer.capture.filter(|(_, at)| at.elapsed() < Duration::from_secs(4)).map(|(fps, _)| fps);
                                    let _ = out.try_send(json!({"kind":"video_stats","session":session,"view":view,
                                        "receive":received,"capture":capture,"queued":buffer.packets.len()}));
                                }
                                metric_window = Instant::now();
                            }
                            if offer_due.is_some_and(|deadline|Instant::now()>deadline) {
                                offer_due=None;
                                identity=None;
                                listeners.clear();
                                routes.waiting_for_lan=false;
                                route_status(&format!("中继连接 · 未收到 Agent 直连端点 #{attempt}"))?;
                            }
                        }
                        Ok(())=release.changed()=>{
                            if *release.borrow_and_update()!=Some((session,view)) || releasing || epoch==0 {continue;}
                            controlled=false; requested=false; releasing=true;
                            send(Action::Control{epoch,enabled:false}).await?;
                            status("releasing","输入积压，正在释放键鼠并保留画面",epoch)?;
                        }
                        _=peer.state.changed()=>{
                            route_seen=peer.state.borrow_and_update().clone();
                            if matches!(route_seen,direct::PeerState::Failed(_) | direct::PeerState::Fallback) {
                                routes.waiting_for_lan=false;
                            }
                            if (controlled || requested) && matches!(route_seen, direct::PeerState::Direct | direct::PeerState::Fallback) {
                                controlled=false; requested=false; releasing=true;
                                send(Action::Control{epoch,enabled:false}).await?;
                                status("releasing","路径已切换，正在交还键鼠",epoch)?;
                            }
                            let text = match &route_seen {
                                direct::PeerState::Relay => "中继连接".to_owned(),
                                direct::PeerState::Direct => format!("局域网直连 #{attempt}"),
                                direct::PeerState::Fallback => format!("中继连接 · 直连中断后已回退 #{attempt}"),
                                direct::PeerState::Failed(reason) => format!("中继连接 · {reason} #{attempt}"),
                            };
                            route_status(&text)?;
                        }
                        msg=events.recv()=>{
                            let ev=msg.context("桌面状态已中断")?;
                            if ev.session!=session || ev.stream!=stream {continue}
                            match ev.action {
                                Action::Closed=>break "收到 Relay 的桌面关闭通知，可在 Agent 实时日志核对现场原因",
                                Action::Heartbeat=>{
                                    last=Instant::now();
                                    #[cfg(windows)]
                                    if request_crossdesk && let Some(worker)=&rtc {worker.heartbeat();}
                                },
                                Action::Attach(ticket) if media.is_none()=>{
                                    let client=client.clone(); let mut pictures=direct::Pictures::new(video.clone(),stream,false,peer.route.clone());
                                    media=Some(tokio::spawn(async move {
                                        let mut socket=timeout(LEASE,client.desktop_media(ticket)).await??;
                                        loop {
                                            let packet=read_video(&mut socket).await?;
                                            pictures.put(packet).await?;
                                        }
                                    }));
                                }
                                #[cfg(windows)]
                                Action::CrossDeskGrant(ticket) if request_crossdesk && rtc_deadline.is_some() && rtc.is_none() => {
                                    match remoteops_desktop_media::minirtc::Peer::start_crossdesk(false,&ticket) {
                                        Ok(worker)=>rtc=Some(worker),
                                        Err(_)=>{rtc_deadline=None;send(Action::CrossDeskStop).await?;route_status("TCP 中继 · CrossDesk 组件校验或启动失败")?;}
                                    }
                                }
                                #[cfg(windows)]
                                Action::CrossDeskOffer(s) if request_crossdesk && rtc.is_some() => {
                                    if !rtc.as_ref().unwrap().signal(s.0) {
                                        rtc=None;rtc_deadline=None;rtc_receiving=false;peer.resume_tcp_after_rtc();send(Action::CrossDeskStop).await?;
                                        route_status("TCP 中继 · CrossDesk 协商中断")?;
                                    }
                                }
                                #[cfg(windows)]
                                Action::RtcGranted(grant) if request_rtc && rtc_deadline.is_some() && rtc.is_none() => {
                                    match remoteops_desktop_media::minirtc::Peer::start(false,&stream.to_string(),&grant) {
                                        Ok(worker)=>rtc=Some(worker),
                                        Err(_)=>{ rtc_deadline=None; send(Action::RtcStop).await?; route_status("TCP 中继 · MiniRTC 组件校验或启动失败")?; }
                                    }
                                }
                                #[cfg(windows)]
                                Action::RtcSignal(s) if request_rtc && rtc.is_some() => {
                                    if !rtc.as_ref().unwrap().signal(s.0){
                                        rtc=None; rtc_deadline=None; rtc_receiving=false;
                                        peer.resume_tcp_after_rtc();
                                        send(Action::RtcStop).await?; route_status("TCP 中继 · MiniRTC 协商中断")?;
                                    }
                                }
                                #[cfg(windows)]
                                action if action==stop_rtc() && (rtc_deadline.is_some() || rtc_receiving) => {
                                    rtc=None; rtc_deadline=None; rtc_receiving=false;
                                    peer.resume_tcp_after_rtc();
                                    route_status(if request_crossdesk {"TCP 中继 · 对端已结束 CrossDesk"} else {"TCP 中继 · 对端已结束 MiniRTC"})?;
                                }
                                Action::LanOffer {address,certificate,reverse}=>{
                                    offer_due=None;
                                    if let Some(id)=identity.take() {
                                        let log = out.clone();
                                        let tag = attempt.clone();
                                        let listener=reverse.and_then(|key|listeners.remove(&key));
                                        listeners.clear();
                                        peer.connect(address,id,certificate,listener,video.clone(),stream,move |text| {
                                            let _ = log.try_send(json!({"kind":"route_detail","session":session,"view":view,"text":format!("私网 #{tag} · {text}")}));
                                        });
                                    }
                                }
                                Action::Geometry{epoch:next,lan,clipboard:advertised,#[cfg(windows)] mini_rtc,#[cfg(windows)] crossdesk,..}=>{
                                    clip_supported=advertised && cfg!(windows);
                                    let _=out.try_send(json!({"kind":"clipboard_support","session":session,"view":view,"available":clip_supported}));
                                    if !geometry_ready {
                                        let mode=*picture_rx.borrow_and_update();
                                        send(mode.settings()).await?;
                                        geometry_ready=true;
                                        #[cfg(windows)]
                                        let capabilities=(request_rtc && mini_rtc && remoteops_desktop_media::minirtc::available(),
                                            request_crossdesk && crossdesk && remoteops_desktop_media::minirtc::Peer::crossdesk_available());
                                        #[cfg(not(windows))]
                                        let capabilities=(false,false);
                                        let unavailable=routes.negotiate(lan,capabilities.0,capabilities.1);
                                        if routes.waiting_for_lan {
                                            let addresses=remoteops_protocol::desktop::lan::addresses();
                                            if !addresses.is_empty() && let Ok(id)=remoteops_protocol::desktop::lan::Identity::new() {
                                                for ip in &addresses {
                                                    if let Ok(socket)=tokio::net::TcpListener::bind((*ip,0)).await
                                                        && let Ok(std::net::SocketAddr::V4(endpoint))=socket.local_addr() {
                                                        listeners.insert(endpoint,socket);
                                                    }
                                                }
                                                let reverse=listeners.keys().copied().collect();
                                                send(Action::LanRequest{addresses,certificate:id.certificate(),reverse}).await?;
                                                identity=Some(id);
                                                offer_due=Some(Instant::now()+Duration::from_secs(7));
                                                route_status("TCP 中继 · 正在尝试局域网直连")?;
                                            } else {
                                                routes.waiting_for_lan=false;
                                                route_status("TCP 中继 · 本机没有可用私网端点")?;
                                            }
                                        } else if let Some(backend)=unavailable {
                                            route_status(&format!("TCP 中继 · 端点或 Relay 未提供 {}",backend.name()))?;
                                        } else if routes.upgrade.is_none() {route_status("TCP 中继")?;}
                                    }
                                    epoch=next; controlled=false; requested=false; releasing=false; status("view","观察模式",epoch)?;
                                }
                                Action::Control{enabled,epoch:next}=>{
                                    if enabled && releasing {continue;}
                                    let denied=requested && !enabled;
                                    let returned=controlled && !enabled;
                                    releasing=false;
                                    epoch=next; controlled=enabled; requested=false;
                                    let text=if enabled {"控制模式"} else if denied {"当前授权或在途任务暂不允许控制，已保持观察"} else if returned {"键鼠已释放，画面继续观察"} else {"观察模式"};
                                    status(if enabled {"control"} else {"view"},text,epoch)?;
                                }
                                #[cfg(windows)]
                                Action::ClipboardData{epoch:seen,channel,packet} if controlled && seen==epoch=>{
                                    if let Some((_,id,local))=&mut clipboard && *id==channel && let Some(data)=packet.decode() {
                                        local.receive(&data);
                                    }
                                }
                                _=>{}
                            }
                        }
                        _=clip_tick.tick(), if controlled && clip_supported=>{
                            #[cfg(windows)]
                            if let Some((_,channel,local))=&mut clipboard {
                                match local.next_packet() {
                                    Ok(Some(bytes))=>if let Some(packet)=remoteops_protocol::desktop::clipboard::Packet::encode(bytes) {
                                        send(Action::ClipboardData{epoch,channel:*channel,packet}).await?;
                                    },
                                    Err(_)=>{let id=*channel;clipboard=None;send(Action::ClipboardShare{epoch,channel:id,enabled:false}).await?;}
                                    _=>{}
                                }
                            }
                        }
                        Ok(())=picture_rx.changed(), if geometry_ready=>{
                            let mode=*picture_rx.borrow_and_update();
                            send(mode.settings()).await?;
                        }
                        cmd=rx.recv()=>{
                            let Some(cmd)=cmd else {break "本机桌面命令通道已关闭"};
                            match cmd["op"].as_str() {
                                Some("control") if epoch!=0 && !controlled && !requested && !releasing && cmd["epoch"].as_u64().is_none_or(|seen| seen==epoch)=>{
                                    send(Action::Control{epoch,enabled:true}).await?;
                                    requested=true; status("pending","正在切换至控制",epoch)?;
                                }
                                Some("release") if epoch!=0 && (controlled || requested)=>{
                                    controlled=false; requested=false; releasing=true;
                                    send(Action::Control{epoch,enabled:false}).await?;
                                    status("releasing","正在切回观察",epoch)?;
                                }
                                #[cfg(windows)]
                                Some("clipboard") if clip_supported && controlled && cmd["epoch"].as_u64()==Some(epoch)=>{
                                    if cmd["enabled"].as_bool()==Some(true) {
                                        if clipboard.is_none() && let Some(local)=remoteops_desktop_media::clipboard::Clipboard::open() {
                                            let channel=RequestId::new();
                                            send(Action::ClipboardShare{epoch,channel,enabled:true}).await?;
                                            clipboard=Some((epoch,channel,local));
                                        }
                                    }else if let Some((_,channel,_))=clipboard.take(){
                                        send(Action::ClipboardShare{epoch,channel,enabled:false}).await?;
                                    }
                                }
                                Some("input") if controlled && cmd["epoch"].as_u64()==Some(epoch)=>{
                                    let event:Input=serde_json::from_value(cmd["event"].clone())?;
                                    if *peer.state.borrow()!=route_seen {continue;}
                                    if route_seen==direct::PeerState::Direct {
                                        if peer.keys.try_send((epoch,event)).is_err() {
                                            controlled=false; releasing=true;
                                            send(Action::Control{epoch,enabled:false}).await?;
                                            status("releasing","输入已暂停，正在交还键鼠",epoch)?;
                                        }
                                    } else {send(Action::Input{epoch,event}).await?;}
                                }
                                _=>{}
                            }
                        }
                    }
                };
                Ok::<_,anyhow::Error>(reason)
            }.await;
            if let Some(task) = media {
                task.abort();
                let _ = task.await;
            }
            if let Ok(mut v) = video.lock() {
                v.reset(None, 0);
            }
            if opened {
                let _ = send(Action::Closed).await;
            }
            match result {
                Ok(reason) => {
                    let _ = status("ended", reason, 0);
                }
                Err(e) => {
                    let _ = status("error", &format!("{e:#}"), 0);
                }
            }
        });
        Self {
            session,
            view,
            commands,
            picture,
            stop,
            work,
        }
    }
}

#[cfg(test)]
mod picture_tests {
    use super::*;

    #[test]
    fn picture_modes_use_supported_wire_settings() {
        for (name, quality, fps, bitrate) in [
            ("auto", Quality::High, 30, 6_000_000),
            ("smooth", Quality::Low, 8, 600_000),
            ("balanced", Quality::Balanced, 15, 2_000_000),
            ("high", Quality::High, 15, 6_000_000),
        ] {
            let mode: PictureMode = serde_json::from_value(json!(name)).unwrap();
            assert!(
                matches!(mode.settings(), Action::Settings {quality:q, fps:f, bitrate:b}
                if (q,f,b)==(quality,fps,bitrate))
            );
        }
    }
}
