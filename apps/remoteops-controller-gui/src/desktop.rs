//! 一个窗口对应一次打开请求，控制连接恢复后必须由人工重新打开。
use anyhow::{Context, Result, bail};
use remoteops_application::RelayClient;
use remoteops_desktop_media::{Decoder, Viewer};
use remoteops_domain::{RequestId, SessionId};
use remoteops_protocol::desktop::{Action, DesktopEvent, Input, LEASE, Quality, read_video};
use std::time::{Duration, Instant};
use tokio::{
    runtime::Handle,
    sync::{broadcast, mpsc},
    task::JoinHandle,
};

struct Media(JoinHandle<()>);
impl Drop for Media {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) async fn open(client: RelayClient, session: SessionId, title: String) -> Result<()> {
    let stream = RequestId::new();
    let events = client.desktop_events();
    let runtime = Handle::current();
    let (done, result) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("desktop-viewer".into())
        .spawn(move || {
            let opened = (|| {
                let (mut viewer, decoder) = Viewer::with_decoder(&format!("实时桌面 · {title}"))
                    .map_err(anyhow::Error::msg)?;
                viewer.status("正在请求现场允许观看", false);
                let command = |action| DesktopEvent {
                    session,
                    stream,
                    action,
                };
                runtime.block_on(client.desktop_command(command(Action::Open)))?;
                let result = run(&runtime, &client, session, stream, viewer, decoder, events);
                let _ = runtime.block_on(async {
                    tokio::time::timeout(LEASE, client.desktop_command(command(Action::Closed)))
                        .await
                });
                result
            })();
            let _ = done.send(opened);
        })?;
    result.await.context("观看线程提前退出")?
}

#[derive(Default)]
struct Adaptive {
    quality: Quality,
    healthy: u8,
}
impl Adaptive {
    fn update(&mut self, measured: f64, behind: bool) -> Option<Quality> {
        let target = f64::from(self.quality.limits().2);
        let next = if behind || measured < target * 0.70 {
            self.healthy = 0;
            match self.quality {
                Quality::High => Quality::Balanced,
                _ => Quality::Low,
            }
        } else {
            if measured >= target * 0.87 {
                self.healthy += 1;
            } else {
                self.healthy = 0;
            }
            if self.healthy < 3 {
                return None;
            }
            self.healthy = 0;
            match self.quality {
                Quality::Low => Quality::Balanced,
                _ => Quality::High,
            }
        };
        if self.quality == next {
            return None;
        }
        self.quality = next;
        Some(next)
    }
}
fn settings(quality: Quality) -> Action {
    let (_, _, fps, bitrate) = quality.limits();
    Action::Settings {
        quality,
        fps: u8::try_from(fps).unwrap_or(u8::MAX),
        bitrate,
    }
}

fn desktop_input(kind: i32, a: i32, b: i32) -> Option<Input> {
    match kind {
        0 => Some(Input::Move {
            x: u16::try_from(a.clamp(0, i32::from(u16::MAX))).ok()?,
            y: u16::try_from(b.clamp(0, i32::from(u16::MAX))).ok()?,
        }),
        1 if (0..=2).contains(&a) => Some(Input::Button {
            button: u8::try_from(a).ok()?,
            down: b != 0,
        }),
        2 => Some(Input::Wheel {
            delta: i16::try_from(a.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).ok()?,
        }),
        3 if (1..=254).contains(&a) => Some(Input::Key {
            vk: u8::try_from(a).ok()?,
            down: b != 0,
        }),
        4 => Some(Input::Release),
        _ => None,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "原生窗口在一个事件循环内处理租约、显示和键鼠状态"
)]
fn run(
    runtime: &Handle,
    client: &RelayClient,
    session: SessionId,
    stream: RequestId,
    mut viewer: Viewer,
    mut decoder: Decoder,
    mut events: broadcast::Receiver<DesktopEvent>,
) -> Result<()> {
    let send = |action| {
        runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(1),
                client.desktop_command(DesktopEvent {
                    session,
                    stream,
                    action,
                }),
            )
            .await?
            .map_err(anyhow::Error::from)
        })
    };
    viewer.status("等待现场允许观看", false);
    let mut image = vec![0; 1920 * 1080 * 4];
    let (frame_tx, mut frames) = mpsc::channel::<(Vec<u8>, Instant)>(2);
    let mut media = None::<Media>;
    let mut epoch = 0;
    let mut control = false;
    let mut pending = false;
    let mut last_received = Instant::now();
    let mut last_sent = None::<Instant>;
    let mut window = Instant::now();
    let mut count = 0_u32;
    let mut bytes = 0.0;
    let mut congested = false;
    let mut adaptive = Adaptive::default();
    let mut auto = true;
    let mut started = false;
    loop {
        if !client.supports_desktop() || last_received.elapsed() >= LEASE {
            bail!("控制连接心跳超时")
        }
        if last_sent.is_none_or(|last| last.elapsed() >= Duration::from_secs(1)) {
            send(Action::Heartbeat)?;
            last_sent = Some(Instant::now());
        }
        loop {
            let event = match events.try_recv() {
                Ok(e) => e,
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(_) => bail!("控制端错过桌面状态更新"),
            };
            if event.session != session || event.stream != stream {
                continue;
            }
            match event.action {
                Action::Attach(ticket) if media.is_none() => {
                    let client = client.clone();
                    let tx = frame_tx.clone();
                    media = Some(Media(runtime.spawn(async move {
                        if let Ok(Ok(mut input)) =
                            tokio::time::timeout(LEASE, client.desktop_media(ticket)).await
                        {
                            while let Ok(frame) = read_video(&mut input).await {
                                if tx.send((frame, Instant::now())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    })));
                }
                Action::Heartbeat => last_received = Instant::now(),
                Action::Geometry { epoch: next, .. } => {
                    epoch = next;
                    control = false;
                    pending = false;
                    viewer.status("仅查看", false);
                }
                Action::Control {
                    epoch: next,
                    enabled,
                } => {
                    epoch = next;
                    control = enabled;
                    pending = false;
                    viewer.status(
                        if enabled {
                            "人工已接管"
                        } else {
                            "仅查看"
                        },
                        enabled,
                    );
                }
                Action::Closed => return Ok(()),
                _ => {}
            }
        }
        if media.as_ref().is_some_and(|m| m.0.is_finished()) {
            bail!("视频连接已关闭")
        }
        for _ in 0..32 {
            let event = match viewer.poll() {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(_) => return Ok(()),
            };
            let [kind, a, b, c] = event;
            if kind == 5 {
                if epoch > 0 && (!pending || a == 0) {
                    let enabled = a != 0;
                    send(Action::Control { enabled, epoch })?;
                    control = false;
                    pending = enabled;
                    viewer.status(
                        if enabled {
                            "等待现场允许人工接管"
                        } else {
                            "仅查看"
                        },
                        false,
                    );
                }
            } else if kind == 6 {
                auto = a == 0;
                let quality = match a {
                    1 => Quality::Low,
                    3 => Quality::High,
                    _ => Quality::Balanced,
                };
                adaptive = Adaptive {
                    quality,
                    ..Default::default()
                };
                let action = if auto {
                    settings(quality)
                } else {
                    Action::Settings {
                        quality,
                        fps: u8::try_from(
                            u32::try_from(b).unwrap_or(1).clamp(1, quality.limits().2),
                        )?,
                        bitrate: u32::try_from(c.clamp(100_000, 12_000_000))?,
                    }
                };
                send(action)?;
                window = Instant::now();
                count = 0;
                bytes = 0.0;
                congested = false;
            } else if control {
                let Some(event) = desktop_input(kind, a, b) else {
                    continue;
                };
                send(Action::Input { epoch, event })?;
            }
        }
        if let Ok((data, arrived)) = frames.try_recv() {
            congested |= arrived.elapsed() > Duration::from_millis(200);
            bytes += f64::from(u32::try_from(data.len())?);
            if let Some((w, h)) = decoder
                .decode(&data, &mut image)
                .map_err(anyhow::Error::msg)?
            {
                viewer.present(&image, w, h);
                if !started {
                    started = true;
                    window = Instant::now();
                }
                count += 1;
            }
        }
        if started && window.elapsed() >= Duration::from_secs(2) {
            let secs = window.elapsed().as_secs_f64();
            let fps = f64::from(count) / secs;
            if auto && let Some(q) = adaptive.update(fps, congested) {
                send(settings(q)).context("切换桌面画质")?;
            }
            let mode = if control {
                "人工接管"
            } else if pending {
                "等待现场允许接管"
            } else {
                "仅查看"
            };
            viewer.status(
                &format!(
                    "{mode} · {fps:.1} 帧/秒 · {:.2} Mb/s",
                    bytes * 8. / secs / 1e6
                ),
                control,
            );
            window = Instant::now();
            count = 0;
            bytes = 0.0;
            congested = false;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_input_clamps_coordinates_and_rejects_unknown_keys() {
        assert_eq!(
            desktop_input(0, -5, 90_000),
            Some(Input::Move { x: 0, y: u16::MAX })
        );
        assert_eq!(
            desktop_input(2, -90_000, 0),
            Some(Input::Wheel { delta: i16::MIN })
        );
        assert_eq!(desktop_input(3, 255, 1), None);
        assert_eq!(desktop_input(1, -1, 1), None);
    }

    #[test]
    fn auto_requires_sustained_headroom_and_reacts_to_delay() {
        let mut q = Adaptive::default();
        assert_eq!(q.update(15., false), None);
        assert_eq!(q.update(15., false), None);
        assert_eq!(q.update(15., false), Some(Quality::High));
        assert_eq!(q.update(30., true), Some(Quality::Balanced));
        assert_eq!(q.update(5., false), Some(Quality::Low));
        assert_eq!(q.update(4., true), None);
    }
}
