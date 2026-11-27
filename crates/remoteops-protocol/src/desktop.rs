//! 实时桌面的有界媒体帧、观看参数和人工输入契约。
use std::fmt;
#[path = "desktop_clipboard.rs"]
pub mod clipboard;
#[path = "desktop_fps.rs"]
pub mod fps;
#[path = "desktop_rtc.rs"]
pub mod rtc;
use std::net::{Ipv4Addr, SocketAddrV4};

#[cfg(feature = "desktop-lan")]
#[path = "desktop_lan.rs"]
pub mod lan;
use std::time::Duration;

use remoteops_domain::{RequestId, SessionId};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

/// 同一桌面流最长无控制链心跳时间。
pub const LEASE: Duration = Duration::from_secs(5);
/// 单个 H.264 访问单元允许的最大字节数。
pub const MAX_VIDEO_BYTES: usize = 2 * 1024 * 1024;

/// 媒体接入票据，经已配对的控制连接分发，一次使用。
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ticket(Uuid);

impl Ticket {
    /// 为媒体连接签发随机票据。
    #[must_use]
    pub fn issue() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Debug for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ticket([redacted])")
    }
}

/// 主显示器传输尺寸与帧率上限，按原始宽高比缩放。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Quality {
    /// 弱网，480p/8 帧。
    Low,
    /// 常规，720p/15 帧。
    #[default]
    Balanced,
    /// 高清，1080p/30 帧。
    High,
}

impl Quality {
    /// 返回宽、高、帧率、目标码率上限。
    #[must_use]
    pub const fn limits(self) -> (u32, u32, u32, u32) {
        match self {
            Self::Low => (854, 480, 8, 600_000),
            Self::Balanced => (1280, 720, 15, 2_000_000),
            Self::High => (1920, 1080, 30, 6_000_000),
        }
    }
}

/// 仅表示键鼠事件，坐标使用原始主屏归一化物理坐标。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Input {
    /// 移动鼠标到指定位置。
    Move { x: u16, y: u16 },
    /// 按下或释放鼠标键，0、1、2 分别为左、右、中。
    Button { button: u8, down: bool },
    /// 垂直滚轮增量。
    Wheel { delta: i16 },
    /// Windows 虚拟键。
    Key { vk: u8, down: bool },
    /// 释放当前桌面流持有的全部输入。
    Release,
}

/// 已配对控制链上的实时桌面事件。视频内容使用独立 TLS 套接字。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesktopEvent {
    /// 父会话。
    pub session: SessionId,
    /// 每次打开重新产生的标识。
    pub stream: RequestId,
    /// 消息内容。
    pub action: Action,
}

/// 桌面控制命令与回执。Relay 按消息来源和当前绑定逐项校验。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Action {
    /// 人工请求打开只看窗口。
    Open,
    /// Relay 向各端发送该端专有的单次媒体票据。
    Attach(Ticket),
    /// 两端都需独立发送控制通道心跳。
    Heartbeat,
    /// Agent 同意共享，向控制端报告当前采集代次与物理像素。
    Geometry {
        epoch: u64,
        width: u32,
        height: u32,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        lan: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        clipboard: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        mini_rtc: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        crossdesk: bool,
    },
    /// 仅能力协商成功后，面板为当前流申请 TURN 媒体。
    RtcRequest,
    /// Relay 分别向已认证的两端提供各自的临时凭据。
    RtcGranted(rtc::TurnGrant),
    /// 当前桌面流内有界的媒体信令。
    RtcSignal(rtc::Signal),
    /// 放弃可选媒体链并回到原媒体连接。
    RtcStop,
    /// 运维端已收到 `MiniRTC` 视频关键帧，可以暂停 TCP 视频。
    RtcReady,
    /// 已配对的运维端在支持 `CrossDesk` 的现场流上申请一次性观看票据。
    CrossDeskRequest,
    /// 绑定当前会话和桌面流的观看票据。键鼠权限仍须现场回执。
    CrossDeskGrant(Ticket),
    /// 上游一次性会合信息只经已鉴权的现场控制链交给本流运维端。
    CrossDeskOffer(rtc::Signal),
    /// 运维端已显示本后端的首个关键帧。
    CrossDeskReady,
    /// 放弃当前流的 `CrossDesk` 媒体，回到原有画面。
    CrossDeskStop,
    /// 已配对面板提供本机候选地址和本流临时客户端证书。
    LanRequest {
        addresses: Vec<Ipv4Addr>,
        certificate: Vec<u8>,
        /// 已绑定本机私网地址的一次性反向监听端点。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reverse: Vec<SocketAddrV4>,
    },
    /// Agent 回答所选端点和本流临时服务端证书。
    LanOffer {
        address: SocketAddrV4,
        certificate: Vec<u8>,
        /// Agent 选中的反向端点。旧版缺省仅尝试正向连接。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reverse: Option<SocketAddrV4>,
    },
    /// Relay 对 Agent 确认的当代人工租约签发直连输入许可。
    LanPermit { epoch: u64 },
    /// 请求或确认人工桌面输入租约；来自 Agent 的回执才解锁面板。
    Control { enabled: bool, epoch: u64 },
    /// 经租约与画面代次验证后注入人工键鼠。
    Input { epoch: u64, event: Input },
    /// 更新视频设置。
    Settings {
        quality: Quality,
        fps: u8,
        bitrate: u32,
    },
    /// 当前输入租约下独立开关剪贴板。每次启用使用新的通道标识。
    ClipboardShare {
        epoch: u64,
        channel: RequestId,
        enabled: bool,
    },
    /// 已开启的通道上发送有界内容，流、租约和通道均须匹配。
    ClipboardData {
        epoch: u64,
        channel: RequestId,
        packet: clipboard::Packet,
    },
    /// 关闭桌面并释放关联资源。
    Closed,
}

/// 读取一个 H.264 访问单元。先限制帧长，再为内容分配空间。
/// # Errors
/// 帧长度无效或流关闭时返回错误。
pub async fn read_video<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let size = reader.read_u32().await? as usize;
    if !(1..=MAX_VIDEO_BYTES).contains(&size) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "桌面视频帧长度无效",
        ));
    }
    let mut data = vec![0; size];
    reader.read_exact(&mut data).await?;
    Ok(data)
}

/// 写入一个有界 H.264 访问单元。
/// # Errors
/// 帧长度无效或写入失败时返回错误。
pub async fn write_video<W: AsyncWrite + Unpin>(
    writer: &mut W,
    data: &[u8],
) -> std::io::Result<()> {
    let size = u32::try_from(data.len())
        .ok()
        .filter(|_| (1..=MAX_VIDEO_BYTES).contains(&data.len()))
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "桌面视频帧长度无效")
        })?;
    writer.write_u32(size).await?;
    writer.write_all(data).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_invalid_sizes_before_allocation() {
        for size in [0u32, u32::try_from(MAX_VIDEO_BYTES + 1).unwrap()] {
            assert_eq!(
                read_video(&mut size.to_be_bytes().as_slice())
                    .await
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidData
            );
        }
        assert!(write_video(&mut tokio::io::sink(), &[]).await.is_err());
        assert!(read_video(&mut [0, 0, 0, 4, 1].as_slice()).await.is_err());
    }
}
