//! 单次人工桌面流的 `MiniRTC` 媒体协商。键鼠始终走原有人工租约。
use serde::{Deserialize, Serialize};
use std::fmt;

/// 仅对已配对桌面流签发的限时 TURN 凭据。调试输出不得包含密码。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnGrant {
    /// TURN 公网 IPv4 地址。
    pub address: std::net::Ipv4Addr,
    /// TURN UDP 监听端口。
    pub port: u16,
    /// 凭据到期的 Unix 时间。
    pub expires: i64,
    /// 限时用户名。
    pub username: String,
    /// 此用户名对应的 HMAC，无法用于生成其他凭据。
    pub password: String,
}
impl fmt::Debug for TurnGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnGrant")
            .field("expires", &self.expires)
            .finish_non_exhaustive()
    }
}

/// 当前流的 SDP 或 ICE 消息，仅可由已配对的两端交换。
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Signal(pub String);
impl fmt::Debug for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signal({} bytes)", self.0.len())
    }
}
