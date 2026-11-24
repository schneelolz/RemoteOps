//! 带完整性校验的单个剪贴板消息。文件内容按实际读取范围传输。
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// 原生剪贴板每条消息上限，含文件清单或一个文件分块。
pub const MAX_PACKET: usize = 256 * 1024;

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Packet {
    bytes: String,
    digest: String,
}
impl fmt::Debug for Packet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardPacket").field("encoded_len",&self.bytes.len()).finish()
    }
}
impl Packet {
    /// 校验消息大小，并编码当前包的 SHA-256。
    pub fn encode(data: &[u8]) -> Option<Self> {
        if data.is_empty() || data.len() > MAX_PACKET {return None;}
        Some(Self {bytes:STANDARD.encode(data),digest:STANDARD.encode(Sha256::digest(data))})
    }
    /// 中继只验证边界，避免解码并接触用户剪贴板。
    pub fn bounded(&self) -> bool {
        !self.bytes.is_empty() && self.bytes.len() <= MAX_PACKET.div_ceil(3)*4 && self.digest.len()==44
    }
    /// 端点校验完整性后才交给系统剪贴板。
    pub fn decode(&self) -> Option<Vec<u8>> {
        if !self.bounded() {return None;}
        let data=STANDARD.decode(&self.bytes).ok()?;
        (data.len() <= MAX_PACKET && STANDARD.encode(Sha256::digest(&data))==self.digest).then_some(data)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contents_are_bounded_verified_and_redacted() {
        let mut p=Packet::encode(b"private clipboard").unwrap();
        assert_eq!(p.decode().unwrap(),b"private clipboard");
        assert!(!format!("{p:?}").contains(&p.bytes));
        p.bytes=STANDARD.encode("altered bytes");
        assert!(p.decode().is_none());
        assert!(Packet::encode(&vec![0;MAX_PACKET+1]).is_none());
    }
}
