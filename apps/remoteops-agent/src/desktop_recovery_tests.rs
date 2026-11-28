//! 在有交互桌面的 Windows 实机验证。拒绝的虚键码不会产生系统输入。
use super::*;
use std::sync::atomic::AtomicU64;
use tokio::time::{sleep, timeout};

/// 在独立测试 Relay 上运行正常配对入口，控制码只交给启动本进程的回环测试程序。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "由隔离中继的图形延迟测试启动"]
async fn isolated_latency_agent() {
    use std::{
        env,
        io::Write,
        net::{Ipv4Addr, TcpStream},
        path::PathBuf,
    };
    let root = PathBuf::from(env::var_os("REMOTEOPS_BENCH_DIR").unwrap());
    let port: u16 = env::var("REMOTEOPS_BENCH_RELAY_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let sink: u16 = env::var("REMOTEOPS_BENCH_PARENT_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let relay = format!("127.0.0.1:{port}");
    let config = crate::AgentConfig {
        relay: relay.clone(),
        server_name: "localhost".into(),
        ca_cert: Some(root.join("ca.pem")),
        state_file: root.join("agent-state.json"),
        transfer_root: root.join("files"),
        ..Default::default()
    };
    let product = crate::ProductProfile {
        endpoint: crate::EndpointPolicy::Fixed {
            relay,
            server_name: "localhost".into(),
        },
        ..Default::default()
    };
    let (events, receiver) = std::sync::mpsc::channel();
    let (end, stopped) = tokio::sync::watch::channel(false);
    let agent = tokio::spawn(crate::run_agent_with_options(
        config,
        Some(events),
        stopped,
        crate::AgentPermissionControl::default(),
        crate::AgentRuntimeOptions::interactive(product),
    ));
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut parent = TcpStream::connect((Ipv4Addr::LOCALHOST,sink)).unwrap();
        parent.set_read_timeout(Some(Duration::from_secs(300))).unwrap();
        while let Ok(event) = receiver.recv_timeout(Duration::from_secs(30)) {
            if let crate::AgentEvent::Connected { pairing_code, .. } = event {
                writeln!(parent, "{pairing_code}").unwrap();
                let mut done = [0;1];
                parent.set_read_timeout(Some(Duration::from_millis(250))).unwrap();
                while matches!(parent.read(&mut done), Err(ref e) if matches!(e.kind(),std::io::ErrorKind::TimedOut|std::io::ErrorKind::WouldBlock)) {
                    while let Ok(event)=receiver.try_recv() {
                        if let crate::AgentEvent::OperationLog(entry)=event
                            && let Some(detail)=entry.message.strip_prefix("实时桌面 · ") {
                            let safe: String=detail.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).take(100).collect();
                            eprintln!("DesktopCheck {safe}");
                        }
                    }
                }
                break;
            }
        }
        end.send_replace(true);
    }).await.unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
#[ignore = "需要可见且解锁的 Windows 交互桌面"]
async fn observing_receives_a_decodable_frame_after_each_open() {
    use crate::{AgentEvent, AgentEventSink};
    use remoteops_desktop_media::Decoder;

    for attempt in 1..=2 {
        let inbox = Arc::new(Inbox::new());
        inbox.push(Action::Settings {
            quality: Quality::Low,
            fps: 8,
            bitrate: 600_000,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let (events, _events_rx) = mpsc::unbounded_channel();
        let (video, mut frames) = mpsc::channel(2);
        let (_direct, mut keys) = mpsc::channel(4);
        let (log, records) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            capture(
                SessionId::new(),
                RequestId::new(),
                &signal,
                &events,
                inbox,
                video,
                &AtomicU64::new(0),
                Some(&AgentEventSink::Legacy(log)),
                &mut keys,
            )
        });
        let mut decoder = Decoder::new().expect("创建接收端解码器");
        let mut pixels = vec![0; 1920 * 1080 * 4];
        let received = timeout(LEASE, async {
            while let Some(packet) = frames.recv().await {
                let rate = remoteops_protocol::desktop::fps::capture_fps(&packet);
                if let Some(size) = decoder.decode(&packet, &mut pixels)? {
                    if attempt == 1 {
                        return Ok(size);
                    }
                    if let Some(fps) = rate {
                        assert!(fps > 0 && fps <= 12);
                        println!("解码带 SEI 统计的实机帧，采集 {fps} FPS");
                        return Ok(size);
                    }
                }
            }
            Err("首帧解码前视频通道已关闭")
        })
        .await;
        stop.store(true, Ordering::Release);
        timeout(LEASE, worker)
            .await
            .expect("等待采集退出超时")
            .unwrap()
            .expect("现场采集失败");
        let (width, height) = received.expect("接收首帧超时").expect("解码首帧");
        assert!(width > 0 && height > 0);
        let messages: Vec<_> = records
            .try_iter()
            .filter_map(|event| match event {
                AgentEvent::OperationLog(log) => Some(log.message),
                _ => None,
            })
            .collect();
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            messages[0].as_str(),
            "实时桌面 · 现场共享窗口已就绪（本地控制台）"
                | "实时桌面 · 现场共享窗口已就绪（RDP 会话）"
        ));
        assert_eq!(messages[1], "实时桌面 · 首帧已编码并进入发送队列");
        println!("第 {attempt} 次打开观察已解码 {width}×{height}，采集已退出");
    }
}

async fn next_epoch(
    events: &mut mpsc::UnboundedReceiver<WireMessage>,
    predicate: impl Fn(Action) -> Option<u64>,
) -> u64 {
    timeout(LEASE, async {
        while let Some(message) = events.recv().await {
            if let WireMessage::Desktop(event) = message
                && let Some(epoch) = predicate(event.action)
            {
                return epoch;
            }
        }
        panic!("采集已提前结束")
    })
    .await
    .expect("等待桌面状态超时")
}

async fn control(events: &mut mpsc::UnboundedReceiver<WireMessage>, enabled: bool) -> u64 {
    next_epoch(events, |event| match event {
        Action::Control {
            enabled: received,
            epoch,
        } if enabled == received => Some(epoch),
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "需要可见的 Windows 交互桌面"]
async fn live_capture_survives_refused_input_and_stops_on_lease_loss() {
    let inbox = Arc::new(Inbox::new());
    let (events, mut received) = mpsc::unbounded_channel();
    let (video, mut frames) = mpsc::channel::<Vec<u8>>(2);
    let (direct, mut keys) = mpsc::channel(4);
    let route = Arc::new(AtomicU64::new(0));
    let host_route = route.clone();
    let renewed = inbox.clone();
    let heartbeat = tokio::spawn(async move {
        loop {
            renewed.push(Action::Heartbeat);
            sleep(Duration::from_millis(250)).await;
        }
    });
    let host_inbox = inbox.clone();
    inbox.push(Action::Settings {
        quality: Quality::Low,
        fps: 8,
        bitrate: 600_000,
    });
    let worker = tokio::task::spawn_blocking(move || {
        capture(
            SessionId::new(),
            RequestId::new(),
            &AtomicBool::new(false),
            &events,
            host_inbox,
            video,
            &host_route,
            None,
            &mut keys,
        )
    });
    let count = Arc::new(AtomicU64::new(0));
    let seen = count.clone();
    let consumer = tokio::spawn(async move {
        while let Some(frame) = frames.recv().await {
            assert!(!frame.is_empty());
            seen.fetch_add(1, Ordering::Relaxed);
        }
    });
    let geometry = next_epoch(&mut received, |event| match event {
        Action::Geometry { epoch, .. } => Some(epoch),
        _ => None,
    })
    .await;
    for mode in ["观察", "控制"] {
        if mode == "控制" {
            inbox.push(Action::Control {
                enabled: true,
                epoch: geometry,
            });
        }
        let epoch = if mode == "控制" {
            control(&mut received, true).await
        } else {
            geometry
        };
        let before = count.load(Ordering::Relaxed);
        sleep(LEASE + Duration::from_secs(1)).await;
        let gained = count.load(Ordering::Relaxed) - before;
        assert!(gained >= 2);
        println!("{mode}连续 6 秒收到 {gained} 个 H.264 帧");
        if mode == "控制" {
            inbox.push(Action::Input {
                epoch,
                event: Input::Key { vk: 0, down: true },
            });
            let returned = control(&mut received, false).await;
            assert!(returned > epoch);
        }
    }
    // 人工重新申请，验证直连下 Windows 拒绝输入也仅撤回输入权。
    route.store(1, Ordering::Release);
    let epoch = control(&mut received, false).await;
    inbox.push(Action::Control {
        enabled: true,
        epoch,
    });
    let granted = control(&mut received, true).await;
    inbox.push(Action::LanPermit { epoch: granted });
    direct
        .send((granted, Input::Key { vk: 0, down: true }))
        .await
        .unwrap();
    let released = control(&mut received, false).await;
    assert!(released > granted);
    let before = count.load(Ordering::Relaxed);
    sleep(Duration::from_secs(1)).await;
    assert!(count.load(Ordering::Relaxed) > before);
    println!("中继与直连输入被拒后均收到新的观察授权，继续收到视频帧");
    inbox.push(Action::Control {
        enabled: true,
        epoch: released,
    });
    let restored = control(&mut received, true).await;
    for _ in 0..1024 {
        inbox.push(Action::Input {
            epoch: 0,
            event: Input::Release,
        });
    }
    let observed = control(&mut received, false).await;
    assert!(observed > restored);
    let before = count.load(Ordering::Relaxed);
    sleep(Duration::from_secs(1)).await;
    assert!(count.load(Ordering::Relaxed) > before);
    println!("输入积压后释放控制，观察画面继续更新");
    heartbeat.abort();
    let _ = heartbeat.await;
    let reason = timeout(LEASE + Duration::from_secs(3), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(reason.contains("心跳超时"));
    consumer.await.unwrap();
    println!("停止心跳后采集线程和视频通道均已结束");
}
