//! 桌面心跳和媒体独立于命令工具的执行时间。
use super::{Events,Video};
use anyhow::{Result,anyhow,bail,Context};
use remoteops_application::RelayClient;
use remoteops_domain::{RequestId,SessionId};
use remoteops_protocol::desktop::{Action,DesktopEvent,Input,LEASE,Quality,read_video};
use serde_json::{Value,json};
use tokio::{sync::mpsc,task::JoinHandle,time::{Duration,Instant,interval,timeout}};

#[path = "desktop_direct.rs"]
mod direct;

pub(super) struct Desktop {
    pub session:SessionId,
    pub view:u64,
    stop:tokio::sync::oneshot::Sender<()>,
    pub commands:mpsc::Sender<Value>,
    work:JoinHandle<()>,
}
impl Desktop {
    pub fn finished(&self)->bool { self.work.is_finished() }
    pub async fn end(self) {
        let _=self.stop.send(());
        drop(self.commands);
        let _=self.work.await;
    }

    pub fn start(client:RelayClient,session:SessionId,view:u64,out:Events,video:Video,mut release:tokio::sync::watch::Receiver<Option<(SessionId,u64)>>)->Self {
        let (commands,rx)=mpsc::channel::<Value>(128);
        let (stop,mut stopped)=tokio::sync::oneshot::channel();
        let work=tokio::spawn(async move {
            let stream=RequestId::new();
            let event=|action| DesktopEvent{session,stream,action};
            let mut events=client.desktop_events();
            let status=|state:&str,text:&str,epoch:u64| -> Result<()> {
                out.try_send(json!({"kind":"desktop","session":session,"view":view,"state":state,"epoch":epoch,"text":text}))
                    .map_err(|_| anyhow!("控制端状态队列不可用"))
            };
            let send=|action| {let msg=event(action); let client=&client; async move {
                timeout(Duration::from_secs(1),client.desktop_command(msg)).await??;
                Ok::<(),anyhow::Error>(())
            }};
            let mut media:Option<JoinHandle<Result<()>>>=None;
            let mut opened=false;
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
                let mut quality_set=false;
                let mut peer=direct::Peer::new();
                let mut identity=None;
                let mut route_seen=0;
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
                    #[cfg(windows)]
                    if !controlled || releasing || clipboard.as_ref().is_some_and(|c|c.0!=epoch){clipboard=None;}
                    tokio::select! {
                        biased;
                        _=&mut stopped=>break "本机已关闭桌面窗口或断开连接",
                        _=heartbeat.tick()=>{
                            if !client.supports_desktop(){bail!("Relay 控制连接已断开")}
                            if last.elapsed()>=LEASE{bail!("Agent 桌面心跳超时")}
                            if media.as_ref().is_some_and(|t| t.is_finished()) {
                                media.take().unwrap().await.context("视频接收任务已中止")?.context("中继视频连接已关闭")?;
                                bail!("中继视频连接已结束")
                            }
                            send(Action::Heartbeat).await?;
                            if offer_due.is_some_and(|deadline|Instant::now()>deadline) {
                                offer_due=None;
                                route_status("中继连接 · 尚未收到 Agent 直连端点")?;
                            }
                        }
                        Ok(())=release.changed()=>{
                            if *release.borrow_and_update()!=Some((session,view)) || releasing || epoch==0 {continue;}
                            controlled=false; requested=false; releasing=true;
                            send(Action::Control{epoch,enabled:false}).await?;
                            status("releasing","输入积压，正在释放键鼠并保留画面",epoch)?;
                        }
                        _=peer.state.changed()=>{
                            route_seen=*peer.state.borrow_and_update();
                            if controlled || requested {
                                controlled=false; requested=false; releasing=true;
                                send(Action::Control{epoch,enabled:false}).await?;
                                status("releasing","路径已切换，正在交还键鼠",epoch)?;
                            }
                            route_status(match route_seen {1=>"局域网直连",2=>"中继连接 · 直连中断后已回退",3=>"中继连接 · 私网 TCP 或 TLS 握手失败",_=>"中继连接"})?;
                        }
                        msg=events.recv()=>{
                            let ev=msg.context("桌面状态已中断")?;
                            if ev.session!=session || ev.stream!=stream {continue}
                            match ev.action {
                                Action::Closed=>break "收到 Relay 的桌面关闭通知，可在 Agent 实时日志核对现场原因",
                                Action::Heartbeat=>last=Instant::now(),
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
                                Action::LanOffer {address,certificate}=>{
                                    offer_due=None;
                                    if let Some(id)=identity.take(){peer.connect(address,id,certificate,video.clone(),stream);}
                                }
                                Action::Geometry{epoch:next,lan,clipboard:advertised,..}=>{
                                    clip_supported=advertised && cfg!(windows);
                                    let _=out.try_send(json!({"kind":"clipboard_support","session":session,"view":view,"available":clip_supported}));
                                    if !quality_set {
                                        send(Action::Settings{quality:Quality::High,fps:30,bitrate:6_000_000}).await?;
                                        quality_set=true;
                                        if lan {
                                            let addresses=remoteops_protocol::desktop::lan::addresses();
                                            if !addresses.is_empty() && let Ok(id)=remoteops_protocol::desktop::lan::Identity::new() {
                                                send(Action::LanRequest{addresses,certificate:id.certificate()}).await?;
                                                identity=Some(id);
                                                offer_due=Some(Instant::now()+Duration::from_secs(7));
                                            } else {route_status("中继连接 · 本机没有可用私网端点")?;}
                                        } else {route_status("中继连接 · 对端未通告直连能力")?;}
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
                                    if route_seen==1 {
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
            if let Some(task)=media {task.abort(); let _=task.await;}
            if let Ok(mut v)=video.lock(){v.reset(None,0);}
            if opened {let _=send(Action::Closed).await;}
            match result {
                Ok(reason)=>{let _=status("ended",reason,0);}
                Err(e)=>{let _=status("error",&format!("{e:#}"),0);}
            }
        });
        Self{session,view,commands,stop,work}
    }
}
