use std::io;

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 单个协议帧允许的最大字节数。
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// 协议帧读写错误。
#[derive(Debug, Error)]
pub enum FrameError {
    /// 底层流错误。
    #[error("传输读写失败：{0}")]
    Io(#[from] io::Error),
    /// JSON 编解码错误。
    #[error("协议 JSON 编解码失败：{0}")]
    Json(#[from] serde_json::Error),
    /// 对端声明的帧过大。
    #[error("协议帧超过限制：{0} 字节")]
    FrameTooLarge(usize),
}

/// 读取一个四字节大端长度前缀的 JSON 帧。
///
/// # Errors
///
/// 当底层流读取失败、帧超过限制或 JSON 无法解码时返回错误。
pub async fn read_frame<T, R>(reader: &mut R) -> Result<T, FrameError>
where
    T: DeserializeOwned,
    R: AsyncRead + Unpin,
{
    let length = reader.read_u32().await? as usize;
    if length > MAX_FRAME_SIZE {
        return Err(FrameError::FrameTooLarge(length));
    }
    let mut buffer = vec![0_u8; length];
    reader.read_exact(&mut buffer).await?;
    Ok(serde_json::from_slice(&buffer)?)
}

/// 写入一个四字节大端长度前缀的 JSON 帧。
///
/// # Errors
///
/// 当 JSON 无法编码、帧超过限制或底层流写入失败时返回错误。
pub async fn write_frame<T, W>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    T: Serialize,
    W: AsyncWrite + Unpin,
{
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_SIZE {
        return Err(FrameError::FrameTooLarge(bytes.len()));
    }
    let length = u32::try_from(bytes.len()).map_err(|_| FrameError::FrameTooLarge(bytes.len()))?;
    writer.write_u32(length).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use remoteops_domain::{ControllerInstanceId, ControllerOwnerId};
    use tokio::io::duplex;

    use crate::{ClientHello, ControllerHello, ControllerKind, PROTOCOL_VERSION, WireMessage};

    use super::*;

    #[tokio::test]
    async fn frame_codec_round_trips_message() {
        let message = WireMessage::Hello(ClientHello::Controller(ControllerHello {
            protocol_version: PROTOCOL_VERSION,
            controller_instance_id: ControllerInstanceId::new(),
            owner_id: ControllerOwnerId::new(),
            kind: ControllerKind::Human,
            auth_token: "test-controller-token".to_owned(),
        }));
        let (mut client, mut server) = duplex(4096);

        let expected = message.clone();
        let writer = tokio::spawn(async move {
            write_frame(&mut client, &message)
                .await
                .expect("协议帧应写入成功");
        });
        let decoded: WireMessage = read_frame(&mut server).await.expect("协议帧应读取成功");
        writer.await.expect("写入任务不应失败");

        assert_eq!(decoded, expected);
    }
}
