//! 在有交互桌面的 Windows 实机验证。拒绝的虚键码不会产生系统输入。
use super::*;
use std::sync::atomic::AtomicU64;
use tokio::time::{sleep, timeout};

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
    }).await.expect("等待桌面状态超时")
}

async fn control(events: &mut mpsc::UnboundedReceiver<WireMessage>, enabled: bool) -> u64 {
    next_epoch(events, |event| match event {
        Action::Control { enabled: received, epoch } if enabled == received => Some(epoch),
        _ => None,
    }).await
}

#[tokio::test(flavor="multi_thread", worker_threads=2)]
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
    let host_inbox=inbox.clone();
    inbox.push(Action::Settings {quality:Quality::Low, fps:8, bitrate:600_000});
    let worker = tokio::task::spawn_blocking(move || {
        capture(SessionId::new(), RequestId::new(), &AtomicBool::new(false), &events,
            host_inbox, video, &host_route, None, &mut keys)
    });
    let count=Arc::new(AtomicU64::new(0));
    let seen=count.clone();
    let consumer=tokio::spawn(async move {
        while let Some(frame)=frames.recv().await {
            assert!(!frame.is_empty());
            seen.fetch_add(1,Ordering::Relaxed);
        }
    });
    let geometry = next_epoch(&mut received, |event| match event {
        Action::Geometry{epoch,..}=>Some(epoch), _=>None
    }).await;
    for mode in ["观察","控制"] {
        if mode=="控制" {
            inbox.push(Action::Control{enabled:true,epoch:geometry});
        }
        let epoch=if mode=="控制" {control(&mut received,true).await} else {geometry};
        let before=count.load(Ordering::Relaxed);
        sleep(LEASE+Duration::from_secs(1)).await;
        let gained=count.load(Ordering::Relaxed)-before;
        assert!(gained>=2);
        println!("{mode}连续 6 秒收到 {gained} 个 H.264 帧");
        if mode=="控制" {
            inbox.push(Action::Input {epoch,event:Input::Key{vk:0,down:true}});
            let returned=control(&mut received,false).await;
            assert!(returned>epoch);
        }
    }
    // 人工重新申请，验证直连下 Windows 拒绝输入也仅撤回输入权。
    route.store(1,Ordering::Release);
    let epoch=control(&mut received,false).await;
    inbox.push(Action::Control{enabled:true,epoch});
    let granted=control(&mut received,true).await;
    inbox.push(Action::LanPermit{epoch:granted});
    direct.send((granted,Input::Key{vk:0,down:true})).await.unwrap();
    let released=control(&mut received,false).await;
    assert!(released>granted);
    let before=count.load(Ordering::Relaxed);
    sleep(Duration::from_secs(1)).await;
    assert!(count.load(Ordering::Relaxed)>before);
    println!("中继与直连输入被拒后均收到新的观察授权，继续收到视频帧");
    inbox.push(Action::Control{enabled:true,epoch:released});
    let restored=control(&mut received,true).await;
    for _ in 0..1024 {
        inbox.push(Action::Input {epoch:0,event:Input::Release});
    }
    let observed=control(&mut received,false).await;
    assert!(observed>restored);
    let before=count.load(Ordering::Relaxed);
    sleep(Duration::from_secs(1)).await;
    assert!(count.load(Ordering::Relaxed)>before);
    println!("输入积压后释放控制，观察画面继续更新");
    heartbeat.abort();
    let _=heartbeat.await;
    let reason=timeout(LEASE+Duration::from_secs(3),worker).await.unwrap().unwrap().unwrap_err();
    assert!(reason.contains("心跳超时"));
    consumer.await.unwrap();
    println!("停止心跳后采集线程和视频通道均已结束");
}
