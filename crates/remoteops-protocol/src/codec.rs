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
    use remoteops_domain::{AgentInstanceId, ControllerInstanceId, ControllerOwnerId};
    use tokio::io::duplex;

    use crate::{
        AgentHello, ClientHello, ControllerHello, ControllerKind, PROTOCOL_VERSION, WireMessage,
    };

    use super::*;

    #[tokio::test]
    async fn frame_codec_round_trips_message() {
        let message = WireMessage::Hello(ClientHello::Controller(ControllerHello {
            protocol_version: PROTOCOL_VERSION,
            controller_instance_id: ControllerInstanceId::new(),
            owner_id: ControllerOwnerId::new(),
            kind: ControllerKind::Human,
            auth_token: "test-controller-token".to_owned(),
            hostname: None,
            mac_address: None,
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

    #[test]
    fn legacy_controller_hello_defaults_new_identity_fields() {
        let value = serde_json::json!({
            "role": "controller",
            "protocol_version": PROTOCOL_VERSION,
            "controller_instance_id": "00000000-0000-0000-0000-000000000001",
            "owner_id": "00000000-0000-0000-0000-000000000002",
            "kind": "ai",
            "auth_token": "legacy-controller-token"
        });
        let hello: ClientHello =
            serde_json::from_value(value).expect("旧 Controller Hello 应可解析");
        let ClientHello::Controller(hello) = hello else {
            panic!("应解析为 Controller Hello");
        };
        assert_eq!(hello.hostname, None);
        assert_eq!(hello.mac_address, None);
    }

    #[test]
    fn legacy_agent_hello_defaults_mac_address() {
        let mut value = serde_json::to_value(ClientHello::Agent(AgentHello {
            protocol_version: PROTOCOL_VERSION,
            agent_instance_id: AgentInstanceId::new(),
            boot_id: remoteops_domain::AgentBootId::new(),
            target_context: None,
            pairing_mode: crate::PairingMode::AutoPerBoot,
            managed_enrollment: None,
            resume_token: None,
            hostname: "legacy-agent".to_owned(),
            operating_system: "Windows".to_owned(),
            capabilities: remoteops_domain::CapabilitySet::default(),
            environment: remoteops_domain::EnvironmentProfile::empty(),
            credential_encryption_public_key: String::new(),
            credential_encryption_key_id: String::new(),
            mac_address: None,
            supports_agent_shutdown: false,
        }))
        .expect("Agent Hello 应可序列化");
        let mut legacy = value.clone();
        let object = legacy.as_object_mut().unwrap();
        object.insert(
            "protocol_version".to_owned(),
            crate::LEGACY_PROTOCOL_VERSION.into(),
        );
        for key in ["target_context", "pairing_mode", "managed_enrollment"] {
            object.remove(key);
        }
        let ClientHello::Agent(legacy) =
            serde_json::from_value(legacy).expect("协议17 Hello应保留旧模式")
        else {
            panic!("应为Agent")
        };
        assert_eq!(legacy.pairing_mode, crate::PairingMode::AutoPerBoot);
        assert!(legacy.target_context.is_none() && legacy.managed_enrollment.is_none());
        let mut missing_boot_id = value.clone();
        missing_boot_id.as_object_mut().unwrap().remove("boot_id");
        assert!(
            serde_json::from_value::<ClientHello>(missing_boot_id).is_err(),
            "缺少启动标识时必须拒绝握手"
        );
        value
            .as_object_mut()
            .expect("Agent Hello 应为对象")
            .remove("mac_address");
        let hello: ClientHello = serde_json::from_value(value).expect("旧 Agent Hello 应可解析");
        let ClientHello::Agent(hello) = hello else {
            panic!("应解析为 Agent Hello");
        };
        assert_eq!(hello.mac_address, None);
    }
}
