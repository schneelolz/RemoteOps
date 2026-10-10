//! Shell、SSH、串口、文件和端口探测的系统能力适配层。
//!
//! 本 crate 只负责在已经通过上层策略检查后执行具体能力；它不会根据自然
//! 语言判断权限，也不会替代 Agent 或 Relay 的 Session/审批校验。

mod file_commit;
pub use file_commit::FileCommitControl;

use std::{
    env,
    fs::OpenOptions as StdOpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    ops::{Deref, DerefMut},
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Instant,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
#[cfg(windows)]
use local_encoding_ng::{Encoder, windows::EncoderCodePage};
use process_wrap::tokio::ChildWrapper;
#[cfg(unix)]
use process_wrap::tokio::{CommandWrap, KillOnDrop, ProcessGroup};
use remoteops_domain::{
    PowerAction, SerialDataBits, SerialFlowControl, SerialParity, SerialSettings, SerialStopBits,
    ServiceAction, ShellKind,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    io::{AsyncRead as TokioAsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    process::{ChildStdin, Command},
    sync::{Mutex as AsyncMutex, Notify, mpsc},
    time::{Duration, timeout},
};
#[cfg(windows)]
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use zeroize::{Zeroize as _, Zeroizing};
#[cfg(windows)]
const WINDOWS_OEM_CODE_PAGE: u32 = 1;

/// 单次命令允许产生的标准输出和标准错误总字节数。
pub const MAX_COMMAND_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

// 请求标识用于输出分帧，必须保持解析窗口有界（Agent 当前使用 UUID）。
const MAX_SHELL_COMMAND_TAG_BYTES: usize = 128;

/// 单个文件分块允许的最大字节数。
pub const MAX_FILE_CHUNK_BYTES: usize = 1024 * 1024;

/// 一次 SSH 主机密钥扫描的本地确认信息。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshHostKeyScan {
    /// 目标主机。
    pub host: String,
    /// SSH 端口。
    pub port: u16,
    /// `ssh-keyscan` 返回的 `known_hosts` 记录。
    entries: Vec<String>,
    /// 供现场用户核对的 SHA-256 指纹。
    pub fingerprints: Vec<String>,
}

impl SshHostKeyScan {
    /// 返回扫描到的主机密钥数量。
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.entries.len()
    }
}

// 为同一进程内的临时文件名提供无锁递增后缀。
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);
// 防止多个首次 SSH 连接并行更新同一个 known_hosts 文件。
static SSH_KNOWN_HOSTS_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
struct ManagedChild {
    inner: Box<dyn ChildWrapper>,
    #[cfg(windows)]
    job: Option<win32job::Job>,
}

#[derive(Clone, Copy, Debug)]
enum ProcessOutputEncoding {
    Utf8,
    #[cfg(not(windows))]
    PlainUtf8,
    #[cfg(windows)]
    WindowsCmdAuto,
    #[cfg(windows)]
    WindowsOem,
}

impl ProcessOutputEncoding {
    fn for_shell(shell: ShellKind) -> Self {
        #[cfg(windows)]
        if matches!(shell, ShellKind::Cmd | ShellKind::System) {
            return Self::WindowsCmdAuto;
        }
        let _ = shell;
        #[cfg(not(windows))]
        return Self::PlainUtf8;
        #[cfg(windows)]
        Self::Utf8
    }

    fn for_interactive_shell(shell: ShellKind) -> Self {
        #[cfg(windows)]
        if matches!(shell, ShellKind::Cmd | ShellKind::System) {
            return Self::WindowsOem;
        }
        let _ = shell;
        #[cfg(not(windows))]
        return Self::PlainUtf8;
        #[cfg(windows)]
        Self::Utf8
    }
}

#[derive(Debug)]
struct ProcessOutputDecoder {
    encoding: ProcessOutputEncoding,
    pending: Vec<u8>,
    #[cfg(not(windows))]
    terminal: TerminalEscapeFilter,
}

impl ProcessOutputDecoder {
    fn new(encoding: ProcessOutputEncoding) -> Self {
        Self {
            encoding,
            pending: Vec::new(),
            #[cfg(not(windows))]
            terminal: TerminalEscapeFilter::default(),
        }
    }

    fn push(&mut self, bytes: &[u8], final_chunk: bool) -> String {
        self.pending.extend_from_slice(bytes);
        match self.encoding {
            ProcessOutputEncoding::Utf8 => decode_utf8_incremental(&mut self.pending, final_chunk),
            #[cfg(not(windows))]
            ProcessOutputEncoding::PlainUtf8 => self
                .terminal
                .push(&decode_utf8_incremental(&mut self.pending, final_chunk)),
            #[cfg(windows)]
            ProcessOutputEncoding::WindowsCmdAuto => {
                decode_windows_cmd_incremental(&mut self.pending, final_chunk)
            }
            #[cfg(windows)]
            ProcessOutputEncoding::WindowsOem => decode_windows_code_page_incremental(
                WINDOWS_OEM_CODE_PAGE,
                &mut self.pending,
                final_chunk,
            ),
        }
    }
}

#[cfg(not(windows))]
#[derive(Debug, Default)]
enum TerminalEscapeFilter {
    #[default]
    Text,
    Escape,
    Csi,
    String,
    StringEscape,
}

#[cfg(not(windows))]
impl TerminalEscapeFilter {
    fn push(&mut self, text: &str) -> String {
        let mut clean = String::with_capacity(text.len());
        for ch in text.chars() {
            match self {
                Self::Text if ch == '\u{1b}' => *self = Self::Escape,
                Self::Text => clean.push(ch),
                Self::Escape => {
                    *self = match ch {
                        '[' => Self::Csi,
                        ']' | 'P' | '^' | '_' => Self::String,
                        _ => Self::Text,
                    };
                }
                Self::Csi if ('@'..='~').contains(&ch) => *self = Self::Text,
                Self::String if ch == '\u{7}' => *self = Self::Text,
                Self::String if ch == '\u{1b}' => *self = Self::StringEscape,
                Self::Csi | Self::String => {}
                Self::StringEscape => {
                    *self = if ch == '\\' { Self::Text } else { Self::String };
                }
            }
        }
        clean
    }
}

impl Deref for ManagedChild {
    type Target = dyn ChildWrapper;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref()
    }
}

impl DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut()
    }
}

impl ManagedChild {
    /// Windows Job Object 会继续包含父进程创建的后代；正常等待只应跟踪顶层进程。
    async fn wait_for_parent(&mut self) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(windows)]
        {
            self.inner.inner_mut().wait().await
        }
        #[cfg(not(windows))]
        {
            self.inner.wait().await
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        let _ = self.inner.start_kill();
        #[cfg(windows)]
        drop(self.job.take());
    }
}

fn spawn_managed_process(command: Command) -> Result<ManagedChild, DeviceError> {
    #[cfg(windows)]
    {
        let mut command = command;
        command.creation_flags(CREATE_NO_WINDOW.0);
        let job = win32job::Job::create()
            .map_err(|error| DeviceError::Operation(format!("无法创建 Windows Job：{error}")))?;
        let mut limits = job.query_extended_limit_info().map_err(|error| {
            DeviceError::Operation(format!("无法读取 Windows Job 限制：{error}"))
        })?;
        limits.limit_kill_on_job_close();
        job.set_extended_limit_info(&limits).map_err(|error| {
            DeviceError::Operation(format!("无法设置 Windows Job 限制：{error}"))
        })?;
        let mut child = command
            .spawn()
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        let process_handle = child
            .raw_handle()
            .ok_or_else(|| DeviceError::Operation("无法取得 Windows 子进程句柄".to_owned()))?;
        if let Err(error) = job.assign_process(process_handle as isize) {
            let _ = child.start_kill();
            return Err(DeviceError::Operation(format!(
                "无法把 Windows 子进程加入 Job：{error}"
            )));
        }
        Ok(ManagedChild {
            inner: Box::new(child),
            job: Some(job),
        })
    }
    #[cfg(unix)]
    {
        let mut command = CommandWrap::from(command);
        command.wrap(KillOnDrop);
        command.wrap(ProcessGroup::leader());
        command
            .spawn()
            .map(|inner| ManagedChild { inner })
            .map_err(|error| DeviceError::Operation(error.to_string()))
    }
}

async fn terminate_process_tree(child: &mut ManagedChild) -> Result<(), DeviceError> {
    #[cfg(windows)]
    if let Some(job) = child.job.take() {
        drop(job);
        child
            .wait_for_parent()
            .await
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        return Ok(());
    }
    if let Err(kill_error) = child.start_kill() {
        if child
            .try_wait()
            .map_err(|error| DeviceError::Operation(error.to_string()))?
            .is_some()
        {
            return Ok(());
        }
        return Err(DeviceError::Operation(kill_error.to_string()));
    }
    child
        .wait_for_parent()
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?;
    Ok(())
}

/// 设备适配器统一错误。
#[derive(Debug, Error)]
pub enum DeviceError {
    /// Agent 不具备请求的能力。
    #[error("当前 Agent 不支持该能力：{0}")]
    Unsupported(String),
    /// 输入参数无效。
    #[error("设备参数无效：{0}")]
    InvalidInput(String),
    /// 系统或设备访问失败。
    #[error("设备访问失败：{0}")]
    Operation(String),
    /// 操作超时。
    #[error("设备操作超时")]
    Timeout,
    /// 命令输出超过安全上限。
    #[error("命令输出超过 {limit} 字节安全上限")]
    OutputLimit {
        /// 当前配置的总输出上限。
        limit: usize,
    },
}

/// 一次命令执行的完整结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandResult {
    /// 标准输出。
    pub stdout: String,
    /// 标准错误。
    pub stderr: String,
    /// 进程退出码。
    pub exit_code: Option<i32>,
}

/// 命令执行期间产生的一段实时输出。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandOutputChunk {
    /// 是否来自标准错误。
    pub stderr: bool,
    /// 使用 UTF-8 损失转换后的文本。
    pub text: String,
}

/// TCP 端口探测结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortProbeResult {
    /// 目标主机。
    pub host: String,
    /// 目标端口。
    pub port: u16,
    /// 是否连接成功。
    pub open: bool,
    /// 探测耗时。
    pub elapsed_millis: u128,
    /// 失败时的安全摘要。
    pub error: Option<String>,
}

/// 文件元数据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileMetadata {
    /// 相对文件路径。
    pub path: String,
    /// 文件字节数。
    pub size: u64,
    /// 最后修改时间的 Unix 毫秒；系统不支持时为空。
    pub modified_unix_millis: Option<u128>,
}

/// 一次文件分块读取结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileReadChunk {
    /// 分块原始字节。
    pub bytes: Vec<u8>,
    /// 本次读取后是否已经到达文件末尾。
    pub eof: bool,
}

/// 指定 TCP 目标的有界收发结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TcpExchangeResult {
    /// 目标主机。
    pub host: String,
    /// 目标端口。
    pub port: u16,
    /// 已发送字节数。
    pub sent_bytes: usize,
    /// 收到的原始字节。
    pub received: Vec<u8>,
    /// 是否因读取超时而停止。
    pub read_timed_out: bool,
}

/// 串口信息。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SerialPortInfo {
    /// 平台串口名称。
    pub port_name: String,
    /// 可选设备类型说明。
    pub port_type: Option<String>,
}

/// 本地 Shell 适配器。
#[async_trait]
pub trait ShellProvider: Send + Sync {
    /// 返回当前可用 Shell。
    async fn available_shells(&self) -> Result<Vec<ShellKind>, DeviceError>;

    /// 执行一次非交互命令。
    async fn run(
        &self,
        shell: ShellKind,
        command: &str,
        timeout_seconds: u64,
    ) -> Result<CommandResult, DeviceError>;

    /// 执行一次非交互命令，并在进程运行时逐块发送输出。
    async fn run_streaming(
        &self,
        shell: ShellKind,
        command: &str,
        timeout_seconds: u64,
        output: mpsc::UnboundedSender<CommandOutputChunk>,
    ) -> Result<CommandResult, DeviceError>;
}

/// TCP 端口探测适配器。
#[async_trait]
pub trait PortProbeProvider: Send + Sync {
    /// 探测远端 TCP 端口。
    async fn test_port(
        &self,
        host: &str,
        port: u16,
        timeout_millis: u64,
    ) -> Result<PortProbeResult, DeviceError>;
}

/// Agent 文件系统适配器。
#[async_trait]
pub trait FileTransferProvider: Send + Sync {
    /// 读取文件。
    async fn read_file(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, DeviceError>;

    /// 写入文件。
    async fn write_file(
        &self,
        path: &str,
        bytes: &[u8],
        overwrite: bool,
    ) -> Result<(), DeviceError>;

    /// 读取普通文件元数据。
    async fn file_metadata(&self, path: &str) -> Result<FileMetadata, DeviceError>;

    /// 移动或重命名普通文件。
    async fn move_file(
        &self,
        source_path: &str,
        destination_path: &str,
        overwrite: bool,
    ) -> Result<(), DeviceError>;

    /// 删除单个普通文件。
    async fn delete_file(&self, path: &str) -> Result<(), DeviceError>;
}

/// 指定 TCP 目标的受控收发适配器。
#[async_trait]
pub trait TcpExchangeProvider: Send + Sync {
    /// 连接单个明确目标并执行一次有界收发。
    async fn exchange(
        &self,
        host: &str,
        port: u16,
        payload: &[u8],
        max_response_bytes: usize,
        timeout_millis: u64,
    ) -> Result<TcpExchangeResult, DeviceError>;
}

/// 结构化进程、服务和电源操作适配器。
#[async_trait]
pub trait SystemProvider: Send + Sync {
    /// 查询进程列表。
    async fn list_processes(&self) -> Result<CommandResult, DeviceError>;

    /// 终止指定进程树。
    async fn terminate_process(&self, process_id: u32) -> Result<CommandResult, DeviceError>;

    /// 查询服务列表。
    async fn list_services(&self) -> Result<CommandResult, DeviceError>;

    /// 控制指定服务。
    async fn control_service(
        &self,
        service_name: &str,
        action: ServiceAction,
    ) -> Result<CommandResult, DeviceError>;

    /// 执行重启或关机。
    async fn power_control(&self, action: PowerAction) -> Result<CommandResult, DeviceError>;
}

/// Agent 串口适配器。
#[async_trait]
pub trait SerialProvider: Send + Sync {
    /// 枚举串口。
    async fn list_ports(&self) -> Result<Vec<SerialPortInfo>, DeviceError>;
}

/// Agent SSH 适配器。
#[async_trait]
pub trait SshProvider: Send + Sync {
    /// 在 SSH 目标执行命令。
    #[allow(clippy::too_many_arguments)]
    async fn run_command(
        &self,
        host: &str,
        port: u16,
        username: &str,
        password: Option<&str>,
        identity_file: Option<&str>,
        known_hosts_file: Option<&str>,
        command: &str,
        timeout_seconds: u64,
    ) -> Result<CommandResult, DeviceError>;
}

/// 已打开的本机串口会话。
#[derive(Clone)]
pub struct SystemSerialSession {
    inner: Arc<Mutex<Box<dyn serialport::SerialPort>>>,
}

impl SystemSerialSession {
    /// 在阻塞线程中读取最多指定字节；串口超时返回空数组。
    ///
    /// # Errors
    ///
    /// 当串口锁损坏、设备断开或读取失败时返回错误。
    pub async fn read(&self, max_bytes: usize) -> Result<Vec<u8>, DeviceError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut buffer = vec![0_u8; max_bytes.clamp(1, 64 * 1024)];
            let mut port = inner
                .lock()
                .map_err(|_| DeviceError::Operation("串口锁已损坏".to_owned()))?;
            match port.read(&mut buffer) {
                Ok(count) => {
                    buffer.truncate(count);
                    Ok(buffer)
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => Ok(Vec::new()),
                Err(error) => Err(DeviceError::Operation(error.to_string())),
            }
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }

    /// 写入原始字节并刷新。
    ///
    /// # Errors
    ///
    /// 当串口锁损坏、设备断开、写入或刷新失败时返回错误。
    pub async fn write(&self, bytes: Vec<u8>) -> Result<usize, DeviceError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut port = inner
                .lock()
                .map_err(|_| DeviceError::Operation("串口锁已损坏".to_owned()))?;
            write_all_and_flush(&mut **port, &bytes)
                .map_err(|error| DeviceError::Operation(error.to_string()))
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }
}

/// 读写使用独立系统句柄的本机串口会话。
#[derive(Clone)]
pub struct SystemDuplexSerialSession {
    reader: Arc<Mutex<Box<dyn serialport::SerialPort>>>,
    writer: Arc<Mutex<Box<dyn serialport::SerialPort>>>,
}

impl SystemDuplexSerialSession {
    /// 在阻塞线程中读取最多指定字节；串口超时返回空数组。
    ///
    /// # Errors
    ///
    /// 当串口锁损坏、设备断开或读取失败时返回错误。
    pub async fn read(&self, max_bytes: usize) -> Result<Vec<u8>, DeviceError> {
        let reader = self.reader.clone();
        tokio::task::spawn_blocking(move || {
            let mut buffer = vec![0_u8; max_bytes.clamp(1, 64 * 1024)];
            let mut port = reader
                .lock()
                .map_err(|_| DeviceError::Operation("串口锁已损坏".to_owned()))?;
            match port.read(&mut buffer) {
                Ok(count) => {
                    buffer.truncate(count);
                    Ok(buffer)
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => Ok(Vec::new()),
                Err(error) => Err(DeviceError::Operation(error.to_string())),
            }
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }

    /// 写入原始字节并刷新。
    ///
    /// # Errors
    ///
    /// 当串口锁损坏、设备断开、写入或刷新失败时返回错误。
    pub async fn write(&self, bytes: Vec<u8>) -> Result<usize, DeviceError> {
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            let mut port = writer
                .lock()
                .map_err(|_| DeviceError::Operation("串口锁已损坏".to_owned()))?;
            write_all_and_flush(&mut **port, &bytes)
                .map_err(|error| DeviceError::Operation(error.to_string()))
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }

    /// 设置数据终端就绪线（DTR）。
    ///
    /// # Errors
    ///
    /// 当串口锁损坏、设备断开或驱动拒绝设置控制线时返回错误。
    pub async fn set_dtr(&self, level: bool) -> Result<(), DeviceError> {
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            writer
                .lock()
                .map_err(|_| DeviceError::Operation("串口锁已损坏".to_owned()))?
                .write_data_terminal_ready(level)
                .map_err(|error| DeviceError::Operation(error.to_string()))
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }

    /// 设置请求发送线（RTS）。
    ///
    /// # Errors
    ///
    /// 当串口锁损坏、设备断开或驱动拒绝设置控制线时返回错误。
    pub async fn set_rts(&self, level: bool) -> Result<(), DeviceError> {
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            writer
                .lock()
                .map_err(|_| DeviceError::Operation("串口锁已损坏".to_owned()))?
                .write_request_to_send(level)
                .map_err(|error| DeviceError::Operation(error.to_string()))
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }
}

fn write_all_and_flush(writer: &mut dyn Write, bytes: &[u8]) -> std::io::Result<usize> {
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(bytes.len())
}

/// 子进程状态由独立任务持有，中断不需要等待命令的 I/O 锁。
type ShellExit = Option<Result<Option<i32>, String>>;

struct InteractiveShellState {
    stdin: ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    stdout_pending: Vec<u8>,
    stderr_pending: Vec<u8>,
    shell: ShellKind,
    output_encoding: ProcessOutputEncoding,
}

/// 一个真实、持久且串行写入的本机交互式 Shell 会话。
#[derive(Clone)]
pub struct SystemInteractiveShellSession {
    inner: Arc<AsyncMutex<InteractiveShellState>>,
    interrupted: Arc<AtomicBool>,
    interrupt_notify: Arc<Notify>,
    closed: Arc<AtomicBool>,
    shutdown: mpsc::UnboundedSender<()>,
    exit: tokio::sync::watch::Receiver<ShellExit>,
}

/// 调用方取消执行 future 时，也不能遗留已部分写入的命令。
struct InteractiveShellRunGuard {
    closed: Arc<AtomicBool>,
    shutdown: mpsc::UnboundedSender<()>,
    completed: bool,
}

impl Drop for InteractiveShellRunGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.closed.store(true, Ordering::Release);
            let _ = self.shutdown.send(());
        }
    }
}

async fn supervise_interactive_shell(
    mut child: ManagedChild,
    mut shutdown: mpsc::UnboundedReceiver<()>,
    closed: Arc<AtomicBool>,
    exit: tokio::sync::watch::Sender<ShellExit>,
) {
    let result = tokio::select! {
        status = child.wait_for_parent() => status.map(|status| status.code())
            .map_err(|error| error.to_string()),
        // 最后一个会话被释放时，通道关闭也会清理整棵进程树。
        _ = shutdown.recv() => terminate_process_tree(&mut child).await
            .map(|()| None).map_err(|error| error.to_string()),
    };
    drop(child);
    closed.store(true, Ordering::Release);
    exit.send_replace(Some(result));
}

async fn wait_for_interactive_shell_exit(
    mut exit: tokio::sync::watch::Receiver<ShellExit>,
) -> Result<Option<i32>, DeviceError> {
    loop {
        if let Some(result) = exit.borrow().clone() {
            return result.map_err(DeviceError::Operation);
        }
        exit.changed()
            .await
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
    }
}

impl SystemInteractiveShellSession {
    /// 返回会话是否已关闭或不再能安全接收命令，不会等待正在执行的 I/O。
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// 返回持久 Shell 是否已经退出或关闭，不等待命令的 I/O 锁。
    ///
    /// # Errors
    ///
    /// 此兼容查询当前不会返回错误。
    #[allow(unknown_lints)]
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn has_exited(&self) -> Result<bool, DeviceError> {
        Ok(self.is_closed())
    }

    /// 在持久 Shell 中执行命令，按有界数据块返回两路实时输出。
    ///
    /// # Errors
    ///
    /// 当命令无效、Shell 已退出、输出超限、超时或被中断时返回错误。
    pub async fn run(
        &self,
        command: &str,
        command_tag: &str,
        timeout_seconds: u64,
        output: mpsc::UnboundedSender<CommandOutputChunk>,
    ) -> Result<CommandResult, DeviceError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds.max(1));
        if command.trim().is_empty() {
            return Err(DeviceError::InvalidInput("命令不能为空".to_owned()));
        }
        if command_tag.is_empty()
            || command_tag.len() > MAX_SHELL_COMMAND_TAG_BYTES
            || !command_tag
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(DeviceError::InvalidInput(
                "持久 Shell 命令标识无效".to_owned(),
            ));
        }
        let mut state = tokio::select! {
            () = wait_for_shell_interrupt(self.interrupted.clone(), self.interrupt_notify.clone()) => {
                return Err(DeviceError::Operation("持久 Shell 已被中断".to_owned()));
            }
            () = tokio::time::sleep_until(deadline) => return Err(DeviceError::Timeout),
            state = self.inner.lock() => state,
        };
        if self.is_closed() {
            return Err(DeviceError::Operation("持久 Shell 进程已经退出".to_owned()));
        }
        let marker = format!("__REMOTEOPS_DONE_{command_tag}__");
        let payload = interactive_command_payload(state.shell, command, &marker);
        let payload = encode_shell_input(state.shell, &payload)?;
        let mut guard = InteractiveShellRunGuard {
            closed: self.closed.clone(),
            shutdown: self.shutdown.clone(),
            completed: false,
        };
        let result = self
            .run_io(&mut state, command, &marker, &payload, deadline, &output)
            .await;
        if result.is_err() {
            self.interrupt().await?;
        }
        guard.completed = result.is_ok();
        result
    }

    #[allow(clippy::too_many_lines)]
    async fn run_io(
        &self,
        state: &mut InteractiveShellState,
        command: &str,
        marker: &str,
        payload: &[u8],
        deadline: tokio::time::Instant,
        output: &mpsc::UnboundedSender<CommandOutputChunk>,
    ) -> Result<CommandResult, DeviceError> {
        let mut stdout = InteractiveShellOutput::new(marker, state.output_encoding, false);
        let mut stderr = InteractiveShellOutput::new(marker, state.output_encoding, true);
        let mut output_bytes = 0;
        stdout.push(
            &std::mem::take(&mut state.stdout_pending),
            &mut output_bytes,
            output,
        )?;
        stderr.push(
            &std::mem::take(&mut state.stderr_pending),
            &mut output_bytes,
            output,
        )?;
        let mut stdout_buffer = [0_u8; 4096];
        let mut stderr_buffer = [0_u8; 4096];
        let mut written = 0;
        let mut flushed = false;
        let mut exit_status = None;
        let exit = self.exit.clone();
        loop {
            if self.interrupted.load(Ordering::Acquire) {
                return Err(DeviceError::Operation("持久 Shell 已被中断".to_owned()));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(DeviceError::Timeout);
            }
            if let (Some(stdout_code), Some(stderr_code)) = (stdout.exit_code, stderr.exit_code) {
                if stdout_code != stderr_code {
                    return Err(DeviceError::Operation(
                        "持久 Shell 输出边界不一致".to_owned(),
                    ));
                }
                state.stdout_pending = stdout.pending;
                state.stderr_pending = stderr.pending;
                return Ok(CommandResult {
                    stdout: stdout.collected,
                    stderr: stderr.collected,
                    exit_code: Some(stdout_code),
                });
            }
            if let Some(code) = exit_status
                && stdout.finished()
                && stderr.finished()
            {
                if is_explicit_shell_exit(state.shell, command) {
                    return Ok(CommandResult {
                        stdout: stdout.collected,
                        stderr: stderr.collected,
                        exit_code: code,
                    });
                }
                return Err(DeviceError::Operation(format!(
                    "持久 Shell 在命令完成前退出，退出码 {code:?}"
                )));
            }
            tokio::select! {
                () = wait_for_shell_interrupt(self.interrupted.clone(), self.interrupt_notify.clone()) => {
                    return Err(DeviceError::Operation("持久 Shell 已被中断".to_owned()));
                }
                () = tokio::time::sleep_until(deadline) => return Err(DeviceError::Timeout),
                status = wait_for_interactive_shell_exit(exit.clone()), if exit_status.is_none() => {
                    exit_status = Some(status?);
                }
                result = async {
                    if written < payload.len() {
                        state.stdin.write(&payload[written..]).await
                    } else {
                        state.stdin.flush().await.map(|()| 0)
                    }
                }, if !flushed => {
                    let count = result.map_err(|error| DeviceError::Operation(error.to_string()))?;
                    if written == payload.len() {
                        flushed = true;
                    } else if count == 0 {
                        return Err(DeviceError::Operation("无法写入持久 Shell 标准输入".to_owned()));
                    } else {
                        written += count;
                    }
                }
                count = state.stdout.read(&mut stdout_buffer), if !stdout.finished() => {
                    let count = count.map_err(|error| DeviceError::Operation(error.to_string()))?;
                    if count == 0 {
                        stdout.finish(&mut output_bytes, output)?;
                    } else {
                        stdout.push(&stdout_buffer[..count], &mut output_bytes, output)?;
                    }
                }
                count = state.stderr.read(&mut stderr_buffer), if !stderr.finished() => {
                    let count = count.map_err(|error| DeviceError::Operation(error.to_string()))?;
                    if count == 0 {
                        stderr.finish(&mut output_bytes, output)?;
                    } else {
                        stderr.push(&stderr_buffer[..count], &mut output_bytes, output)?;
                    }
                }
            }
        }
    }

    /// 关闭持久 Shell 子进程。
    ///
    /// # Errors
    ///
    /// 当无法终止 Shell 子进程时返回错误。
    pub async fn close(&self) -> Result<(), DeviceError> {
        self.interrupt().await
    }

    /// 不等待命令的 I/O 锁，中断并回收持久 Shell 及其子进程。
    ///
    /// # Errors
    ///
    /// 当无法终止 Shell 子进程时返回错误。
    pub async fn interrupt(&self) -> Result<(), DeviceError> {
        self.closed.store(true, Ordering::Release);
        self.interrupted.store(true, Ordering::Release);
        self.interrupt_notify.notify_waiters();
        let _ = self.shutdown.send(());
        wait_for_interactive_shell_exit(self.exit.clone()).await?;
        Ok(())
    }
}

/// 分帧只保留 marker 前缀和最多 12 字节退出码，不依赖换行读取普通输出。
struct InteractiveShellOutput {
    marker: Vec<u8>,
    pending: Vec<u8>,
    decoder: ProcessOutputDecoder,
    collected: String,
    stderr: bool,
    eof: bool,
    exit_code: Option<i32>,
}

impl InteractiveShellOutput {
    fn new(marker: &str, encoding: ProcessOutputEncoding, stderr: bool) -> Self {
        Self {
            marker: marker.as_bytes().to_vec(),
            pending: Vec::new(),
            decoder: ProcessOutputDecoder::new(encoding),
            collected: String::new(),
            stderr,
            eof: false,
            exit_code: None,
        }
    }

    fn finished(&self) -> bool {
        self.eof || self.exit_code.is_some()
    }

    fn emit(
        &mut self,
        count: usize,
        output_bytes: &mut usize,
        output: &mpsc::UnboundedSender<CommandOutputChunk>,
    ) -> Result<(), DeviceError> {
        *output_bytes = output_bytes.saturating_add(count);
        if *output_bytes > MAX_COMMAND_OUTPUT_BYTES {
            return Err(DeviceError::OutputLimit {
                limit: MAX_COMMAND_OUTPUT_BYTES,
            });
        }
        let bytes = self.pending.drain(..count).collect::<Vec<_>>();
        let text = self.decoder.push(&bytes, false);
        self.emit_text(text, output);
        Ok(())
    }

    fn emit_text(&mut self, text: String, output: &mpsc::UnboundedSender<CommandOutputChunk>) {
        if !text.is_empty() {
            self.collected.push_str(&text);
            let _ = output.send(CommandOutputChunk {
                stderr: self.stderr,
                text,
            });
        }
    }

    fn flush_decoder(&mut self, output: &mpsc::UnboundedSender<CommandOutputChunk>) {
        let text = self.decoder.push(&[], true);
        self.emit_text(text, output);
    }

    fn push(
        &mut self,
        bytes: &[u8],
        output_bytes: &mut usize,
        output: &mpsc::UnboundedSender<CommandOutputChunk>,
    ) -> Result<(), DeviceError> {
        self.pending.extend_from_slice(bytes);
        while !self.pending.is_empty() && self.exit_code.is_none() {
            let marker_index = self
                .pending
                .windows(self.marker.len())
                .position(|window| window == self.marker);
            let Some(marker_index) = marker_index else {
                let retained = (1..self.marker.len().min(self.pending.len() + 1))
                    .rev()
                    .find(|&count| self.pending.ends_with(&self.marker[..count]))
                    .unwrap_or(0);
                self.emit(self.pending.len() - retained, output_bytes, output)?;
                break;
            };
            self.emit(marker_index, output_bytes, output)?;
            let suffix = &self.pending[self.marker.len()..];
            if let Some(end) = suffix.iter().position(|&byte| byte == b'\n') {
                let code = suffix[..end].strip_suffix(b"\r").unwrap_or(&suffix[..end]);
                if code.len() <= 11
                    && let Some(code) = std::str::from_utf8(code)
                        .ok()
                        .and_then(|value| value.parse::<i32>().ok())
                {
                    self.exit_code = Some(code);
                    self.pending.drain(..=self.marker.len() + end);
                    self.flush_decoder(output);
                    break;
                }
            } else if suffix.len() <= 12 {
                break;
            }
            // marker 相似文本仍是用户输出；不允许畸形候选无限累积。
            self.emit(1, output_bytes, output)?;
        }
        Ok(())
    }

    fn finish(
        &mut self,
        output_bytes: &mut usize,
        output: &mpsc::UnboundedSender<CommandOutputChunk>,
    ) -> Result<(), DeviceError> {
        self.emit(self.pending.len(), output_bytes, output)?;
        self.flush_decoder(output);
        self.eof = true;
        Ok(())
    }
}

/// Agent 端一次分块文件上传的受控临时会话。
#[derive(Clone, Debug)]
pub struct SystemFileUploadSession {
    /// 最终目标文件。
    destination: Arc<PathBuf>,
    /// 与目标位于同一目录的临时文件。
    temporary: Arc<PathBuf>,
    /// 声明的完整文件大小。
    expected_size: u64,
    /// 声明的完整文件哈希。
    expected_sha256: Arc<str>,
    /// 是否允许替换既有普通文件。
    overwrite: bool,
    /// 已按顺序写入的字节数。
    written: Arc<Mutex<u64>>,
}

impl SystemFileUploadSession {
    /// 返回当前已写入的字节数。
    ///
    /// # Panics
    ///
    /// 当内部上传计数锁已中毒时触发 panic。
    #[must_use]
    pub fn written(&self) -> u64 {
        *self.written.lock().expect("文件上传计数锁不应中毒")
    }

    /// 按精确偏移追加一个分块。
    ///
    /// # Errors
    ///
    /// 当偏移不连续、分块过大或写入失败时返回错误。
    pub async fn write_chunk(&self, offset: u64, bytes: &[u8]) -> Result<(), DeviceError> {
        if bytes.is_empty() || bytes.len() > MAX_FILE_CHUNK_BYTES {
            return Err(DeviceError::InvalidInput(format!(
                "文件分块大小必须在 1 到 {MAX_FILE_CHUNK_BYTES} 字节之间"
            )));
        }
        let bytes = bytes.to_vec();
        let temporary = Arc::clone(&self.temporary);
        let written = Arc::clone(&self.written);
        let expected_size = self.expected_size;
        tokio::task::spawn_blocking(move || {
            let mut current = written
                .lock()
                .map_err(|_| DeviceError::Operation("文件上传计数锁已损坏".to_owned()))?;
            if *current != offset {
                return Err(DeviceError::InvalidInput(format!(
                    "文件分块偏移不连续：期望 {}，收到 {offset}",
                    *current
                )));
            }
            let next = offset
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| DeviceError::InvalidInput("文件分块偏移溢出".to_owned()))?;
            if next > expected_size {
                return Err(DeviceError::InvalidInput(
                    "文件分块超过声明的完整大小".to_owned(),
                ));
            }
            let mut file = open_existing_no_follow(&temporary, true)?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| DeviceError::Operation(error.to_string()))?;
            file.write_all(&bytes)
                .map_err(|error| DeviceError::Operation(error.to_string()))?;
            file.flush()
                .map_err(|error| DeviceError::Operation(error.to_string()))?;
            *current = next;
            Ok(())
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }

    /// 校验完整大小和 SHA-256 后提交临时文件。
    ///
    /// # Errors
    ///
    /// 当文件不完整、哈希不一致或替换失败时返回错误。
    pub async fn complete(&self) -> Result<String, DeviceError> {
        self.complete_with_control(&FileCommitControl::default())
            .await
    }

    /// 校验后提交文件，与取消请求竞争同一个不可逆提交点。
    ///
    /// # Errors
    /// 当文件不完整、哈希不一致、已取消或替换失败时返回错误。
    pub async fn complete_with_control(
        &self,
        control: &FileCommitControl,
    ) -> Result<String, DeviceError> {
        let destination = Arc::clone(&self.destination);
        let temporary = Arc::clone(&self.temporary);
        let expected_size = self.expected_size;
        let expected_sha256 = Arc::clone(&self.expected_sha256);
        let overwrite = self.overwrite;
        let written = Arc::clone(&self.written);
        let _cancel = control.cancel_on_drop();
        let worker = control.worker()?;
        let control = control.clone();
        tokio::task::spawn_blocking(move || {
            let _worker = worker;
            // A chunk or explicit abort cannot race the hash and publication.
            let written = written
                .lock()
                .map_err(|_| DeviceError::Operation("文件上传计数锁已损坏".to_owned()))?;
            let result = (|| {
                control.checkpoint()?;
                if *written != expected_size {
                    return Err(DeviceError::InvalidInput(format!(
                        "上传文件尚未完成：{written} / {expected_size} 字节"
                    )));
                }
                let actual_hash = sha256_file_with_control(&temporary, Some(&control))?;
                if !actual_hash.eq_ignore_ascii_case(&expected_sha256) {
                    return Err(DeviceError::InvalidInput(
                        "上传文件 SHA-256 校验失败".to_owned(),
                    ));
                }
                control.begin_commit()?;
                commit_temporary_file(&temporary, &destination, overwrite)?;
                Ok(actual_hash)
            })();
            // Cancellation is terminal for this temporary upload. Other failures
            // retain it so the caller can inspect or explicitly abort the upload.
            if result.is_err() && control.is_cancelled() {
                let _ = std::fs::remove_file(&*temporary);
            }
            result
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }

    /// 删除尚未提交的临时文件。
    ///
    /// # Errors
    ///
    /// 当临时文件存在但无法删除时返回错误。
    pub async fn abort(&self) -> Result<(), DeviceError> {
        let temporary = Arc::clone(&self.temporary);
        let written = Arc::clone(&self.written);
        tokio::task::spawn_blocking(move || {
            let _written = written
                .lock()
                .map_err(|_| DeviceError::Operation("文件上传计数锁已损坏".to_owned()))?;
            match std::fs::remove_file(&*temporary) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(DeviceError::Operation(error.to_string())),
            }
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }
}

/// 使用操作系统 Shell、SSH、文件、串口和 TCP 能力的默认适配器。
#[derive(Clone, Debug)]
pub struct SystemDevice {
    /// 是否允许使用已配置密钥的无密码 SSH。
    pub allow_ssh_without_password: bool,
    /// Agent 文件上传、下载和 SSH 凭据允许访问的根目录。
    transfer_root: Arc<PathBuf>,
}

impl SystemDevice {
    /// 创建默认设备适配器。
    #[must_use]
    pub fn new() -> Self {
        let root = std::env::current_dir()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("remoteops-transfer");
        Self {
            allow_ssh_without_password: true,
            transfer_root: Arc::new(root),
        }
    }

    /// 创建限定在指定文件交换根目录内的设备适配器。
    ///
    /// # Errors
    ///
    /// 当交换目录无法创建或规范化时返回错误。
    pub fn with_transfer_root(root: impl AsRef<Path>) -> Result<Self, DeviceError> {
        std::fs::create_dir_all(root.as_ref())
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        let canonical = std::fs::canonicalize(root.as_ref())
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        Ok(Self {
            allow_ssh_without_password: true,
            transfer_root: Arc::new(canonical),
        })
    }

    /// 返回规范化后的文件交换根目录。
    #[must_use]
    pub fn transfer_root(&self) -> &Path {
        self.transfer_root.as_path()
    }

    /// 创建一次限定在交换目录内的分块上传会话。
    ///
    /// # Errors
    ///
    /// 当目标路径、大小、哈希或既有文件状态不合法时返回错误。
    pub fn begin_file_upload(
        &self,
        path: &str,
        expected_size: u64,
        expected_sha256: &str,
        overwrite: bool,
    ) -> Result<SystemFileUploadSession, DeviceError> {
        if expected_sha256.len() != 64
            || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(DeviceError::InvalidInput(
                "上传文件 SHA-256 格式无效".to_owned(),
            ));
        }
        let destination = resolve_transfer_destination(self.transfer_root(), path)?;
        if destination.exists() {
            let existing = open_existing_no_follow(&destination, false)?;
            if !existing
                .metadata()
                .map_err(|error| DeviceError::Operation(error.to_string()))?
                .is_file()
            {
                return Err(DeviceError::InvalidInput("只允许覆盖普通文件".to_owned()));
            }
            if !overwrite {
                return Err(DeviceError::Operation("目标文件已存在".to_owned()));
            }
        }
        let temporary = create_temporary_file(&destination)?;
        Ok(SystemFileUploadSession {
            destination: Arc::new(destination),
            temporary: Arc::new(temporary),
            expected_size,
            expected_sha256: Arc::from(expected_sha256.to_ascii_lowercase()),
            overwrite,
            written: Arc::new(Mutex::new(0)),
        })
    }

    /// 写入文件，并在发布前允许协作取消。
    ///
    /// # Errors
    /// 当路径无效、取消获胜、目标已存在或写入失败时返回错误。
    pub async fn write_file_with_control(
        &self,
        path: &str,
        bytes: &[u8],
        overwrite: bool,
        control: &FileCommitControl,
    ) -> Result<(), DeviceError> {
        let resolved = resolve_transfer_destination(self.transfer_root(), path)?;
        let bytes = bytes.to_vec();
        let _cancel = control.cancel_on_drop();
        let worker = control.worker()?;
        let control = control.clone();
        tokio::task::spawn_blocking(move || {
            let _worker = worker;
            write_file_no_follow(&resolved, &bytes, overwrite, &control)
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }

    /// 从交换目录普通文件的指定偏移读取一个有界分块。
    ///
    /// # Errors
    ///
    /// 当路径、偏移、分块大小或读取操作无效时返回错误。
    pub async fn read_file_chunk(
        &self,
        path: &str,
        offset: u64,
        max_bytes: usize,
    ) -> Result<FileReadChunk, DeviceError> {
        if max_bytes == 0 || max_bytes > MAX_FILE_CHUNK_BYTES {
            return Err(DeviceError::InvalidInput(format!(
                "文件分块大小必须在 1 到 {MAX_FILE_CHUNK_BYTES} 字节之间"
            )));
        }
        let resolved = resolve_existing_transfer_path(self.transfer_root(), path)?;
        tokio::task::spawn_blocking(move || {
            let mut file = open_existing_no_follow(&resolved, false)?;
            let metadata = file
                .metadata()
                .map_err(|error| DeviceError::Operation(error.to_string()))?;
            if !metadata.is_file() {
                return Err(DeviceError::InvalidInput("只允许下载普通文件".to_owned()));
            }
            if offset > metadata.len() {
                return Err(DeviceError::InvalidInput(
                    "文件分块偏移超过文件大小".to_owned(),
                ));
            }
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| DeviceError::Operation(error.to_string()))?;
            let mut bytes = vec![0_u8; max_bytes];
            let read = file
                .read(&mut bytes)
                .map_err(|error| DeviceError::Operation(error.to_string()))?;
            bytes.truncate(read);
            let read = read as u64;
            Ok(FileReadChunk {
                bytes,
                eof: offset.saturating_add(read) >= metadata.len(),
            })
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }

    /// 流式计算交换目录普通文件的 SHA-256。
    ///
    /// # Errors
    ///
    /// 当路径无效或读取失败时返回错误。
    pub async fn file_sha256(&self, path: &str) -> Result<String, DeviceError> {
        let resolved = resolve_existing_transfer_path(self.transfer_root(), path)?;
        tokio::task::spawn_blocking(move || sha256_file_no_follow(&resolved))
            .await
            .map_err(|error| DeviceError::Operation(error.to_string()))?
    }

    /// 打开原始串口会话。
    ///
    /// # Errors
    ///
    /// 当串口参数无效、设备不存在、被占用或无法打开时返回错误。
    pub async fn open_serial(
        &self,
        port_name: &str,
        settings: SerialSettings,
    ) -> Result<SystemSerialSession, DeviceError> {
        if port_name.trim().is_empty() || settings.baud_rate == 0 {
            return Err(DeviceError::InvalidInput(
                "串口名称不能为空且波特率必须大于零".to_owned(),
            ));
        }
        let port_name = port_name.to_owned();
        let port = tokio::task::spawn_blocking(move || {
            serialport::new(port_name, settings.baud_rate)
                .data_bits(match settings.data_bits {
                    SerialDataBits::Five => serialport::DataBits::Five,
                    SerialDataBits::Six => serialport::DataBits::Six,
                    SerialDataBits::Seven => serialport::DataBits::Seven,
                    SerialDataBits::Eight => serialport::DataBits::Eight,
                })
                .stop_bits(match settings.stop_bits {
                    SerialStopBits::One => serialport::StopBits::One,
                    SerialStopBits::Two => serialport::StopBits::Two,
                })
                .parity(match settings.parity {
                    SerialParity::None => serialport::Parity::None,
                    SerialParity::Odd => serialport::Parity::Odd,
                    SerialParity::Even => serialport::Parity::Even,
                })
                .flow_control(match settings.flow_control {
                    SerialFlowControl::None => serialport::FlowControl::None,
                    SerialFlowControl::Software => serialport::FlowControl::Software,
                    SerialFlowControl::Hardware => serialport::FlowControl::Hardware,
                })
                .timeout(std::time::Duration::from_millis(200))
                .open()
                .map_err(|error| DeviceError::Operation(error.to_string()))
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))??;
        Ok(SystemSerialSession {
            inner: Arc::new(Mutex::new(port)),
        })
    }

    /// 打开读写使用独立系统句柄的串口会话。
    ///
    /// # Errors
    ///
    /// 当串口参数无效、设备不存在、被占用、无法打开或无法复制系统句柄时返回错误。
    pub async fn open_duplex_serial(
        &self,
        port_name: &str,
        settings: SerialSettings,
    ) -> Result<SystemDuplexSerialSession, DeviceError> {
        if port_name.trim().is_empty() || settings.baud_rate == 0 {
            return Err(DeviceError::InvalidInput(
                "串口名称不能为空且波特率必须大于零".to_owned(),
            ));
        }
        let port_name = port_name.to_owned();
        let port = tokio::task::spawn_blocking(move || {
            serialport::new(port_name, settings.baud_rate)
                .data_bits(match settings.data_bits {
                    SerialDataBits::Five => serialport::DataBits::Five,
                    SerialDataBits::Six => serialport::DataBits::Six,
                    SerialDataBits::Seven => serialport::DataBits::Seven,
                    SerialDataBits::Eight => serialport::DataBits::Eight,
                })
                .stop_bits(match settings.stop_bits {
                    SerialStopBits::One => serialport::StopBits::One,
                    SerialStopBits::Two => serialport::StopBits::Two,
                })
                .parity(match settings.parity {
                    SerialParity::None => serialport::Parity::None,
                    SerialParity::Odd => serialport::Parity::Odd,
                    SerialParity::Even => serialport::Parity::Even,
                })
                .flow_control(match settings.flow_control {
                    SerialFlowControl::None => serialport::FlowControl::None,
                    SerialFlowControl::Software => serialport::FlowControl::Software,
                    SerialFlowControl::Hardware => serialport::FlowControl::Hardware,
                })
                .timeout(std::time::Duration::from_millis(200))
                .open()
                .map_err(|error| DeviceError::Operation(error.to_string()))
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))??;
        let reader = port
            .try_clone()
            .map_err(|error| DeviceError::Operation(format!("无法创建串口读取句柄：{error}")))?;
        Ok(SystemDuplexSerialSession {
            reader: Arc::new(Mutex::new(reader)),
            writer: Arc::new(Mutex::new(port)),
        })
    }

    /// 打开一个真实持久 Shell，会话内命令按顺序执行。
    ///
    /// # Errors
    ///
    /// 当请求的 Shell 不可用或子进程管道无法创建时返回错误。
    #[allow(unknown_lints)]
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn open_interactive_shell(
        &self,
        shell: ShellKind,
    ) -> Result<SystemInteractiveShellSession, DeviceError> {
        let output_encoding = ProcessOutputEncoding::for_interactive_shell(shell);
        let (executable, arguments) = self.command_for_interactive_shell(shell)?;
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = spawn_managed_process(command)?;
        let stdin = child
            .stdin()
            .take()
            .ok_or_else(|| DeviceError::Operation("无法取得 Shell 标准输入".to_owned()))?;
        let stdout = child
            .stdout()
            .take()
            .ok_or_else(|| DeviceError::Operation("无法取得 Shell 标准输出".to_owned()))?;
        let stderr = child
            .stderr()
            .take()
            .ok_or_else(|| DeviceError::Operation("无法取得 Shell 标准错误".to_owned()))?;
        let closed = Arc::new(AtomicBool::new(false));
        let (shutdown, shutdown_receiver) = mpsc::unbounded_channel();
        let (exit_sender, exit) = tokio::sync::watch::channel(None);
        tokio::spawn(supervise_interactive_shell(
            child,
            shutdown_receiver,
            closed.clone(),
            exit_sender,
        ));
        Ok(SystemInteractiveShellSession {
            inner: Arc::new(AsyncMutex::new(InteractiveShellState {
                stdin,
                stdout,
                stderr,
                stdout_pending: Vec::new(),
                stderr_pending: Vec::new(),
                shell,
                output_encoding,
            })),
            interrupted: Arc::new(AtomicBool::new(false)),
            interrupt_notify: Arc::new(Notify::new()),
            closed,
            shutdown,
            exit,
        })
    }

    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    fn command_for_shell(
        &self,
        shell: ShellKind,
        command: &str,
    ) -> Result<(String, Vec<String>), DeviceError> {
        #[cfg(windows)]
        {
            match shell {
                ShellKind::Cmd | ShellKind::System => Ok((
                    "cmd.exe".to_owned(),
                    vec![
                        "/D".to_owned(),
                        "/S".to_owned(),
                        "/C".to_owned(),
                        command.to_owned(),
                    ],
                )),
                ShellKind::WindowsPowerShell => Ok((
                    "powershell.exe".to_owned(),
                    vec![
                        "-NoLogo".to_owned(),
                        "-NoProfile".to_owned(),
                        "-NonInteractive".to_owned(),
                        "-Command".to_owned(),
                        format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8; {command}"),
                    ],
                )),
                ShellKind::PowerShell => Ok((
                    "pwsh.exe".to_owned(),
                    vec![
                        "-NoLogo".to_owned(),
                        "-NoProfile".to_owned(),
                        "-NonInteractive".to_owned(),
                        "-Command".to_owned(),
                        format!(
                            "[Console]::OutputEncoding = [Text.Encoding]::UTF8; $PSStyle.OutputRendering = 'PlainText'; {command}"
                        ),
                    ],
                )),
            }
        }
        #[cfg(not(windows))]
        {
            let executable = match shell {
                ShellKind::System => "/bin/sh",
                ShellKind::Cmd => {
                    return Err(DeviceError::Unsupported(
                        "CMD is only available on Windows".to_owned(),
                    ));
                }
                ShellKind::WindowsPowerShell => {
                    return Err(DeviceError::Unsupported(
                        "Windows PowerShell is not available on Unix".to_owned(),
                    ));
                }
                ShellKind::PowerShell => {
                    if command_exists("pwsh") {
                        "pwsh"
                    } else {
                        return Err(DeviceError::Unsupported(
                            "当前 Linux Agent 没有可用 PowerShell".to_owned(),
                        ));
                    }
                }
            };
            let flag = if executable == "pwsh" {
                "-Command"
            } else {
                "-c"
            };
            Ok((
                executable.to_owned(),
                vec![flag.to_owned(), command.to_owned()],
            ))
        }
    }

    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    fn command_for_interactive_shell(
        &self,
        shell: ShellKind,
    ) -> Result<(String, Vec<String>), DeviceError> {
        #[cfg(windows)]
        {
            match shell {
                ShellKind::Cmd | ShellKind::System => {
                    Ok(("cmd.exe".to_owned(), vec!["/D".to_owned(), "/Q".to_owned()]))
                }
                ShellKind::WindowsPowerShell => Ok((
                    "powershell.exe".to_owned(),
                    vec![
                        "-NoLogo".to_owned(),
                        "-NoProfile".to_owned(),
                        "-NonInteractive".to_owned(),
                        "-NoExit".to_owned(),
                        "-Command".to_owned(),
                        "-".to_owned(),
                    ],
                )),
                ShellKind::PowerShell => Ok((
                    "pwsh.exe".to_owned(),
                    vec![
                        "-NoLogo".to_owned(),
                        "-NoProfile".to_owned(),
                        "-NonInteractive".to_owned(),
                        "-NoExit".to_owned(),
                        "-Command".to_owned(),
                        "-".to_owned(),
                    ],
                )),
            }
        }
        #[cfg(not(windows))]
        {
            match shell {
                ShellKind::System => Ok(("/bin/sh".to_owned(), Vec::new())),
                ShellKind::Cmd => Err(DeviceError::Unsupported(
                    "CMD is only available on Windows".to_owned(),
                )),
                ShellKind::WindowsPowerShell => Err(DeviceError::Unsupported(
                    "Windows PowerShell is not available on Unix".to_owned(),
                )),
                ShellKind::PowerShell => {
                    if command_exists("pwsh") {
                        Ok((
                            "pwsh".to_owned(),
                            vec![
                                "-NoLogo".to_owned(),
                                "-NoProfile".to_owned(),
                                "-NonInteractive".to_owned(),
                                "-NoExit".to_owned(),
                                "-Command".to_owned(),
                                "-".to_owned(),
                            ],
                        ))
                    } else {
                        Err(DeviceError::Unsupported(
                            "当前 Linux Agent 没有可用 PowerShell".to_owned(),
                        ))
                    }
                }
            }
        }
    }

    #[cfg(windows)]
    async fn run_process(
        &self,
        executable: String,
        arguments: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<CommandResult, DeviceError> {
        let (output, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        self.run_process_streaming(executable, arguments, timeout_seconds, output)
            .await
    }

    #[cfg(windows)]
    async fn run_process_streaming(
        &self,
        executable: String,
        arguments: Vec<String>,
        timeout_seconds: u64,
        output: mpsc::UnboundedSender<CommandOutputChunk>,
    ) -> Result<CommandResult, DeviceError> {
        self.run_process_streaming_with_encoding(
            executable,
            arguments,
            timeout_seconds,
            output,
            ProcessOutputEncoding::Utf8,
        )
        .await
    }

    async fn run_process_streaming_with_encoding(
        &self,
        executable: String,
        arguments: Vec<String>,
        timeout_seconds: u64,
        output: mpsc::UnboundedSender<CommandOutputChunk>,
        output_encoding: ProcessOutputEncoding,
    ) -> Result<CommandResult, DeviceError> {
        self.run_process_streaming_with_environment(
            executable,
            arguments,
            timeout_seconds,
            output,
            output_encoding,
            Vec::new(),
        )
        .await
    }

    async fn run_process_streaming_with_environment(
        &self,
        executable: String,
        arguments: Vec<String>,
        timeout_seconds: u64,
        output: mpsc::UnboundedSender<CommandOutputChunk>,
        output_encoding: ProcessOutputEncoding,
        mut environment: Vec<(String, String)>,
    ) -> Result<CommandResult, DeviceError> {
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.envs(environment.iter().cloned());
        let child_result = spawn_managed_process(command);
        for (_, value) in &mut environment {
            value.zeroize();
        }
        let mut child = child_result?;
        let stdout = child
            .stdout()
            .take()
            .ok_or_else(|| DeviceError::Operation("无法取得进程标准输出".to_owned()))?;
        let stderr = child
            .stderr()
            .take()
            .ok_or_else(|| DeviceError::Operation("无法取得进程标准错误".to_owned()))?;
        let output_bytes = Arc::new(AtomicUsize::new(0));
        let output_exceeded = Arc::new(AtomicBool::new(false));
        let output_limit_notify = Arc::new(Notify::new());
        let stdout_task = tokio::spawn(read_process_output(
            stdout,
            false,
            output.clone(),
            output_bytes.clone(),
            output_exceeded.clone(),
            output_limit_notify.clone(),
            output_encoding,
        ));
        let stderr_task = tokio::spawn(read_process_output(
            stderr,
            true,
            output,
            output_bytes,
            output_exceeded.clone(),
            output_limit_notify.clone(),
            output_encoding,
        ));
        let deadline = tokio::time::sleep(Duration::from_secs(timeout_seconds.max(1)));
        tokio::pin!(deadline);
        let status = tokio::select! {
            result = child.wait_for_parent() => {
                result.map_err(|error| DeviceError::Operation(error.to_string()))?
            }
            () = output_limit_notify.notified() => {
                terminate_process_tree(&mut child).await?;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(DeviceError::OutputLimit {
                    limit: MAX_COMMAND_OUTPUT_BYTES,
                });
            }
            () = &mut deadline => {
                terminate_process_tree(&mut child).await?;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(DeviceError::Timeout);
            }
        };
        drop(child);
        let stdout = stdout_task
            .await
            .map_err(|error| DeviceError::Operation(error.to_string()))??;
        let stderr = stderr_task
            .await
            .map_err(|error| DeviceError::Operation(error.to_string()))??;
        if output_exceeded.load(Ordering::Acquire) {
            return Err(DeviceError::OutputLimit {
                limit: MAX_COMMAND_OUTPUT_BYTES,
            });
        }
        Ok(CommandResult {
            stdout,
            stderr,
            exit_code: status.code(),
        })
    }
}

async fn wait_for_shell_interrupt(interrupted: Arc<AtomicBool>, notify: Arc<Notify>) {
    let notified = notify.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    if interrupted.load(Ordering::Acquire) {
        return;
    }
    notified.await;
}

#[async_trait]
impl ShellProvider for SystemDevice {
    async fn available_shells(&self) -> Result<Vec<ShellKind>, DeviceError> {
        let mut shells = Vec::new();
        #[cfg(windows)]
        {
            shells.push(ShellKind::Cmd);
            shells.push(ShellKind::WindowsPowerShell);
            if command_exists("pwsh.exe") {
                shells.push(ShellKind::PowerShell);
            }
        }
        #[cfg(not(windows))]
        {
            if command_exists("/bin/sh") {
                shells.push(ShellKind::System);
            }
            if command_exists("pwsh") {
                shells.push(ShellKind::PowerShell);
            }
        }
        Ok(shells)
    }

    async fn run(
        &self,
        shell: ShellKind,
        command: &str,
        timeout_seconds: u64,
    ) -> Result<CommandResult, DeviceError> {
        if command.trim().is_empty() {
            return Err(DeviceError::InvalidInput("命令不能为空".to_owned()));
        }
        let (executable, arguments) = self.command_for_shell(shell, command)?;
        let output_encoding = ProcessOutputEncoding::for_shell(shell);
        let (output, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        self.run_process_streaming_with_encoding(
            executable,
            arguments,
            timeout_seconds,
            output,
            output_encoding,
        )
        .await
    }

    async fn run_streaming(
        &self,
        shell: ShellKind,
        command: &str,
        timeout_seconds: u64,
        output: mpsc::UnboundedSender<CommandOutputChunk>,
    ) -> Result<CommandResult, DeviceError> {
        if command.trim().is_empty() {
            return Err(DeviceError::InvalidInput("命令不能为空".to_owned()));
        }
        let (executable, arguments) = self.command_for_shell(shell, command)?;
        let output_encoding = ProcessOutputEncoding::for_shell(shell);
        self.run_process_streaming_with_encoding(
            executable,
            arguments,
            timeout_seconds,
            output,
            output_encoding,
        )
        .await
    }
}

#[async_trait]
impl PortProbeProvider for SystemDevice {
    async fn test_port(
        &self,
        host: &str,
        port: u16,
        timeout_millis: u64,
    ) -> Result<PortProbeResult, DeviceError> {
        if host.trim().is_empty() {
            return Err(DeviceError::InvalidInput("目标主机不能为空".to_owned()));
        }
        let started = Instant::now();
        let result = timeout(
            Duration::from_millis(timeout_millis.max(1)),
            TcpStream::connect((host, port)),
        )
        .await;
        let elapsed_millis = started.elapsed().as_millis();
        match result {
            Ok(Ok(_stream)) => Ok(PortProbeResult {
                host: host.to_owned(),
                port,
                open: true,
                elapsed_millis,
                error: None,
            }),
            Ok(Err(error)) => Ok(PortProbeResult {
                host: host.to_owned(),
                port,
                open: false,
                elapsed_millis,
                error: Some(error.kind().to_string()),
            }),
            Err(_) => Ok(PortProbeResult {
                host: host.to_owned(),
                port,
                open: false,
                elapsed_millis,
                error: Some("timeout".to_owned()),
            }),
        }
    }
}

#[async_trait]
impl FileTransferProvider for SystemDevice {
    async fn read_file(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, DeviceError> {
        let resolved = resolve_existing_transfer_path(self.transfer_root(), path)?;
        let file = open_existing_no_follow(&resolved, false)?;
        let file = tokio::fs::File::from_std(file);
        let metadata = file
            .metadata()
            .await
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        if !metadata.is_file() {
            return Err(DeviceError::InvalidInput("只允许下载普通文件".to_owned()));
        }
        if metadata.len() > max_bytes {
            return Err(DeviceError::InvalidInput(format!(
                "文件超过大小限制：{} > {}",
                metadata.len(),
                max_bytes
            )));
        }
        let read_limit = max_bytes.saturating_add(1);
        let capacity = usize::try_from(metadata.len().min(max_bytes))
            .map_err(|_| DeviceError::InvalidInput("文件大小超出当前平台限制".to_owned()))?;
        let mut bytes = Vec::with_capacity(capacity);
        file.take(read_limit)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        if bytes.len() as u64 > max_bytes {
            return Err(DeviceError::InvalidInput(format!(
                "文件在读取期间超过 {max_bytes} 字节限制"
            )));
        }
        Ok(bytes)
    }

    async fn write_file(
        &self,
        path: &str,
        bytes: &[u8],
        overwrite: bool,
    ) -> Result<(), DeviceError> {
        self.write_file_with_control(path, bytes, overwrite, &FileCommitControl::default())
            .await
    }

    async fn file_metadata(&self, path: &str) -> Result<FileMetadata, DeviceError> {
        let resolved = resolve_existing_transfer_path(self.transfer_root(), path)?;
        let file = open_existing_no_follow(&resolved, false)?;
        let metadata = file
            .metadata()
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        if !metadata.is_file() {
            return Err(DeviceError::InvalidInput(
                "只允许读取普通文件元数据".to_owned(),
            ));
        }
        let modified_unix_millis = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis());
        Ok(FileMetadata {
            path: validate_relative_transfer_path(path)?
                .to_string_lossy()
                .replace('\\', "/"),
            size: metadata.len(),
            modified_unix_millis,
        })
    }

    async fn move_file(
        &self,
        source_path: &str,
        destination_path: &str,
        overwrite: bool,
    ) -> Result<(), DeviceError> {
        let source = resolve_existing_transfer_path(self.transfer_root(), source_path)?;
        let destination = resolve_transfer_destination(self.transfer_root(), destination_path)?;
        if source == destination {
            return Ok(());
        }
        let source_file = open_existing_no_follow(&source, false)?;
        if !source_file
            .metadata()
            .map_err(|error| DeviceError::Operation(error.to_string()))?
            .is_file()
        {
            return Err(DeviceError::InvalidInput("只允许移动普通文件".to_owned()));
        }
        drop(source_file);
        commit_temporary_file(&source, &destination, overwrite)
    }

    async fn delete_file(&self, path: &str) -> Result<(), DeviceError> {
        let resolved = resolve_existing_transfer_path(self.transfer_root(), path)?;
        let file = open_existing_no_follow(&resolved, false)?;
        if !file
            .metadata()
            .map_err(|error| DeviceError::Operation(error.to_string()))?
            .is_file()
        {
            return Err(DeviceError::InvalidInput("只允许删除普通文件".to_owned()));
        }
        drop(file);
        std::fs::remove_file(resolved).map_err(|error| DeviceError::Operation(error.to_string()))
    }
}

#[async_trait]
impl TcpExchangeProvider for SystemDevice {
    async fn exchange(
        &self,
        host: &str,
        port: u16,
        payload: &[u8],
        max_response_bytes: usize,
        timeout_millis: u64,
    ) -> Result<TcpExchangeResult, DeviceError> {
        if host.trim().is_empty()
            || host.chars().any(char::is_whitespace)
            || host.chars().any(char::is_control)
            || port == 0
        {
            return Err(DeviceError::InvalidInput("TCP 主机或端口无效".to_owned()));
        }
        if payload.len() > 1024 * 1024
            || max_response_bytes == 0
            || max_response_bytes > 1024 * 1024
            || !(100..=120_000).contains(&timeout_millis)
        {
            return Err(DeviceError::InvalidInput(
                "TCP 收发限制无效：请求和响应均不得超过 1 MiB，超时必须为 100..120000 毫秒"
                    .to_owned(),
            ));
        }
        let duration = Duration::from_millis(timeout_millis);
        let mut stream = timeout(duration, TcpStream::connect((host, port)))
            .await
            .map_err(|_| DeviceError::Timeout)?
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        timeout(duration, stream.write_all(payload))
            .await
            .map_err(|_| DeviceError::Timeout)?
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        let _ = stream.shutdown().await;
        let mut received = Vec::with_capacity(max_response_bytes.min(16 * 1024));
        let mut read_timed_out = false;
        while received.len() < max_response_bytes {
            let mut chunk = vec![0_u8; (max_response_bytes - received.len()).min(16 * 1024)];
            match timeout(duration, stream.read(&mut chunk)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(read)) => received.extend_from_slice(&chunk[..read]),
                Ok(Err(error)) => return Err(DeviceError::Operation(error.to_string())),
                Err(_) => {
                    read_timed_out = true;
                    break;
                }
            }
        }
        Ok(TcpExchangeResult {
            host: host.to_owned(),
            port,
            sent_bytes: payload.len(),
            received,
            read_timed_out,
        })
    }
}

#[cfg(not(windows))]
fn require_systemd() -> Result<(), DeviceError> {
    if !std::path::Path::new("/run/systemd/system").is_dir() || !command_exists("systemctl") {
        return Err(DeviceError::Unsupported(
            "systemd system service manager is unavailable".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn normalize_unix_inventory(result: &mut CommandResult, services: bool) -> Result<(), DeviceError> {
    if result.exit_code != Some(0) {
        return Err(DeviceError::Operation(format!(
            "Inventory command failed (exit {:?}): {}",
            result.exit_code,
            result.stderr.trim()
        )));
    }
    let mut items = Vec::new();
    for line in result
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(512)
    {
        let mut rest = line.trim();
        let mut columns = Vec::new();
        for _ in 0..if services { 4 } else { 2 } {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            columns.push(&rest[..end]);
            rest = rest[end..].trim_start();
        }
        let item = if services {
            if columns.iter().any(|column| column.is_empty()) {
                return Err(DeviceError::Operation(
                    "Invalid systemctl inventory row".to_owned(),
                ));
            }
            serde_json::json!({"Name":columns[0],"DisplayName":rest,"Status":columns[2],"LoadState":columns[1],"SubState":columns[3]})
        } else {
            let pid = columns[0]
                .parse::<u32>()
                .map_err(|_| DeviceError::Operation("Invalid process inventory PID".to_owned()))?;
            serde_json::json!({"Id":pid,"ProcessName":columns[1],"CommandLine":rest})
        };
        items.push(item);
    }
    result.stdout = serde_json::json!({"returned":items.len(),"items":items}).to_string();
    Ok(())
}

#[async_trait]
impl SystemProvider for SystemDevice {
    async fn list_processes(&self) -> Result<CommandResult, DeviceError> {
        #[cfg(windows)]
        {
            return ShellProvider::run(
                self,
                ShellKind::WindowsPowerShell,
                "[Console]::OutputEncoding = [Text.Encoding]::UTF8; $items = @(Get-Process | Sort-Object Id | Select-Object -First 512 Id,ProcessName,CPU,WorkingSet64); [pscustomobject]@{ returned = $items.Count; items = $items } | ConvertTo-Json -Depth 3 -Compress",
                30,
            )
            .await;
        }
        #[cfg(not(windows))]
        {
            let mut result = ShellProvider::run(
                self,
                ShellKind::System,
                "LC_ALL=C ps -eo pid=,comm=,args=",
                30,
            )
            .await?;
            normalize_unix_inventory(&mut result, false)?;
            Ok(result)
        }
    }

    async fn terminate_process(&self, process_id: u32) -> Result<CommandResult, DeviceError> {
        if process_id <= 1 || process_id == std::process::id() {
            return Err(DeviceError::InvalidInput(
                "不能终止系统空闲进程或当前 Agent 进程".to_owned(),
            ));
        }
        #[cfg(windows)]
        {
            return ShellProvider::run(
                self,
                ShellKind::WindowsPowerShell,
                &format!("& taskkill.exe /PID {process_id} /T /F"),
                30,
            )
            .await;
        }
        #[cfg(not(windows))]
        ShellProvider::run(
            self,
            ShellKind::System,
            &format!("kill -TERM {process_id}"),
            30,
        )
        .await
    }

    async fn list_services(&self) -> Result<CommandResult, DeviceError> {
        #[cfg(windows)]
        {
            return ShellProvider::run(
                self,
                ShellKind::WindowsPowerShell,
                "[Console]::OutputEncoding = [Text.Encoding]::UTF8; $items = @(Get-Service | Sort-Object Name | Select-Object -First 512 Name,DisplayName,Status); [pscustomobject]@{ returned = $items.Count; items = $items } | ConvertTo-Json -Depth 3 -Compress",
                30,
            )
            .await;
        }
        #[cfg(not(windows))]
        {
            require_systemd()?;
            let mut result = ShellProvider::run(
                self,
                ShellKind::System,
                "LC_ALL=C systemctl list-units --type=service --all --no-pager --plain --no-legend",
                45,
            )
            .await?;
            normalize_unix_inventory(&mut result, true)?;
            Ok(result)
        }
    }

    async fn control_service(
        &self,
        service_name: &str,
        action: ServiceAction,
    ) -> Result<CommandResult, DeviceError> {
        validate_service_name(service_name)?;
        #[cfg(windows)]
        {
            return match action {
                ServiceAction::Start => {
                    let command = format!("& sc.exe start '{service_name}'");
                    ShellProvider::run(self, ShellKind::WindowsPowerShell, &command, 60).await
                }
                ServiceAction::Stop => {
                    let command = format!("& sc.exe stop '{service_name}'");
                    ShellProvider::run(self, ShellKind::WindowsPowerShell, &command, 60).await
                }
                ServiceAction::Restart => {
                    let stop_command = format!("& sc.exe stop '{service_name}'");
                    let stopped =
                        ShellProvider::run(self, ShellKind::WindowsPowerShell, &stop_command, 60)
                            .await?;
                    let start_command = format!("& sc.exe start '{service_name}'");
                    let started =
                        ShellProvider::run(self, ShellKind::WindowsPowerShell, &start_command, 60)
                            .await?;
                    Ok(CommandResult {
                        exit_code: started.exit_code,
                        stdout: format!("{}{}", stopped.stdout, started.stdout),
                        stderr: format!("{}{}", stopped.stderr, started.stderr),
                    })
                }
            };
        }
        #[cfg(not(windows))]
        {
            require_systemd()?;
            let action = match action {
                ServiceAction::Start => "start",
                ServiceAction::Stop => "stop",
                ServiceAction::Restart => "restart",
            };
            ShellProvider::run(
                self,
                ShellKind::System,
                &format!("systemctl --no-ask-password {action} -- {service_name}"),
                60,
            )
            .await
        }
    }

    async fn power_control(&self, action: PowerAction) -> Result<CommandResult, DeviceError> {
        #[cfg(windows)]
        {
            let mode = match action {
                PowerAction::Restart => "/r",
                PowerAction::Shutdown => "/s",
            };
            return self
                .run_process(
                    "shutdown.exe".to_owned(),
                    vec![mode.to_owned(), "/t".to_owned(), "0".to_owned()],
                    30,
                )
                .await;
        }
        #[cfg(target_os = "linux")]
        {
            require_systemd()?;
            ShellProvider::run(
                self,
                ShellKind::System,
                match action {
                    PowerAction::Restart => "systemctl --no-ask-password reboot",
                    PowerAction::Shutdown => "systemctl --no-ask-password poweroff",
                },
                30,
            )
            .await
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        ShellProvider::run(
            self,
            ShellKind::System,
            match action {
                PowerAction::Restart => "shutdown -r now",
                PowerAction::Shutdown => "shutdown -h now",
            },
            30,
        )
        .await
    }
}

#[async_trait]
impl SerialProvider for SystemDevice {
    async fn list_ports(&self) -> Result<Vec<SerialPortInfo>, DeviceError> {
        tokio::task::spawn_blocking(|| {
            serialport::available_ports()
                .map(|ports| {
                    ports
                        .into_iter()
                        .map(|port| SerialPortInfo {
                            port_name: port.port_name,
                            port_type: Some(format!("{:?}", port.port_type)),
                        })
                        .collect()
                })
                .map_err(|error| DeviceError::Operation(error.to_string()))
        })
        .await
        .map_err(|error| DeviceError::Operation(error.to_string()))?
    }
}

#[async_trait]
impl SshProvider for SystemDevice {
    async fn run_command(
        &self,
        host: &str,
        port: u16,
        username: &str,
        password: Option<&str>,
        identity_file: Option<&str>,
        known_hosts_file: Option<&str>,
        command: &str,
        timeout_seconds: u64,
    ) -> Result<CommandResult, DeviceError> {
        if host.trim().is_empty() || username.trim().is_empty() || command.trim().is_empty() {
            return Err(DeviceError::InvalidInput(
                "SSH 主机、用户名和命令不能为空".to_owned(),
            ));
        }
        validate_ssh_destination(host, username)?;
        let password = password.map(|value| Zeroizing::new(value.to_owned()));
        if password.is_none() && !self.allow_ssh_without_password {
            return Err(DeviceError::Unsupported(
                "当前策略禁止无密码 SSH".to_owned(),
            ));
        }
        let known_hosts_file = resolve_ssh_known_hosts_file(
            self.transfer_root(),
            known_hosts_file,
            password.is_some(),
            host,
            port,
        )?;
        let null_config = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let mut arguments = vec![
            "-F".to_owned(),
            null_config.to_owned(),
            "-o".to_owned(),
            format!(
                "BatchMode={}",
                if password.is_some() { "no" } else { "yes" }
            ),
            "-o".to_owned(),
            "StrictHostKeyChecking=yes".to_owned(),
            "-o".to_owned(),
            format!(
                "UserKnownHostsFile={}",
                path_for_child_process(&known_hosts_file)
            ),
        ];
        if let Some(identity_file) = identity_file {
            let identity_file =
                resolve_existing_transfer_path(self.transfer_root(), identity_file)?;
            arguments.extend([
                "-o".to_owned(),
                "IdentitiesOnly=yes".to_owned(),
                "-i".to_owned(),
                path_for_child_process(&identity_file),
            ]);
        }
        let mut environment = Vec::new();
        if let Some(password) = password {
            let askpass = std::env::current_exe()
                .ok()
                .and_then(|path| {
                    path.parent()
                        .map(|parent| parent.join("remoteops-ssh-askpass.exe"))
                })
                .filter(|path| path.is_file())
                .ok_or_else(|| {
                    DeviceError::InvalidInput(
                        "密码 SSH 需要与 Agent 同目录的 remoteops-ssh-askpass.exe".to_owned(),
                    )
                })?;
            arguments.extend([
                "-o".to_owned(),
                "NumberOfPasswordPrompts=1".to_owned(),
                "-o".to_owned(),
                "PreferredAuthentications=password,keyboard-interactive".to_owned(),
                "-o".to_owned(),
                "PubkeyAuthentication=no".to_owned(),
            ]);
            environment.extend([
                ("SSH_ASKPASS".to_owned(), path_for_child_process(&askpass)),
                ("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned()),
                ("DISPLAY".to_owned(), "remoteops".to_owned()),
                ("REMOTEOPS_SSH_PASSWORD".to_owned(), password.to_string()),
            ]);
        }
        arguments.extend([
            "-p".to_owned(),
            port.to_string(),
            format!("{username}@{host}"),
            command.to_owned(),
        ]);
        let executable = if cfg!(windows) { "ssh.exe" } else { "ssh" };
        let (output, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        self.run_process_streaming_with_environment(
            executable.to_owned(),
            arguments,
            timeout_seconds,
            output,
            ProcessOutputEncoding::Utf8,
            environment,
        )
        .await
    }
}

/// 扫描 SSH 主机公钥并计算供本地用户确认的 SHA-256 指纹。
///
/// # Errors
///
/// 当目标格式无效、`ssh-keyscan` 不可用、扫描失败或没有返回有效密钥时返回错误。
pub fn scan_ssh_host_keys(
    host: &str,
    port: u16,
    timeout_seconds: u64,
) -> Result<SshHostKeyScan, DeviceError> {
    validate_ssh_destination(host, "scan")?;
    let mut command = std::process::Command::new(if cfg!(windows) {
        "ssh-keyscan.exe"
    } else {
        "ssh-keyscan"
    });
    command.args([
        "-T",
        &timeout_seconds.clamp(1, 30).to_string(),
        "-p",
        &port.to_string(),
        host,
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW.0);
    }
    let output = command
        .output()
        .map_err(|error| DeviceError::Operation(format!("无法启动 ssh-keyscan：{error}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();
    let mut fingerprints = Vec::new();
    for line in stdout.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 3 {
            continue;
        }
        let key = BASE64
            .decode(fields[2])
            .map_err(|_| DeviceError::Operation("ssh-keyscan 返回了无效公钥".to_owned()))?;
        let digest = Sha256::digest(key);
        let fingerprint = base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
        entries.push(line.to_owned());
        fingerprints.push(format!("{} SHA256:{fingerprint}", fields[1]));
    }
    if entries.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DeviceError::Operation(format!(
            "未扫描到 SSH 主机密钥{}",
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!("：{}", stderr.trim())
            }
        )));
    }
    Ok(SshHostKeyScan {
        host: host.to_owned(),
        port,
        entries,
        fingerprints,
    })
}

/// 把现场用户已经确认的 SSH 主机密钥写入 Agent 专用信任文件。
///
/// # Errors
///
/// 当信任目录或文件无法创建、读取或写入时返回错误。
pub fn trust_ssh_host_keys(scan: &SshHostKeyScan) -> Result<PathBuf, DeviceError> {
    let _guard = SSH_KNOWN_HOSTS_LOCK
        .lock()
        .map_err(|_| DeviceError::Operation("SSH 主机信任锁已损坏".to_owned()))?;
    let path = default_ssh_known_hosts_file()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
    }
    let target = known_hosts_target(&scan.host, scan.port);
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(DeviceError::Operation(error.to_string())),
    };
    let mut lines = existing
        .lines()
        .filter(|line| line.split_whitespace().next() != Some(target.as_str()))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    lines.extend(scan.entries.iter().cloned());
    let contents = format!("{}\n", lines.join("\n"));
    let temporary = path.with_extension(format!("known-hosts.{}.tmp", std::process::id()));
    std::fs::write(&temporary, contents)
        .map_err(|error| DeviceError::Operation(error.to_string()))?;
    commit_temporary_file(&temporary, &path, true)?;
    Ok(path)
}

fn default_ssh_known_hosts_file() -> Result<PathBuf, DeviceError> {
    #[cfg(windows)]
    let base = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    base.map(|path| path.join("RemoteOps").join("ssh-known-hosts"))
        .ok_or_else(|| DeviceError::Operation("无法确定 Agent 本地数据目录".to_owned()))
}

fn resolve_ssh_known_hosts_file(
    transfer_root: &Path,
    requested_path: Option<&str>,
    uses_password: bool,
    host: &str,
    port: u16,
) -> Result<PathBuf, DeviceError> {
    if uses_password {
        if requested_path.is_some() {
            return Err(DeviceError::InvalidInput(
                "密码 SSH 不允许由远端指定 known_hosts 文件".to_owned(),
            ));
        }
        return ensure_default_ssh_host_trusted(host, port);
    }
    requested_path.map_or_else(default_ssh_known_hosts_file, |path| {
        resolve_existing_transfer_path(transfer_root, path)
    })
}

/// 首次密码 SSH 自动采用 TOFU 固定主机密钥；已有目标记录时仍由 OpenSSH 严格校验。
fn ensure_default_ssh_host_trusted(host: &str, port: u16) -> Result<PathBuf, DeviceError> {
    let _guard = SSH_KNOWN_HOSTS_LOCK
        .lock()
        .map_err(|_| DeviceError::Operation("SSH 主机信任锁已损坏".to_owned()))?;
    let path = default_ssh_known_hosts_file()?;
    let parent = path
        .parent()
        .ok_or_else(|| DeviceError::Operation("无法确定 SSH 主机信任目录".to_owned()))?;
    std::fs::create_dir_all(parent).map_err(|error| DeviceError::Operation(error.to_string()))?;
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(DeviceError::Operation(error.to_string())),
    };
    let target = known_hosts_target(host, port);
    if existing
        .lines()
        .any(|line| line.split_whitespace().next() == Some(target.as_str()))
    {
        return Ok(path);
    }
    let scan = scan_ssh_host_keys(host, port, 8)?;
    let mut lines = existing.lines().map(str::to_owned).collect::<Vec<_>>();
    lines.extend(scan.entries);
    let temporary = path.with_extension(format!("known-hosts.{}.tmp", std::process::id()));
    std::fs::write(&temporary, format!("{}\n", lines.join("\n")))
        .map_err(|error| DeviceError::Operation(error.to_string()))?;
    commit_temporary_file(&temporary, &path, true)?;
    Ok(path)
}

fn known_hosts_target(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    }
}

impl Default for SystemDevice {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_relative_transfer_path(path: &str) -> Result<&Path, DeviceError> {
    if path.trim().is_empty() || path.contains('\0') {
        return Err(DeviceError::InvalidInput("文件路径无效".to_owned()));
    }
    let path = Path::new(path);
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_string_lossy();
                if value.is_empty()
                    || value == "."
                    || value == ".."
                    || value.contains(':')
                    || value.ends_with('.')
                    || value.ends_with(' ')
                {
                    return Err(DeviceError::InvalidInput(
                        "文件路径包含不安全的路径片段".to_owned(),
                    ));
                }
            }
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => {
                return Err(DeviceError::InvalidInput(
                    "文件路径必须是交换目录内的相对路径".to_owned(),
                ));
            }
        }
    }
    Ok(path)
}

fn resolve_existing_transfer_path(root: &Path, path: &str) -> Result<PathBuf, DeviceError> {
    let relative = validate_relative_transfer_path(path)?;
    let canonical = std::fs::canonicalize(root.join(relative))
        .map_err(|error| DeviceError::Operation(error.to_string()))?;
    if !canonical.starts_with(root) {
        return Err(DeviceError::InvalidInput(
            "文件路径超出 Agent 文件交换目录".to_owned(),
        ));
    }
    Ok(canonical)
}

fn path_for_child_process(path: &Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(path) = value.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{path}");
        }
        if let Some(path) = value.strip_prefix(r"\\?\") {
            return path.to_owned();
        }
    }
    value.into_owned()
}

fn resolve_transfer_destination(root: &Path, path: &str) -> Result<PathBuf, DeviceError> {
    let relative = validate_relative_transfer_path(path)?;
    let file_name = relative
        .file_name()
        .ok_or_else(|| DeviceError::InvalidInput("目标文件名无效".to_owned()))?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let canonical_parent = ensure_transfer_directory(root, parent)?;
    if !canonical_parent.starts_with(root) {
        return Err(DeviceError::InvalidInput(
            "目标路径超出 Agent 文件交换目录".to_owned(),
        ));
    }
    Ok(canonical_parent.join(file_name))
}

fn ensure_transfer_directory(root: &Path, relative: &Path) -> Result<PathBuf, DeviceError> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(DeviceError::InvalidInput("文件目录路径无效".to_owned()));
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || metadata_is_reparse_point(&metadata) {
                    return Err(DeviceError::InvalidInput(
                        "不允许通过链接或重解析点访问文件目录".to_owned(),
                    ));
                }
                if !metadata.is_dir() {
                    return Err(DeviceError::InvalidInput(
                        "文件目标的父路径不是目录".to_owned(),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)
                    .map_err(|error| DeviceError::Operation(error.to_string()))?;
            }
            Err(error) => return Err(DeviceError::Operation(error.to_string())),
        }
        let canonical = std::fs::canonicalize(&current)
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        if !canonical.starts_with(root) {
            return Err(DeviceError::InvalidInput(
                "目标路径超出 Agent 文件交换目录".to_owned(),
            ));
        }
        current = canonical;
    }
    Ok(current)
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn open_existing_no_follow(path: &Path, write: bool) -> Result<std::fs::File, DeviceError> {
    let mut options = StdOpenOptions::new();
    options.read(!write).write(write);
    apply_no_follow_flag(&mut options);
    let file = options
        .open(path)
        .map_err(|error| DeviceError::Operation(error.to_string()))?;
    reject_reparse_file(&file)?;
    Ok(file)
}

fn write_file_no_follow(
    path: &Path,
    bytes: &[u8],
    overwrite: bool,
    control: &FileCommitControl,
) -> Result<(), DeviceError> {
    control.checkpoint()?;
    let temporary = create_temporary_file(path)?;
    let result = (|| {
        let mut file = open_existing_no_follow(&temporary, true)?;
        for chunk in bytes.chunks(MAX_FILE_CHUNK_BYTES) {
            control.checkpoint()?;
            file.write_all(chunk)
                .map_err(|error| DeviceError::Operation(error.to_string()))?;
        }
        file.flush()
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        file.sync_all()
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        drop(file);
        control.begin_commit()?;
        commit_temporary_file(&temporary, path, overwrite)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn create_temporary_file(destination: &Path) -> Result<PathBuf, DeviceError> {
    let parent = destination
        .parent()
        .ok_or_else(|| DeviceError::InvalidInput("目标文件缺少父目录".to_owned()))?;
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    for _ in 0..100 {
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{file_name}.remoteops-{}-{sequence}.part",
            std::process::id()
        ));
        let mut options = StdOpenOptions::new();
        options.write(true).create_new(true);
        apply_no_follow_flag(&mut options);
        match options.open(&candidate) {
            Ok(file) => {
                reject_reparse_file(&file)?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(DeviceError::Operation(error.to_string())),
        }
    }
    Err(DeviceError::Operation(
        "无法创建唯一的文件传输临时文件".to_owned(),
    ))
}

fn unique_backup_path(destination: &Path) -> Result<PathBuf, DeviceError> {
    let parent = destination
        .parent()
        .ok_or_else(|| DeviceError::InvalidInput("目标文件缺少父目录".to_owned()))?;
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    for _ in 0..100 {
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{file_name}.remoteops-{}-{sequence}.backup",
            std::process::id()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(DeviceError::Operation(
        "无法创建唯一的文件替换备份路径".to_owned(),
    ))
}

fn commit_temporary_file(
    temporary: &Path,
    destination: &Path,
    overwrite: bool,
) -> Result<(), DeviceError> {
    if !overwrite {
        // Both paths share a filesystem. Creating the destination hard link is an
        // atomic no-replace publication, unlike an exists() check followed by rename.
        // Unsupported filesystems fail closed rather than falling back to overwrite.
        std::fs::hard_link(temporary, destination).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                DeviceError::Operation("目标文件已存在".to_owned())
            } else {
                DeviceError::Operation(error.to_string())
            }
        })?;
        return std::fs::remove_file(temporary).map_err(|error| {
            DeviceError::Operation(format!("新文件已提交，但清理临时文件失败：{error}"))
        });
    }
    if !destination.exists() {
        return std::fs::rename(temporary, destination)
            .map_err(|error| DeviceError::Operation(error.to_string()));
    }
    let destination_file = open_existing_no_follow(destination, false)?;
    if !destination_file
        .metadata()
        .map_err(|error| DeviceError::Operation(error.to_string()))?
        .is_file()
    {
        return Err(DeviceError::InvalidInput("只允许覆盖普通文件".to_owned()));
    }
    drop(destination_file);
    let backup = unique_backup_path(destination)?;
    std::fs::rename(destination, &backup)
        .map_err(|error| DeviceError::Operation(error.to_string()))?;
    if let Err(error) = std::fs::rename(temporary, destination) {
        let restore_error = std::fs::rename(&backup, destination).err();
        return Err(DeviceError::Operation(match restore_error {
            Some(restore_error) => {
                format!("替换目标文件失败：{error}；恢复原文件也失败：{restore_error}")
            }
            None => format!("替换目标文件失败，已恢复原文件：{error}"),
        }));
    }
    std::fs::remove_file(&backup).map_err(|error| {
        DeviceError::Operation(format!("新文件已提交，但清理旧文件备份失败：{error}"))
    })
}

fn sha256_file_no_follow(path: &Path) -> Result<String, DeviceError> {
    sha256_file_with_control(path, None)
}

fn sha256_file_with_control(
    path: &Path,
    control: Option<&FileCommitControl>,
) -> Result<String, DeviceError> {
    let mut file = open_existing_no_follow(path, false)?;
    let metadata = file
        .metadata()
        .map_err(|error| DeviceError::Operation(error.to_string()))?;
    if !metadata.is_file() {
        return Err(DeviceError::InvalidInput(
            "只允许计算普通文件哈希".to_owned(),
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; MAX_FILE_CHUNK_BYTES];
    loop {
        if let Some(control) = control {
            control.checkpoint()?;
        }
        let read = file
            .read(&mut buffer)
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(unix)]
fn apply_no_follow_flag(options: &mut StdOpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    const O_NOFOLLOW: i32 = 0x2_0000;
    options.custom_flags(O_NOFOLLOW);
}

#[cfg(windows)]
fn apply_no_follow_flag(options: &mut StdOpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(unix, windows)))]
fn apply_no_follow_flag(_options: &mut StdOpenOptions) {}

#[cfg(windows)]
fn reject_reparse_file(file: &std::fs::File) -> Result<(), DeviceError> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let attributes = file
        .metadata()
        .map_err(|error| DeviceError::Operation(error.to_string()))?
        .file_attributes();
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(DeviceError::InvalidInput(
            "不允许通过重解析点访问文件".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn reject_reparse_file(file: &std::fs::File) -> Result<(), DeviceError> {
    let metadata = file
        .metadata()
        .map_err(|error| DeviceError::Operation(error.to_string()))?;
    if !metadata.is_file() {
        return Err(DeviceError::InvalidInput("只允许访问普通文件".to_owned()));
    }
    Ok(())
}

fn validate_ssh_destination(host: &str, username: &str) -> Result<(), DeviceError> {
    if host.starts_with('-')
        || host.chars().any(char::is_whitespace)
        || host.chars().any(char::is_control)
        || username.chars().any(char::is_whitespace)
        || username.chars().any(char::is_control)
        || username.starts_with('-')
        || username.contains('@')
    {
        return Err(DeviceError::InvalidInput(
            "SSH 主机或用户名格式无效".to_owned(),
        ));
    }
    Ok(())
}

fn validate_service_name(service_name: &str) -> Result<(), DeviceError> {
    if service_name.is_empty()
        || service_name.len() > 256
        || service_name.starts_with('-')
        || service_name
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        || !service_name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '@')
        })
    {
        return Err(DeviceError::InvalidInput("服务名称格式无效".to_owned()));
    }
    Ok(())
}

fn command_exists(executable: &str) -> bool {
    let locator = if cfg!(windows) { "where" } else { "which" };
    let mut command = std::process::Command::new(locator);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        command.creation_flags(CREATE_NO_WINDOW.0);
    }
    command
        .arg(executable)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn decode_utf8_incremental(pending: &mut Vec<u8>, final_chunk: bool) -> String {
    if pending.is_empty() {
        return String::new();
    }
    if final_chunk {
        let text = String::from_utf8_lossy(pending).into_owned();
        pending.clear();
        return text;
    }
    match std::str::from_utf8(pending) {
        Ok(text) => {
            let text = text.to_owned();
            pending.clear();
            text
        }
        Err(error) => {
            let valid_bytes = error.valid_up_to();
            if valid_bytes == 0 {
                return String::new();
            }
            let text = std::str::from_utf8(&pending[..valid_bytes])
                .expect("UTF-8 校验返回的有效前缀必须可解码")
                .to_owned();
            pending.drain(..valid_bytes);
            text
        }
    }
}

#[cfg(windows)]
fn decode_windows_code_page_incremental(
    code_page: u32,
    pending: &mut Vec<u8>,
    final_chunk: bool,
) -> String {
    if pending.is_empty() {
        return String::new();
    }
    let encoder = EncoderCodePage(code_page);
    if let Ok(text) = encoder.to_string(pending) {
        pending.clear();
        return text;
    }
    if !final_chunk {
        for trailing_bytes in 1..=pending.len().min(3) {
            let prefix_length = pending.len() - trailing_bytes;
            if let Ok(text) = encoder.to_string(&pending[..prefix_length]) {
                pending.drain(..prefix_length);
                return text;
            }
        }
        return String::new();
    }
    let text = local_encoding_ng::windows::multi_byte_to_wide_char(code_page, 0, pending)
        .unwrap_or_else(|_| String::from_utf8_lossy(pending).into_owned());
    pending.clear();
    text
}

#[cfg(windows)]
fn decode_windows_cmd_incremental(pending: &mut Vec<u8>, final_chunk: bool) -> String {
    let mut decoded = String::new();
    while let Some(line_end) = pending.iter().position(|byte| *byte == b'\n') {
        let line = pending.drain(..=line_end).collect::<Vec<_>>();
        decoded.push_str(&decode_windows_cmd_line(&line));
    }
    if final_chunk && !pending.is_empty() {
        let line = std::mem::take(pending);
        decoded.push_str(&decode_windows_cmd_line(&line));
    }
    decoded
}

#[cfg(windows)]
fn decode_windows_cmd_line(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    let mut bytes = bytes.to_vec();
    decode_windows_code_page_incremental(WINDOWS_OEM_CODE_PAGE, &mut bytes, true)
}

#[cfg_attr(not(windows), allow(clippy::unnecessary_wraps))]
fn encode_shell_input(shell: ShellKind, text: &str) -> Result<Vec<u8>, DeviceError> {
    #[cfg(windows)]
    if matches!(shell, ShellKind::Cmd | ShellKind::System) {
        return local_encoding_ng::Encoding::OEM
            .to_bytes(text)
            .map_err(|error| {
                DeviceError::Operation(format!("无法按 CMD 代码页编码命令：{error}"))
            });
    }
    #[cfg(windows)]
    if matches!(shell, ShellKind::WindowsPowerShell | ShellKind::PowerShell) {
        let encoded = BASE64.encode(text.as_bytes());
        return Ok(format!(
            "Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded}')))\r\n"
        )
        .into_bytes());
    }
    let _ = shell;
    Ok(text.as_bytes().to_vec())
}

async fn read_process_output<R>(
    mut reader: R,
    stderr: bool,
    output: mpsc::UnboundedSender<CommandOutputChunk>,
    output_bytes: Arc<AtomicUsize>,
    output_exceeded: Arc<AtomicBool>,
    output_limit_notify: Arc<Notify>,
    output_encoding: ProcessOutputEncoding,
) -> Result<String, DeviceError>
where
    R: TokioAsyncRead + Unpin,
{
    let mut collected = String::new();
    let mut decoder = ProcessOutputDecoder::new(output_encoding);
    let mut buffer = vec![0_u8; 4096];
    loop {
        let count = reader
            .read(&mut buffer)
            .await
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        if count == 0 {
            break;
        }
        let previous = output_bytes.fetch_add(count, Ordering::AcqRel);
        let remaining = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(previous);
        let accepted = remaining.min(count);
        if accepted > 0 {
            let text = decoder.push(&buffer[..accepted], false);
            if !text.is_empty() {
                collected.push_str(&text);
                let _ = output.send(CommandOutputChunk { stderr, text });
            }
        }
        if accepted < count && !output_exceeded.swap(true, Ordering::AcqRel) {
            output_limit_notify.notify_one();
        }
    }
    let text = decoder.push(&[], true);
    if !text.is_empty() {
        collected.push_str(&text);
        let _ = output.send(CommandOutputChunk { stderr, text });
    }
    Ok(collected)
}

fn interactive_command_payload(shell: ShellKind, command: &str, marker: &str) -> String {
    if is_explicit_shell_exit(shell, command) {
        return format!("{command}{}", if cfg!(windows) { "\r\n" } else { "\n" });
    }
    let shell = if cfg!(windows) && shell == ShellKind::System {
        ShellKind::Cmd
    } else {
        shell
    };
    match shell {
        ShellKind::Cmd => {
            format!(
                "{command}\r\nset \"__remoteops_exit_code=%errorlevel%\"\r\necho {marker}%__remoteops_exit_code%\r\n1>&2 echo {marker}%__remoteops_exit_code%\r\n"
            )
        }
        ShellKind::WindowsPowerShell | ShellKind::PowerShell => {
            let style = if shell == ShellKind::PowerShell {
                "$PSStyle.OutputRendering = 'PlainText'; "
            } else {
                ""
            };
            format!(
                "[Console]::OutputEncoding = [Text.Encoding]::UTF8; {style}$remoteOpsExitCode = 1; $LASTEXITCODE = 0; try {{ . {{ {command}\n}}; if ($?) {{ $remoteOpsExitCode = 0 }} elseif ($LASTEXITCODE -ne 0) {{ $remoteOpsExitCode = $LASTEXITCODE }} }} catch {{ $remoteOpsExitCode = 1; [Console]::Error.WriteLine($_.ToString()) }} finally {{ Write-Output \"{marker}$remoteOpsExitCode\"; [Console]::Error.WriteLine(\"{marker}$remoteOpsExitCode\") }}\r\n\r\n"
            )
        }
        ShellKind::System => format!(
            "{command}\n__remoteops_exit_code=$?\nprintf '{marker}%s\\n' \"$__remoteops_exit_code\"\nprintf '{marker}%s\\n' \"$__remoteops_exit_code\" >&2\n"
        ),
    }
}

fn is_explicit_shell_exit(shell: ShellKind, command: &str) -> bool {
    let normalized = command.trim().trim_end_matches(';').trim();
    let mut parts = normalized.split_whitespace();
    if !parts
        .next()
        .is_some_and(|program| program.eq_ignore_ascii_case("exit"))
    {
        return false;
    }
    let arguments = parts.collect::<Vec<_>>();
    match shell {
        ShellKind::Cmd => match arguments.as_slice() {
            [] => true,
            [flag] => flag.eq_ignore_ascii_case("/b") || flag.parse::<i32>().is_ok(),
            [flag, code] => flag.eq_ignore_ascii_case("/b") && code.parse::<i32>().is_ok(),
            _ => false,
        },
        ShellKind::WindowsPowerShell | ShellKind::PowerShell | ShellKind::System => {
            matches!(arguments.as_slice(), [] | [_])
                && arguments
                    .first()
                    .is_none_or(|code| code.parse::<i32>().is_ok())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn competing_no_replace_publications_preserve_the_winner() {
        let directory = TestDirectory::new("atomic-no-replace-race");
        let destination = directory.0.join("winner.bin");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles = (0..2)
            .map(|index| {
                let temporary = directory.0.join(format!("candidate-{index}.part"));
                std::fs::write(&temporary, [index]).unwrap();
                let destination = destination.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (
                        index,
                        commit_temporary_file(&temporary, &destination, false),
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
        let winner = results.iter().find(|(_, result)| result.is_ok()).unwrap().0;
        assert_eq!(std::fs::read(destination).unwrap(), [winner]);
    }

    #[tokio::test]
    async fn failed_chunked_publication_preserves_staged_bytes_for_retry() {
        let directory = TestDirectory::new("failed-chunked-publication");
        let device = SystemDevice::with_transfer_root(&directory.0).unwrap();
        let contents = b"safe fixture";
        let hash = format!("{:x}", Sha256::digest(contents));
        let upload = device
            .begin_file_upload("result.bin", contents.len() as u64, &hash, false)
            .unwrap();
        upload.write_chunk(0, contents).await.unwrap();
        let destination = directory.0.join("result.bin");
        std::fs::write(&destination, b"other writer").unwrap();
        assert!(upload.complete().await.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"other writer");
        assert_eq!(std::fs::read(&*upload.temporary).unwrap(), contents);
        std::fs::remove_file(&destination).unwrap();
        assert_eq!(upload.complete().await.unwrap(), hash);
        assert_eq!(std::fs::read(&destination).unwrap(), contents);
        assert!(!upload.temporary.exists());
    }

    #[tokio::test]
    async fn cancelled_write_cannot_publish_or_leave_a_temporary() {
        let directory = TestDirectory::new("cancelled-single-write");
        let device = SystemDevice::with_transfer_root(&directory.0).unwrap();
        let control = FileCommitControl::default();
        assert!(control.try_cancel());
        assert!(
            device
                .write_file_with_control("result.bin", b"safe fixture", false, &control)
                .await
                .is_err()
        );
        assert!(!directory.0.join("result.bin").exists());
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn cancelled_chunked_commit_cleans_up_before_publication() {
        let directory = TestDirectory::new("cancelled-chunked-commit");
        let device = SystemDevice::with_transfer_root(&directory.0).unwrap();
        let contents = b"safe fixture";
        let hash = format!("{:x}", Sha256::digest(contents));
        let upload = device
            .begin_file_upload("result.bin", contents.len() as u64, &hash, false)
            .unwrap();
        upload.write_chunk(0, contents).await.unwrap();
        let control = FileCommitControl::default();
        let written = upload.written.clone();
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let lock_task = tokio::task::spawn_blocking(move || {
            let _lock = written.lock().unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.await.unwrap();
        let task_upload = upload.clone();
        let task_control = control.clone();
        let task =
            tokio::spawn(async move { task_upload.complete_with_control(&task_control).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !control.worker_active() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(control.try_cancel());
        release_tx.send(()).unwrap();
        lock_task.await.unwrap();
        assert!(task.await.unwrap().is_err());
        control.wait_idle().await;
        assert!(!directory.0.join("result.bin").exists());
        assert!(!upload.temporary.exists());
    }

    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("系统时间应有效")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "remoteops-device-{name}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("应创建测试目录");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct PartialWriter {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for PartialWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            let count = buffer.len().min(2);
            self.bytes.extend_from_slice(&buffer[..count]);
            Ok(count)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn serial_write_retries_partial_writes_and_flushes_once() {
        let mut writer = PartialWriter::default();
        let count = write_all_and_flush(&mut writer, b"display version\r").expect("完整写入应成功");
        assert_eq!(count, 16);
        assert_eq!(writer.bytes, b"display version\r");
        assert_eq!(writer.flushes, 1);
    }

    #[cfg(windows)]
    fn windows_process_exists(process_id: u32) -> bool {
        let powershell = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        std::process::Command::new(
            powershell.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
        )
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "if (Get-Process -Id {process_id} -ErrorAction SilentlyContinue) \
                     {{ exit 0 }} else {{ exit 1 }}"
            ),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
    }

    #[cfg(windows)]
    fn force_kill_windows_test_process(process_id: u32) {
        let system_root = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        let _ = std::process::Command::new(system_root.join(r"System32\taskkill.exe"))
            .args(["/PID", &process_id.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    #[cfg(windows)]
    #[test]
    fn normalizes_verbatim_windows_paths_for_child_processes() {
        assert_eq!(
            path_for_child_process(Path::new(r"\\?\C:\ProgramData\RemoteOps\known_hosts")),
            r"C:\ProgramData\RemoteOps\known_hosts"
        );
        assert_eq!(
            path_for_child_process(Path::new(r"\\?\UNC\server\share\id_ed25519")),
            r"\\server\share\id_ed25519"
        );
    }

    #[tokio::test]
    async fn streams_one_shot_command_output() {
        let device = SystemDevice::new();
        let shell = if cfg!(windows) {
            ShellKind::Cmd
        } else {
            ShellKind::System
        };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let result = device
            .run_streaming(shell, "echo REMOTEOPS_STREAMING", 10, sender)
            .await
            .expect("流式命令应执行成功");
        let mut streamed = String::new();
        while let Ok(chunk) = receiver.try_recv() {
            streamed.push_str(&chunk.text);
        }

        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("REMOTEOPS_STREAMING"));
        assert!(streamed.contains("REMOTEOPS_STREAMING"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_one_shot_preserves_utf8_output() {
        let script =
            "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Write-Output 'CMD中文-RemoteOps'";
        let encoded_script = BASE64.encode(
            script
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let command = format!(
            "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded_script}"
        );
        let device = SystemDevice::new();
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);

        let result = device
            .run_streaming(ShellKind::Cmd, &command, 10, sender)
            .await
            .expect("CMD 子进程的 UTF-8 输出应执行成功");

        assert_eq!(result.exit_code, Some(0));
        assert!(
            result.stdout.contains("CMD中文-RemoteOps"),
            "实际输出：{:?}",
            result.stdout
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_cp936_decoder_preserves_split_chinese_character() {
        let mut pending = Vec::new();

        assert_eq!(
            decode_windows_code_page_incremental(936, &mut pending, false),
            ""
        );
        pending.push(0xD6);
        assert_eq!(
            decode_windows_code_page_incremental(936, &mut pending, false),
            ""
        );
        assert_eq!(pending, [0xD6]);
        assert_eq!(
            {
                pending.extend_from_slice(&[0xD0, 0xCE, 0xC4]);
                decode_windows_code_page_incremental(936, &mut pending, false)
            },
            "中文"
        );
        assert!(pending.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_oem_codec_round_trips_chinese_when_supported() {
        let Ok(encoded) = local_encoding_ng::Encoding::OEM.to_bytes("持久CMD中文-RemoteOps")
        else {
            return;
        };

        assert_eq!(
            decode_windows_cmd_line(&encoded),
            "持久CMD中文-RemoteOps",
            "OEM 字节：{encoded:02X?}"
        );
    }

    #[test]
    fn utf8_decoder_preserves_split_multibyte_character() {
        let mut decoder = ProcessOutputDecoder::new(ProcessOutputEncoding::Utf8);

        assert_eq!(decoder.push(&[0xE4, 0xB8], false), "");
        assert_eq!(decoder.push(&[0xAD], false), "中");
        assert_eq!(decoder.push(&[], true), "");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn managed_windows_process_has_no_console_window() {
        let device = SystemDevice::new();
        let script = r#"Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class NativeMethods { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'; if ([NativeMethods]::GetConsoleWindow() -eq [IntPtr]::Zero) { Write-Output 'NO_CONSOLE' } else { exit 1 }"#;
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);

        let result = device
            .run_streaming(ShellKind::WindowsPowerShell, script, 20, sender)
            .await
            .expect("后台 Windows 命令应执行成功");

        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("NO_CONSOLE"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn managed_powershell_7_process_is_hidden_and_returns_output() {
        if !command_exists("pwsh.exe") {
            return;
        }
        let device = SystemDevice::new();
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);

        let result = device
            .run_streaming(
                ShellKind::PowerShell,
                r#"Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class RemoteOpsPwshNativeMethods { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'; if ([RemoteOpsPwshNativeMethods]::GetConsoleWindow() -eq [IntPtr]::Zero) { Write-Output 'NO_CONSOLE' } else { exit 1 }"#,
                20,
                sender,
            )
            .await
            .expect("PowerShell 7 后台命令应执行成功");

        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("NO_CONSOLE"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_one_shot_and_persistent_processes_have_no_console_window() {
        let directory = TestDirectory::new("cmd-console-probe");
        let script_path = directory.0.join("console-probe.ps1");
        std::fs::write(
            &script_path,
            r#"Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class RemoteOpsCmdNativeMethods { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'; if ([RemoteOpsCmdNativeMethods]::GetConsoleWindow() -eq [IntPtr]::Zero) { Write-Output 'NO_CONSOLE' } else { exit 1 }"#,
        )
        .expect("应写入控制台探测脚本");
        let command = format!(
            "powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File {}",
            path_for_child_process(&script_path)
        );
        let device = SystemDevice::new();
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let one_shot = device
            .run_streaming(ShellKind::Cmd, &command, 20, sender)
            .await
            .expect("CMD 一次性命令应静默执行");
        assert_eq!(
            one_shot.exit_code,
            Some(0),
            "stdout={} stderr={}",
            one_shot.stdout,
            one_shot.stderr
        );
        assert!(one_shot.stdout.contains("NO_CONSOLE"));

        let session = device
            .open_interactive_shell(ShellKind::Cmd)
            .await
            .expect("CMD 持久 Shell 应静默打开");
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let persistent = session
            .run(&command, "cmd-console-probe", 20, sender)
            .await
            .expect("CMD 持久命令应静默执行");
        assert_eq!(persistent.exit_code, Some(0));
        assert!(persistent.stdout.contains("NO_CONSOLE"));
        session.close().await.expect("应关闭 CMD 持久 Shell");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn one_shot_command_completes_while_persistent_powershell_is_open() {
        if !command_exists("pwsh.exe") {
            return;
        }
        let device = SystemDevice::new();
        let session = device
            .open_interactive_shell(ShellKind::PowerShell)
            .await
            .expect("应打开持久 PowerShell 7");
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);

        let result = device
            .run_streaming(
                ShellKind::WindowsPowerShell,
                "Write-Output 'REMOTEOPS_CONCURRENT_ONESHOT'",
                20,
                sender,
            )
            .await
            .expect("持久 Shell 存在时一次性命令应执行成功");
        session.close().await.expect("应关闭持久 PowerShell 7");

        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("REMOTEOPS_CONCURRENT_ONESHOT"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_powershell_commands_avoid_conflicting_window_style_flags() {
        let device = SystemDevice::new();

        for shell in [ShellKind::WindowsPowerShell, ShellKind::PowerShell] {
            let (_, one_shot) = device
                .command_for_shell(shell, "Write-Output 'REMOTEOPS'")
                .expect("Windows PowerShell 命令应可创建");
            let (_, interactive) = device
                .command_for_interactive_shell(shell)
                .expect("Windows PowerShell 持久命令应可创建");

            assert!(!one_shot.iter().any(|argument| argument == "-WindowStyle"));
            assert!(
                !interactive
                    .iter()
                    .any(|argument| argument == "-WindowStyle")
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn aborting_one_shot_command_kills_windows_process_tree() {
        let directory = TestDirectory::new("process-tree");
        let process_id_path = directory.0.join("child-process-id.txt");
        let escaped_path = process_id_path.to_string_lossy().replace('\'', "''");
        let script = format!(
            "$child = Start-Process -FilePath ping.exe \
             -ArgumentList '127.0.0.1','-n','30' -NoNewWindow -PassThru; \
             $child.Id | Set-Content -LiteralPath '{escaped_path}' -Encoding ascii; \
             $child.WaitForExit()"
        );
        let device = SystemDevice::new();
        let runner = tokio::spawn(async move {
            let (sender, receiver) = mpsc::unbounded_channel();
            drop(receiver);
            device
                .run_process_streaming(
                    "powershell.exe".to_owned(),
                    vec![
                        "-NoProfile".to_owned(),
                        "-NonInteractive".to_owned(),
                        "-Command".to_owned(),
                        script,
                    ],
                    60,
                    sender,
                )
                .await
        });

        // CI 上首次启动 PowerShell 可能较慢；等待测试就绪与取消后的退出时限分开。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let process_id = loop {
            if let Ok(text) = std::fs::read_to_string(&process_id_path)
                && let Ok(process_id) = text.trim().parse::<u32>()
            {
                break process_id;
            }
            if tokio::time::Instant::now() >= deadline {
                runner.abort();
                let _ = runner.await;
                panic!("子进程没有及时写出 PID");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        if !windows_process_exists(process_id) {
            runner.abort();
            let _ = runner.await;
            panic!("取消前子进程应仍在运行");
        }

        runner.abort();
        let join_error = runner.await.expect_err("命令任务应被取消");
        assert!(join_error.is_cancelled());

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while windows_process_exists(process_id) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if windows_process_exists(process_id) {
            force_kill_windows_test_process(process_id);
            panic!("取消任务后 Windows 子进程树仍在运行");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn parent_exit_reaps_windows_descendant_without_waiting() {
        let directory = TestDirectory::new("parent-exit-process-tree");
        let process_id_path = directory.0.join("child-process-id.txt");
        let escaped_path = process_id_path.to_string_lossy().replace('\'', "''");
        let script = format!(
            "$child = Start-Process -FilePath ping.exe \
             -ArgumentList '127.0.0.1','-n','30' -NoNewWindow -PassThru; \
             $child.Id | Set-Content -LiteralPath '{escaped_path}' -Encoding ascii"
        );
        let device = SystemDevice::new();
        let runner = tokio::spawn(async move {
            let (sender, receiver) = mpsc::unbounded_channel();
            drop(receiver);
            device
                .run_process_streaming(
                    "powershell.exe".to_owned(),
                    vec![
                        "-NoProfile".to_owned(),
                        "-NonInteractive".to_owned(),
                        "-Command".to_owned(),
                        script,
                    ],
                    60,
                    sender,
                )
                .await
        });

        // 与取消测试使用相同的启动等待上限，不放宽后续进程树回收断言。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let process_id = loop {
            if let Ok(text) = std::fs::read_to_string(&process_id_path)
                && let Ok(process_id) = text.trim().parse::<u32>()
            {
                break process_id;
            }
            if tokio::time::Instant::now() >= deadline {
                runner.abort();
                let _ = runner.await;
                panic!("父进程退出测试没有及时写出后代 PID");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let result = timeout(Duration::from_secs(5), runner)
            .await
            .expect("顶层进程退出后命令不应继续等待后代")
            .expect("命令任务不应崩溃")
            .expect("顶层进程退出应返回成功");
        assert_eq!(result.exit_code, Some(0));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while windows_process_exists(process_id) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if windows_process_exists(process_id) {
            force_kill_windows_test_process(process_id);
            panic!("顶层进程退出后 Windows 后代进程仍在运行");
        }
    }

    #[tokio::test]
    async fn rejects_command_output_above_limit() {
        let device = SystemDevice::new();
        let (shell, command) = if cfg!(windows) {
            (
                ShellKind::WindowsPowerShell,
                format!(
                    "[Console]::Out.Write(('x' * {}))",
                    MAX_COMMAND_OUTPUT_BYTES + 4096
                ),
            )
        } else {
            (
                ShellKind::System,
                format!("yes x | head -c {}", MAX_COMMAND_OUTPUT_BYTES + 4096),
            )
        };
        let error = device
            .run(shell, &command, 20)
            .await
            .expect_err("超量输出应被中断");

        assert!(matches!(
            error,
            DeviceError::OutputLimit {
                limit: MAX_COMMAND_OUTPUT_BYTES
            }
        ));
    }

    #[tokio::test]
    async fn confines_file_operations_to_transfer_root() {
        let directory = TestDirectory::new("transfer-root");
        std::fs::create_dir_all(directory.0.join("nested")).expect("应创建子目录");
        let device = SystemDevice::with_transfer_root(&directory.0).expect("应创建受限设备适配器");

        device
            .write_file("nested/result.txt", b"remoteops", false)
            .await
            .expect("应写入交换目录");
        let bytes = device
            .read_file("nested/result.txt", 1024)
            .await
            .expect("应读取交换目录");

        assert_eq!(bytes, b"remoteops");
        assert!(
            device
                .write_file("../escape.txt", b"x", false)
                .await
                .is_err()
        );
        assert!(device.read_file("../escape.txt", 1024).await.is_err());
        assert!(
            device
                .write_file("nested/stream:name.txt", b"x", false)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn no_overwrite_mode_is_atomic() {
        let directory = TestDirectory::new("no-overwrite");
        let device = SystemDevice::with_transfer_root(&directory.0).expect("应创建受限设备适配器");
        device
            .write_file("existing.txt", b"first", false)
            .await
            .expect("首次写入应成功");
        let error = device
            .write_file("existing.txt", b"second", false)
            .await
            .expect_err("禁止覆盖时应拒绝现有文件");
        let bytes = device
            .read_file("existing.txt", 1024)
            .await
            .expect("原文件仍应可读");

        assert!(matches!(error, DeviceError::Operation(_)));
        assert_eq!(bytes, b"first");
    }

    #[tokio::test]
    async fn completes_chunked_upload_and_creates_nested_directories() {
        let directory = TestDirectory::new("chunked-upload");
        let device = SystemDevice::with_transfer_root(&directory.0).expect("应创建受限设备适配器");
        let contents = b"remoteops-chunked-transfer";
        let expected_hash = format!("{:x}", Sha256::digest(contents));
        let upload = device
            .begin_file_upload(
                "new/nested/result.bin",
                contents.len() as u64,
                &expected_hash,
                false,
            )
            .expect("应开始分块上传并自动创建目录");

        upload
            .write_chunk(0, &contents[..9])
            .await
            .expect("首块应写入");
        assert!(upload.write_chunk(10, &contents[9..]).await.is_err());
        upload
            .write_chunk(9, &contents[9..])
            .await
            .expect("连续分块应写入");
        let actual_hash = upload.complete().await.expect("完整上传应提交");

        assert_eq!(actual_hash, expected_hash);
        assert_eq!(
            device
                .read_file("new/nested/result.bin", 1024)
                .await
                .expect("应读取已提交文件"),
            contents
        );
    }

    #[tokio::test]
    async fn failed_chunked_overwrite_preserves_original_and_abort_removes_temporary() {
        let directory = TestDirectory::new("chunked-overwrite");
        let device = SystemDevice::with_transfer_root(&directory.0).expect("应创建受限设备适配器");
        device
            .write_file("existing.bin", b"original", false)
            .await
            .expect("应创建原文件");
        let upload = device
            .begin_file_upload("existing.bin", 3, &"0".repeat(64), true)
            .expect("应创建覆盖上传临时文件");
        upload.write_chunk(0, b"new").await.expect("应写入临时文件");

        assert!(upload.complete().await.is_err());
        assert_eq!(
            device
                .read_file("existing.bin", 1024)
                .await
                .expect("校验失败后原文件仍应存在"),
            b"original"
        );
        upload.abort().await.expect("应删除未提交临时文件");
        assert!(
            std::fs::read_dir(&directory.0)
                .expect("应读取交换目录")
                .all(|entry| !entry
                    .expect("目录项应有效")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".part"))
        );
    }

    #[tokio::test]
    async fn reads_file_chunks_with_exact_offsets_and_streaming_hash() {
        let directory = TestDirectory::new("chunked-download");
        let device = SystemDevice::with_transfer_root(&directory.0).expect("应创建受限设备适配器");
        let contents = b"abcdefghij";
        device
            .write_file("download.bin", contents, false)
            .await
            .expect("应创建下载文件");

        let first = device
            .read_file_chunk("download.bin", 0, 4)
            .await
            .expect("应读取首块");
        let last = device
            .read_file_chunk("download.bin", 4, 16)
            .await
            .expect("应读取末块");

        assert_eq!(first.bytes, b"abcd");
        assert!(!first.eof);
        assert_eq!(last.bytes, b"efghij");
        assert!(last.eof);
        assert_eq!(
            device
                .file_sha256("download.bin")
                .await
                .expect("应计算哈希"),
            format!("{:x}", Sha256::digest(contents))
        );
        assert!(device.read_file_chunk("download.bin", 11, 4).await.is_err());
    }

    #[tokio::test]
    async fn manages_file_metadata_move_and_delete_inside_transfer_root() {
        let directory = TestDirectory::new("file-management");
        let device = SystemDevice::with_transfer_root(&directory.0).expect("应创建设备");
        device
            .write_file("source.txt", b"remoteops", false)
            .await
            .expect("应创建源文件");

        let metadata = device
            .file_metadata("source.txt")
            .await
            .expect("应读取元数据");
        assert_eq!(metadata.size, 9);
        assert_eq!(metadata.path, "source.txt");

        std::fs::create_dir_all(directory.0.join("nested")).expect("应创建目标目录");
        device
            .move_file("source.txt", "nested/renamed.txt", false)
            .await
            .expect("应移动文件");
        assert!(device.file_metadata("source.txt").await.is_err());
        assert_eq!(
            device
                .file_metadata("nested/renamed.txt")
                .await
                .expect("应读取移动后的文件")
                .size,
            9
        );

        device
            .delete_file("nested/renamed.txt")
            .await
            .expect("应删除文件");
        assert!(device.file_metadata("nested/renamed.txt").await.is_err());
    }

    #[tokio::test]
    async fn exchanges_bounded_tcp_payload_without_scanning() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("应绑定测试 TCP 监听");
        let address = listener.local_addr().expect("应读取测试地址");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("应接受测试连接");
            let mut input = Vec::new();
            stream
                .read_to_end(&mut input)
                .await
                .expect("应读取测试请求");
            stream
                .write_all(&input.iter().rev().copied().collect::<Vec<_>>())
                .await
                .expect("应发送测试响应");
        });
        let device = SystemDevice::new();
        let result = device
            .exchange("127.0.0.1", address.port(), b"abc", 64, 5_000)
            .await
            .expect("TCP 收发应成功");
        assert_eq!(result.sent_bytes, 3);
        assert_eq!(result.received, b"cba");
        server.await.expect("测试服务端应结束");
    }

    #[test]
    fn persistent_shell_frames_preserve_every_chunk_boundary() {
        let marker = "__REMOTEOPS_DONE_frame-test__";
        let prefix = "前缀\nwithout-newline";
        let wire = format!("{prefix}{marker}-2147483648\r\nnext-command");
        for split in 0..=wire.len() {
            let (sender, mut receiver) = mpsc::unbounded_channel();
            let mut stream = InteractiveShellOutput::new(marker, ProcessOutputEncoding::Utf8, true);
            let mut bytes = 0;
            stream
                .push(&wire.as_bytes()[..split], &mut bytes, &sender)
                .unwrap();
            stream
                .push(&wire.as_bytes()[split..], &mut bytes, &sender)
                .unwrap();
            assert_eq!(stream.collected, prefix, "split {split}");
            assert_eq!(stream.exit_code, Some(i32::MIN));
            assert_eq!(stream.pending, b"next-command");
            assert_eq!(bytes, prefix.len());
            let mut streamed = String::new();
            while let Ok(chunk) = receiver.try_recv() {
                assert!(chunk.stderr);
                streamed.push_str(&chunk.text);
            }
            assert_eq!(streamed, prefix);
        }
    }

    #[test]
    fn persistent_shell_parser_bounds_unterminated_marker_candidates() {
        let marker = "__REMOTEOPS_DONE_bounded__";
        let mut stream = InteractiveShellOutput::new(marker, ProcessOutputEncoding::Utf8, false);
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let malformed = format!("{marker}{}", "7".repeat(8192));
        let mut bytes = 0;
        for byte in malformed.bytes() {
            stream.push(&[byte], &mut bytes, &sender).unwrap();
            assert!(stream.pending.len() <= marker.len() + 12);
        }
        stream.finish(&mut bytes, &sender).unwrap();
        assert_eq!(stream.collected, malformed);
        assert_eq!(stream.exit_code, None);
        assert_eq!(bytes, malformed.len());
    }

    #[test]
    fn persistent_shell_streams_share_a_raw_byte_limit() {
        let marker = "__REMOTEOPS_DONE_limit__";
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let mut stdout = InteractiveShellOutput::new(marker, ProcessOutputEncoding::Utf8, false);
        let mut stderr = InteractiveShellOutput::new(marker, ProcessOutputEncoding::Utf8, true);
        let mut bytes = 0;
        // 非 UTF-8 字节损失解码后变长，预算仍依据原始字节。
        let chunk = [0xff; 4096];
        for index in 0..MAX_COMMAND_OUTPUT_BYTES / chunk.len() {
            let stream = if index % 2 == 0 {
                &mut stdout
            } else {
                &mut stderr
            };
            stream.push(&chunk, &mut bytes, &sender).unwrap();
            assert!(stream.pending.len() <= marker.len() + 12);
        }
        assert_eq!(bytes, MAX_COMMAND_OUTPUT_BYTES);
        assert!(matches!(
            stderr.push(b"x", &mut bytes, &sender),
            Err(DeviceError::OutputLimit { .. })
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn persistent_shell_limit_counts_filtered_terminal_bytes() {
        let marker = "__REMOTEOPS_DONE_escape-limit__";
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let mut stream =
            InteractiveShellOutput::new(marker, ProcessOutputEncoding::PlainUtf8, false);
        let mut bytes = 0;
        let chunk = b"\x1b[0m".repeat(1024);
        for _ in 0..MAX_COMMAND_OUTPUT_BYTES / chunk.len() {
            stream.push(&chunk, &mut bytes, &sender).unwrap();
        }
        assert_eq!(stream.collected, "");
        assert!(matches!(
            stream.push(&chunk, &mut bytes, &sender),
            Err(DeviceError::OutputLimit { .. })
        ));
    }

    #[tokio::test]
    async fn persistent_shell_interrupt_does_not_wait_for_io_lock() {
        let shell = if cfg!(windows) {
            ShellKind::Cmd
        } else {
            ShellKind::System
        };
        let session = SystemDevice::new()
            .open_interactive_shell(shell)
            .await
            .unwrap();
        let _io = session.inner.lock().await;
        timeout(Duration::from_secs(5), session.interrupt())
            .await
            .unwrap()
            .unwrap();
        assert!(session.is_closed());
        assert!(session.has_exited().await.unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persistent_shell_stdin_backpressure_obeys_total_timeout() {
        let session = SystemDevice::new()
            .open_interactive_shell(ShellKind::System)
            .await
            .unwrap();
        let command = format!("sleep 30\n#{}", "x".repeat(512 * 1024));
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let started = Instant::now();
        let result = timeout(
            Duration::from_secs(5),
            session.run(&command, "write-timeout", 1, sender),
        )
        .await
        .expect("背压写入必须遵守总超时");
        assert!(matches!(result, Err(DeviceError::Timeout)));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(session.is_closed());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persistent_shell_stdin_backpressure_can_be_interrupted() {
        let session = SystemDevice::new()
            .open_interactive_shell(ShellKind::System)
            .await
            .unwrap();
        let command = format!("printf ready\nsleep 30\n#{}", "x".repeat(512 * 1024));
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let worker = session.clone();
        let task =
            tokio::spawn(async move { worker.run(&command, "write-cancel", 60, sender).await });
        let first = timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.text, "ready");
        timeout(Duration::from_secs(5), session.interrupt())
            .await
            .unwrap()
            .unwrap();
        let result = timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert!(session.is_closed());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persistent_shell_drains_output_while_writing_large_input() {
        let session = SystemDevice::new()
            .open_interactive_shell(ShellKind::System)
            .await
            .unwrap();
        let command = format!("head -c 131072 /dev/zero\n#{}", "x".repeat(512 * 1024));
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let result = session
            .run(&command, "write-read", 5, sender)
            .await
            .unwrap();
        assert_eq!(result.stdout.as_bytes(), vec![0; 131_072]);
        assert_eq!(result.exit_code, Some(0));
        assert!(!session.is_closed());
        session.close().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persistent_shell_preserves_interleaved_unterminated_output() {
        let session = SystemDevice::new()
            .open_interactive_shell(ShellKind::System)
            .await
            .unwrap();
        let cases = [
            (
                "printf 'out-'; printf 'err-line\\n' >&2; sleep 0.05; printf 'end\\n'; printf 'err-tail' >&2",
                "out-end\n",
                "err-line\nerr-tail",
            ),
            (
                "printf 'err-' >&2; printf 'out-line\\n'; sleep 0.05; printf 'end\\n' >&2; printf 'out-tail'",
                "out-line\nout-tail",
                "err-end\n",
            ),
            ("printf 'only-error' >&2", "", "only-error"),
            (":", "", ""),
        ];
        for (index, (command, expected_stdout, expected_stderr)) in cases.into_iter().enumerate() {
            let (sender, mut receiver) = mpsc::unbounded_channel();
            let result = session
                .run(command, &format!("interleaved-{index}"), 5, sender)
                .await
                .unwrap();
            assert_eq!(result.stdout, expected_stdout);
            assert_eq!(result.stderr, expected_stderr);
            assert_eq!(result.exit_code, Some(0));
            let mut streamed_stdout = String::new();
            let mut streamed_stderr = String::new();
            while let Ok(chunk) = receiver.try_recv() {
                if chunk.stderr {
                    streamed_stderr.push_str(&chunk.text);
                } else {
                    streamed_stdout.push_str(&chunk.text);
                }
            }
            assert_eq!(streamed_stdout, expected_stdout);
            assert_eq!(streamed_stderr, expected_stderr);
        }
        session.close().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persistent_shell_limits_no_newline_output_on_either_stream() {
        for redirect in ["", " >&2"] {
            let session = SystemDevice::new()
                .open_interactive_shell(ShellKind::System)
                .await
                .unwrap();
            let command = format!(
                "head -c {} /dev/zero{redirect}",
                MAX_COMMAND_OUTPUT_BYTES + 8192
            );
            let (sender, receiver) = mpsc::unbounded_channel();
            drop(receiver);
            let result = session.run(&command, "no-newline-limit", 5, sender).await;
            assert!(matches!(result, Err(DeviceError::OutputLimit { .. })));
            assert!(session.is_closed());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persistent_shell_aborted_run_closes_session() {
        let session = SystemDevice::new()
            .open_interactive_shell(ShellKind::System)
            .await
            .unwrap();
        let worker = session.clone();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            worker
                .run("printf ready; sleep 30", "abort-run", 60, sender)
                .await
        });
        timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(session.is_closed());
        timeout(
            Duration::from_secs(5),
            wait_for_interactive_shell_exit(session.exit.clone()),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persistent_shell_queue_wait_is_included_in_timeout() {
        let session = SystemDevice::new()
            .open_interactive_shell(ShellKind::System)
            .await
            .unwrap();
        let _io = session.inner.lock().await;
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let result = timeout(
            Duration::from_secs(3),
            session.run(":", "queue-timeout", 1, sender),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(DeviceError::Timeout)));
        assert!(!session.is_closed());
        session.close().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persistent_shell_unexpected_exit_drains_both_streams() {
        let session = SystemDevice::new()
            .open_interactive_shell(ShellKind::System)
            .await
            .unwrap();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let result = session
            .run(
                "printf out-tail; printf err-tail >&2; exit 9",
                "unexpected-exit",
                5,
                sender,
            )
            .await;
        assert!(matches!(result, Err(DeviceError::Operation(_))));
        assert!(session.is_closed());
        let mut stdout = String::new();
        let mut stderr = String::new();
        while let Ok(chunk) = receiver.try_recv() {
            if chunk.stderr {
                stderr.push_str(&chunk.text);
            } else {
                stdout.push_str(&chunk.text);
            }
        }
        assert_eq!(stdout, "out-tail");
        assert_eq!(stderr, "err-tail");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn persistent_shell_interrupt_reaps_descendant_processes() {
        let session = SystemDevice::new()
            .open_interactive_shell(ShellKind::System)
            .await
            .unwrap();
        let worker = session.clone();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            worker
                .run(
                    "sleep 30 & printf '%s\\n' \"$!\"; wait",
                    "descendant",
                    60,
                    sender,
                )
                .await
        });
        let process_id = timeout(Duration::from_secs(5), async {
            let mut line = String::new();
            loop {
                line.push_str(&receiver.recv().await.unwrap().text);
                if line.ends_with('\n') {
                    break line.trim().parse::<u32>().unwrap();
                }
            }
        })
        .await
        .unwrap();
        session.interrupt().await.unwrap();
        assert!(task.await.unwrap().is_err());
        timeout(Duration::from_secs(5), async {
            loop {
                let status = std::fs::read_to_string(format!("/proc/{process_id}/stat"));
                if status.is_err() || status.unwrap().split_whitespace().nth(2) == Some("Z") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("中断必须清理 Shell 的后代进程");
    }

    #[tokio::test]
    async fn persistent_powershell_stdin_submits_multiline_payload_when_available() {
        use tokio::io::{AsyncBufReadExt as _, BufReader};
        let executable = if cfg!(windows) { "pwsh.exe" } else { "pwsh" };
        if !command_exists(executable) {
            return;
        }
        let mut child = Command::new(executable)
            .args(["-NoProfile", "-NonInteractive", "-Command", "-"])
            .env("TERM", "dumb")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        for (index, command) in [
            "Write-Output 'FIRST'",
            "Write-Output 'SECOND'",
            "throw 'FIXTURE_ERROR'",
        ]
        .into_iter()
        .enumerate()
        {
            let marker = format!("__REMOTEOPS_DONE_submission{index}__");
            let payload = interactive_command_payload(ShellKind::PowerShell, command, &marker);
            stdin.write_all(payload.as_bytes()).await.unwrap();
            stdin.flush().await.unwrap();
            let expected_code = i32::from(index == 2);
            let expected_marker = format!("{marker}{expected_code}");
            let (out, err) = timeout(Duration::from_secs(20), async {
                let mut out = String::new();
                loop {
                    let mut line = String::new();
                    assert_ne!(stdout.read_line(&mut line).await.unwrap(), 0);
                    if line.trim() == expected_marker {
                        break;
                    }
                    out.push_str(&line);
                }
                let mut err = String::new();
                loop {
                    let mut line = String::new();
                    assert_ne!(stderr.read_line(&mut line).await.unwrap(), 0);
                    if line.trim() == expected_marker {
                        break;
                    }
                    err.push_str(&line);
                }
                (out, err)
            })
            .await
            .expect("multiline stdin must submit without waiting for EOF or the next command");
            match index {
                0 => assert!(out.contains("FIRST")),
                1 => assert!(out.contains("SECOND")),
                _ => assert!(err.contains("FIXTURE_ERROR")),
            }
        }
        stdin.write_all(b"exit\n").await.unwrap();
        drop(stdin);
        timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn persistent_powershell_errors_complete_and_preserve_session() {
        let mut shells = vec![ShellKind::WindowsPowerShell];
        if command_exists("pwsh.exe") {
            shells.push(ShellKind::PowerShell);
        }
        for shell in shells {
            let session = SystemDevice::new()
                .open_interactive_shell(shell)
                .await
                .unwrap();
            for (tag, text) in [
                ("first-submission", "FIRST"),
                ("second-submission", "SECOND"),
            ] {
                let (sender, receiver) = mpsc::unbounded_channel();
                drop(receiver);
                let result = session
                    .run(&format!("Write-Output '{text}'"), tag, 20, sender)
                    .await
                    .expect("simple consecutive PowerShell commands must submit independently");
                assert_eq!(result.exit_code, Some(0));
                assert!(result.stdout.contains(text));
            }
            for (index, (command, expected_code)) in [
                ("throw 'REMOTEOPS_THROW_REGRESSION'", 1),
                (
                    "Write-Error 'REMOTEOPS_STOP_REGRESSION' -ErrorAction Stop",
                    1,
                ),
                ("cmd.exe /D /C exit 7", 7),
                (
                    "[Console]::Error.Write('REMOTEOPS_STDERR'); Write-Output 'REMOTEOPS_OK'",
                    0,
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let (sender, receiver) = mpsc::unbounded_channel();
                drop(receiver);
                let result = session
                    .run(command, &format!("powershell-error-{index}"), 20, sender)
                    .await
                    .expect("终止性错误也应返回完成边界");
                assert_eq!(
                    result.exit_code,
                    Some(expected_code),
                    "{shell:?}: {result:?}"
                );
                if index < 2 {
                    assert!(result.stderr.contains("REGRESSION"));
                }
                if index == 3 {
                    assert!(result.stdout.contains("REMOTEOPS_OK"));
                    assert_eq!(result.stderr, "REMOTEOPS_STDERR");
                }
                assert!(!session.is_closed());
            }
            let (sender, receiver) = mpsc::unbounded_channel();
            drop(receiver);
            let result = session
                .run("exit 3", "powershell-explicit-exit", 20, sender)
                .await
                .unwrap();
            assert_eq!(result.exit_code, Some(3));
            assert!(session.is_closed());
        }
    }

    #[tokio::test]
    async fn persistent_shell_keeps_process_state() {
        let device = SystemDevice::new();
        let shell = if cfg!(windows) {
            ShellKind::Cmd
        } else {
            ShellKind::System
        };
        let session = device
            .open_interactive_shell(shell)
            .await
            .expect("应打开持久 Shell");
        let set_command = if cfg!(windows) {
            "set REMOTEOPS_PERSIST=alpha"
        } else {
            "REMOTEOPS_PERSIST=alpha"
        };
        let get_command = if cfg!(windows) {
            "echo %REMOTEOPS_PERSIST%"
        } else {
            "echo $REMOTEOPS_PERSIST"
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        session
            .run(set_command, "set-state", 10, sender)
            .await
            .expect("应设置持久状态");
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let result = session
            .run(get_command, "get-state", 10, sender)
            .await
            .expect("应读取持久状态");
        session.close().await.expect("应关闭持久 Shell");

        assert!(result.stdout.to_lowercase().contains("alpha"));
        assert_eq!(result.exit_code, Some(0));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn persistent_powershell_keeps_local_variable_state() {
        let device = SystemDevice::new();
        let mut shells = vec![ShellKind::WindowsPowerShell];
        if command_exists("pwsh.exe") {
            shells.push(ShellKind::PowerShell);
        }

        for shell in shells {
            let session = device
                .open_interactive_shell(shell)
                .await
                .expect("应打开持久 PowerShell");
            let (sender, receiver) = mpsc::unbounded_channel();
            drop(receiver);
            session
                .run(
                    "$RemoteOpsPersistentVariable = '持久变量值'",
                    "set-powershell-variable",
                    10,
                    sender,
                )
                .await
                .expect("应设置持久 PowerShell 变量");
            let (sender, receiver) = mpsc::unbounded_channel();
            drop(receiver);
            let result = session
                .run(
                    "Write-Output $RemoteOpsPersistentVariable",
                    "get-powershell-variable",
                    10,
                    sender,
                )
                .await
                .expect("应读取持久 PowerShell 变量");
            session.close().await.expect("应关闭持久 PowerShell");

            assert_eq!(result.exit_code, Some(0));
            assert!(
                result.stdout.contains("持久变量值"),
                "Shell {shell:?} 实际输出：{:?}",
                result.stdout
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn persistent_cmd_preserves_chinese_output() {
        if local_encoding_ng::Encoding::OEM
            .to_bytes("持久CMD中文-RemoteOps")
            .is_err()
        {
            return;
        }
        let device = SystemDevice::new();
        let session = device
            .open_interactive_shell(ShellKind::Cmd)
            .await
            .expect("应打开持久 CMD");
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);

        let result = session
            .run("echo 持久CMD中文-RemoteOps", "cmd-chinese", 10, sender)
            .await
            .expect("持久 CMD 中文命令应执行成功");
        session.close().await.expect("应关闭持久 CMD");

        assert_eq!(result.exit_code, Some(0));
        assert!(
            result.stdout.contains("持久CMD中文-RemoteOps"),
            "实际输出：{:?}",
            result.stdout
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn persistent_powershell_accepts_utf8_commands() {
        let device = SystemDevice::new();
        let shells = if command_exists("pwsh.exe") {
            vec![ShellKind::WindowsPowerShell, ShellKind::PowerShell]
        } else {
            vec![ShellKind::WindowsPowerShell]
        };

        for shell_kind in shells {
            let session = device
                .open_interactive_shell(shell_kind)
                .await
                .expect("持久 PowerShell 应能打开");
            let (sender, receiver) = mpsc::unbounded_channel();
            drop(receiver);
            let result = session
                .run(
                    "Write-Output '持久PowerShell中文'",
                    "powershell-chinese-input",
                    10,
                    sender,
                )
                .await
                .expect("持久 PowerShell 中文命令应执行成功");
            session.close().await.expect("应关闭持久 PowerShell");

            assert_eq!(result.exit_code, Some(0));
            assert!(
                result.stdout.contains("持久PowerShell中文"),
                "实际输出：{:?}",
                result.stdout
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_service_listing_preserves_display_name_encoding() {
        let device = SystemDevice::new();
        let result = device
            .list_services()
            .await
            .expect("Windows 服务列表应执行成功");

        assert_eq!(result.exit_code, Some(0));
        let document: serde_json::Value =
            serde_json::from_str(&result.stdout).expect("服务列表应返回 JSON");
        assert!(document["returned"].as_u64().is_some_and(|count| count > 0));
        assert!(
            !result.stdout.contains('�'),
            "服务列表包含替换字符：{:?}",
            result.stdout
        );
    }

    #[test]
    fn ssh_destination_rejects_option_like_hosts() {
        assert!(validate_ssh_destination("192.0.2.10", "admin").is_ok());
        assert!(validate_ssh_destination("-oProxyCommand=calc", "admin").is_err());
        assert!(validate_ssh_destination("192.0.2.10", "-oProxyCommand=calc").is_err());
    }

    #[test]
    fn known_hosts_target_uses_openssh_port_syntax() {
        assert_eq!(known_hosts_target("192.0.2.10", 22), "192.0.2.10");
        assert_eq!(known_hosts_target("192.0.2.10", 2222), "[192.0.2.10]:2222");
    }

    #[tokio::test]
    async fn explicit_exit_closes_persistent_shell_without_reporting_failure() {
        let device = SystemDevice::new();
        let (shell, command) = if cfg!(windows) {
            (ShellKind::Cmd, "exit /b 0")
        } else {
            (ShellKind::System, "exit 0")
        };
        let session = device
            .open_interactive_shell(shell)
            .await
            .expect("应打开持久 Shell");
        let (output, _receiver) = mpsc::unbounded_channel();

        let result = session
            .run(command, "explicit-exit", 10, output)
            .await
            .expect("显式 exit 应作为正常结束返回");

        assert_eq!(result.exit_code, Some(0));
        assert!(session.has_exited().await.expect("应查询 Shell 状态"));
    }

    #[tokio::test]
    async fn persistent_shell_can_be_interrupted_from_another_task() {
        let device = SystemDevice::new();
        let shell = if cfg!(windows) {
            ShellKind::Cmd
        } else {
            ShellKind::System
        };
        let session = device
            .open_interactive_shell(shell)
            .await
            .expect("应打开持久 Shell");
        let command = if cfg!(windows) {
            "ping -n 31 127.0.0.1 >NUL"
        } else {
            "sleep 30"
        };
        let runner_session = session.clone();
        let runner = tokio::spawn(async move {
            let (sender, receiver) = mpsc::unbounded_channel();
            drop(receiver);
            runner_session
                .run(command, "interrupt-test", 60, sender)
                .await
        });

        tokio::time::sleep(Duration::from_millis(250)).await;
        tokio::time::timeout(Duration::from_secs(5), session.interrupt())
            .await
            .expect("中断不应超时")
            .expect("中断应成功");
        let error = tokio::time::timeout(Duration::from_secs(5), runner)
            .await
            .expect("命令任务应及时结束")
            .expect("命令任务不应崩溃")
            .expect_err("命令应被中断");

        assert!(error.to_string().contains("已被中断"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn persistent_windows_powershell_returns_marker_and_output() {
        let device = SystemDevice::new();
        let session = device
            .open_interactive_shell(ShellKind::WindowsPowerShell)
            .await
            .expect("Windows PowerShell 5.1 应能打开");
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let result = session
            .run(
                "Write-Output 'REMOTEOPS_PERSIST_PS'",
                "powershell-output",
                10,
                sender,
            )
            .await
            .expect("Windows PowerShell 持久命令应成功");
        session.close().await.expect("应关闭 Windows PowerShell");
        let mut streamed = String::new();
        while let Ok(chunk) = receiver.try_recv() {
            streamed.push_str(&chunk.text);
        }

        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("REMOTEOPS_PERSIST_PS"));
        assert!(streamed.contains("REMOTEOPS_PERSIST_PS"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn persistent_windows_powershell_stays_hidden_across_codex_style_calls() {
        let device = SystemDevice::new();
        let console_probe = r#"Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class RemoteOpsNativeMethods { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }' -ErrorAction SilentlyContinue; if ([RemoteOpsNativeMethods]::GetConsoleWindow() -eq [IntPtr]::Zero) { 'NO_CONSOLE' } else { 'HAS_CONSOLE'; exit 1 }"#;
        let mut shells = vec![ShellKind::WindowsPowerShell];
        if command_exists("pwsh.exe") {
            shells.push(ShellKind::PowerShell);
        }
        for shell in shells {
            let session = device
                .open_interactive_shell(shell)
                .await
                .expect("PowerShell 持久 Shell 应能静默打开");
            for index in 0..3 {
                let (sender, receiver) = mpsc::unbounded_channel();
                drop(receiver);
                let result = session
                    .run(console_probe, &format!("hidden-call-{index}"), 20, sender)
                    .await
                    .expect("Codex 风格的连续持久命令应静默执行");

                assert_eq!(result.exit_code, Some(0));
                assert!(result.stdout.contains("NO_CONSOLE"));
                assert!(!result.stdout.contains("HAS_CONSOLE"));
            }
            session.close().await.expect("应关闭静默 PowerShell");
        }
    }
}

#[cfg(all(test, not(windows)))]
mod unix_inventory_tests {
    use super::*;

    #[test]
    fn terminal_filter_preserves_split_utf8_and_removes_csi_and_osc() {
        let mut decoder = ProcessOutputDecoder::new(ProcessOutputEncoding::PlainUtf8);
        let input = "\u{1b}[31m中文\u{1b}[0m\u{1b}]0;hidden title\u{7}text\n";
        let clean: String = input
            .as_bytes()
            .iter()
            .map(|byte| decoder.push(&[*byte], false))
            .collect();
        assert_eq!(clean, "中文text\n");
    }

    #[test]
    fn inventory_preserves_text_and_limits_reported_count() {
        let mut result = CommandResult {
            stdout: "42 myproc /usr/bin/myproc --name \"中文  text\"\n".repeat(600),
            stderr: String::new(),
            exit_code: Some(0),
        };
        normalize_unix_inventory(&mut result, false).unwrap();
        let data: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(data["returned"], 512);
        assert_eq!(data["items"].as_array().unwrap().len(), 512);
        assert_eq!(
            data["items"][0]["CommandLine"],
            "/usr/bin/myproc --name \"中文  text\""
        );
        let mut result = CommandResult {
            stdout: "test@a.service loaded active running A service with spaces\n".into(),
            stderr: String::new(),
            exit_code: Some(0),
        };
        normalize_unix_inventory(&mut result, true).unwrap();
        let data: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(data["items"][0]["DisplayName"], "A service with spaces");
        assert_eq!(data["items"][0]["Status"], "active");
    }

    #[test]
    fn inventory_does_not_mask_command_failure() {
        let mut result = CommandResult {
            stdout: String::new(),
            stderr: "permission denied".into(),
            exit_code: Some(1),
        };
        assert!(normalize_unix_inventory(&mut result, true).is_err());
    }

    #[tokio::test]
    async fn actual_unix_process_inventory_is_json() {
        let device = SystemDevice::default();
        let result = device.list_processes().await.unwrap();
        let data: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
        let items = data["items"].as_array().unwrap();
        assert_ne!(items.as_slice(), &[] as &[serde_json::Value]);
        assert!(items.iter().all(|item| item["Id"].as_u64().is_some()));
        assert_eq!(data["returned"], items.len());
        assert!(device.run(ShellKind::Cmd, "echo wrong", 5).await.is_err());
    }
}
