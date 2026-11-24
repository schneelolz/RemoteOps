//! 桌面媒体只从 Agent 流向已配对的人工面板，输入始终随控制连接路由。
use super::{Relay, RelayState, Sender, active_controller};
use anyhow::{Result, bail};
use chrono::Utc;
use remoteops_domain::{
    AgentInstanceId, ControllerInstanceId, ControllerOwnerId, PermissionMode, RequestId, SessionId,
};
use remoteops_protocol::{
    ControllerKind, WireMessage,
    desktop::{Action, DesktopEvent, LEASE, Ticket, read_video, write_video},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, watch},
    time::{Duration, Instant},
};

pub(super) struct Stream {
    id: RequestId,
    agent: AgentInstanceId,
    agent_generation: u64,
    controller: ControllerInstanceId,
    controller_generation: u64,
    owner: ControllerOwnerId,
    binding_token: String,
    agent_sender: Sender,
    controller_sender: Sender,
    agent_ticket: Option<Ticket>,
    viewer_ticket: Option<Ticket>,
    opened: Instant,
    agent_seen: Instant,
    controller_seen: Instant,
    input_epoch: Option<u64>,
    geometry_epoch: Option<u64>,
    lan_capable: bool,
    clipboard_supported: bool,
    clipboard_channel: Option<RequestId>,
    lan_requested: bool,
    lan_offered: bool,
    // 有界数据队列与独立取消信号，慢视频消费者不会阻塞急停。
    video: mpsc::Sender<Vec<u8>>,
    receiver: Option<mpsc::Receiver<Vec<u8>>>,
    cancel: watch::Sender<bool>,
}

fn reply(sender: &Sender, session: SessionId, stream: RequestId, action: Action) {
    let _ = sender.send(WireMessage::Desktop(DesktopEvent {
        session,
        stream,
        action,
    }));
}

fn set_takeover(state: &mut RelayState, session: SessionId, enabled: bool) {
    let Some(b) = state.session_bindings.get_mut(&session) else {
        return;
    };
    if b.desktop_takeover == enabled {
        return;
    }
    b.desktop_takeover = enabled;
    if let Some(s) = state.desktops.get(&session) {
        for binding in b.all() {
            let update = super::controller_binding(
                session,
                binding,
                b.permission_mode_for(binding.controller_kind),
            );
            let _ = s.agent_sender.send(WireMessage::ControllerBinding(update));
        }
    }
    super::append_audit(
        state,
        if enabled {
            "desktop_control_requested"
        } else {
            "desktop_control_released"
        },
        None,
        true,
        "human_desktop",
        if enabled {
            "人工申请接管，等待现场确认"
        } else {
            "桌面输入租约已释放"
        },
    );
    super::send_session_connection_update(state, session, None);
}

pub(super) fn release_input(state: &mut RelayState, session: SessionId) {
    set_takeover(state, session, false);
    if let Some(s) = state.desktops.get_mut(&session) {
        s.input_epoch = None;
        s.clipboard_channel = None;
        reply(
            &s.agent_sender,
            session,
            s.id,
            Action::Control {
                enabled: false,
                epoch: 0,
            },
        );
    }
}

pub(super) fn close(state: &mut RelayState, session: SessionId) {
    set_takeover(state, session, false);
    if let Some(s) = state.desktops.remove(&session) {
        super::append_audit(
            state,
            "desktop_closed",
            None,
            true,
            "desktop",
            "桌面流已关闭",
        );
        s.cancel.send_replace(true);
        reply(&s.agent_sender, session, s.id, Action::Closed);
        reply(&s.controller_sender, session, s.id, Action::Closed);
    }
}

impl Relay {
    fn desktop_valid(&self, state: &RelayState, session: SessionId, s: &Stream) -> bool {
        let Some(c) = active_controller(state, s.controller, s.controller_generation) else {
            return false;
        };
        c.kind == ControllerKind::Human
            && c.owner_id == s.owner
            && c.sessions.contains(&session)
            && self.managed_request_allowed(state, session, s.controller, s.controller_generation)
            && state
                .session_bindings
                .get(&session)
                .and_then(|b| b.human.as_ref())
                .is_some_and(|b| {
                    b.binding_token == s.binding_token
                        && b.owner_id == s.owner
                        && b.controller_id == s.controller
                        && b.controller_generation == s.controller_generation
                })
            && state.agents.get(&s.agent).is_some_and(|a| {
                a.ready
                    && a.sender.is_some()
                    && a.lease.is_valid_at(Utc::now())
                    && a.session_id == session
                    && a.connection_generation == s.agent_generation
            })
            && s.agent_seen.elapsed() < LEASE
            && s.controller_seen.elapsed() < LEASE
    }

    pub(super) fn prune_desktops(&self, state: &mut RelayState) {
        let stale: Vec<_> = state
            .desktops
            .iter()
            .filter_map(|(id, s)| (!self.desktop_valid(state, *id, s)).then_some(*id))
            .collect();
        for id in stale {
            close(state, id);
        }
    }

    fn can_input(state: &RelayState, session: SessionId) -> bool {
        state.session_bindings.get(&session).is_some_and(|b| {
            b.desktop_takeover
                && b.permission_mode_for(ControllerKind::Human) != PermissionMode::ReadOnly
        })
    }

    pub(super) async fn desktop_controller(
        &self,
        id: ControllerInstanceId,
        generation: u64,
        event: DesktopEvent,
        sender: &Sender,
    ) {
        let mut state = self.state.lock().await;
        let session = event.session;
        if event.action == Action::Open {
            let grant = (|| {
                let c = active_controller(&state, id, generation)?;
                if c.kind != ControllerKind::Human
                    || !c.sessions.contains(&session)
                    || state.desktops.len() >= 8
                    || state.desktops.contains_key(&session)
                    || !self.managed_request_allowed(&state, session, id, generation)
                {
                    return None;
                }
                let b = state.session_bindings.get(&session)?.human.as_ref()?;
                if b.controller_id != id
                    || b.controller_generation != generation
                    || b.owner_id != c.owner_id
                {
                    return None;
                }
                let a = state.agents.values().find(|a| {
                    a.session_id == session
                        && a.ready
                        && a.lease.is_valid_at(Utc::now())
                        && a.hello
                            .environment
                            .tools
                            .iter()
                            .any(|t| t.name == "remoteops-desktop-v1" && t.available)
                })?;
                Some((
                    c.owner_id,
                    b.binding_token.clone(),
                    a.hello.agent_instance_id,
                    a.connection_generation,
                    a.sender.clone()?,
                ))
            })();
            let Some((owner, binding_token, agent, agent_generation, agent_sender)) = grant else {
                reply(sender, session, event.stream, Action::Closed);
                return;
            };
            super::append_audit(
                &mut state,
                "desktop_opened",
                None,
                true,
                "human_desktop",
                "已向现场申请观看",
            );
            let (video, receiver) = mpsc::channel(2);
            let agent_ticket = Ticket::issue();
            let viewer_ticket = Ticket::issue();
            let now = Instant::now();
            state.desktops.insert(
                session,
                Stream {
                    id: event.stream,
                    binding_token,
                    agent,
                    agent_generation,
                    controller: id,
                    controller_generation: generation,
                    owner,
                    agent_sender: agent_sender.clone(),
                    controller_sender: sender.clone(),
                    agent_ticket: Some(agent_ticket.clone()),
                    viewer_ticket: Some(viewer_ticket.clone()),
                    opened: now,
                    agent_seen: now,
                    controller_seen: now,
                    input_epoch: None,
                    geometry_epoch: None,
                    lan_capable: false,
                    clipboard_supported: false,
                    clipboard_channel: None,
                    lan_requested: false,
                    lan_offered: false,
                    video,
                    receiver: Some(receiver),
                    cancel: watch::channel(false).0,
                },
            );
            reply(
                &agent_sender,
                session,
                event.stream,
                Action::Attach(agent_ticket),
            );
            reply(sender, session, event.stream, Action::Attach(viewer_ticket));
            return;
        }
        let Some(s) = state.desktops.get(&session) else {
            return;
        };
        if s.controller != id || s.controller_generation != generation || s.id != event.stream {
            return;
        }
        if !self.desktop_valid(&state, session, s) || event.action == Action::Closed {
            close(&mut state, session);
            return;
        }
        if let Action::Control { enabled, epoch } = event.action {
            if enabled
                && (s.geometry_epoch != Some(epoch)
                    || state.session_bindings[&session].permission_mode_for(ControllerKind::Human)
                        == PermissionMode::ReadOnly
                    || state.in_flight.values().any(|r| {
                        r.session_id == session && r.controller_kind == ControllerKind::Ai
                    }))
            {
                reply(
                    sender,
                    session,
                    s.id,
                    Action::Control {
                        enabled: false,
                        epoch: s.geometry_epoch.unwrap_or(0),
                    },
                );
                return;
            }
            set_takeover(&mut state, session, enabled);
        }
        let can_input = Self::can_input(&state, session);
        let s = state.desktops.get_mut(&session).expect("已校验桌面流");
        match &event.action {
            Action::Heartbeat => s.controller_seen = Instant::now(),
            Action::LanRequest { addresses, certificate }
                if s.lan_capable && !s.lan_requested && (1..=8).contains(&addresses.len())
                && addresses.iter().all(std::net::Ipv4Addr::is_private)
                && (128..=2048).contains(&certificate.len()) => s.lan_requested = true,
            Action::Settings {
                quality,
                fps,
                bitrate,
            } if *fps > 0
                && u32::from(*fps) <= quality.limits().2
                && (100_000..=12_000_000).contains(bitrate) => {}
            Action::Control { enabled, .. } if !enabled || can_input => {
                s.clipboard_channel = None;
                s.input_epoch = None;
            }
            Action::ClipboardShare {epoch,channel,enabled} if can_input && s.clipboard_supported && s.input_epoch==Some(*epoch) => {
                if *enabled { s.clipboard_channel=Some(*channel); }
                else if s.clipboard_channel==Some(*channel) { s.clipboard_channel=None; } else {return;}
            }
            Action::ClipboardData {epoch,channel,packet} if can_input && s.input_epoch==Some(*epoch) && s.clipboard_channel==Some(*channel) && packet.bounded() => {}
            Action::Input { epoch, .. } if can_input && s.input_epoch == Some(*epoch) => {}
            _ => return,
        }
        let _ = s.agent_sender.send(WireMessage::Desktop(event));
    }

    pub(super) async fn desktop_agent(
        &self,
        id: AgentInstanceId,
        generation: u64,
        event: DesktopEvent,
    ) {
        let mut state = self.state.lock().await;
        let session = event.session;
        let Some(s) = state.desktops.get(&session) else {
            return;
        };
        if s.id != event.stream || s.agent != id || s.agent_generation != generation {
            return;
        }
        if !self.desktop_valid(&state, session, s) || event.action == Action::Closed {
            close(&mut state, session);
            return;
        }
        if matches!(
            event.action,
            Action::Geometry { .. } | Action::Control { enabled: false, .. }
        ) {
            set_takeover(&mut state, session, false);
        }
        let can_input = Self::can_input(&state, session);
        let s = state.desktops.get_mut(&session).expect("已校验桌面流");
        match &event.action {
            Action::Heartbeat => s.agent_seen = Instant::now(),
            Action::LanOffer { address, certificate }
                if s.lan_requested && !s.lan_offered && address.ip().is_private()
                && address.port() >= 1024 && (128..=2048).contains(&certificate.len()) => s.lan_offered = true,
            Action::ClipboardData {epoch,channel,packet} if can_input && s.input_epoch==Some(*epoch) && s.clipboard_channel==Some(*channel) && packet.bounded() => {}
            Action::Geometry { epoch, lan, clipboard, .. } => {
                s.clipboard_supported=*clipboard;
                s.clipboard_channel=None;
                s.lan_capable = *lan;
                s.geometry_epoch = Some(*epoch);
                s.input_epoch = None;
            }
            Action::Control { enabled, epoch } if !enabled || can_input => {
                s.geometry_epoch = Some(*epoch);
                s.clipboard_channel=None;
                s.input_epoch = enabled.then_some(*epoch);
                if *enabled && s.lan_offered {
                    reply(&s.agent_sender, session, s.id, Action::LanPermit { epoch: *epoch });
                }
            }
            _ => return,
        }
        let _ = s.controller_sender.send(WireMessage::Desktop(event));
    }

    pub(super) async fn attach_desktop_media<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
        &self,
        ticket: Ticket,
        mut reader: R,
        mut writer: W,
    ) -> Result<()> {
        let (session, stream, mut cancel, video, receiver) = {
            let mut state = self.state.lock().await;
            let Some((&session, s)) = state.desktops.iter().find(|(_, s)| {
                s.agent_ticket.as_ref() == Some(&ticket)
                    || s.viewer_ticket.as_ref() == Some(&ticket)
            }) else {
                bail!("无效桌面票据")
            };
            if !self.desktop_valid(&state, session, s)
                || s.opened.elapsed() > Duration::from_secs(10)
            {
                bail!("桌面票据已过期")
            }
            let s = state.desktops.get_mut(&session).expect("已校验桌面票据");
            let receiver = if s.agent_ticket.as_ref() == Some(&ticket) {
                s.agent_ticket.take();
                None
            } else {
                s.viewer_ticket.take();
                s.receiver.take()
            };
            (
                session,
                s.id,
                s.cancel.subscribe(),
                s.video.clone(),
                receiver,
            )
        };
        let transfer = async {
            if let Some(mut receiver) = receiver {
                while let Some(data) = receiver.recv().await {
                    write_video(&mut writer, &data).await?;
                }
            } else {
                loop {
                    let data = read_video(&mut reader).await?;
                    if video.send(data).await.is_err() {
                        break;
                    }
                }
            }
            Ok::<(), std::io::Error>(())
        };
        tokio::select! {biased; _=cancel.changed()=>{}, _=transfer=>{}}
        let mut state = self.state.lock().await;
        if state.desktops.get(&session).is_some_and(|s| s.id == stream) {
            close(&mut state, session);
        }
        Ok(())
    }
}
