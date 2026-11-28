//! 现场桌面流绑定于已配对的 Human Controller；离开连接作用域即停止。
use remoteops_desktop_media::{Encoder, Host};
use remoteops_domain::{PermissionMode, RequestId, SessionId};
use remoteops_protocol::{
    ControllerBinding, WireMessage, connect_tls,
    desktop::{Action, DesktopEvent, Input, LEASE, MAX_VIDEO_BYTES, Quality},
    write_frame,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

#[cfg(feature = "paired-human-desktop")]
#[path = "desktop_direct.rs"]
mod direct;

#[path = "desktop_inbox.rs"]
mod inbox;
use inbox::{Command, Inbox};

pub(super) fn crossdesk_available() -> bool {
    cfg!(feature = "paired-human-desktop")
        && option_env!("REMOTEOPS_CROSSDESK_CAPTURE_SHA256").is_some_and(|s| s.len() == 64)
        && std::env::current_exe()
            .is_ok_and(|p| p.with_file_name("remoteops-crossdesk-native.dll").is_file())
        && remoteops_desktop_media::minirtc::Peer::crossdesk_available()
}

fn log_desktop(sink: Option<&super::AgentEventSink>, message: &str) {
    super::emit_agent_event(
        sink,
        super::AgentEvent::OperationLog(super::AgentOperationLog {
            repetition: None,
            request_id: None,
            occurred_at: chrono::Utc::now(),
            level: super::AgentLogLevel::Info,
            message: format!("实时桌面 · {message}"),
        }),
    );
}

#[derive(Clone)]
pub(super) struct Endpoint {
    pub address: String,
    pub name: String,
    pub tls: Arc<rustls::ClientConfig>,
}
struct Active {
    session: SessionId,
    stream: RequestId,
    binding: String,
    stop: Arc<AtomicBool>,
    input: Arc<Inbox>,
    #[cfg(feature = "paired-human-desktop")]
    lan_request: Option<tokio::sync::oneshot::Sender<Action>>,
    transport: tokio::task::JoinHandle<()>,
}
impl Drop for Active {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.transport.abort();
    }
}
pub(super) struct Manager {
    endpoint: Option<Endpoint>,
    out: mpsc::UnboundedSender<WireMessage>,
    active: Option<Active>,
    log: Option<super::AgentEventSink>,
}
impl Manager {
    pub fn new(
        endpoint: Option<Endpoint>,
        out: mpsc::UnboundedSender<WireMessage>,
        log: Option<super::AgentEventSink>,
    ) -> Self {
        Self {
            endpoint,
            out,
            active: None,
            log,
        }
    }
    pub fn stop(&mut self, reason: &str) {
        if self.active.take().is_some() {
            log_desktop(self.log.as_ref(), reason);
        }
    }
    pub fn revoke(&mut self, token: &str) {
        if self.active.as_ref().is_some_and(|s| s.binding == token) {
            self.stop("人工绑定被撤销或权限改变")
        }
    }
    pub fn handle(
        &mut self,
        event: DesktopEvent,
        binding: Option<&ControllerBinding>,
        local: PermissionMode,
    ) {
        if self
            .active
            .as_ref()
            .is_some_and(|s| s.stop.load(Ordering::Acquire))
        {
            self.active.take();
        }
        let Some(b) = binding.filter(|b| {
            b.session_id == event.session
                && b.controller_kind == remoteops_protocol::ControllerKind::Human
                && !b.binding_token.is_empty()
        }) else {
            return;
        };
        if let Action::Attach(ticket) = event.action {
            let Some(endpoint) = self.endpoint.clone() else {
                return;
            };
            if self.active.is_some() {
                return;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let input = Arc::new(Inbox::new());
            let commands = input.clone();
            let capture_log = self.log.clone();
            let transport_log = self.log.clone();
            let (frames, mut video) = mpsc::channel::<Vec<u8>>(2);
            let closing = stop.clone();
            let stream = event.stream;
            let session = event.session;
            let notify = self.out.clone();
            #[cfg(feature = "paired-human-desktop")]
            let (lan_request, request) = tokio::sync::oneshot::channel();
            #[cfg(feature = "paired-human-desktop")]
            let (lan_keys, mut direct_input) = mpsc::channel(64);
            let route = Arc::new(std::sync::atomic::AtomicU64::new(0));
            #[cfg(feature = "paired-human-desktop")]
            let route_changed = route.clone();
            let transport = tokio::spawn(async move {
                let result = async {
                    let mut conn = tokio::time::timeout(
                        LEASE,
                        connect_tls(&endpoint.address, &endpoint.name, endpoint.tls),
                    )
                    .await??;
                    write_frame(&mut conn, &WireMessage::DesktopMedia(ticket)).await?;
                    #[cfg(feature = "paired-human-desktop")]
                    let attempt = remoteops_protocol::desktop::lan::diagnostic_id(stream);
                    #[cfg(feature = "paired-human-desktop")]
                    direct::transfer(
                        &mut conn,
                        &mut video,
                        request,
                        lan_keys,
                        &route_changed,
                        |action| {
                            let _ = notify.send(WireMessage::Desktop(DesktopEvent {
                                session,
                                stream,
                                action,
                            }));
                        },
                        |text| {
                            log_desktop(
                                transport_log.as_ref(),
                                &format!("私网 #{attempt} · {text}"),
                            )
                        },
                    )
                    .await?;
                    #[cfg(not(feature = "paired-human-desktop"))]
                    while let Some(data) = video.recv().await {
                        tokio::time::timeout(
                            LEASE,
                            remoteops_protocol::desktop::write_video(&mut conn, &data),
                        )
                        .await??;
                    }
                    Ok::<(), anyhow::Error>(())
                }
                .await;
                if let Err(error) = result {
                    if !closing.load(Ordering::Acquire) {
                        log_desktop(transport_log.as_ref(), &format!("媒体传输结束 · {error}"));
                    }
                }
                closing.store(true, Ordering::Release);
                let _ = notify.send(WireMessage::Desktop(DesktopEvent {
                    session,
                    stream,
                    action: Action::Closed,
                }));
            });
            let signal = stop.clone();
            let out = self.out.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(reason) = capture(
                    session,
                    stream,
                    &signal,
                    &out,
                    commands,
                    frames,
                    &route,
                    capture_log.as_ref(),
                    #[cfg(feature = "paired-human-desktop")]
                    &mut direct_input,
                ) {
                    log_desktop(capture_log.as_ref(), reason);
                    tracing::warn!(%reason,"人工桌面采集已停止");
                }
                signal.store(true, Ordering::Release);
                let _ = out.send(WireMessage::Desktop(DesktopEvent {
                    session,
                    stream,
                    action: Action::Closed,
                }));
            });
            self.active = Some(Active {
                session,
                stream,
                binding: b.binding_token.clone(),
                stop,
                input,
                #[cfg(feature = "paired-human-desktop")]
                lan_request: Some(lan_request),
                transport,
            });
            return;
        }
        let Some(s) = self.active.as_mut() else {
            return;
        };
        if event.session != s.session || event.stream != s.stream {
            return;
        }
        #[cfg(feature = "paired-human-desktop")]
        if matches!(&event.action, Action::LanRequest { .. }) && s.binding == b.binding_token {
            if let Some(request) = s.lan_request.take() {
                let _ = request.send(event.action);
            }
            return;
        }
        // 权限降为只读时撤销键鼠，观察流仍可继续。
        let action = match event.action {
            Action::ClipboardShare { epoch, .. }
            | Action::ClipboardData { epoch, .. }
            | Action::LanPermit { epoch }
            | Action::Input { epoch, .. }
            | Action::Control {
                enabled: true,
                epoch,
            } if local == PermissionMode::ReadOnly
                || b.permission_mode == PermissionMode::ReadOnly =>
            {
                Action::Control {
                    enabled: false,
                    epoch,
                }
            }
            action => action,
        };
        if s.binding != b.binding_token {
            self.stop("人工绑定已变更");
        } else if matches!(action, Action::Closed) {
            self.stop("收到 Relay 的桌面关闭通知");
        } else {
            s.input.push(action);
        }
    }
}

fn inject(host: &mut Host, input: Input) -> bool {
    match input {
        Input::Move { x, y } => host.input(0, x.into(), y.into()),
        Input::Button { button, down } if button <= 2 => host.input(1, button.into(), down.into()),
        Input::Wheel { delta } => host.input(2, delta.into(), 0),
        Input::Key { vk, down } => host.input(3, vk.into(), down.into()),
        Input::Release => host.input(4, 0, 0),
        _ => false,
    }
}

// 按采集端发送队列的背压分档。静止桌面和空闲连接不会触发降档。
#[derive(Default)]
struct AdaptiveRate {
    level: u8,
    healthy: u8,
    samples: u32,
    blocked: u32,
}
impl AdaptiveRate {
    fn sample(&mut self, full: bool) {
        self.samples += 1;
        self.blocked += u32::from(full);
    }
    fn finish_window(&mut self) {
        if self.samples >= 8 && self.blocked * 4 >= self.samples {
            self.level = (self.level + 1).min(3);
            self.healthy = 0;
        } else if self.samples >= 8 && self.blocked * 20 <= self.samples {
            self.healthy += 1;
            if self.healthy == 5 {
                self.level = self.level.saturating_sub(1);
                self.healthy = 0;
            }
        } else {
            self.healthy = 0;
        }
        self.samples = 0;
        self.blocked = 0;
    }
    fn settings(&self, quality: Quality, fps: u8, bitrate: u32) -> (Quality, u8, u32) {
        let (q, f, b) = match self.level {
            0 => return (quality, fps, bitrate),
            1 => (quality, 15, 3_500_000),
            2 => (quality, 8, 2_000_000),
            _ => (
                if quality == Quality::High {
                    Quality::Balanced
                } else {
                    quality
                },
                8,
                1_200_000,
            ),
        };
        (q, fps.min(f), bitrate.min(b))
    }
}

#[cfg(all(test, feature = "paired-human-desktop"))]
#[path = "desktop_recovery_tests.rs"]
mod recovery_tests;

#[cfg(test)]
mod adaptive_tests {
    use super::*;
    #[test]
    fn backpressure_lowers_framerate_before_resolution_and_recovers_slowly() {
        let mut r = AdaptiveRate::default();
        for expected in [
            (Quality::High, 15),
            (Quality::High, 8),
            (Quality::Balanced, 8),
        ] {
            for _ in 0..30 {
                r.sample(true);
            }
            r.finish_window();
            let (q, f, _) = r.settings(Quality::High, 30, 6_000_000);
            assert_eq!((q, f), expected);
        }
        for _ in 0..5 {
            for _ in 0..30 {
                r.sample(false);
            }
            r.finish_window();
        }
        assert_eq!(r.level, 2);
    }
}

fn capture(
    session: SessionId,
    stream: RequestId,
    stop: &AtomicBool,
    out: &mpsc::UnboundedSender<WireMessage>,
    commands: Arc<Inbox>,
    frames: mpsc::Sender<Vec<u8>>,
    route: &std::sync::atomic::AtomicU64,
    log: Option<&super::AgentEventSink>,
    #[cfg(feature = "paired-human-desktop")] direct_input: &mut mpsc::Receiver<(u64, Input)>,
) -> Result<(), &'static str> {
    // 上一条流的 Win32 停止按钮销毁后，下一条流才能创建自己的现场窗口。
    static CAPTURE_WINDOW: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _exclusive_host = CAPTURE_WINDOW.lock().unwrap_or_else(|err| err.into_inner());
    if stop.load(Ordering::Acquire) || !commands.live() {
        return Ok(());
    }
    let notify = |action| {
        out.send(WireMessage::Desktop(DesktopEvent {
            session,
            stream,
            action,
        }))
        .map_err(|_| "控制通道已关闭")
    };
    #[cfg(feature = "paired-human-desktop")]
    let mut host = Host::open_paired()?;
    #[cfg(not(feature = "paired-human-desktop"))]
    let mut host = Host::open()?;
    log_desktop(
        log,
        if host.is_rdp() {
            "现场共享窗口已就绪（RDP 会话）"
        } else {
            "现场共享窗口已就绪（本地控制台）"
        },
    );
    let mut first_frame_sent = false;
    let mut capture_window = Instant::now();
    let mut captures = 0u32;
    let (mut quality, mut fps, mut bitrate) = (Quality::High, 30_u8, 6_000_000_u32);
    let (mut last_sent, mut next_frame) = (None::<Instant>, Instant::now());
    let mut congestion = AdaptiveRate::default();
    let mut window = Instant::now();
    let mut geometry = None;
    let mut epoch = 0_u64;
    let mut rtc = None::<remoteops_desktop_media::minirtc::Peer>;
    let crossdesk_capable = crossdesk_available();
    let mut using_crossdesk = false;
    let mut rtc_heartbeat = commands.heartbeat_time();
    let mut rtc_connected = false;
    let mut rtc_primary = false;
    let mut rtc_attempted = false;
    let rtc_capable =
        cfg!(feature = "paired-human-desktop") && remoteops_desktop_media::minirtc::available();
    let mut controlling = false;
    let mut releasing = false;
    let mut requested = false;
    let mut request_time = Instant::now();
    let mut route_seen = 0;
    #[cfg(feature = "paired-human-desktop")]
    let mut direct_permit = None;
    let mut clipboard = None::<(
        u64,
        RequestId,
        remoteops_desktop_media::clipboard::Clipboard,
    )>;
    let mut encoder = None::<((i32, i32, u8, u32), Encoder)>;
    let mut pixels = vec![0; 1920 * 1080 * 4];
    let mut compressed = vec![0; MAX_VIDEO_BYTES];
    while !stop.load(Ordering::Acquire) && commands.live() {
        let status = host.poll();
        if status <= 0 {
            return Err(host.stop_reason());
        }
        if last_sent.is_none_or(|last| last.elapsed() >= Duration::from_secs(1)) {
            notify(Action::Heartbeat)?;
            last_sent = Some(Instant::now());
        }
        if requested && !controlling && request_time.elapsed() >= Duration::from_secs(30) {
            requested = false;
            releasing = true;
        }
        if releasing {
            clipboard = None;
            if !host.request_control(false) {
                // 物理键仍按下时保留心跳，暂停处理输入，并在同一现场线程重试。
                std::thread::sleep(Duration::from_millis(8));
                continue;
            }
            epoch += 1;
            notify(Action::Control {
                enabled: false,
                epoch,
            })?;
            releasing = false;
        }
        if status == 3 && requested && !controlling && geometry.is_some() {
            controlling = true;
            epoch += 1;
            notify(Action::Control {
                enabled: true,
                epoch,
            })?;
        } else if controlling && status != 3 {
            requested = false;
            controlling = false;
            releasing = true;
            continue;
        }
        let current_route = route.load(Ordering::Acquire);
        if current_route != route_seen {
            route_seen = current_route;
            encoder = None;
            requested = false;
            controlling = false;
            releasing = true;
            continue;
        }
        if !controlling || clipboard.as_ref().is_some_and(|c| c.0 != epoch) {
            clipboard = None;
        }
        if let Some(worker) = &rtc {
            use remoteops_desktop_media::minirtc::Event;
            let current_heartbeat = commands.heartbeat_time();
            if using_crossdesk && current_heartbeat != rtc_heartbeat && worker.heartbeat() {
                rtc_heartbeat = current_heartbeat;
            }
            match worker.poll() {
                Some(Event::Ready) => rtc_connected = true,
                Some(Event::Signal(s)) if using_crossdesk => notify(Action::CrossDeskOffer(
                    remoteops_protocol::desktop::rtc::Signal(s),
                ))?,
                Some(Event::Signal(s)) => notify(Action::RtcSignal(
                    remoteops_protocol::desktop::rtc::Signal(s),
                ))?,
                Some(Event::Failed) => {
                    rtc = None;
                    rtc_connected = false;
                    rtc_primary = false;
                    encoder = None;
                    notify(if using_crossdesk {
                        Action::CrossDeskStop
                    } else {
                        Action::RtcStop
                    })?;
                }
                _ => {}
            }
        }
        let mut recovery = None;
        match commands.next() {
            Some(Command::Wire(Action::CrossDeskGrant(ticket)))
                if crossdesk_capable && !rtc_attempted =>
            {
                rtc_attempted = true;
                if let Ok(worker) =
                    remoteops_desktop_media::minirtc::Peer::start_crossdesk(true, &ticket)
                    && host.select_crossdesk_capture().is_ok()
                {
                    using_crossdesk = true;
                    rtc = Some(worker);
                } else {
                    notify(Action::CrossDeskStop)?;
                    log_desktop(log, "CrossDesk 无法启动，继续使用 TCP");
                }
            }
            Some(Command::Wire(Action::CrossDeskReady)) if using_crossdesk && rtc.is_some() => {
                rtc_primary = true
            }
            Some(Command::Wire(Action::CrossDeskStop)) if using_crossdesk => {
                rtc = None;
                rtc_connected = false;
                rtc_primary = false;
                encoder = None;
            }
            Some(Command::Wire(Action::RtcGranted(grant))) if rtc_capable && !rtc_attempted => {
                rtc_attempted = true;
                match remoteops_desktop_media::minirtc::Peer::start(
                    true,
                    &stream.to_string(),
                    &grant,
                ) {
                    Ok(worker) => rtc = Some(worker),
                    Err(_) => {
                        notify(Action::RtcStop)?;
                        log_desktop(log, "MiniRTC 组件无法启动，使用 TCP");
                    }
                }
            }
            Some(Command::Wire(Action::RtcSignal(s))) if !using_crossdesk => {
                if let Some(worker) = &rtc
                    && !worker.signal(s.0)
                {
                    rtc = None;
                    rtc_connected = false;
                    rtc_primary = false;
                    encoder = None;
                    notify(Action::RtcStop)?;
                }
            }
            Some(Command::Wire(Action::RtcReady)) if !using_crossdesk && rtc_connected => {
                rtc_primary = true
            }
            Some(Command::Wire(Action::RtcStop)) if !using_crossdesk => {
                rtc = None;
                rtc_connected = false;
                rtc_primary = false;
                encoder = None;
            }
            #[cfg(feature = "paired-human-desktop")]
            Some(Command::Wire(Action::LanPermit { epoch: seen }))
                if controlling && seen == epoch =>
            {
                direct_permit = Some(seen)
            }
            #[cfg(feature = "paired-human-desktop")]
            None => {
                if let Ok((seen, event)) = direct_input.try_recv()
                    && direct::permits_input(current_route, controlling, epoch, direct_permit, seen)
                    && !stop.load(Ordering::Acquire)
                    && !inject(&mut host, event)
                {
                    recovery = Some("现场未接受直连键鼠输入");
                }
            }
            Some(Command::Wire(Action::ClipboardShare {
                epoch: seen,
                channel,
                enabled,
            })) if controlling && seen == epoch && cfg!(feature = "paired-human-desktop") => {
                if enabled {
                    clipboard = remoteops_desktop_media::clipboard::Clipboard::open()
                        .map(|c| (epoch, channel, c));
                } else if clipboard.as_ref().is_some_and(|c| c.1 == channel) {
                    clipboard = None;
                }
            }
            Some(Command::Wire(Action::ClipboardData {
                epoch: seen,
                channel,
                packet,
            })) if controlling && seen == epoch => {
                if let Some((_, id, local)) = &mut clipboard
                    && *id == channel
                    && let Some(bytes) = packet.decode()
                {
                    local.receive(&bytes);
                }
            }
            Some(Command::ReleaseOverload) => recovery = Some("输入队列积压"),
            Some(Command::Wire(Action::Control {
                enabled,
                epoch: seen,
            })) => {
                if !enabled || (geometry.is_some() && seen == epoch) {
                    requested = enabled;
                    request_time = Instant::now();
                    if !enabled {
                        controlling = false;
                        releasing = true;
                        continue;
                    } else if !host.request_control(true) {
                        recovery = Some("现场暂时无法接管键鼠");
                    }
                }
            }
            Some(Command::Wire(Action::Input { event, epoch: seen }))
                if controlling && seen == epoch =>
            {
                if !inject(&mut host, event) {
                    recovery = Some("现场未接受中继键鼠输入");
                }
            }
            Some(Command::Wire(Action::Settings {
                quality: q,
                fps: f,
                bitrate: b,
            })) => {
                quality = q;
                fps = f.clamp(1, q.limits().2 as u8);
                bitrate = b.clamp(100_000, 12_000_000);
            }
            _ => {}
        }
        if let Some(reason) = recovery {
            requested = false;
            controlling = false;
            releasing = true;
            log_desktop(log, &format!("{reason}，正在释放键鼠"));
            continue;
        }
        if !controlling || clipboard.as_ref().is_some_and(|c| c.0 != epoch) {
            clipboard = None;
        }
        if let Some((_, id, local)) = &mut clipboard {
            match local.next_packet() {
                Ok(Some(bytes)) => {
                    if let Some(packet) =
                        remoteops_protocol::desktop::clipboard::Packet::encode(bytes)
                    {
                        notify(Action::ClipboardData {
                            epoch,
                            channel: *id,
                            packet,
                        })?;
                    }
                }
                Err(_) => {
                    clipboard = None;
                    log_desktop(log, "系统剪贴板已暂停");
                }
                _ => {}
            }
        }
        if status >= 2 && Instant::now() >= next_frame {
            let (quality, fps, bitrate) = congestion.settings(quality, fps, bitrate);
            let (mw, mh, _, _) = quality.limits();
            next_frame = Instant::now() + Duration::from_micros(1_000_000 / u64::from(fps));
            let full = frames.capacity() == 0;
            congestion.sample(full);
            if window.elapsed() >= Duration::from_secs(3) {
                congestion.finish_window();
                window = Instant::now();
            }
            if full {
                continue;
            }
            if let Some((w, h, pw, ph)) = host.capture(&mut pixels, mw as i32, mh as i32)? {
                captures += 1;
                if geometry != Some((pw, ph)) {
                    requested = false;
                    controlling = false;
                    if !host.request_control(false) {
                        releasing = true;
                        continue;
                    }
                    geometry = Some((pw, ph));
                    epoch += 1;
                    notify(Action::Geometry {
                        epoch,
                        width: pw as u32,
                        height: ph as u32,
                        lan: cfg!(feature = "paired-human-desktop"),
                        clipboard: cfg!(feature = "paired-human-desktop"),
                        mini_rtc: rtc_capable,
                        crossdesk: crossdesk_capable,
                    })?;
                }
                if rtc_connected && commands.live() && !stop.load(Ordering::Acquire) {
                    if let Some(worker) = &rtc {
                        worker.frame(w as u32, h as u32, &pixels[..w as usize * h as usize * 4]);
                    }
                    if rtc_primary {
                        continue;
                    }
                }
                let settings = (w, h, fps, bitrate);
                if encoder.as_ref().is_none_or(|(s, _)| *s != settings) {
                    encoder = Some((
                        settings,
                        Encoder::new(w, h, i32::from(fps), bitrate as i32)?,
                    ));
                }
                let n = encoder
                    .as_mut()
                    .unwrap()
                    .1
                    .encode(&pixels[..w as usize * h as usize * 4], &mut compressed)?;
                if n > 0 && !stop.load(Ordering::Acquire) && commands.live() {
                    let mut packet = compressed[..n].to_vec();
                    let elapsed = capture_window.elapsed();
                    if elapsed >= Duration::from_secs(1) {
                        let fps = (f64::from(captures) / elapsed.as_secs_f64()).round() as u8;
                        remoteops_protocol::desktop::fps::stamp(&mut packet, fps);
                        capture_window = Instant::now();
                        captures = 0;
                    }
                    frames.try_send(packet).map_err(|_| "桌面视频连接已关闭")?;
                    if !first_frame_sent {
                        first_frame_sent = true;
                        log_desktop(log, "首帧已编码并进入发送队列");
                    }
                }
            }
        }
        if commands.is_empty() {
            std::thread::sleep(Duration::from_millis(4));
        }
    }
    host.request_control(false);
    if stop.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err("控制链心跳超时，已停止共享")
    }
}
