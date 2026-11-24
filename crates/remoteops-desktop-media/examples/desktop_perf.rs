//! 只对内存中的动态标尺做 60 秒编解码，独立统计已解出的 1080p 画面。
#[cfg(windows)]
fn main() {
    use remoteops_desktop_media::{Decoder, Encoder};
    use std::time::{Duration, Instant};
    let (w, h) = (1920, 1080);
    let mut enc = Encoder::new(w, h, 30, 6_000_000).expect("H.264 编码器");
    let mut dec = Decoder::new().expect("H.264 解码器");
    let mut img = vec![80; w as usize * h as usize * 4];
    let mut compressed = vec![0; 2 * 1024 * 1024];
    let mut canvas = vec![0; img.len()];
    let start = Instant::now();
    let mut sent = 0_u32;
    let mut frames = 0_u32;
    let mut bytes = 0usize;
    let mut levels = [false; 2];
    while start.elapsed() < Duration::from_secs(60) {
        let level = if sent % 30 < 15 { 30 } else { 210 };
        for y in 0..90 {
            let row = &mut img[y * w as usize * 4..][..320 * 4];
            row.fill(level);
        }
        let n = enc.encode(&img, &mut compressed).expect("合成帧编码");
        if n > 0 {
            bytes += n;
            if let Some(size) = dec
                .decode(&compressed[..n], &mut canvas)
                .expect("独立解码器")
            {
                assert_eq!(size, (w, h));
                frames += 1;
                levels[usize::from(canvas[32] > 120)] = true;
            }
        }
        sent += 1;
        if let Some(delay) = (start + Duration::from_secs_f64(f64::from(sent) / 30.))
            .checked_duration_since(Instant::now())
        {
            std::thread::sleep(delay);
        }
    }
    let secs = start.elapsed().as_secs_f64();
    let fps = f64::from(frames) / secs;
    println!("hardware_encoder={}", enc.is_hardware());
    println!(
        "1920x1080 elapsed={secs:.2}s submitted={sent} decoded={frames} fps={fps:.2} bytes={bytes} levels={levels:?}"
    );
    assert!(
        levels.iter().all(|&b| b) && fps >= 25.,
        "合成视频未达到验收门槛"
    );
}

#[cfg(not(windows))]
fn main() {
    eprintln!("此基准需要 Windows Media Foundation");
}
