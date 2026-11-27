//! 可选 MiniRTC 子进程。每个实例只服务于一条已授权桌面流。
use remoteops_protocol::desktop::{Ticket, rtc::TurnGrant};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::windows::{fs::OpenOptionsExt, process::CommandExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
};

fn worker_path() -> io::Result<PathBuf> {
    Ok(std::env::current_exe()?.with_file_name("remoteops-minirtc.exe"))
}

/// 只公告随本产品构建时固定摘要的可选组件。
pub fn available() -> bool {
    option_env!("REMOTEOPS_MINIRTC_SHA256").is_some_and(|h| h.len() == 64)
        && worker_path().is_ok_and(|p| p.is_file())
}

/// 媒体子进程输出的有界消息。
pub enum Event {
    Signal(String),
    Video(Vec<u8>),
    Ready,
    /// 上游统计已确认 SRTP，参数指示是否实际使用 TURN。
    CrossDeskRoute(bool),
    Failed,
}

/// 子进程、只读映像锁和该桌面流专用的 IPC 队列。
pub struct Peer {
    child: Child,
    _image: File,
    tx: SyncSender<(u8, Vec<u8>)>,
    rx: Receiver<Event>,
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Peer {
    /// 校验本机构建记录并启动媒体组件。凭据只写入匿名管道。
    pub fn start(sender: bool, stream: &str, grant: &TurnGrant) -> io::Result<Self> {
        let path = worker_path()?;
        let init = serde_json::json!({"sender":sender,"stream":stream,"grant":grant});
        Self::spawn(path,option_env!("REMOTEOPS_MINIRTC_SHA256"), &init)
    }

    /// 两端都从本地构建配置读取私有信令地址。网络消息不能覆盖信令主机。
    pub fn start_crossdesk(sender: bool, ticket: &Ticket) -> io::Result<Self> {
        let address = option_env!("REMOTEOPS_CROSSDESK_SIGNAL_HOST").filter(|s| !s.is_empty())
            .ok_or_else(|| io::Error::other("缺少私有 CrossDesk 信令主机"))?;
        let port = option_env!("REMOTEOPS_CROSSDESK_SIGNAL_PORT").and_then(|s|s.parse::<u16>().ok())
            .filter(|p| *p > 0).ok_or_else(|| io::Error::other("缺少私有 CrossDesk 信令端口"))?;
        let path = std::env::current_exe()?.with_file_name("remoteops-crossdesk.exe");
        let logs = std::env::temp_dir().join("RemoteOps-CrossDesk-Logs");
        std::fs::create_dir_all(&logs)?;
        let init = serde_json::json!({"sender":sender,"ticket":ticket,"host":address,"port":port,
            "log_directory":logs,"force_turn":option_env!("REMOTEOPS_CROSSDESK_FORCE_TURN")==Some("1")});
        Self::spawn(path,option_env!("REMOTEOPS_CROSSDESK_SHA256"), &init)
    }

    /// 当前构建与安装目录均提供可验证的私有后端时才协商。
    pub fn crossdesk_available() -> bool {
        option_env!("REMOTEOPS_CROSSDESK_SHA256").is_some_and(|hash|hash.len()==64)
            && option_env!("REMOTEOPS_CROSSDESK_SIGNAL_HOST").is_some_and(|host| !host.is_empty())
            && option_env!("REMOTEOPS_CROSSDESK_SIGNAL_PORT").and_then(|s|s.parse::<u16>().ok()).is_some_and(|p|p>0)
            && std::env::current_exe().is_ok_and(|p| p.with_file_name("remoteops-crossdesk.exe").is_file())
    }

    fn spawn(path:PathBuf, expected:Option<&str>, init:&serde_json::Value) -> io::Result<Self> {
        let mut image = OpenOptions::new().read(true).share_mode(1).open(&path)?;
        let mut hash = Sha256::new();
        io::copy(&mut image, &mut hash)?;
        let expected = expected.unwrap_or("");
        if !expected.eq_ignore_ascii_case(&format!("{:x}", hash.finalize())) {
            return Err(io::Error::other("媒体组件与本次构建摘要不匹配"));
        }
        let mut child = Command::new(path)
            .creation_flags(0x0800_0000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let (tx, writes) = mpsc::sync_channel::<(u8, Vec<u8>)>(3);
        let (events, rx) = mpsc::sync_channel(8);
        thread::spawn(move || {
            for (kind, bytes) in writes {
                if input
                    .write_all(&[kind])
                    .and_then(|()| input.write_all(&(bytes.len() as u32).to_le_bytes()))
                    .and_then(|()| input.write_all(&bytes))
                    .is_err()
                {
                    break;
                }
            }
        });
        thread::spawn(move || {
            loop {
                let mut header = [0; 5];
                if output.read_exact(&mut header).is_err() {
                    break;
                }
                let n = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
                let bound = match header[0] {
                    b'S' => 131_072,
                    b'V' => 2_097_152,
                    b'R' | b'E' | b'P' | b'T' => 0,
                    _ => break,
                };
                if n > bound {
                    break;
                }
                let mut bytes = vec![0; n];
                if output.read_exact(&mut bytes).is_err() {
                    break;
                }
                let item = match header[0] {
                    b'S' => match String::from_utf8(bytes) {
                        Ok(t) => Event::Signal(t),
                        Err(_) => break,
                    },
                    b'V' => Event::Video(bytes),
                    b'R' => Event::Ready,
                    b'P' | b'T' => Event::CrossDeskRoute(header[0] == b'T'),
                    _ => Event::Failed,
                };
                if events.send(item).is_err() {
                    break;
                }
            }
        });
        let peer = Self {
            child,
            _image: image,
            tx,
            rx,
        };
        let init = serde_json::to_vec(init)?;
        peer.tx
            .try_send((b'I', init))
            .map_err(|_| io::Error::other("MiniRTC 管道关闭"))?;
        Ok(peer)
    }

    /// 处理已在父会话中确认来源的信令。
    pub fn signal(&self, msg: String) -> bool {
        msg.len() <= 131_072 && self.tx.try_send((b'S', msg.into_bytes())).is_ok()
    }
    /// 由父会话已经校验的控制链心跳为 CrossDesk 子进程续租。
    pub fn heartbeat(&self) -> bool {
        self.tx.try_send((b'H', Vec::new())).is_ok()
    }
    /// 提交一帧 BGRA；背压时丢弃过期画面。
    pub fn frame(&self, w: u32, h: u32, pixels: &[u8]) {
        if w > 1920 || h > 1080 || pixels.len() != w as usize * h as usize * 4 {
            return;
        }
        let mut data = Vec::with_capacity(8 + pixels.len());
        data.extend(w.to_le_bytes());
        data.extend(h.to_le_bytes());
        data.extend_from_slice(pixels);
        let _ = self.tx.try_send((b'F', data));
    }
    /// 取出一个事件，管道结束视为媒体故障以触发 TCP 回退。
    pub fn poll(&self) -> Option<Event> {
        match self.rx.try_recv() {
            Ok(e) => Some(e),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Event::Failed),
        }
    }
}
