//! 把采集帧率写入 H.264 的 `user_data_unregistered` SEI；旧解码器可跳过该消息。
const PREFIX: &[u8] = b"\x06\x05\x13RemoteOps-FPS-v1";

// 只查看第一个切片之前的 NAL，不扫描压缩像素数据。
fn headers(packet: &[u8]) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut cursor = 0;
    let mut finished = false;
    std::iter::from_fn(move || {
        if finished {
            return None;
        }
        let start = cursor
            + packet
                .get(cursor..)?
                .windows(3)
                .position(|p| p == [0, 0, 1])?;
        let nal = start + 3;
        let kind = packet.get(nal)? & 31;
        finished = (1..=5).contains(&kind);
        cursor = nal + 1;
        Some((
            start - usize::from(start > 0 && packet[start - 1] == 0),
            nal,
        ))
    })
}

/// 在切片之前、参数集之后插入一条尺寸固定的采集统计。
pub fn stamp(packet: &mut Vec<u8>, fps: u8) {
    if packet.len() + PREFIX.len() + 8 > super::MAX_VIDEO_BYTES {
        return;
    }
    let Some((offset, _)) = headers(packet).find(|&(_, n)| (1..=5).contains(&(packet[n] & 31)))
    else {
        return;
    };
    let digits = [
        b'0' + fps / 100,
        b'0' + fps / 10 % 10,
        b'0' + fps % 10,
        0x80,
    ];
    packet.splice(
        offset..offset,
        [0, 0, 0, 1]
            .into_iter()
            .chain(PREFIX.iter().copied())
            .chain(digits),
    );
}

/// 读取已知统计；缺失、截断或来自其他编码器的消息不影响视频。
#[must_use]
pub fn capture_fps(packet: &[u8]) -> Option<u8> {
    headers(packet).find_map(|(_, n)| {
        let data = packet[n..].strip_prefix(PREFIX)?;
        let digits = data.get(..3)?;
        if !digits.iter().all(u8::is_ascii_digit) || data.get(3) != Some(&0x80) {
            return None;
        }
        std::str::from_utf8(digits).ok()?.parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn telemetry_preserves_parameter_sets_and_slices() {
        let original = [0, 0, 0, 1, 9, 16, 0, 0, 1, 0x67, 12, 0, 0, 0, 1, 0x65, 71];
        let mut sample = original.to_vec();
        stamp(&mut sample, 27);
        assert_eq!(capture_fps(&sample), Some(27));
        assert_eq!(&sample[..11], &original[..11]);
        assert!(sample.ends_with(&original[11..]));
        assert_eq!(capture_fps(&original), None);
        for i in 0..sample.len() {
            let _ = capture_fps(&sample[..i]);
        }
    }
}
