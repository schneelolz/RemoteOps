use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc as std_mpsc,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
};

use anyhow::{Context, anyhow, bail};
#[cfg(test)]
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Utc};
use clap::Parser;
use remoteops_audit::sha256_bytes;
pub use remoteops_device::SshCredentialStore;
use remoteops_device::{
    CommandOutputChunk, FileTransferProvider, MAX_FILE_CHUNK_BYTES, PortProbeProvider,
    SerialProvider, ShellProvider, SshProvider, SystemDevice, SystemDuplexSerialSession,
    SystemFileUploadSession, SystemInteractiveShellSession, SystemProvider, TcpExchangeProvider,
};
use remoteops_domain::{
    AgentInstanceId, ApprovalState, Capability, CapabilitySet, ControllerInstanceId,
    ControllerOwnerId, EnvironmentProfile, EventPayload, FileTransferId, PermissionMode,
    RemoteEvent, RemoteOperation, RequestId, SerialSessionId, SessionId, ShellId, ShellKind,
    ShellProfile, ToolProfile,
};
use remoteops_policy::{DefaultPolicy, PolicyDecision, RiskLevel};
use remoteops_protocol::{
    AgentHello, AgentLeaseRenewed, AgentPermissionModeChanged, AgentResumeCommitAck,
    AgentWelcomeAck, AuthorizedRemoteRequest, ClientHello, ControllerBinding, PROTOCOL_VERSION,
    RemoteRequest, RemoteResponse, WireMessage, connect_tls, load_client_config,
    load_native_client_config, load_pinned_client_config, normalize_certificate_fingerprint,
    read_frame, write_frame,
};
use remoteops_serial::{
    SerialDirection, SerialObservedChunk, SerialQueryError, SerialQueryPlan, SerialQueryRunner,
    SerialQueryTransport, SerialTranscript,
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Mutex, Notify, mpsc, watch},
    task::JoinHandle,
    time::{Duration, interval, sleep},
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TRANSFER_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_PENDING_TASKS: usize = 32;
const MAX_PENDING_TASKS_PER_SESSION: usize = 8;
const MAX_SHELL_SESSIONS: usize = 8;
const MAX_SHELL_SESSIONS_PER_SESSION: usize = 4;
const MAX_SERIAL_SESSIONS: usize = 8;
const MAX_SERIAL_SESSIONS_PER_SESSION: usize = 4;
const MAX_FILE_UPLOAD_SESSIONS: usize = 8;
const MAX_FILE_UPLOAD_SESSIONS_PER_SESSION: usize = 4;
const AGENT_CONFIG_FILE_NAME: &str = "agent-config.json";
const DEFAULT_RETRY_SECONDS: u64 = 3;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 可以安全写入普通 JSON 文件的 Agent 配置。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AgentFileConfig {
    /// Relay TLS 地址，例如 `relay.example.com:7443`。
    pub relay: Option<String>,
    /// Relay 证书中的 DNS 名称或 IP；省略时从 Relay 地址推导。
    pub server_name: Option<String>,
    /// 可选的自签名 CA 证书路径。
    pub ca_cert: Option<PathBuf>,
    /// 用户确认并固定的 Relay 叶证书 SHA-256 指纹。
    pub tls_fingerprint: Option<String>,
    /// 断线重试间隔。
    pub retry_seconds: Option<u64>,
    /// 文件交换目录。
    pub transfer_root: Option<PathBuf>,
    /// Agent 身份和恢复令牌状态文件。
    pub state_file: Option<PathBuf>,
    /// 由 Agent 本地用户持久授权 `FullAccess` 的可信 Owner。
    #[serde(default)]
    pub trusted_full_access_owners: Vec<TrustedOwnerAuthorization>,
}

/// Agent 本地持久 `FullAccess` 授权记录。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct TrustedOwnerAuthorization {
    /// Human 与 AI 共同使用的稳定 Owner 标识。
    pub owner_id: ControllerOwnerId,
    /// 本地用户最后确认该授权的时间。
    pub granted_at: DateTime<Utc>,
}

/// Agent 运行参数，由 CLI 和 GUI 共同使用。
#[derive(Clone, Debug)]
pub struct AgentConfig {
    /// Relay TLS 地址。
    pub relay: String,
    /// Relay 证书中的 DNS 名称或 IP。
    pub server_name: String,
    /// 可选的自签名 CA 证书。
    pub ca_cert: Option<PathBuf>,
    /// 用户确认并固定的 Relay 叶证书 SHA-256 指纹。
    pub tls_fingerprint: Option<String>,
    /// 断线重试间隔。
    pub retry_seconds: u64,
    /// 文件交换目录。
    pub transfer_root: PathBuf,
    /// 测试时覆盖 Agent 实例标识。
    pub instance_id: Option<AgentInstanceId>,
    /// Agent 身份和恢复令牌状态文件。
    pub state_file: PathBuf,
    /// 由配置文件持久授权 `FullAccess` 的可信 Owner 及变更时间。
    pub trusted_full_access_owners: BTreeMap<ControllerOwnerId, DateTime<Utc>>,
    /// 仅当前 Agent 进程生命周期授权 `FullAccess` 的 Owner。
    pub session_full_access_owners: BTreeSet<ControllerOwnerId>,
    /// 仅当前 Agent 进程使用的 SSH 密码凭据。
    pub ssh_credentials: SshCredentialStore,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            relay: String::new(),
            server_name: String::new(),
            ca_cert: None,
            tls_fingerprint: None,
            retry_seconds: DEFAULT_RETRY_SECONDS,
            transfer_root: default_transfer_root(),
            instance_id: None,
            state_file: default_state_file(),
            trusted_full_access_owners: BTreeMap::new(),
            session_full_access_owners: BTreeSet::new(),
            ssh_credentials: SshCredentialStore::default(),
        }
    }
}

impl AgentConfig {
    /// 读取默认或显式指定的 JSON 配置文件；默认文件不存在时返回空白配置供参数覆盖。
    ///
    /// # Errors
    ///
    /// 显式配置文件不存在、无法读取或 JSON 格式无效时返回错误。
    pub fn load_file(config_path: Option<&Path>) -> anyhow::Result<Self> {
        let path = config_path.map_or_else(active_agent_config_path, Path::to_path_buf);
        if !path.exists() {
            if config_path.is_some() {
                bail!("Agent 配置文件不存在：{}", path.display());
            }
            return Ok(Self::default());
        }

        let text = fs::read_to_string(&path)
            .with_context(|| format!("无法读取 Agent 配置文件 {}", path.display()))?;
        let file_config: AgentFileConfig = serde_json::from_str(&text)
            .with_context(|| format!("Agent 配置文件格式无效：{}", path.display()))?;
        let base_directory = path.parent().unwrap_or_else(|| Path::new("."));
        let mut config = Self::default();
        if let Some(relay) = file_config.relay {
            config.relay = relay;
        }
        if let Some(server_name) = file_config.server_name {
            config.server_name = server_name;
        }
        config.ca_cert = file_config
            .ca_cert
            .map(|path| resolve_config_path(base_directory, path));
        config.tls_fingerprint = file_config.tls_fingerprint;
        if let Some(retry_seconds) = file_config.retry_seconds {
            config.retry_seconds = retry_seconds;
        }
        if let Some(transfer_root) = file_config.transfer_root {
            config.transfer_root = resolve_config_path(base_directory, transfer_root);
        }
        if let Some(state_file) = file_config.state_file {
            config.state_file = resolve_config_path(base_directory, state_file);
        }
        for authorization in file_config.trusted_full_access_owners {
            if config
                .trusted_full_access_owners
                .insert(authorization.owner_id, authorization.granted_at)
                .is_some()
            {
                bail!(
                    "Agent 配置中的 trusted_full_access_owners 包含重复 Owner：{}",
                    authorization.owner_id
                );
            }
        }
        Ok(config)
    }

    /// 把当前非敏感运行配置保存到 JSON 文件。
    ///
    /// # Errors
    ///
    /// 当父目录无法创建、JSON 无法序列化或文件无法替换时返回错误。
    pub fn save_file(&self, path: &Path) -> anyhow::Result<()> {
        let file_config = AgentFileConfig {
            relay: Some(self.relay.clone()),
            server_name: (!self.server_name.is_empty()).then(|| self.server_name.clone()),
            ca_cert: self.ca_cert.clone(),
            tls_fingerprint: self.tls_fingerprint.clone(),
            retry_seconds: Some(self.retry_seconds),
            transfer_root: Some(self.transfer_root.clone()),
            state_file: Some(self.state_file.clone()),
            trusted_full_access_owners: self
                .trusted_full_access_owners
                .iter()
                .map(|(owner_id, granted_at)| TrustedOwnerAuthorization {
                    owner_id: *owner_id,
                    granted_at: *granted_at,
                })
                .collect(),
        };
        let contents = serde_json::to_vec_pretty(&file_config).context("无法序列化 Agent 配置")?;
        replace_file_recoverably(path, &contents)
            .with_context(|| format!("无法保存 Agent 配置文件 {}", path.display()))
    }

    /// 校验连接配置并在需要时从 Relay 地址推导 TLS 服务名。
    ///
    /// # Errors
    ///
    /// Relay 地址为空、格式无效或重试间隔为零时返回错误。
    pub fn normalize_and_validate(mut self) -> anyhow::Result<Self> {
        self.relay = self.relay.trim().to_owned();
        self.server_name = self.server_name.trim().to_owned();
        if self.relay.is_empty() {
            bail!(
                "未配置 Relay。请创建 {}，或使用 --relay / REMOTEOPS_RELAY",
                default_agent_config_path().display()
            );
        }
        if self.server_name.is_empty() {
            self.server_name = infer_server_name(&self.relay)?;
        }
        if let Some(fingerprint) = self.tls_fingerprint.take() {
            self.tls_fingerprint = Some(
                normalize_certificate_fingerprint(&fingerprint)
                    .context("Agent 配置中的 tls_fingerprint 无效")?,
            );
        }
        if self.retry_seconds == 0 {
            bail!("retry_seconds 必须大于 0");
        }
        Ok(self)
    }
}

/// 返回便携模式下与 Agent 可执行文件同级的配置文件路径。
#[must_use]
pub fn default_agent_config_path() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(AGENT_CONFIG_FILE_NAME)
}

/// 返回旧版按用户保存 Agent 配置的路径。
#[must_use]
pub fn legacy_agent_config_path() -> PathBuf {
    env::var_os("LOCALAPPDATA").map_or_else(
        || PathBuf::from("RemoteOps").join(AGENT_CONFIG_FILE_NAME),
        |root| {
            PathBuf::from(root)
                .join("RemoteOps")
                .join(AGENT_CONFIG_FILE_NAME)
        },
    )
}

/// 返回当前应读取的 Agent 配置路径，并兼容旧版用户目录配置。
#[must_use]
pub fn active_agent_config_path() -> PathBuf {
    let portable = default_agent_config_path();
    let legacy = legacy_agent_config_path();
    select_agent_config_path(portable, legacy)
}

fn select_agent_config_path(portable: PathBuf, legacy: PathBuf) -> PathBuf {
    if portable.is_file() {
        portable
    } else if legacy.is_file() {
        legacy
    } else {
        portable
    }
}

fn replace_file_recoverably(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map_or_else(|| "tmp".to_owned(), |value| format!("{value}.tmp"));
    let temporary = path.with_extension(extension);
    let backup = path.with_extension("json.bak");
    {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    let had_original = path.exists();
    if had_original {
        if backup.exists() {
            fs::remove_file(&backup)?;
        }
        fs::rename(path, &backup)?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if had_original {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if had_original {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn resolve_config_path(base_directory: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base_directory.join(path)
    }
}

fn infer_server_name(relay: &str) -> anyhow::Result<String> {
    let relay = relay.trim();
    if let Some((host, _)) = relay
        .strip_prefix('[')
        .and_then(|value| value.split_once(']'))
    {
        return Ok(host.to_owned());
    }
    relay
        .rsplit_once(':')
        .map(|(host, _)| host.trim().to_owned())
        .filter(|host| !host.is_empty())
        .ok_or_else(|| anyhow!("Relay 地址必须使用 host:port 格式：{relay}"))
}

/// Agent 向表现层报告的生命周期事件。
#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// 本地状态与设备能力已经初始化。
    Started {
        /// 跨进程保持不变的 Agent 标识。
        agent_instance_id: AgentInstanceId,
        /// 当前目标 Relay。
        relay: String,
        /// 文件交换目录。
        transfer_root: PathBuf,
        /// 当前检测到的远程能力。
        capabilities: CapabilitySet,
    },
    /// 正在建立 Relay 连接。
    Connecting,
    /// 已建立 Relay 连接并取得控制码。
    Connected {
        /// 供工程师配对的临时控制码。
        pairing_code: String,
        /// 控制码租约到期时间。
        lease_expires_at: DateTime<Utc>,
    },
    /// Relay 接受心跳续租后更新控制码租约到期时间。
    LeaseRenewed {
        /// 控制码租约新的到期时间。
        lease_expires_at: DateTime<Utc>,
    },
    /// 当前已绑定的控制端数量发生变化。
    ControllerCountChanged {
        /// 当前活动 Controller 绑定数量。
        active_connections: usize,
    },
    /// 当前 Controller Owner、请求权限和 Agent 本地授权状态发生变化。
    ControllerBindingsChanged {
        /// 当前活动 Controller 绑定快照。
        bindings: Vec<AgentControllerBinding>,
    },
    /// 连接中断，等待自动重试。
    Reconnecting {
        /// 面向现场人员的简短原因。
        message: String,
        /// 下一次重试前的等待秒数。
        retry_seconds: u64,
    },
    /// Agent 已按用户要求停止。
    Stopped,
    /// Agent 无法继续启动或运行。
    Failed {
        /// 可用于现场排查的脱敏错误信息。
        message: String,
    },
}

/// Agent 向本地表现层公开的 Controller 绑定摘要。
#[derive(Clone, Debug)]
pub struct AgentControllerBinding {
    /// Controller 的稳定 Owner。
    pub owner_id: ControllerOwnerId,
    /// Human 或 AI Controller 类型。
    pub controller_kind: remoteops_protocol::ControllerKind,
    /// Relay 会话当前请求的权限。
    pub permission_mode: PermissionMode,
    /// Agent 本地是否允许该 Owner 使用 `FullAccess`。
    pub full_access_authorized_locally: bool,
}

/// Agent 本地用户对当前进程选择的权限模式。
#[derive(Clone, Debug)]
pub struct AgentPermissionControl {
    /// 向 Agent 运行时广播权限模式变化。
    sender: watch::Sender<PermissionMode>,
}

impl AgentPermissionControl {
    /// 创建一个会话级权限控制器。
    #[must_use]
    pub fn new(permission_mode: PermissionMode) -> Self {
        let (sender, _) = watch::channel(permission_mode);
        Self { sender }
    }

    /// 根据 Agent 配置中的既有 Owner 授权确定初始模式。
    #[must_use]
    pub fn from_config(config: &AgentConfig) -> Self {
        let permission_mode = if config.trusted_full_access_owners.is_empty()
            && config.session_full_access_owners.is_empty()
        {
            PermissionMode::ApprovalRequired
        } else {
            PermissionMode::FullAccess
        };
        Self::new(permission_mode)
    }

    /// 返回当前本地权限模式。
    #[must_use]
    pub fn permission_mode(&self) -> PermissionMode {
        *self.sender.borrow()
    }

    /// 更新当前进程的本地权限模式。
    pub fn set_permission_mode(&self, permission_mode: PermissionMode) {
        self.sender.send_replace(permission_mode);
    }

    /// 订阅后续权限模式变化。
    fn subscribe(&self) -> watch::Receiver<PermissionMode> {
        self.sender.subscribe()
    }
}

impl Default for AgentPermissionControl {
    fn default() -> Self {
        Self::new(PermissionMode::ApprovalRequired)
    }
}

#[derive(Clone, Debug, Default)]
struct LocalPermissionPolicy {
    /// Agent GUI 或宿主为当前进程选择的权限模式。
    permission_control: AgentPermissionControl,
    /// 当前会话已经绑定的唯一 Owner。
    active_owner: Arc<RwLock<Option<ControllerOwnerId>>>,
    /// 配置文件中持久信任的 Owner。
    persistent_full_access_owners: BTreeSet<ControllerOwnerId>,
    /// 启动参数为当前进程信任的 Owner。
    session_full_access_owners: BTreeSet<ControllerOwnerId>,
}

impl LocalPermissionPolicy {
    fn from_config(config: &AgentConfig, permission_control: AgentPermissionControl) -> Self {
        Self {
            permission_control,
            active_owner: Arc::new(RwLock::new(None)),
            persistent_full_access_owners: config
                .trusted_full_access_owners
                .keys()
                .copied()
                .collect(),
            session_full_access_owners: config.session_full_access_owners.clone(),
        }
    }

    fn allows(&self, owner_id: ControllerOwnerId, permission_mode: PermissionMode) -> bool {
        permission_mode != PermissionMode::FullAccess
            || self.persistent_full_access_owners.contains(&owner_id)
            || self.session_full_access_owners.contains(&owner_id)
            || (self.permission_control.permission_mode() == PermissionMode::FullAccess
                && self
                    .active_owner
                    .read()
                    .is_ok_and(|active_owner| *active_owner == Some(owner_id)))
    }

    fn set_active_owner(&self, owner_id: Option<ControllerOwnerId>) {
        if let Ok(mut active_owner) = self.active_owner.write() {
            *active_owner = owner_id;
        }
    }
}

/// Agent 生命周期事件发送端。
pub type AgentEventSender = std_mpsc::Sender<AgentEvent>;

fn emit_agent_event(sender: Option<&AgentEventSender>, event: AgentEvent) {
    if let Some(sender) = sender {
        let _ = sender.send(event);
    }
}

/// Agent 本地身份和 Relay 恢复令牌。
#[derive(Debug, Deserialize, Serialize)]
struct AgentState {
    /// 跨进程保持不变的 Agent 实例标识。
    agent_instance_id: AgentInstanceId,
    /// Relay 最近确认的恢复令牌。
    resume_token: Option<String>,
}

#[derive(Clone)]
struct SerialRuntime {
    session_id: SessionId,
    port_name: String,
    writable: bool,
    port: SystemDuplexSerialSession,
    transcript: Arc<Mutex<SerialTranscript>>,
    activity: Arc<Notify>,
    operation_lock: Arc<Mutex<()>>,
    cancelled: Arc<AtomicBool>,
}

struct AgentSerialQueryTransport {
    runtime: SerialRuntime,
}

#[async_trait::async_trait]
impl SerialQueryTransport for AgentSerialQueryTransport {
    async fn write_all(&self, bytes: &[u8]) -> Result<(), SerialQueryError> {
        let written = self
            .runtime
            .port
            .write(bytes.to_vec())
            .await
            .map_err(|error| SerialQueryError::Transport(error.to_string()))?;
        if written != bytes.len() {
            return Err(SerialQueryError::Transport(format!(
                "期望写入 {} 字节，实际写入 {written} 字节",
                bytes.len()
            )));
        }
        self.runtime
            .transcript
            .lock()
            .await
            .push(SerialDirection::Transmit, bytes.to_vec());
        self.runtime.activity.notify_waiters();
        Ok(())
    }

    async fn latest_sequence(&self) -> Result<u64, SerialQueryError> {
        Ok(self.runtime.transcript.lock().await.latest_sequence())
    }

    async fn wait_for_chunks(
        &self,
        after_sequence: u64,
        wait: Duration,
    ) -> Result<Vec<SerialObservedChunk>, SerialQueryError> {
        loop {
            let notified = self.runtime.activity.notified();
            let chunks = self
                .runtime
                .transcript
                .lock()
                .await
                .entries_after(after_sequence)
                .into_iter()
                .map(|entry| SerialObservedChunk {
                    sequence: entry.sequence,
                    direction: entry.direction,
                    bytes: entry.bytes,
                })
                .collect::<Vec<_>>();
            if !chunks.is_empty() {
                return Ok(chunks);
            }
            if tokio::time::timeout(wait, notified).await.is_err() {
                return Ok(Vec::new());
            }
        }
    }
}

#[derive(Clone)]
struct ShellRuntime {
    /// 所属远程逻辑会话。
    session_id: SessionId,
    /// 实际 Shell 类型。
    shell: ShellKind,
    /// 持久 Shell 子进程。
    session: SystemInteractiveShellSession,
}

#[derive(Clone, Debug)]
struct FileUploadRuntime {
    /// 所属远程逻辑会话。
    session_id: SessionId,
    /// Agent 端受控临时上传会话。
    upload: SystemFileUploadSession,
}

#[derive(Clone)]
struct RequestRuntimeState {
    /// 持久 Shell 会话。
    shell_sessions: Arc<Mutex<BTreeMap<ShellId, ShellRuntime>>>,
    /// 串口会话。
    serial_sessions: Arc<Mutex<BTreeMap<SerialSessionId, SerialRuntime>>>,
    /// 分块文件上传会话。
    file_uploads: Arc<Mutex<BTreeMap<FileTransferId, FileUploadRuntime>>>,
}

struct PendingTask {
    /// 请求所属的逻辑会话，用于限制单会话并发。
    session_id: SessionId,
    /// Tokio 请求任务；取消时必须等待其底层资源完成清理。
    task: JoinHandle<()>,
    /// 请求唯一的原子终态提交点。
    terminal: Arc<AtomicTaskTerminal>,
    /// 持久 Shell 命令需要先中断底层进程，再结束 Rust 任务。
    interactive_shell: Option<SystemInteractiveShellSession>,
}

/// 请求任务的一次性终态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum TaskTerminal {
    /// 请求仍在执行。
    Running = 0,
    /// 请求已成功完成。
    Completed = 1,
    /// 请求已失败。
    Failed = 2,
    /// 请求已被人工取消。
    Cancelled = 3,
    /// 连接断开后静默终止。
    Aborted = 4,
}

impl TaskTerminal {
    /// 从原子整数恢复任务终态。
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Running,
            1 => Self::Completed,
            2 => Self::Failed,
            3 => Self::Cancelled,
            4 => Self::Aborted,
            _ => unreachable!("任务终态原子值不应超出枚举范围"),
        }
    }

    /// 是否已经产生面向控制端的业务终态。
    const fn is_reported(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// 保证每个请求只有一个终态提交者。
struct AtomicTaskTerminal {
    /// 当前任务终态。
    value: AtomicU8,
}

impl AtomicTaskTerminal {
    /// 创建运行中的任务终态。
    const fn running() -> Self {
        Self {
            value: AtomicU8::new(TaskTerminal::Running as u8),
        }
    }

    /// 读取当前任务终态。
    fn load(&self) -> TaskTerminal {
        TaskTerminal::from_u8(self.value.load(Ordering::Acquire))
    }

    /// 尝试从运行中提交唯一终态。
    fn try_commit(&self, terminal: TaskTerminal) -> Result<(), TaskTerminal> {
        debug_assert_ne!(terminal, TaskTerminal::Running);
        self.value
            .compare_exchange(
                TaskTerminal::Running as u8,
                terminal as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(TaskTerminal::from_u8)
    }
}

/// 取消请求对目标任务的最终判定。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CancelTaskOutcome {
    /// 当前取消请求成功抢占终态。
    Cancelled,
    /// 目标已经完成或失败，不能再改写终态。
    Completed,
    /// 目标已经被其他取消请求中断。
    AlreadyCancelled,
    /// 当前连接不知道该目标请求。
    NotFound,
}

const MAX_RECENT_TASK_TERMINALS: usize = 1_024;

/// Windows 现场 Agent。
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Agent JSON 配置文件；默认读取用户本地 `RemoteOps` 目录。
    #[arg(long, env = "REMOTEOPS_AGENT_CONFIG")]
    config: Option<PathBuf>,
    /// Relay TLS 地址。
    #[arg(long, env = "REMOTEOPS_RELAY")]
    relay: Option<String>,
    /// Relay 证书中的 DNS 名称或 IP。
    #[arg(long, env = "REMOTEOPS_SERVER_NAME")]
    server_name: Option<String>,
    /// Relay 自签名 PEM 证书；不指定时使用操作系统可信根证书。
    #[arg(long, env = "REMOTEOPS_CA_CERT")]
    ca_cert: Option<PathBuf>,
    /// 已由本地用户确认的 Relay 叶证书 SHA-256 指纹。
    #[arg(long, env = "REMOTEOPS_TLS_FINGERPRINT")]
    tls_fingerprint: Option<String>,
    /// 断线后的重试间隔。
    #[arg(long, env = "REMOTEOPS_RETRY_SECONDS")]
    retry_seconds: Option<u64>,
    /// 文件上传、下载和 SSH 凭据允许访问的根目录。
    #[arg(long, env = "REMOTEOPS_TRANSFER_ROOT")]
    transfer_root: Option<PathBuf>,
    /// 测试时固定 Agent GUID；默认每次进程启动随机生成。
    #[arg(long)]
    instance_id: Option<AgentInstanceId>,
    /// Agent 身份和恢复令牌状态文件。
    #[arg(long, env = "REMOTEOPS_AGENT_STATE_FILE")]
    state_file: Option<PathBuf>,
    /// 仅本次 Agent 进程授权 `FullAccess` 的 Owner；可重复指定。
    #[arg(
        long = "session-full-access-owner",
        env = "REMOTEOPS_SESSION_FULL_ACCESS_OWNERS",
        value_delimiter = ';'
    )]
    session_full_access_owners: Vec<ControllerOwnerId>,
}

impl Args {
    fn into_config(self) -> anyhow::Result<AgentConfig> {
        let mut config = AgentConfig::load_file(self.config.as_deref())?;
        if let Some(relay) = self.relay {
            config.relay = relay;
        }
        if let Some(server_name) = self.server_name {
            config.server_name = server_name;
        }
        if let Some(ca_cert) = self.ca_cert {
            config.ca_cert = Some(ca_cert);
        }
        if let Some(tls_fingerprint) = self.tls_fingerprint {
            config.tls_fingerprint = Some(tls_fingerprint);
        }
        if let Some(retry_seconds) = self.retry_seconds {
            config.retry_seconds = retry_seconds;
        }
        if let Some(transfer_root) = self.transfer_root {
            config.transfer_root = transfer_root;
        }
        if let Some(state_file) = self.state_file {
            config.state_file = state_file;
        }
        config
            .session_full_access_owners
            .extend(self.session_full_access_owners);
        config.instance_id = self.instance_id;
        config.normalize_and_validate()
    }
}

/// 初始化 Agent 日志；重复调用不会导致进程崩溃。
pub fn initialize_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("remoteops_agent=info")),
        )
        .with_target(false)
        .compact()
        .try_init();
}

/// 运行传统命令行 Agent。
///
/// # Errors
///
/// 当 Agent 初始化、信号处理或后台任务失败时返回错误。
pub async fn run_cli() -> anyhow::Result<()> {
    initialize_tracing();
    let config = Args::parse().into_config()?;
    let (event_sender, event_receiver) = std_mpsc::channel();
    let printer = std::thread::spawn(move || {
        while let Ok(event) = event_receiver.recv() {
            print_agent_event(&event);
        }
    });
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let mut task = tokio::spawn(run_agent(config, Some(event_sender), shutdown_receiver));
    let result = tokio::select! {
        result = &mut task => result.context("Agent 运行任务异常结束")?,
        signal = tokio::signal::ctrl_c() => {
            signal.context("无法监听 Ctrl+C")?;
            let _ = shutdown_sender.send(true);
            if let Ok(result) = tokio::time::timeout(Duration::from_secs(5), &mut task).await {
                result.context("Agent 停止任务异常结束")?
            } else {
                task.abort();
                let _ = task.await;
                Ok(())
            }
        }
    };
    drop(shutdown_sender);
    let _ = printer.join();
    result
}

/// 运行可由 CLI 或 GUI 观察和停止的 Agent 生命周期。
///
/// # Errors
///
/// 当本地状态、证书、设备或 Relay 生命周期无法继续时返回错误。
#[allow(clippy::too_many_lines)]
pub async fn run_agent(
    config: AgentConfig,
    event_sender: Option<AgentEventSender>,
    shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let permission_control = AgentPermissionControl::from_config(&config);
    run_agent_with_permission_control(config, event_sender, shutdown, permission_control).await
}

/// 使用表现层提供的动态本地权限控制器运行 Agent。
///
/// # Errors
///
/// 当本地状态、证书、设备或 Relay 生命周期无法继续时返回错误。
#[allow(clippy::too_many_lines)]
pub async fn run_agent_with_permission_control(
    config: AgentConfig,
    event_sender: Option<AgentEventSender>,
    mut shutdown: watch::Receiver<bool>,
    permission_control: AgentPermissionControl,
) -> anyhow::Result<()> {
    let client_config = if let Some(certificate_path) = config.ca_cert.as_deref() {
        load_client_config(certificate_path)
            .with_context(|| format!("无法加载 Relay 证书 {}", certificate_path.display()))?
    } else if let Some(fingerprint) = config.tls_fingerprint.as_deref() {
        load_pinned_client_config(&config.relay, &config.server_name, fingerprint)
            .await
            .context("无法使用固定指纹验证 Relay 证书")?
    } else {
        load_native_client_config().context("无法加载操作系统可信根证书")?
    };
    let state = load_agent_state(&config.state_file, config.instance_id)?;
    let agent_instance_id = state.agent_instance_id;
    let device = Arc::new(
        SystemDevice::with_transfer_root(&config.transfer_root)
            .map(|device| device.with_ssh_credentials(config.ssh_credentials.clone()))
            .with_context(|| {
                format!(
                    "无法初始化 Agent 文件交换目录 {}",
                    config.transfer_root.display()
                )
            })?,
    );
    let environment = detect_environment_profile(device.as_ref()).await;
    let capabilities = capabilities_from_environment(&environment);
    let hostname = local_hostname();
    let operating_system = environment.os_version.clone().map_or_else(
        || format!("{} {}", environment.os_family, environment.architecture),
        |version| format!("{version} {}", environment.architecture),
    );
    let resume_token = Arc::new(Mutex::new(state.resume_token));
    let sequence = Arc::new(AtomicU64::new(1));
    let shell_sessions = Arc::new(Mutex::new(BTreeMap::<ShellId, ShellRuntime>::new()));
    let serial_sessions = Arc::new(Mutex::new(BTreeMap::<SerialSessionId, SerialRuntime>::new()));
    let local_permission_policy = Arc::new(LocalPermissionPolicy::from_config(
        &config,
        permission_control.clone(),
    ));

    emit_agent_event(
        event_sender.as_ref(),
        AgentEvent::Started {
            agent_instance_id,
            relay: config.relay.clone(),
            transfer_root: device.transfer_root().to_path_buf(),
            capabilities: capabilities.clone(),
        },
    );

    loop {
        if *shutdown.borrow() {
            break;
        }
        emit_agent_event(event_sender.as_ref(), AgentEvent::Connecting);
        let connect = tokio::select! {
            result = connect_tls(&config.relay, &config.server_name, client_config.clone()) => result,
            () = wait_for_shutdown(&mut shutdown) => break,
        };
        let stream = match connect {
            Ok(stream) => stream,
            Err(error) => {
                warn!(error = %error, "连接 Relay 失败，将自动重试");
                emit_agent_event(
                    event_sender.as_ref(),
                    AgentEvent::Reconnecting {
                        message: format!("无法连接 Relay：{error}"),
                        retry_seconds: config.retry_seconds.max(1),
                    },
                );
                tokio::select! {
                    () = sleep(Duration::from_secs(config.retry_seconds.max(1))) => {}
                    () = wait_for_shutdown(&mut shutdown) => break,
                }
                continue;
            }
        };
        let current_resume_token = resume_token.lock().await.clone();
        let connection_shutdown = shutdown.clone();
        match run_connection(
            stream,
            agent_instance_id,
            current_resume_token,
            resume_token.clone(),
            config.state_file.clone(),
            hostname.clone(),
            operating_system.clone(),
            capabilities.clone(),
            environment.clone(),
            device.clone(),
            sequence.clone(),
            shell_sessions.clone(),
            serial_sessions.clone(),
            local_permission_policy.clone(),
            permission_control.subscribe(),
            event_sender.as_ref(),
            connection_shutdown,
        )
        .await
        {
            Ok(()) => break,
            Err(error) => {
                error!(error = %error, "Relay 连接中断");
                emit_agent_event(
                    event_sender.as_ref(),
                    AgentEvent::Reconnecting {
                        message: format!("Relay 连接已中断：{error}"),
                        retry_seconds: config.retry_seconds.max(1),
                    },
                );
            }
        }
        tokio::select! {
            () = sleep(Duration::from_secs(config.retry_seconds.max(1))) => {}
            () = wait_for_shutdown(&mut shutdown) => break,
        }
    }
    close_agent_resources(&shell_sessions, &serial_sessions).await;
    emit_agent_event(event_sender.as_ref(), AgentEvent::Stopped);
    Ok(())
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_connection<S>(
    mut stream: S,
    agent_instance_id: AgentInstanceId,
    resume_token: Option<String>,
    resume_token_state: Arc<Mutex<Option<String>>>,
    state_file: PathBuf,
    hostname: String,
    operating_system: String,
    capabilities: CapabilitySet,
    environment: EnvironmentProfile,
    device: Arc<SystemDevice>,
    sequence: Arc<AtomicU64>,
    shell_sessions: Arc<Mutex<BTreeMap<ShellId, ShellRuntime>>>,
    serial_sessions: Arc<Mutex<BTreeMap<SerialSessionId, SerialRuntime>>>,
    local_permission_policy: Arc<LocalPermissionPolicy>,
    mut permission_mode_updates: watch::Receiver<PermissionMode>,
    event_sender: Option<&AgentEventSender>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let hello = WireMessage::Hello(ClientHello::Agent(AgentHello {
        protocol_version: PROTOCOL_VERSION,
        agent_instance_id,
        resume_token,
        hostname,
        operating_system,
        capabilities,
        environment,
    }));
    write_frame(&mut stream, &hello).await?;
    let welcome = match read_frame::<WireMessage, _>(&mut stream).await? {
        WireMessage::AgentWelcome(welcome) => welcome,
        WireMessage::Error { code, message, .. } => bail!("{code}: {message}"),
        other => bail!("Relay 返回了意外消息：{other:?}"),
    };
    write_frame(
        &mut stream,
        &WireMessage::AgentWelcomeAck(AgentWelcomeAck {
            connection_generation: welcome.connection_generation,
        }),
    )
    .await?;
    match read_frame::<WireMessage, _>(&mut stream).await? {
        WireMessage::AgentResumeCommitted(committed)
            if committed.connection_generation == welcome.connection_generation =>
        {
            write_frame(
                &mut stream,
                &WireMessage::AgentResumeCommitAck(AgentResumeCommitAck {
                    connection_generation: welcome.connection_generation,
                }),
            )
            .await?;
            *resume_token_state.lock().await = Some(welcome.resume_token.clone());
            persist_agent_state(
                &state_file,
                &AgentState {
                    agent_instance_id,
                    resume_token: Some(welcome.resume_token.clone()),
                },
            )
            .context("无法持久化 Agent 恢复令牌")?;
        }
        WireMessage::Error { code, message, .. } => bail!("{code}: {message}"),
        other => bail!("Relay 返回了意外的恢复令牌确认消息：{other:?}"),
    }
    emit_agent_event(
        event_sender,
        AgentEvent::Connected {
            pairing_code: welcome.pairing_code.display_grouped(),
            lease_expires_at: welcome.lease_expires_at,
        },
    );
    emit_agent_event(
        event_sender,
        AgentEvent::ControllerCountChanged {
            active_connections: 0,
        },
    );

    let (mut reader, mut writer) = tokio::io::split(stream);
    let (sender, mut receiver) = mpsc::unbounded_channel::<WireMessage>();
    let _ = sender.send(WireMessage::AgentPermissionModeChanged(
        AgentPermissionModeChanged {
            permission_mode: *permission_mode_updates.borrow_and_update(),
        },
    ));
    let writer_task = tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            match write_frame(&mut writer, &message).await {
                Ok(()) => {}
                Err(error) => {
                    error!(error = %error, "Agent 写通道发送协议帧失败");
                    break;
                }
            }
        }
    });
    let tasks = Arc::new(Mutex::new(BTreeMap::<RequestId, PendingTask>::new()));
    let recent_task_terminals = Arc::new(Mutex::new(BTreeMap::<RequestId, TaskTerminal>::new()));
    let file_uploads = Arc::new(Mutex::new(
        BTreeMap::<FileTransferId, FileUploadRuntime>::new(),
    ));
    let mut controller_bindings = BTreeMap::<ControllerInstanceId, ControllerBinding>::new();
    let policy = DefaultPolicy::default();
    let heartbeat_sender = sender.clone();
    let heartbeat_seconds = welcome.heartbeat_interval_seconds.max(1);
    let heartbeat_task = tokio::spawn(async move {
        let mut heartbeat = interval(Duration::from_secs(heartbeat_seconds));
        loop {
            heartbeat.tick().await;
            if heartbeat_sender
                .send(WireMessage::Heartbeat {
                    sent_at: Utc::now(),
                })
                .is_err()
            {
                break;
            }
        }
    });

    let connection_error = loop {
        let incoming = tokio::select! {
            result = read_frame::<WireMessage, _>(&mut reader) => Some(result),
            _changed = async {
                if permission_mode_updates.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            } => {
                let permission_mode = *permission_mode_updates.borrow_and_update();
                let _ = sender.send(WireMessage::AgentPermissionModeChanged(
                    AgentPermissionModeChanged { permission_mode },
                ));
                emit_controller_bindings(
                    event_sender,
                    &controller_bindings,
                    &local_permission_policy,
                );
                continue;
            },
            () = wait_for_shutdown(&mut shutdown) => None,
        };
        let Some(incoming) = incoming else {
            break None;
        };
        match incoming {
            Ok(WireMessage::ControllerBinding(binding)) => {
                info!(
                    session_id = %binding.session_id,
                    controller_instance_id = %binding.controller_instance_id,
                    "Agent 已更新 Controller 绑定"
                );
                controller_bindings.insert(binding.controller_instance_id, binding);
                update_active_owner(&local_permission_policy, &controller_bindings);
                emit_agent_event(
                    event_sender,
                    AgentEvent::ControllerCountChanged {
                        active_connections: active_controller_owner_count(&controller_bindings),
                    },
                );
                emit_controller_bindings(
                    event_sender,
                    &controller_bindings,
                    &local_permission_policy,
                );
            }
            Ok(WireMessage::AgentLeaseRenewed(AgentLeaseRenewed { lease_expires_at })) => {
                emit_agent_event(event_sender, AgentEvent::LeaseRenewed { lease_expires_at });
            }
            Ok(WireMessage::ControllerBindingRevoked {
                session_id,
                binding_token,
            }) => {
                if revoke_controller_binding(&mut controller_bindings, session_id, &binding_token) {
                    if !controller_bindings
                        .values()
                        .any(|binding| binding.session_id == session_id)
                    {
                        close_file_uploads_for_session(&file_uploads, session_id).await;
                    }
                    update_active_owner(&local_permission_policy, &controller_bindings);
                    emit_agent_event(
                        event_sender,
                        AgentEvent::ControllerCountChanged {
                            active_connections: active_controller_owner_count(&controller_bindings),
                        },
                    );
                    emit_controller_bindings(
                        event_sender,
                        &controller_bindings,
                        &local_permission_policy,
                    );
                }
            }
            Ok(WireMessage::AuthorizedRemoteRequest(authorized)) => {
                let request_id = authorized.request.request_id;
                info!(
                    %request_id,
                    session_id = %authorized.request.session_id,
                    "Agent 已收到 Relay 授权请求"
                );
                let request = match verify_authorized_request(
                    &policy,
                    &local_permission_policy,
                    &controller_bindings,
                    authorized,
                ) {
                    Ok(request) => request,
                    Err(error) => {
                        let _ = sender.send(WireMessage::Error {
                            code: "relay_authorization_invalid".to_owned(),
                            message: error.to_string(),
                            request_id: Some(request_id),
                        });
                        continue;
                    }
                };
                if let RemoteOperation::CancelRequest {
                    request_id: target_request_id,
                } = &request.operation
                {
                    let outcome =
                        cancel_pending_task(&tasks, &recent_task_terminals, *target_request_id)
                            .await;
                    if outcome == CancelTaskOutcome::Cancelled {
                        send_event(
                            &sender,
                            &sequence,
                            &request,
                            EventPayload::OperationCancelled,
                        );
                        let _ = sender.send(WireMessage::RemoteResponse(RemoteResponse {
                            request_id: *target_request_id,
                            session_id: request.session_id,
                            exit_code: None,
                            summary: "远程请求已被人工中断".to_owned(),
                            error_code: Some("cancelled".to_owned()),
                            payload_base64: None,
                            sha256: None,
                            details: None,
                        }));
                    }
                    let _ = sender.send(WireMessage::RemoteResponse(RemoteResponse {
                        request_id: request.request_id,
                        session_id: request.session_id,
                        exit_code: Some(0),
                        summary: match outcome {
                            CancelTaskOutcome::Cancelled => "已中断目标请求",
                            CancelTaskOutcome::Completed => "目标请求已完成",
                            CancelTaskOutcome::AlreadyCancelled => "目标请求已中断",
                            CancelTaskOutcome::NotFound => "目标请求不存在",
                        }
                        .to_owned(),
                        error_code: None,
                        payload_base64: None,
                        sha256: None,
                        details: None,
                    }));
                    continue;
                }
                if matches!(request.operation, RemoteOperation::EmergencyStop) {
                    abort_pending_tasks(&tasks).await;
                    close_agent_resources(&shell_sessions, &serial_sessions).await;
                    close_file_uploads(&file_uploads).await;
                    send_event(
                        &sender,
                        &sequence,
                        &request,
                        EventPayload::OperationCancelled,
                    );
                    let _ = sender.send(WireMessage::RemoteResponse(RemoteResponse {
                        request_id: request.request_id,
                        session_id: request.session_id,
                        exit_code: Some(0),
                        summary: "已紧急停止全部任务并关闭 Shell、串口和文件上传资源".to_owned(),
                        error_code: None,
                        payload_base64: None,
                        sha256: None,
                        details: None,
                    }));
                    continue;
                }
                {
                    let pending = tasks.lock().await;
                    let request_was_completed = recent_task_terminals
                        .lock()
                        .await
                        .contains_key(&request.request_id);
                    let session_tasks = pending
                        .values()
                        .filter(|task| task.session_id == request.session_id)
                        .count();
                    if ensure_task_capacity(
                        pending.len(),
                        session_tasks,
                        pending.contains_key(&request.request_id) || request_was_completed,
                    )
                    .is_err()
                    {
                        let _ = sender.send(WireMessage::RemoteResponse(RemoteResponse {
                            request_id: request.request_id,
                            session_id: request.session_id,
                            exit_code: None,
                            summary: "Agent 在途请求达到安全上限或 request_id 重复".to_owned(),
                            error_code: Some("agent_request_limit_reached".to_owned()),
                            payload_base64: None,
                            sha256: None,
                            details: None,
                        }));
                        continue;
                    }
                }
                let request_id = request.request_id;
                let request_session_id = request.session_id;
                let device = device.clone();
                let sender_clone = sender.clone();
                let sequence_clone = sequence.clone();
                let tasks_clone = tasks.clone();
                let recent_task_terminals_clone = recent_task_terminals.clone();
                let runtime_state = RequestRuntimeState {
                    shell_sessions: shell_sessions.clone(),
                    serial_sessions: serial_sessions.clone(),
                    file_uploads: file_uploads.clone(),
                };
                let terminal = Arc::new(AtomicTaskTerminal::running());
                let interactive_shell = pending_interactive_shell(&request, &shell_sessions).await;
                let terminal_clone = terminal.clone();
                let (start_sender, start_receiver) = tokio::sync::oneshot::channel();
                let task = tokio::spawn(async move {
                    if start_receiver.await.is_err() {
                        return;
                    }
                    let final_terminal = execute_request(
                        device.as_ref(),
                        &sender_clone,
                        &sequence_clone,
                        &runtime_state,
                        request,
                        terminal_clone.as_ref(),
                    )
                    .await;
                    if final_terminal.is_reported() {
                        remember_task_terminal(
                            &recent_task_terminals_clone,
                            request_id,
                            final_terminal,
                        )
                        .await;
                    }
                    tasks_clone.lock().await.remove(&request_id);
                });
                tasks.lock().await.insert(
                    request_id,
                    PendingTask {
                        session_id: request_session_id,
                        task,
                        terminal,
                        interactive_shell,
                    },
                );
                let _ = start_sender.send(());
            }
            Ok(WireMessage::Error { code, message, .. }) => {
                if code == "connection_superseded" {
                    break Some(anyhow!("{code}: {message}"));
                }
                warn!(%code, %message, "Relay 返回错误");
            }
            Ok(WireMessage::RemoteRequest(request)) => {
                let _ = sender.send(WireMessage::Error {
                    code: "unauthorized_request".to_owned(),
                    message: "Agent 拒绝未携带 Relay 授权声明的请求".to_owned(),
                    request_id: Some(request.request_id),
                });
            }
            Ok(_) => {}
            Err(error) => {
                break Some(anyhow!(error));
            }
        }
    };
    heartbeat_task.abort();
    writer_task.abort();
    abort_pending_tasks(&tasks).await;
    close_file_uploads(&file_uploads).await;
    local_permission_policy.set_active_owner(None);
    emit_agent_event(
        event_sender,
        AgentEvent::ControllerCountChanged {
            active_connections: 0,
        },
    );
    match connection_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn verify_authorized_request(
    policy: &DefaultPolicy,
    local_permission_policy: &LocalPermissionPolicy,
    bindings: &BTreeMap<ControllerInstanceId, ControllerBinding>,
    authorized: AuthorizedRemoteRequest,
) -> anyhow::Result<RemoteRequest> {
    let binding = bindings
        .get(&authorized.authorization.controller_instance_id)
        .ok_or_else(|| anyhow!("目标 Controller 没有活动绑定"))?;
    if binding.session_id != authorized.request.session_id
        || binding.owner_id != authorized.authorization.owner_id
        || binding.controller_kind != authorized.authorization.controller_kind
        || binding.permission_mode != authorized.authorization.permission_mode
        || !constant_time_eq(
            binding.binding_token.as_bytes(),
            authorized.authorization.binding_token.as_bytes(),
        )
    {
        bail!("Relay 授权声明与 Agent 当前 Controller 绑定不匹配");
    }

    let request = authorized.request;
    if request.source != binding.controller_kind.event_source() {
        bail!("请求来源与 Relay 已认证的 Controller 身份不匹配");
    }
    if !local_permission_policy.allows(binding.owner_id, binding.permission_mode) {
        bail!(
            "Owner {} 尚未由 Agent 本地用户授权 FullAccess；请改用 ApprovalRequired，或在 Agent 本地配置会话级/持久授权",
            binding.owner_id
        );
    }
    if normalize_operation(policy, request.operation.clone()) != request.operation {
        bail!("请求中的只读声明不是 Relay 策略计算结果");
    }
    match policy.evaluate_with_mode(
        authorized.authorization.permission_mode,
        request.source,
        &request.operation,
    ) {
        PolicyDecision::Allow => {
            if authorized.authorization.approval != ApprovalState::NotRequired
                || request.approval_id.is_some()
            {
                bail!("无需审批的请求携带了不可信审批声明");
            }
        }
        PolicyDecision::RequireApproval { .. } => {
            if authorized.authorization.approval != ApprovalState::Approved
                || request.approval_id.is_none()
            {
                bail!("需要审批的请求没有 Relay 已批准授权");
            }
        }
        PolicyDecision::Deny { reason } => bail!("Agent 策略拒绝请求：{reason}"),
    }
    Ok(request)
}

fn emit_controller_bindings(
    event_sender: Option<&AgentEventSender>,
    bindings: &BTreeMap<ControllerInstanceId, ControllerBinding>,
    local_permission_policy: &LocalPermissionPolicy,
) {
    let bindings = bindings
        .values()
        .map(|binding| AgentControllerBinding {
            owner_id: binding.owner_id,
            controller_kind: binding.controller_kind,
            permission_mode: binding.permission_mode,
            full_access_authorized_locally: local_permission_policy
                .allows(binding.owner_id, PermissionMode::FullAccess),
        })
        .collect();
    emit_agent_event(
        event_sender,
        AgentEvent::ControllerBindingsChanged { bindings },
    );
}

fn update_active_owner(
    local_permission_policy: &LocalPermissionPolicy,
    bindings: &BTreeMap<ControllerInstanceId, ControllerBinding>,
) {
    let active_owner = bindings.values().next().map(|binding| binding.owner_id);
    local_permission_policy.set_active_owner(active_owner);
}

fn active_controller_owner_count(
    bindings: &BTreeMap<ControllerInstanceId, ControllerBinding>,
) -> usize {
    bindings
        .values()
        .map(|binding| binding.owner_id)
        .collect::<BTreeSet<_>>()
        .len()
}

fn revoke_controller_binding(
    bindings: &mut BTreeMap<ControllerInstanceId, ControllerBinding>,
    session_id: SessionId,
    binding_token: &str,
) -> bool {
    let before = bindings.len();
    bindings.retain(|_, binding| {
        binding.session_id != session_id
            || !constant_time_eq(binding.binding_token.as_bytes(), binding_token.as_bytes())
    });
    bindings.len() != before
}

fn normalize_operation(policy: &DefaultPolicy, mut operation: RemoteOperation) -> RemoteOperation {
    set_operation_readonly(&mut operation, false);
    let readonly = policy.classify(&operation) == RiskLevel::ReadOnly;
    set_operation_readonly(&mut operation, readonly);
    operation
}

fn set_operation_readonly(operation: &mut RemoteOperation, readonly: bool) {
    match operation {
        RemoteOperation::RunCommand {
            readonly: declared, ..
        }
        | RemoteOperation::RunShellCommand {
            readonly: declared, ..
        }
        | RemoteOperation::RunSsh {
            readonly: declared, ..
        }
        | RemoteOperation::RunSerialQuery {
            readonly: declared, ..
        } => *declared = readonly,
        _ => {}
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let maximum = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..maximum {
        difference |= usize::from(left.get(index).copied().unwrap_or_default())
            ^ usize::from(right.get(index).copied().unwrap_or_default());
    }
    difference == 0
}

async fn execute_request(
    device: &SystemDevice,
    sender: &mpsc::UnboundedSender<WireMessage>,
    sequence: &Arc<AtomicU64>,
    runtime_state: &RequestRuntimeState,
    request: RemoteRequest,
    terminal: &AtomicTaskTerminal,
) -> TaskTerminal {
    send_event(sender, sequence, &request, EventPayload::OperationStarted);
    let result = execute_operation(device, device, &request, runtime_state, sender, sequence).await;
    let final_terminal = if result.is_ok() {
        TaskTerminal::Completed
    } else {
        TaskTerminal::Failed
    };
    if terminal.try_commit(final_terminal).is_err() {
        return terminal.load();
    }
    match result {
        Ok(mut response) => {
            if let Some(details) = &response.details {
                send_event(
                    sender,
                    sequence,
                    &request,
                    EventPayload::OutputChunk {
                        stderr: false,
                        text: details.to_string(),
                    },
                );
            }
            send_event(
                sender,
                sequence,
                &request,
                EventPayload::OperationCompleted {
                    exit_code: response.exit_code,
                    summary: response.summary.clone(),
                },
            );
            response.request_id = request.request_id;
            response.session_id = request.session_id;
            let _ = sender.send(WireMessage::RemoteResponse(response));
        }
        Err(error) => {
            let message = error.to_string();
            send_event(
                sender,
                sequence,
                &request,
                EventPayload::OperationFailed {
                    code: "agent_operation_failed".to_owned(),
                    message: message.clone(),
                },
            );
            let _ = sender.send(WireMessage::RemoteResponse(RemoteResponse {
                request_id: request.request_id,
                session_id: request.session_id,
                exit_code: None,
                summary: message,
                error_code: Some("agent_operation_failed".to_owned()),
                payload_base64: None,
                sha256: None,
                details: None,
            }));
        }
    }
    final_terminal
}

#[allow(clippy::too_many_lines)]
async fn execute_operation(
    device: &SystemDevice,
    ssh_provider: &dyn SshProvider,
    request: &RemoteRequest,
    runtime_state: &RequestRuntimeState,
    sender: &mpsc::UnboundedSender<WireMessage>,
    sequence: &Arc<AtomicU64>,
) -> anyhow::Result<RemoteResponse> {
    let shell_sessions = &runtime_state.shell_sessions;
    let serial_sessions = &runtime_state.serial_sessions;
    let file_uploads = &runtime_state.file_uploads;
    let mut response = RemoteResponse {
        request_id: request.request_id,
        session_id: request.session_id,
        exit_code: Some(0),
        summary: String::new(),
        error_code: None,
        payload_base64: None,
        sha256: None,
        details: None,
    };
    match &request.operation {
        RemoteOperation::ProvisionSshCredential {
            host,
            port,
            username,
            credential_ref,
        } => {
            let expected_ref = SshCredentialStore::credential_ref(host, *port, username);
            if credential_ref != &expected_ref {
                bail!("SSH 凭据引用与目标不匹配");
            }
            let payload = request
                .payload_base64
                .as_deref()
                .ok_or_else(|| anyhow!("SSH 凭据注入缺少密码负载"))?;
            let password =
                String::from_utf8(BASE64.decode(payload).context("SSH 凭据负载 Base64 无效")?)
                    .context("SSH 凭据负载不是有效 UTF-8")?;
            device.provision_ssh_credential(host, *port, username, password)?;
            response.summary = format!("已注入 SSH 凭据 {credential_ref}");
        }
        RemoteOperation::OpenShell { shell } => {
            let session = device.open_interactive_shell(*shell).await?;
            let shell_id = ShellId::new();
            let mut sessions = shell_sessions.lock().await;
            let session_count = sessions
                .values()
                .filter(|runtime| runtime.session_id == request.session_id)
                .count();
            if ensure_shell_capacity(sessions.len(), session_count).is_err() {
                drop(sessions);
                session.close().await?;
                bail!("持久 Shell 会话达到安全上限");
            }
            sessions.insert(
                shell_id,
                ShellRuntime {
                    session_id: request.session_id,
                    shell: *shell,
                    session,
                },
            );
            drop(sessions);
            response.summary = format!("持久 Shell {shell_id} 已打开");
            response.details = Some(serde_json::json!({
                "shell_id": shell_id.to_string(),
                "shell": format!("{shell:?}"),
            }));
        }
        RemoteOperation::RunShellCommand {
            shell_id,
            shell,
            command,
            ..
        } => {
            let runtime = shell_sessions
                .lock()
                .await
                .get(shell_id)
                .cloned()
                .ok_or_else(|| anyhow!("持久 Shell 不存在"))?;
            validate_shell_binding(
                runtime.session_id,
                runtime.shell,
                request.session_id,
                *shell,
            )?;
            let (output_sender, output_receiver) = mpsc::unbounded_channel();
            let output_task = spawn_command_output_forwarder(
                output_receiver,
                sender.clone(),
                sequence.clone(),
                request.clone(),
            );
            let result = runtime
                .session
                .run(command, &request.request_id.to_string(), 120, output_sender)
                .await;
            let _ = output_task.await;
            let result = result?;
            let shell_closed = runtime.session.has_exited().await?;
            if shell_closed {
                shell_sessions.lock().await.remove(shell_id);
            }
            response.exit_code = result.exit_code;
            response.summary = command_summary(&result.stdout, &result.stderr);
            response.details = Some(serde_json::json!({
                "shell_id": shell_id.to_string(),
                "shell": format!("{:?}", runtime.shell),
                "shell_closed": shell_closed,
            }));
        }
        RemoteOperation::CloseShell { shell_id } => {
            let runtime = shell_sessions
                .lock()
                .await
                .get(shell_id)
                .cloned()
                .ok_or_else(|| anyhow!("持久 Shell 不存在"))?;
            if runtime.session_id != request.session_id {
                bail!("持久 Shell 不属于指定 session_id");
            }
            runtime.session.close().await?;
            shell_sessions.lock().await.remove(shell_id);
            response.summary = format!("持久 Shell {shell_id} 已关闭");
        }
        RemoteOperation::RunCommand { shell, command, .. } => {
            let (output_sender, output_receiver) = mpsc::unbounded_channel();
            let output_task = spawn_command_output_forwarder(
                output_receiver,
                sender.clone(),
                sequence.clone(),
                request.clone(),
            );
            let result = device
                .run_streaming(*shell, command, 120, output_sender)
                .await;
            let _ = output_task.await;
            let result = result?;
            response.exit_code = result.exit_code;
            response.summary = command_summary(&result.stdout, &result.stderr);
        }
        RemoteOperation::TestPort { host, port } => {
            let result = device.test_port(host, *port, 5_000).await?;
            response.exit_code = Some(i32::from(!result.open));
            response.summary = if result.open {
                format!("{host}:{port} open")
            } else {
                format!("{host}:{port} closed")
            };
            response.details = Some(serde_json::to_value(result)?);
        }
        RemoteOperation::UploadFile {
            remote_path,
            size,
            sha256,
            overwrite,
        } => {
            let payload = request
                .payload_base64
                .as_deref()
                .ok_or_else(|| anyhow!("上传请求缺少文件负载"))?;
            let bytes = BASE64.decode(payload).context("上传文件 Base64 无效")?;
            if bytes.len() as u64 != *size {
                bail!("上传文件大小与声明不一致");
            }
            if bytes.len() as u64 > MAX_FILE_BYTES {
                bail!("上传文件超过 {MAX_FILE_BYTES} 字节限制");
            }
            let actual_hash = sha256_bytes(&bytes);
            if &actual_hash != sha256 {
                bail!("上传文件 SHA-256 校验失败");
            }
            device.write_file(remote_path, &bytes, *overwrite).await?;
            response.summary = format!("已写入 {} 字节", bytes.len());
            response.sha256 = Some(actual_hash);
        }
        RemoteOperation::BeginUploadFile {
            transfer_id,
            remote_path,
            size,
            sha256,
            overwrite,
        } => {
            if *size > MAX_TRANSFER_BYTES {
                bail!("上传文件超过 {MAX_TRANSFER_BYTES} 字节限制");
            }
            {
                let uploads = file_uploads.lock().await;
                let session_count = uploads
                    .values()
                    .filter(|runtime| runtime.session_id == request.session_id)
                    .count();
                if uploads.contains_key(transfer_id)
                    || uploads.len() >= MAX_FILE_UPLOAD_SESSIONS
                    || session_count >= MAX_FILE_UPLOAD_SESSIONS_PER_SESSION
                {
                    bail!("文件上传会话达到安全上限或 transfer_id 重复");
                }
            }
            let upload = device.begin_file_upload(remote_path, *size, sha256, *overwrite)?;
            let mut uploads = file_uploads.lock().await;
            let session_count = uploads
                .values()
                .filter(|runtime| runtime.session_id == request.session_id)
                .count();
            if uploads.contains_key(transfer_id)
                || uploads.len() >= MAX_FILE_UPLOAD_SESSIONS
                || session_count >= MAX_FILE_UPLOAD_SESSIONS_PER_SESSION
            {
                drop(uploads);
                upload.abort().await?;
                bail!("文件上传会话达到安全上限或 transfer_id 重复");
            }
            uploads.insert(
                *transfer_id,
                FileUploadRuntime {
                    session_id: request.session_id,
                    upload,
                },
            );
            response.summary = format!("已开始接收 {size} 字节文件");
            response.details = Some(serde_json::json!({
                "transfer_id": transfer_id.to_string(),
                "size": size,
                "chunk_size_bytes": MAX_FILE_CHUNK_BYTES,
            }));
        }
        RemoteOperation::UploadFileChunk {
            transfer_id,
            offset,
            size,
            sha256,
        } => {
            let payload = request
                .payload_base64
                .as_deref()
                .ok_or_else(|| anyhow!("上传分块缺少文件负载"))?;
            let bytes = BASE64.decode(payload).context("上传分块 Base64 无效")?;
            if bytes.len() as u64 != *size {
                bail!("上传分块大小与声明不一致");
            }
            if bytes.is_empty() || bytes.len() > MAX_FILE_CHUNK_BYTES {
                bail!("上传分块大小必须在 1 到 {MAX_FILE_CHUNK_BYTES} 字节之间");
            }
            let actual_hash = sha256_bytes(&bytes);
            if !actual_hash.eq_ignore_ascii_case(sha256) {
                bail!("上传分块 SHA-256 校验失败");
            }
            let runtime = file_uploads
                .lock()
                .await
                .get(transfer_id)
                .cloned()
                .ok_or_else(|| anyhow!("文件上传会话不存在"))?;
            if runtime.session_id != request.session_id {
                bail!("文件上传会话不属于指定 session_id");
            }
            runtime.upload.write_chunk(*offset, &bytes).await?;
            response.summary = format!("已接收 {} 字节", bytes.len());
            response.sha256 = Some(actual_hash);
            response.details = Some(serde_json::json!({
                "transfer_id": transfer_id.to_string(),
                "offset": offset,
                "size": bytes.len(),
                "next_offset": runtime.upload.written(),
            }));
        }
        RemoteOperation::CompleteUploadFile { transfer_id } => {
            let runtime = file_uploads
                .lock()
                .await
                .get(transfer_id)
                .cloned()
                .ok_or_else(|| anyhow!("文件上传会话不存在"))?;
            if runtime.session_id != request.session_id {
                bail!("文件上传会话不属于指定 session_id");
            }
            let actual_hash = runtime.upload.complete().await?;
            file_uploads.lock().await.remove(transfer_id);
            response.summary = format!("已完成 {} 字节文件上传", runtime.upload.written());
            response.sha256 = Some(actual_hash);
        }
        RemoteOperation::AbortUploadFile { transfer_id } => {
            let runtime = {
                let mut uploads = file_uploads.lock().await;
                let runtime = uploads
                    .get(transfer_id)
                    .ok_or_else(|| anyhow!("文件上传会话不存在"))?;
                if runtime.session_id != request.session_id {
                    bail!("文件上传会话不属于指定 session_id");
                }
                uploads.remove(transfer_id).expect("上传会话刚刚存在")
            };
            runtime.upload.abort().await?;
            "已取消文件上传".clone_into(&mut response.summary);
        }
        RemoteOperation::DownloadFile { remote_path, .. } => {
            let bytes = device.read_file(remote_path, MAX_FILE_BYTES).await?;
            let hash = sha256_bytes(&bytes);
            response.summary = format!("已读取 {} 字节", bytes.len());
            response.payload_base64 = Some(BASE64.encode(&bytes));
            response.sha256 = Some(hash);
        }
        RemoteOperation::AuthorizeDownloadFile {
            remote_path,
            overwrite_local,
        } => {
            let metadata = device.file_metadata(remote_path).await?;
            response.summary = if *overwrite_local {
                "已验证分块下载及控制端本地覆盖授权".to_owned()
            } else {
                "已验证分块下载授权".to_owned()
            };
            response.details = Some(serde_json::json!({
                "size": metadata.size,
                "overwrite_local": overwrite_local,
            }));
        }
        RemoteOperation::DownloadFileChunk {
            remote_path,
            offset,
            max_bytes,
        } => {
            let max_bytes = usize::try_from(*max_bytes)
                .ok()
                .filter(|value| *value > 0 && *value <= MAX_FILE_CHUNK_BYTES)
                .ok_or_else(|| {
                    anyhow!("下载分块大小必须在 1 到 {MAX_FILE_CHUNK_BYTES} 字节之间")
                })?;
            let chunk = device
                .read_file_chunk(remote_path, *offset, max_bytes)
                .await?;
            let hash = sha256_bytes(&chunk.bytes);
            response.summary = format!("已读取 {} 字节", chunk.bytes.len());
            response.payload_base64 = Some(BASE64.encode(&chunk.bytes));
            response.sha256 = Some(hash.clone());
            response.details = Some(serde_json::json!({
                "offset": offset,
                "size": chunk.bytes.len(),
                "next_offset": offset.saturating_add(chunk.bytes.len() as u64),
                "eof": chunk.eof,
                "sha256": hash,
            }));
        }
        RemoteOperation::GetFileMetadata {
            remote_path,
            include_sha256,
        } => {
            let metadata = device.file_metadata(remote_path).await?;
            if metadata.size > MAX_TRANSFER_BYTES {
                bail!("文件超过 {MAX_TRANSFER_BYTES} 字节限制");
            }
            let hash = if *include_sha256 {
                Some(device.file_sha256(remote_path).await?)
            } else {
                None
            };
            response.summary = format!("文件大小 {} 字节", metadata.size);
            response.sha256.clone_from(&hash);
            response.details = Some(serde_json::json!({
                "metadata": metadata,
                "sha256": hash,
                "max_transfer_bytes": MAX_TRANSFER_BYTES,
                "chunk_size_bytes": MAX_FILE_CHUNK_BYTES,
            }));
        }
        RemoteOperation::MoveFile {
            source_path,
            destination_path,
            overwrite,
        } => {
            device
                .move_file(source_path, destination_path, *overwrite)
                .await?;
            "文件已移动".clone_into(&mut response.summary);
        }
        RemoteOperation::DeleteFile { remote_path } => {
            device.delete_file(remote_path).await?;
            "文件已删除".clone_into(&mut response.summary);
        }
        RemoteOperation::TcpExchange {
            host,
            port,
            byte_count,
            request_sha256,
            max_response_bytes,
            timeout_millis,
        } => {
            let payload = request
                .payload_base64
                .as_deref()
                .ok_or_else(|| anyhow!("TCP 收发请求缺少 Base64 负载"))?;
            let payload = BASE64.decode(payload).context("TCP 请求 Base64 无效")?;
            if payload.len() != *byte_count {
                bail!("TCP 请求字节数与声明不一致");
            }
            if sha256_bytes(&payload) != *request_sha256 {
                bail!("TCP 请求 SHA-256 与声明不一致");
            }
            let result = device
                .exchange(host, *port, &payload, *max_response_bytes, *timeout_millis)
                .await?;
            response.summary = format!(
                "TCP 已发送 {} 字节，接收 {} 字节{}",
                result.sent_bytes,
                result.received.len(),
                if result.read_timed_out {
                    "（读取超时）"
                } else {
                    ""
                }
            );
            response.payload_base64 = Some(BASE64.encode(&result.received));
            response.sha256 = Some(sha256_bytes(&result.received));
            response.details = Some(serde_json::json!({
                "host": result.host,
                "port": result.port,
                "sent_bytes": result.sent_bytes,
                "received_bytes": result.received.len(),
                "read_timed_out": result.read_timed_out,
            }));
        }
        RemoteOperation::ListProcesses => {
            let result = device.list_processes().await?;
            response.exit_code = result.exit_code;
            set_structured_inventory_response(&mut response, "进程", &result)?;
        }
        RemoteOperation::TerminateProcess { process_id } => {
            let result = device.terminate_process(*process_id).await?;
            response.exit_code = result.exit_code;
            response.summary = command_summary(&result.stdout, &result.stderr);
        }
        RemoteOperation::ListServices => {
            let result = device.list_services().await?;
            response.exit_code = result.exit_code;
            set_structured_inventory_response(&mut response, "服务", &result)?;
        }
        RemoteOperation::ControlService {
            service_name,
            action,
        } => {
            let result = device.control_service(service_name, *action).await?;
            response.exit_code = result.exit_code;
            response.summary = command_summary(&result.stdout, &result.stderr);
        }
        RemoteOperation::PowerControl { action } => {
            let result = device.power_control(*action).await?;
            response.exit_code = result.exit_code;
            response.summary = command_summary(&result.stdout, &result.stderr);
        }
        RemoteOperation::ListSerial => {
            let ports = device.list_ports().await?;
            response.summary = format!("发现 {} 个串口", ports.len());
            response.details = Some(serde_json::to_value(ports)?);
        }
        RemoteOperation::RunSsh {
            host,
            port,
            username,
            identity_file,
            known_hosts_file,
            command,
            ..
        } => {
            let result = ssh_provider
                .run_command(
                    host,
                    *port,
                    username,
                    None,
                    identity_file.as_deref(),
                    known_hosts_file.as_deref(),
                    command,
                    120,
                )
                .await?;
            response.exit_code = result.exit_code;
            let stdout = remoteops_serial::redact_serial_text(&result.stdout);
            let stderr = remoteops_serial::redact_serial_text(&result.stderr);
            response.summary = if stderr.trim().is_empty() {
                stdout
            } else {
                format!("{stdout}\n[stderr]\n{stderr}")
            };
        }
        RemoteOperation::OpenSerial {
            port_name,
            settings,
            writable,
        } => {
            let port = device.open_duplex_serial(port_name, *settings).await?;
            let serial_session_id = SerialSessionId::new();
            let cancelled = Arc::new(AtomicBool::new(false));
            let transcript = Arc::new(Mutex::new(SerialTranscript::default()));
            let activity = Arc::new(Notify::new());
            let operation_lock = Arc::new(Mutex::new(()));
            let mut sessions = serial_sessions.lock().await;
            let session_count = sessions
                .values()
                .filter(|runtime| runtime.session_id == request.session_id)
                .count();
            if sessions
                .values()
                .any(|runtime| runtime.port_name.eq_ignore_ascii_case(port_name))
            {
                bail!("串口 {port_name} 已被当前 Agent 的其他会话独占");
            }
            if ensure_serial_capacity(sessions.len(), session_count).is_err() {
                bail!("串口会话达到安全上限");
            }
            sessions.insert(
                serial_session_id,
                SerialRuntime {
                    session_id: request.session_id,
                    port_name: port_name.clone(),
                    writable: *writable,
                    port: port.clone(),
                    transcript: transcript.clone(),
                    activity: activity.clone(),
                    operation_lock,
                    cancelled: cancelled.clone(),
                },
            );
            drop(sessions);
            spawn_serial_reader(
                serial_session_id,
                request.session_id,
                request.request_id,
                request.source,
                port,
                transcript,
                activity,
                cancelled,
                serial_sessions.clone(),
                sender.clone(),
                sequence.clone(),
            );
            response.summary = format!("串口 {port_name} 已打开");
            response.details = Some(serde_json::json!({
                "serial_session_id": serial_session_id.to_string(),
                "port_name": port_name,
                "settings": settings,
                "writable": writable
            }));
        }
        RemoteOperation::WriteSerial {
            serial_session_id,
            byte_count,
            sha256,
        } => {
            let serial_session_id = serial_session_id
                .parse::<SerialSessionId>()
                .map_err(|error| anyhow!(error))?;
            let runtime = serial_sessions
                .lock()
                .await
                .get(&serial_session_id)
                .cloned()
                .ok_or_else(|| anyhow!("串口会话不存在"))?;
            if runtime.session_id != request.session_id {
                bail!("串口会话不属于指定 session_id");
            }
            if !runtime.writable {
                bail!("串口会话以只读模式打开");
            }
            let payload = request
                .payload_base64
                .as_deref()
                .ok_or_else(|| anyhow!("串口写入缺少 Base64 负载"))?;
            let bytes = BASE64.decode(payload).context("串口写入 Base64 无效")?;
            if bytes.len() != *byte_count {
                bail!("串口写入字节数与声明不一致");
            }
            if sha256_bytes(&bytes) != *sha256 {
                bail!("串口写入 SHA-256 与声明不一致");
            }
            let _operation_guard = runtime.operation_lock.lock().await;
            let written = runtime.port.write(bytes.clone()).await?;
            runtime
                .transcript
                .lock()
                .await
                .push(SerialDirection::Transmit, bytes);
            runtime.activity.notify_waiters();
            response.summary = format!("串口已写入 {written} 字节");
        }
        RemoteOperation::RunSerialQuery {
            serial_session_id,
            command,
            line_ending,
            profile,
            overall_timeout_millis,
            idle_timeout_millis,
            max_bytes,
            max_pages,
            ..
        } => {
            let serial_session_id = serial_session_id
                .parse::<SerialSessionId>()
                .map_err(|error| anyhow!(error))?;
            let runtime = serial_sessions
                .lock()
                .await
                .get(&serial_session_id)
                .cloned()
                .ok_or_else(|| anyhow!("串口会话不存在"))?;
            if runtime.session_id != request.session_id {
                bail!("串口会话不属于指定 session_id");
            }
            if !runtime.writable {
                bail!("串口会话以只读模式打开，不能主动查询设备");
            }
            let _operation_guard = runtime.operation_lock.lock().await;
            let result = SerialQueryRunner
                .run(
                    &AgentSerialQueryTransport {
                        runtime: runtime.clone(),
                    },
                    SerialQueryPlan {
                        command: command.clone(),
                        line_ending: *line_ending,
                        profile: *profile,
                        overall_timeout_millis: *overall_timeout_millis,
                        idle_timeout_millis: *idle_timeout_millis,
                        max_bytes: *max_bytes,
                        max_pages: *max_pages,
                    },
                )
                .await?;
            response.summary = format!(
                "串口查询完成：{:?}，接收 {} 字节，自动翻页 {} 次",
                result.completion, result.received_bytes, result.pages
            );
            response.details = Some(serde_json::json!({
                "completion": result.completion,
                "received_bytes": result.received_bytes,
                "pages": result.pages,
                "elapsed_millis": result.elapsed_millis,
                "redacted_text": result.redacted_text
            }));
        }
        RemoteOperation::CloseSerial { serial_session_id } => {
            let serial_session_id = serial_session_id
                .parse::<SerialSessionId>()
                .map_err(|error| anyhow!(error))?;
            let runtime = serial_sessions
                .lock()
                .await
                .get(&serial_session_id)
                .cloned()
                .ok_or_else(|| anyhow!("串口会话不存在"))?;
            if runtime.session_id != request.session_id {
                bail!("串口会话不属于指定 session_id");
            }
            let _operation_guard = runtime.operation_lock.lock().await;
            runtime.cancelled.store(true, Ordering::Relaxed);
            serial_sessions.lock().await.remove(&serial_session_id);
            response.summary.push_str("串口会话已关闭");
        }
        RemoteOperation::CloseConnection => {
            let shells_to_close: Vec<_> = shell_sessions
                .lock()
                .await
                .iter()
                .filter(|(_, runtime)| runtime.session_id == request.session_id)
                .map(|(shell_id, runtime)| (*shell_id, runtime.session.clone()))
                .collect();
            for (shell_id, session) in shells_to_close {
                let _ = session.close().await;
                shell_sessions.lock().await.remove(&shell_id);
            }
            let serial_to_close: Vec<_> = serial_sessions
                .lock()
                .await
                .iter()
                .filter(|(_, runtime)| runtime.session_id == request.session_id)
                .map(|(serial_id, runtime)| (*serial_id, runtime.cancelled.clone()))
                .collect();
            for (serial_id, cancelled) in serial_to_close {
                cancelled.store(true, Ordering::Relaxed);
                serial_sessions.lock().await.remove(&serial_id);
            }
            response.summary.push_str("会话关闭请求已确认");
        }
        RemoteOperation::HumanTakeover => {
            response.summary.push_str("人工接管已确认");
        }
        RemoteOperation::ReleaseHumanTakeover => {
            response
                .summary
                .push_str("人工接管已释放，AI 可按策略继续操作");
        }
        RemoteOperation::EmergencyStop => {
            unreachable!("紧急停止在连接循环中处理");
        }
        RemoteOperation::CancelRequest { .. } => {
            unreachable!("取消请求在连接循环中处理");
        }
    }
    Ok(response)
}

fn set_structured_inventory_response(
    response: &mut RemoteResponse,
    noun: &str,
    result: &remoteops_device::CommandResult,
) -> anyhow::Result<()> {
    if !result.stderr.trim().is_empty() {
        bail!("读取{noun}失败：{}", result.stderr.trim());
    }
    let details: serde_json::Value = serde_json::from_str(result.stdout.trim())
        .with_context(|| format!("{noun}查询没有返回有效结构化数据"))?;
    let returned = details
        .get("returned")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    response.summary = format!("发现 {returned} 个{noun}，最多返回 512 条");
    response.details = Some(details);
    Ok(())
}

fn command_summary(stdout: &str, stderr: &str) -> String {
    if stderr.trim().is_empty() {
        stdout.to_owned()
    } else if stdout.trim().is_empty() {
        stderr.to_owned()
    } else {
        format!("{stdout}\n[stderr]\n{stderr}")
    }
}

fn spawn_command_output_forwarder(
    mut receiver: mpsc::UnboundedReceiver<CommandOutputChunk>,
    sender: mpsc::UnboundedSender<WireMessage>,
    sequence: Arc<AtomicU64>,
    request: RemoteRequest,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(chunk) = receiver.recv().await {
            send_event(
                &sender,
                &sequence,
                &request,
                EventPayload::OutputChunk {
                    stderr: chunk.stderr,
                    text: chunk.text,
                },
            );
        }
    })
}

async fn abort_pending_tasks(tasks: &Arc<Mutex<BTreeMap<RequestId, PendingTask>>>) {
    let pending: Vec<_> = {
        let mut tasks = tasks.lock().await;
        std::mem::take(&mut *tasks).into_values().collect()
    };
    for pending in pending {
        let PendingTask {
            session_id: _,
            task,
            terminal,
            interactive_shell,
        } = pending;
        match terminal.try_commit(TaskTerminal::Aborted) {
            Ok(()) => {
                if let Some(shell) = interactive_shell {
                    let _ = shell.interrupt().await;
                }
                task.abort();
                let _ = task.await;
            }
            Err(TaskTerminal::Completed | TaskTerminal::Failed) => {
                let _ = task.await;
            }
            Err(TaskTerminal::Running | TaskTerminal::Cancelled | TaskTerminal::Aborted) => {
                task.abort();
                let _ = task.await;
            }
        }
    }
}

async fn cancel_pending_task(
    tasks: &Arc<Mutex<BTreeMap<RequestId, PendingTask>>>,
    recent_task_terminals: &Arc<Mutex<BTreeMap<RequestId, TaskTerminal>>>,
    request_id: RequestId,
) -> CancelTaskOutcome {
    let pending = tasks.lock().await.remove(&request_id);
    let Some(pending) = pending else {
        return match recent_task_terminals.lock().await.get(&request_id).copied() {
            Some(TaskTerminal::Completed | TaskTerminal::Failed) => CancelTaskOutcome::Completed,
            Some(TaskTerminal::Cancelled) => CancelTaskOutcome::AlreadyCancelled,
            Some(TaskTerminal::Running | TaskTerminal::Aborted) | None => {
                CancelTaskOutcome::NotFound
            }
        };
    };
    let PendingTask {
        session_id: _,
        task,
        terminal,
        interactive_shell,
    } = pending;
    match terminal.try_commit(TaskTerminal::Cancelled) {
        Ok(()) => {
            if let Some(shell) = interactive_shell {
                let _ = shell.interrupt().await;
            }
            task.abort();
            let _ = task.await;
            remember_task_terminal(recent_task_terminals, request_id, TaskTerminal::Cancelled)
                .await;
            CancelTaskOutcome::Cancelled
        }
        Err(TaskTerminal::Completed | TaskTerminal::Failed) => {
            let final_terminal = terminal.load();
            let _ = task.await;
            remember_task_terminal(recent_task_terminals, request_id, final_terminal).await;
            CancelTaskOutcome::Completed
        }
        Err(TaskTerminal::Cancelled) => {
            task.abort();
            let _ = task.await;
            remember_task_terminal(recent_task_terminals, request_id, TaskTerminal::Cancelled)
                .await;
            CancelTaskOutcome::AlreadyCancelled
        }
        Err(TaskTerminal::Running | TaskTerminal::Aborted) => {
            task.abort();
            let _ = task.await;
            CancelTaskOutcome::NotFound
        }
    }
}

async fn remember_task_terminal(
    recent_task_terminals: &Arc<Mutex<BTreeMap<RequestId, TaskTerminal>>>,
    request_id: RequestId,
    terminal: TaskTerminal,
) {
    debug_assert!(terminal.is_reported());
    let mut recent = recent_task_terminals.lock().await;
    if recent.len() >= MAX_RECENT_TASK_TERMINALS
        && !recent.contains_key(&request_id)
        && let Some(evicted) = recent.keys().next().copied()
    {
        recent.remove(&evicted);
    }
    recent.insert(request_id, terminal);
}

fn ensure_task_capacity(total: usize, session: usize, duplicate: bool) -> anyhow::Result<()> {
    if duplicate || total >= MAX_PENDING_TASKS || session >= MAX_PENDING_TASKS_PER_SESSION {
        bail!("Agent 在途请求达到安全上限或 request_id 重复");
    }
    Ok(())
}

fn ensure_shell_capacity(total: usize, session: usize) -> anyhow::Result<()> {
    if total >= MAX_SHELL_SESSIONS || session >= MAX_SHELL_SESSIONS_PER_SESSION {
        bail!("持久 Shell 会话达到安全上限");
    }
    Ok(())
}

fn ensure_serial_capacity(total: usize, session: usize) -> anyhow::Result<()> {
    if total >= MAX_SERIAL_SESSIONS || session >= MAX_SERIAL_SESSIONS_PER_SESSION {
        bail!("串口会话达到安全上限");
    }
    Ok(())
}

async fn pending_interactive_shell(
    request: &RemoteRequest,
    shell_sessions: &Arc<Mutex<BTreeMap<ShellId, ShellRuntime>>>,
) -> Option<SystemInteractiveShellSession> {
    let RemoteOperation::RunShellCommand { shell_id, .. } = &request.operation else {
        return None;
    };
    shell_sessions
        .lock()
        .await
        .get(shell_id)
        .filter(|runtime| runtime.session_id == request.session_id)
        .map(|runtime| runtime.session.clone())
}

fn send_event(
    sender: &mpsc::UnboundedSender<WireMessage>,
    sequence: &AtomicU64,
    request: &RemoteRequest,
    payload: EventPayload,
) {
    let _ = sender.send(WireMessage::RemoteEvent(RemoteEvent {
        sequence: sequence.fetch_add(1, Ordering::Relaxed),
        session_id: request.session_id,
        request_id: Some(request.request_id),
        source: request.source,
        approval: if request.approval_id.is_some() {
            ApprovalState::Approved
        } else {
            ApprovalState::NotRequired
        },
        payload,
        occurred_at: Utc::now(),
    }));
}

#[allow(clippy::too_many_arguments)]
fn spawn_serial_reader(
    serial_session_id: SerialSessionId,
    session_id: SessionId,
    request_id: RequestId,
    source: remoteops_domain::EventSource,
    port: SystemDuplexSerialSession,
    transcript: Arc<Mutex<SerialTranscript>>,
    activity: Arc<Notify>,
    cancelled: Arc<AtomicBool>,
    serial_sessions: Arc<Mutex<BTreeMap<SerialSessionId, SerialRuntime>>>,
    sender: mpsc::UnboundedSender<WireMessage>,
    sequence: Arc<AtomicU64>,
) {
    tokio::spawn(async move {
        while !cancelled.load(Ordering::Relaxed) {
            match port.read(4096).await {
                Ok(bytes) if bytes.is_empty() => {}
                Ok(bytes) => {
                    transcript
                        .lock()
                        .await
                        .push(SerialDirection::Receive, bytes.clone());
                    activity.notify_waiters();
                    let text = serial_output_for_remote_event(&bytes);
                    if sender
                        .send(WireMessage::RemoteEvent(RemoteEvent {
                            sequence: sequence.fetch_add(1, Ordering::Relaxed),
                            session_id,
                            request_id: Some(request_id),
                            source,
                            approval: ApprovalState::NotRequired,
                            payload: EventPayload::OutputChunk {
                                stderr: false,
                                text: format!("[serial:{serial_session_id}] {text}"),
                            },
                            occurred_at: Utc::now(),
                        }))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    serial_sessions.lock().await.remove(&serial_session_id);
                    let _ = sender.send(WireMessage::RemoteEvent(RemoteEvent {
                        sequence: sequence.fetch_add(1, Ordering::Relaxed),
                        session_id,
                        request_id: Some(request_id),
                        source: remoteops_domain::EventSource::System,
                        approval: ApprovalState::NotRequired,
                        payload: EventPayload::OperationFailed {
                            code: "serial_read_failed".to_owned(),
                            message: format!("[serial:{serial_session_id}] {error}"),
                        },
                        occurred_at: Utc::now(),
                    }));
                    break;
                }
            }
        }
    });
}

fn validate_shell_binding(
    runtime_session_id: SessionId,
    runtime_shell: ShellKind,
    request_session_id: SessionId,
    request_shell: ShellKind,
) -> anyhow::Result<()> {
    if runtime_session_id != request_session_id {
        bail!("持久 Shell 不属于指定 session_id");
    }
    if runtime_shell != request_shell {
        bail!("持久 Shell 类型与请求中绑定的 Shell 类型不匹配");
    }
    Ok(())
}

fn serial_output_for_remote_event(bytes: &[u8]) -> String {
    std::str::from_utf8(bytes).map_or_else(
        |_| format!("[binary serial output: {} bytes]", bytes.len()),
        remoteops_serial::redact_serial_text,
    )
}

async fn detect_environment_profile(device: &SystemDevice) -> EnvironmentProfile {
    let available_shells = device.available_shells().await.unwrap_or_default();
    let shells = [
        ShellKind::Cmd,
        ShellKind::WindowsPowerShell,
        ShellKind::PowerShell,
        ShellKind::System,
    ]
    .into_iter()
    .map(|kind| {
        let available = available_shells.contains(&kind);
        ShellProfile {
            kind,
            available,
            executable: available.then(|| shell_executable(kind).to_owned()),
            version: available.then(|| shell_version(kind)).flatten(),
        }
    })
    .collect();
    let ssh_executable = if cfg!(windows) { "ssh.exe" } else { "ssh" };
    let ssh_available = command_exists(ssh_executable);

    EnvironmentProfile {
        schema_version: remoteops_domain::ENVIRONMENT_PROFILE_SCHEMA_VERSION,
        collected_at: Utc::now(),
        refreshed_at: Utc::now(),
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol_version: PROTOCOL_VERSION,
        os_family: std::env::consts::OS.to_owned(),
        os_version: operating_system_version(),
        architecture: std::env::consts::ARCH.to_owned(),
        elevated: detect_elevated(),
        shells,
        tools: vec![ToolProfile {
            name: "ssh".to_owned(),
            available: ssh_available,
            version: ssh_available.then(ssh_version).flatten(),
        }],
    }
}

fn capabilities_from_environment(environment: &EnvironmentProfile) -> CapabilitySet {
    let mut capabilities = vec![
        Capability::PortProbe,
        Capability::TcpExchange,
        Capability::FileTransfer,
        Capability::Serial,
        Capability::SystemOperations,
    ];
    for shell in environment.shells.iter().filter(|shell| shell.available) {
        match shell.kind {
            ShellKind::Cmd | ShellKind::System => capabilities.push(Capability::Cmd),
            ShellKind::WindowsPowerShell => capabilities.push(Capability::WindowsPowerShell),
            ShellKind::PowerShell => capabilities.push(Capability::PowerShell),
        }
    }
    if environment
        .tools
        .iter()
        .any(|tool| tool.name == "ssh" && tool.available)
    {
        capabilities.push(Capability::Ssh);
    }
    CapabilitySet::new(capabilities)
}

fn shell_executable(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Cmd => "cmd.exe",
        ShellKind::WindowsPowerShell => "powershell.exe",
        ShellKind::PowerShell => "pwsh.exe",
        ShellKind::System => {
            if cfg!(windows) {
                "cmd.exe"
            } else {
                "sh"
            }
        }
    }
}

fn shell_version(kind: ShellKind) -> Option<String> {
    match kind {
        ShellKind::Cmd => windows_cmd_version(),
        ShellKind::WindowsPowerShell => command_summary_line(
            "powershell.exe",
            &[
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$PSVersionTable.PSVersion.ToString()",
            ],
        ),
        ShellKind::PowerShell => command_summary_line(
            if cfg!(windows) { "pwsh.exe" } else { "pwsh" },
            &[
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$PSVersionTable.PSVersion.ToString()",
            ],
        ),
        ShellKind::System => operating_system_version(),
    }
}

fn operating_system_version() -> Option<String> {
    if cfg!(windows) {
        windows_cmd_version()
    } else {
        command_summary_line("uname", &["-sr"])
    }
}

fn windows_cmd_version() -> Option<String> {
    command_summary_line_utf16_le("cmd.exe", &["/d", "/u", "/c", "ver"])
}

fn ssh_version() -> Option<String> {
    command_summary_line(if cfg!(windows) { "ssh.exe" } else { "ssh" }, &["-V"])
}

fn command_summary_line(executable: &str, arguments: &[&str]) -> Option<String> {
    let output = background_command(executable)
        .args(arguments)
        .output()
        .ok()?;
    let bytes = if output.stdout.is_empty() {
        output.stderr
    } else {
        output.stdout
    };
    let text = String::from_utf8(bytes).ok()?;
    summary_line(&text)
}

fn command_summary_line_utf16_le(executable: &str, arguments: &[&str]) -> Option<String> {
    let output = background_command(executable)
        .args(arguments)
        .output()
        .ok()?;
    let bytes = if output.stdout.is_empty() {
        output.stderr
    } else {
        output.stdout
    };
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units = bytes
        .as_slice()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    let text = String::from_utf16(&units).ok()?;
    summary_line(&text)
}

fn summary_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(256).collect())
}

fn detect_elevated() -> Option<bool> {
    if cfg!(windows) {
        let output = background_command("whoami.exe")
            .args(["/groups", "/fo", "csv", "/nh"])
            .output()
            .ok()?;
        let groups = String::from_utf8_lossy(&output.stdout);
        Some(groups.contains("S-1-16-12288") || groups.contains("S-1-16-16384"))
    } else {
        command_summary_line("id", &["-u"]).map(|user_id| user_id == "0")
    }
}

fn command_exists(executable: &str) -> bool {
    let locator = if cfg!(windows) { "where" } else { "which" };
    background_command(locator)
        .arg(executable)
        .output()
        .is_ok_and(|output| output.status.success())
}

/// 创建不会在 Windows GUI 旁弹出控制台窗口的后台命令。
fn background_command(executable: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let mut command = std::process::Command::new(executable);
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new(executable)
    }
}

fn local_hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown-host".to_owned())
}

fn default_transfer_root() -> PathBuf {
    default_agent_data_root().join("transfers")
}

fn default_state_file() -> PathBuf {
    #[cfg(windows)]
    {
        let legacy_path = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .map(|path| path.join("RemoteOps").join("agent-state.json"));
        if let Some(path) = legacy_path.filter(|path| path.is_file()) {
            return path;
        }
        default_agent_data_root().join("agent-state.json")
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_STATE_HOME")
            .map_or_else(
                || {
                    std::env::var_os("HOME").map_or_else(
                        || std::env::temp_dir().join("RemoteOps"),
                        |home| {
                            PathBuf::from(home)
                                .join(".local")
                                .join("state")
                                .join("RemoteOps")
                        },
                    )
                },
                PathBuf::from,
            )
            .join("agent-state.json")
    }
}

fn default_agent_data_root() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join("RemoteOps")
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir().join("RemoteOps")
    }
}

fn load_agent_state(
    path: &std::path::Path,
    requested_instance_id: Option<AgentInstanceId>,
) -> anyhow::Result<AgentState> {
    if let Ok(contents) = std::fs::read_to_string(path) {
        let state: AgentState = serde_json::from_str(&contents)
            .with_context(|| format!("Agent 状态文件格式无效：{}", path.display()))?;
        if let Some(requested) = requested_instance_id
            && requested != state.agent_instance_id
        {
            return Ok(AgentState {
                agent_instance_id: requested,
                resume_token: None,
            });
        }
        return Ok(state);
    }
    if path.exists() {
        bail!("无法读取 Agent 状态文件：{}", path.display());
    }
    let state = AgentState {
        agent_instance_id: requested_instance_id.unwrap_or_default(),
        resume_token: None,
    };
    persist_agent_state(
        path,
        &AgentState {
            agent_instance_id: state.agent_instance_id,
            resume_token: None,
        },
    )?;
    Ok(state)
}

fn persist_agent_state(path: &std::path::Path, state: &AgentState) -> anyhow::Result<()> {
    let parent = path.parent().context("Agent 状态文件路径缺少父目录")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("无法创建 Agent 状态目录 {}", parent.display()))?;
    let temporary_path = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("agent-state.json"),
        std::process::id()
    ));
    let contents = serde_json::to_vec_pretty(&state)?;
    {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)
            .with_context(|| format!("无法创建 Agent 状态临时文件 {}", temporary_path.display()))?;
        file.write_all(&contents)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary_path, std::fs::Permissions::from_mode(0o600))?;
    }
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("无法替换 Agent 状态文件 {}", path.display()))?;
    }
    std::fs::rename(&temporary_path, path)
        .with_context(|| format!("无法提交 Agent 状态文件 {}", path.display()))?;
    Ok(())
}

async fn close_agent_resources(
    shell_sessions: &Arc<Mutex<BTreeMap<ShellId, ShellRuntime>>>,
    serial_sessions: &Arc<Mutex<BTreeMap<SerialSessionId, SerialRuntime>>>,
) {
    let shells = {
        let mut sessions = shell_sessions.lock().await;
        std::mem::take(&mut *sessions)
            .into_values()
            .map(|runtime| runtime.session)
            .collect::<Vec<_>>()
    };
    for shell in shells {
        let _ = shell.close().await;
    }
    let serials = {
        let mut sessions = serial_sessions.lock().await;
        std::mem::take(&mut *sessions)
            .into_values()
            .map(|runtime| runtime.cancelled)
            .collect::<Vec<_>>()
    };
    for cancelled in serials {
        cancelled.store(true, Ordering::Relaxed);
    }
}

async fn close_file_uploads(
    file_uploads: &Arc<Mutex<BTreeMap<FileTransferId, FileUploadRuntime>>>,
) {
    let uploads = {
        let mut uploads = file_uploads.lock().await;
        std::mem::take(&mut *uploads)
            .into_values()
            .map(|runtime| runtime.upload)
            .collect::<Vec<_>>()
    };
    for upload in uploads {
        if let Err(error) = upload.abort().await {
            warn!(error = %error, "清理未完成文件上传失败");
        }
    }
}

async fn close_file_uploads_for_session(
    file_uploads: &Arc<Mutex<BTreeMap<FileTransferId, FileUploadRuntime>>>,
    session_id: SessionId,
) {
    let uploads = {
        let mut uploads = file_uploads.lock().await;
        let transfer_ids = uploads
            .iter()
            .filter_map(|(transfer_id, runtime)| {
                (runtime.session_id == session_id).then_some(*transfer_id)
            })
            .collect::<Vec<_>>();
        transfer_ids
            .into_iter()
            .filter_map(|transfer_id| uploads.remove(&transfer_id))
            .map(|runtime| runtime.upload)
            .collect::<Vec<_>>()
    };
    for upload in uploads {
        if let Err(error) = upload.abort().await {
            warn!(error = %error, %session_id, "清理会话未完成文件上传失败");
        }
    }
}

fn print_agent_event(event: &AgentEvent) {
    match event {
        AgentEvent::Started {
            agent_instance_id,
            relay,
            transfer_root,
            ..
        } => {
            println!("RemoteOps Agent 已启动");
            println!("Agent GUID：{agent_instance_id}");
            println!("目标 Relay：{relay}");
            println!("文件交换目录：{}", transfer_root.display());
            println!("按 Ctrl+C 可随时停止远程协助");
        }
        AgentEvent::Connecting => println!("正在连接 Relay…"),
        AgentEvent::Connected {
            pairing_code,
            lease_expires_at,
        } => {
            println!("Relay 已连接");
            println!("控制码：{pairing_code}");
            println!("控制码租约到期时间：{}", lease_expires_at.to_rfc3339());
        }
        AgentEvent::LeaseRenewed { lease_expires_at } => {
            println!(
                "控制码租约已续期，到期时间：{}",
                lease_expires_at.to_rfc3339()
            );
        }
        AgentEvent::ControllerCountChanged { active_connections } => {
            if *active_connections > 0 {
                println!("工程师已连接");
            } else {
                println!("等待工程师连接");
            }
        }
        AgentEvent::ControllerBindingsChanged { bindings } => {
            for binding in bindings {
                println!(
                    "Controller {:?} Owner={} 权限={:?} Agent本地FullAccess授权={}",
                    binding.controller_kind,
                    binding.owner_id,
                    binding.permission_mode,
                    binding.full_access_authorized_locally
                );
            }
        }
        AgentEvent::Reconnecting {
            message,
            retry_seconds,
        } => println!("{message}，将在 {retry_seconds} 秒后重试"),
        AgentEvent::Stopped => println!("RemoteOps Agent 已停止"),
        AgentEvent::Failed { message } => eprintln!("RemoteOps Agent 启动失败：{message}"),
    }
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use remoteops_domain::{
        ApprovalId, ControllerInstanceId, ControllerOwnerId, EventSource, PairingCode,
        PermissionMode, SessionId,
    };
    use remoteops_protocol::{AgentResumeCommitted, AgentWelcome, ControllerKind};
    use tokio::{sync::Barrier, time::timeout};

    use super::*;

    fn test_owner_id() -> ControllerOwnerId {
        "00000000-0000-0000-0000-000000000001"
            .parse()
            .expect("测试 Owner ID 应有效")
    }

    fn test_state_file(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间应有效")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "remoteops-agent-state-{name}-{}-{nonce}.json",
            std::process::id()
        ))
    }

    async fn execute_test_file_operation(
        device: &SystemDevice,
        file_uploads: &Arc<Mutex<BTreeMap<FileTransferId, FileUploadRuntime>>>,
        session_id: SessionId,
        operation: RemoteOperation,
        payload_base64: Option<String>,
    ) -> anyhow::Result<RemoteResponse> {
        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id,
            source: EventSource::Ai,
            operation,
            approval_id: None,
            payload_base64,
        };
        let (sender, _receiver) = mpsc::unbounded_channel();
        let runtime_state = RequestRuntimeState {
            shell_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            serial_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            file_uploads: Arc::clone(file_uploads),
        };
        execute_operation(
            device,
            device,
            &request,
            &runtime_state,
            &sender,
            &Arc::new(AtomicU64::new(1)),
        )
        .await
    }

    #[test]
    fn zero_argument_start_has_no_private_relay_default() {
        let args = Args::try_parse_from(["remoteops-agent"]).expect("零参数启动应能解析");
        assert!(args.relay.is_none());
        assert!(args.server_name.is_none());
        assert!(args.ca_cert.is_none());
    }

    #[test]
    fn server_name_is_inferred_from_relay_address() {
        let config = AgentConfig {
            relay: "relay.example.com:7443".to_owned(),
            ..AgentConfig::default()
        }
        .normalize_and_validate()
        .expect("应从 Relay 地址推导服务名");
        assert_eq!(config.server_name, "relay.example.com");
    }

    #[test]
    fn explicit_certificate_remains_supported() {
        let args = Args::try_parse_from(["remoteops-agent", "--ca-cert", "relay-cert.pem"])
            .expect("显式证书参数应能解析");
        assert_eq!(args.ca_cert, Some(PathBuf::from("relay-cert.pem")));
    }

    #[test]
    fn portable_config_is_preferred_and_legacy_config_remains_compatible() {
        let root = test_state_file("config-path-selection").with_extension("dir");
        let portable = root.join("portable-agent-config.json");
        let legacy = root.join("legacy-agent-config.json");
        fs::create_dir_all(&root).expect("应创建测试目录");
        fs::write(&legacy, b"{}").expect("应写入旧版配置");

        assert_eq!(
            select_agent_config_path(portable.clone(), legacy.clone()),
            legacy
        );

        fs::write(&portable, b"{}").expect("应写入便携配置");
        assert_eq!(
            select_agent_config_path(portable.clone(), legacy.clone()),
            portable
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn agent_config_round_trip_preserves_and_normalizes_tls_fingerprint() {
        let config_file = test_state_file("config-round-trip");
        let compact_fingerprint =
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let config = AgentConfig {
            relay: "relay.example.com:7443".to_owned(),
            tls_fingerprint: Some(compact_fingerprint.to_owned()),
            ..AgentConfig::default()
        };

        config.save_file(&config_file).expect("应保存 Agent 配置");
        let loaded = AgentConfig::load_file(Some(&config_file))
            .expect("应读取 Agent 配置")
            .normalize_and_validate()
            .expect("应规范化 Agent 配置");

        assert_eq!(loaded.server_name, "relay.example.com");
        assert_eq!(
            loaded.tls_fingerprint.as_deref(),
            Some(
                "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF"
            )
        );
        let _ = fs::remove_file(config_file);
    }

    #[test]
    fn resource_capacity_guards_enforce_global_and_session_limits() {
        assert!(ensure_task_capacity(0, 0, false).is_ok());
        assert!(ensure_task_capacity(MAX_PENDING_TASKS, 0, false).is_err());
        assert!(ensure_task_capacity(0, MAX_PENDING_TASKS_PER_SESSION, false).is_err());
        assert!(ensure_task_capacity(0, 0, true).is_err());

        assert!(ensure_shell_capacity(0, 0).is_ok());
        assert!(ensure_shell_capacity(MAX_SHELL_SESSIONS, 0).is_err());
        assert!(ensure_shell_capacity(0, MAX_SHELL_SESSIONS_PER_SESSION).is_err());

        assert!(ensure_serial_capacity(0, 0).is_ok());
        assert!(ensure_serial_capacity(MAX_SERIAL_SESSIONS, 0).is_err());
        assert!(ensure_serial_capacity(0, MAX_SERIAL_SESSIONS_PER_SESSION).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_version_probe_returns_valid_utf8() {
        let version = windows_cmd_version().expect("Windows 版本探测应成功");

        assert!(version.contains("Microsoft Windows"));
        assert!(!version.contains('\u{fffd}'));
    }

    #[cfg(windows)]
    #[test]
    fn windows_probe_process_has_no_console_window() {
        let script = r#"Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class NativeMethods { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'; if ([NativeMethods]::GetConsoleWindow() -eq [IntPtr]::Zero) { Write-Output 'NO_CONSOLE' } else { exit 1 }"#;
        let output = background_command("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
            ])
            .output()
            .expect("后台探测命令应启动成功");

        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("NO_CONSOLE"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_environment_profile_returns_valid_utf8() {
        let transfer_root = test_state_file("environment-profile").with_extension("transfer");
        let device = SystemDevice::with_transfer_root(&transfer_root).expect("应创建测试传输目录");
        let environment = detect_environment_profile(&device).await;
        let environment_json = serde_json::to_string(&environment).expect("环境画像应可序列化");
        let _ = fs::remove_dir_all(&transfer_root);

        assert!(environment_json.contains("Microsoft Windows"));
        assert!(!environment_json.contains('\u{fffd}'));
    }

    #[test]
    fn agent_identity_and_resume_token_survive_process_restart() {
        let state_file = test_state_file("restart");
        let first = load_agent_state(&state_file, None).expect("首次启动应创建 Agent 状态");
        assert!(first.resume_token.is_none());
        let resume_token = "relay-confirmed-resume-token".to_owned();
        persist_agent_state(
            &state_file,
            &AgentState {
                agent_instance_id: first.agent_instance_id,
                resume_token: Some(resume_token.clone()),
            },
        )
        .expect("应持久化恢复令牌");

        let second = load_agent_state(&state_file, None).expect("重启应读取 Agent 状态");
        assert_eq!(second.agent_instance_id, first.agent_instance_id);
        assert_eq!(second.resume_token.as_deref(), Some(resume_token.as_str()));
        let _ = std::fs::remove_file(state_file);
    }

    #[test]
    fn corrupt_agent_state_is_rejected_without_rotating_identity() {
        let state_file = test_state_file("corrupt");
        std::fs::write(&state_file, b"{not-json").expect("应写入损坏状态");

        let error = load_agent_state(&state_file, None).expect_err("损坏状态必须失败关闭");

        assert!(error.to_string().contains("状态文件格式无效"));
        let _ = std::fs::remove_file(state_file);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn keeps_heartbeat_and_remote_requests_independent() {
        let (agent_stream, mut relay_stream) = tokio::io::duplex(64 * 1024);
        let agent_instance_id = AgentInstanceId::new();
        let session_id = SessionId::new();
        let controller_instance_id = ControllerInstanceId::new();
        let binding_token = "relay-only-binding-token".to_owned();
        let response_request_id = RequestId::new();
        let state_file = std::env::temp_dir().join(format!(
            "remoteops-agent-transport-state-{agent_instance_id}.json"
        ));
        let _ = std::fs::remove_file(&state_file);
        let (_shutdown_sender, shutdown_receiver) = watch::channel(false);
        let connection_task = tokio::spawn(run_connection(
            agent_stream,
            agent_instance_id,
            None,
            Arc::new(Mutex::new(None)),
            state_file.clone(),
            "test-host".to_owned(),
            "test-os".to_owned(),
            CapabilitySet::new([Capability::Cmd]),
            EnvironmentProfile::empty(),
            Arc::new(SystemDevice::new()),
            Arc::new(AtomicU64::new(1)),
            Arc::new(Mutex::new(BTreeMap::new())),
            Arc::new(Mutex::new(BTreeMap::new())),
            Arc::new(LocalPermissionPolicy::default()),
            watch::channel(PermissionMode::ApprovalRequired).1,
            None,
            shutdown_receiver,
        ));

        let hello: WireMessage = read_frame(&mut relay_stream)
            .await
            .expect("Relay 应收到 Agent Hello");
        assert!(matches!(
            hello,
            WireMessage::Hello(ClientHello::Agent(AgentHello {
                agent_instance_id: observed,
                ..
            })) if observed == agent_instance_id
        ));

        write_frame(
            &mut relay_stream,
            &WireMessage::AgentWelcome(AgentWelcome {
                pairing_code: PairingCode::parse("123456789").expect("测试控制码应有效"),
                lease_expires_at: Utc::now() + chrono::Duration::minutes(1),
                resume_token: "test-resume-token".to_owned(),
                connection_generation: 1,
                heartbeat_interval_seconds: 1,
            }),
        )
        .await
        .expect("Relay 应发送 AgentWelcome");
        assert!(matches!(
            read_frame::<WireMessage, _>(&mut relay_stream)
                .await
                .expect("Relay 应收到 AgentWelcomeAck"),
            WireMessage::AgentWelcomeAck(AgentWelcomeAck {
                connection_generation: 1
            })
        ));

        write_frame(
            &mut relay_stream,
            &WireMessage::AgentResumeCommitted(AgentResumeCommitted {
                connection_generation: 1,
            }),
        )
        .await
        .expect("Relay 应发送恢复令牌提交确认");
        assert!(matches!(
            read_frame::<WireMessage, _>(&mut relay_stream)
                .await
                .expect("Relay 应收到 AgentResumeCommitAck"),
            WireMessage::AgentResumeCommitAck(AgentResumeCommitAck {
                connection_generation: 1
            })
        ));

        let heartbeat = timeout(Duration::from_secs(2), async {
            loop {
                let message: WireMessage = read_frame(&mut relay_stream)
                    .await
                    .expect("Relay 应继续读取 Agent 帧");
                if matches!(message, WireMessage::Heartbeat { .. }) {
                    break message;
                }
            }
        })
        .await
        .expect("Agent 在没有业务下行时仍应独立发送心跳");
        assert!(matches!(heartbeat, WireMessage::Heartbeat { .. }));

        write_frame(
            &mut relay_stream,
            &WireMessage::ControllerBinding(ControllerBinding {
                session_id,
                controller_instance_id,
                owner_id: test_owner_id(),
                controller_kind: ControllerKind::Human,
                permission_mode: PermissionMode::ApprovalRequired,
                binding_token: binding_token.clone(),
            }),
        )
        .await
        .expect("Relay 应发送 Controller 绑定");
        let shell = if cfg!(windows) {
            ShellKind::Cmd
        } else {
            ShellKind::System
        };
        write_frame(
            &mut relay_stream,
            &WireMessage::AuthorizedRemoteRequest(AuthorizedRemoteRequest {
                request: RemoteRequest {
                    request_id: response_request_id,
                    session_id,
                    source: EventSource::Human,
                    operation: RemoteOperation::RunCommand {
                        shell,
                        command: "echo REMOTEOPS_AGENT_TRANSPORT".to_owned(),
                        readonly: true,
                    },
                    approval_id: None,
                    payload_base64: None,
                },
                authorization: remoteops_protocol::RelayAuthorization {
                    controller_instance_id,
                    owner_id: test_owner_id(),
                    controller_kind: ControllerKind::Human,
                    permission_mode: PermissionMode::ApprovalRequired,
                    binding_token,
                    approval: ApprovalState::NotRequired,
                },
            }),
        )
        .await
        .expect("Relay 应发送授权请求");

        let response = timeout(Duration::from_secs(10), async {
            loop {
                let message: WireMessage = read_frame(&mut relay_stream)
                    .await
                    .expect("Relay 应读取 Agent 响应");
                if let WireMessage::RemoteResponse(response) = message
                    && response.request_id == response_request_id
                {
                    break response;
                }
            }
        })
        .await
        .expect("Agent 应在超时前返回命令结果");
        assert_eq!(response.exit_code, Some(0));
        assert!(response.summary.contains("REMOTEOPS_AGENT_TRANSPORT"));

        drop(relay_stream);
        let connection_result = timeout(Duration::from_secs(2), connection_task)
            .await
            .expect("Relay 关闭后 Agent 连接任务应结束")
            .expect("Agent 连接任务不应 panic");
        assert!(connection_result.is_err());
        let _ = std::fs::remove_file(state_file);
    }

    #[tokio::test]
    async fn executes_local_shell_request() {
        let device = SystemDevice::new();
        let shell = if cfg!(windows) {
            ShellKind::Cmd
        } else {
            ShellKind::System
        };
        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id: SessionId::new(),
            source: EventSource::Human,
            operation: RemoteOperation::RunCommand {
                shell,
                command: "echo remoteops".to_owned(),
                readonly: true,
            },
            approval_id: None,
            payload_base64: None,
        };

        let runtime_state = RequestRuntimeState {
            shell_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            serial_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            file_uploads: Arc::new(Mutex::new(BTreeMap::new())),
        };
        let (sender, _receiver) = mpsc::unbounded_channel();
        let sequence = Arc::new(AtomicU64::new(1));
        let response = execute_operation(
            &device,
            &device,
            &request,
            &runtime_state,
            &sender,
            &sequence,
        )
        .await
        .expect("本机 Shell 命令应成功");

        assert!(response.summary.to_lowercase().contains("remoteops"));
        assert_eq!(response.exit_code, Some(0));
    }

    #[tokio::test]
    async fn executes_chunked_file_transfer_with_session_binding_and_hashes() {
        let transfer_root = test_state_file("chunked-transfer").with_extension("dir");
        let device = SystemDevice::with_transfer_root(&transfer_root).expect("应创建交换目录");
        let uploads = Arc::new(Mutex::new(BTreeMap::new()));
        let session_id = SessionId::new();
        let transfer_id = FileTransferId::new();
        let contents = b"remoteops-agent-chunked-file";
        let hash = sha256_bytes(contents);

        execute_test_file_operation(
            &device,
            &uploads,
            session_id,
            RemoteOperation::BeginUploadFile {
                transfer_id,
                remote_path: "nested/result.bin".to_owned(),
                size: contents.len() as u64,
                sha256: hash.clone(),
                overwrite: false,
            },
            None,
        )
        .await
        .expect("应开始上传");
        let foreign_error = execute_test_file_operation(
            &device,
            &uploads,
            SessionId::new(),
            RemoteOperation::UploadFileChunk {
                transfer_id,
                offset: 0,
                size: contents.len() as u64,
                sha256: hash.clone(),
            },
            Some(BASE64.encode(contents)),
        )
        .await
        .expect_err("其他会话不得写入该上传");
        assert!(foreign_error.to_string().contains("session_id"));
        execute_test_file_operation(
            &device,
            &uploads,
            session_id,
            RemoteOperation::UploadFileChunk {
                transfer_id,
                offset: 0,
                size: contents.len() as u64,
                sha256: hash.clone(),
            },
            Some(BASE64.encode(contents)),
        )
        .await
        .expect("应写入上传分块");
        let complete = execute_test_file_operation(
            &device,
            &uploads,
            session_id,
            RemoteOperation::CompleteUploadFile { transfer_id },
            None,
        )
        .await
        .expect("应完成上传");
        assert_eq!(complete.sha256.as_deref(), Some(hash.as_str()));
        assert!(uploads.lock().await.is_empty());

        let download = execute_test_file_operation(
            &device,
            &uploads,
            session_id,
            RemoteOperation::DownloadFileChunk {
                remote_path: "nested/result.bin".to_owned(),
                offset: 0,
                max_bytes: MAX_FILE_CHUNK_BYTES as u64,
            },
            None,
        )
        .await
        .expect("应下载分块");
        assert_eq!(
            BASE64
                .decode(download.payload_base64.expect("应返回分块负载"))
                .expect("分块负载应为 Base64"),
            contents
        );
        assert_eq!(download.sha256.as_deref(), Some(hash.as_str()));
        assert_eq!(
            download
                .details
                .as_ref()
                .and_then(|value| value["eof"].as_bool()),
            Some(true)
        );

        let _ = fs::remove_dir_all(transfer_root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completion_failure_and_cancellation_have_exactly_one_terminal_winner() {
        for iteration in 0..256 {
            let terminal = Arc::new(AtomicTaskTerminal::running());
            let barrier = Arc::new(Barrier::new(3));
            let operation_terminal = if iteration % 2 == 0 {
                TaskTerminal::Completed
            } else {
                TaskTerminal::Failed
            };

            let operation_state = terminal.clone();
            let operation_barrier = barrier.clone();
            let operation = tokio::spawn(async move {
                operation_barrier.wait().await;
                operation_state.try_commit(operation_terminal).is_ok()
            });
            let cancellation_state = terminal.clone();
            let cancellation_barrier = barrier.clone();
            let cancellation = tokio::spawn(async move {
                cancellation_barrier.wait().await;
                cancellation_state
                    .try_commit(TaskTerminal::Cancelled)
                    .is_ok()
            });

            barrier.wait().await;
            let operation_won = operation.await.expect("业务终态竞争任务不应 panic");
            let cancellation_won = cancellation.await.expect("取消终态竞争任务不应 panic");
            assert_ne!(operation_won, cancellation_won);
            assert_eq!(
                terminal.load(),
                if operation_won {
                    operation_terminal
                } else {
                    TaskTerminal::Cancelled
                }
            );
        }
    }

    #[tokio::test]
    async fn cancel_after_completed_task_reports_completed_without_overwrite() {
        let request_id = RequestId::new();
        let terminal = Arc::new(AtomicTaskTerminal::running());
        terminal
            .try_commit(TaskTerminal::Completed)
            .expect("完成终态应首次提交成功");
        let tasks = Arc::new(Mutex::new(BTreeMap::new()));
        tasks.lock().await.insert(
            request_id,
            PendingTask {
                session_id: SessionId::new(),
                task: tokio::spawn(async {}),
                terminal: terminal.clone(),
                interactive_shell: None,
            },
        );
        let recent = Arc::new(Mutex::new(BTreeMap::new()));

        let outcome = cancel_pending_task(&tasks, &recent, request_id).await;

        assert_eq!(outcome, CancelTaskOutcome::Completed);
        assert_eq!(terminal.load(), TaskTerminal::Completed);
        assert_eq!(
            recent.lock().await.get(&request_id),
            Some(&TaskTerminal::Completed)
        );
        assert!(tasks.lock().await.is_empty());
    }

    #[tokio::test]
    async fn cancellation_claims_running_task_and_blocks_late_completion() {
        let request_id = RequestId::new();
        let terminal = Arc::new(AtomicTaskTerminal::running());
        let tasks = Arc::new(Mutex::new(BTreeMap::new()));
        tasks.lock().await.insert(
            request_id,
            PendingTask {
                session_id: SessionId::new(),
                task: tokio::spawn(std::future::pending()),
                terminal: terminal.clone(),
                interactive_shell: None,
            },
        );
        let recent = Arc::new(Mutex::new(BTreeMap::new()));

        let outcome = cancel_pending_task(&tasks, &recent, request_id).await;

        assert_eq!(outcome, CancelTaskOutcome::Cancelled);
        assert_eq!(terminal.load(), TaskTerminal::Cancelled);
        assert_eq!(
            terminal.try_commit(TaskTerminal::Completed),
            Err(TaskTerminal::Cancelled)
        );
        assert_eq!(
            recent.lock().await.get(&request_id),
            Some(&TaskTerminal::Cancelled)
        );
    }

    #[tokio::test]
    async fn disconnect_abort_preserves_already_committed_terminal() {
        let running_id = RequestId::new();
        let completed_id = RequestId::new();
        let running = Arc::new(AtomicTaskTerminal::running());
        let completed = Arc::new(AtomicTaskTerminal::running());
        completed
            .try_commit(TaskTerminal::Completed)
            .expect("完成终态应首次提交成功");
        let tasks = Arc::new(Mutex::new(BTreeMap::new()));
        {
            let mut tasks = tasks.lock().await;
            tasks.insert(
                running_id,
                PendingTask {
                    session_id: SessionId::new(),
                    task: tokio::spawn(std::future::pending()),
                    terminal: running.clone(),
                    interactive_shell: None,
                },
            );
            tasks.insert(
                completed_id,
                PendingTask {
                    session_id: SessionId::new(),
                    task: tokio::spawn(async {}),
                    terminal: completed.clone(),
                    interactive_shell: None,
                },
            );
        }

        timeout(Duration::from_secs(1), abort_pending_tasks(&tasks))
            .await
            .expect("断线清理不应阻塞");

        assert_eq!(running.load(), TaskTerminal::Aborted);
        assert_eq!(completed.load(), TaskTerminal::Completed);
        assert_eq!(
            running.try_commit(TaskTerminal::Cancelled),
            Err(TaskTerminal::Aborted)
        );
        assert!(tasks.lock().await.is_empty());
    }

    fn binding(
        session_id: SessionId,
        controller_instance_id: remoteops_domain::ControllerInstanceId,
    ) -> ControllerBinding {
        ControllerBinding {
            session_id,
            controller_instance_id,
            owner_id: test_owner_id(),
            controller_kind: ControllerKind::Ai,
            permission_mode: PermissionMode::ApprovalRequired,
            binding_token: "relay-only-binding-token".to_owned(),
        }
    }

    fn authorized_request(
        session_id: SessionId,
        controller_instance_id: remoteops_domain::ControllerInstanceId,
        operation: RemoteOperation,
        approval_id: Option<ApprovalId>,
        approval: ApprovalState,
    ) -> AuthorizedRemoteRequest {
        AuthorizedRemoteRequest {
            request: RemoteRequest {
                request_id: RequestId::new(),
                session_id,
                source: EventSource::Ai,
                operation,
                approval_id,
                payload_base64: None,
            },
            authorization: remoteops_protocol::RelayAuthorization {
                controller_instance_id,
                owner_id: test_owner_id(),
                controller_kind: ControllerKind::Ai,
                permission_mode: PermissionMode::ApprovalRequired,
                binding_token: "relay-only-binding-token".to_owned(),
                approval,
            },
        }
    }

    #[test]
    fn accepts_only_request_matching_current_relay_binding() {
        let policy = DefaultPolicy::default();
        let session_id = SessionId::new();
        let controller_id = remoteops_domain::ControllerInstanceId::new();
        let mut bindings = BTreeMap::new();
        bindings.insert(controller_id, binding(session_id, controller_id));
        let authorized = authorized_request(
            session_id,
            controller_id,
            RemoteOperation::RunCommand {
                shell: ShellKind::WindowsPowerShell,
                command: "Get-Process".to_owned(),
                readonly: true,
            },
            None,
            ApprovalState::NotRequired,
        );

        assert!(
            verify_authorized_request(
                &policy,
                &LocalPermissionPolicy::default(),
                &bindings,
                authorized
            )
            .is_ok()
        );
    }

    #[test]
    fn full_access_requires_agent_local_owner_authorization() {
        let policy = DefaultPolicy::default();
        let session_id = SessionId::new();
        let controller_id = ControllerInstanceId::new();
        let owner_id = test_owner_id();
        let mut current_binding = binding(session_id, controller_id);
        current_binding.permission_mode = PermissionMode::FullAccess;
        let mut bindings = BTreeMap::new();
        bindings.insert(controller_id, current_binding);
        let mut authorized = authorized_request(
            session_id,
            controller_id,
            RemoteOperation::RunCommand {
                shell: ShellKind::WindowsPowerShell,
                command: "Set-Content -Path status.txt -Value ok".to_owned(),
                readonly: false,
            },
            None,
            ApprovalState::NotRequired,
        );
        authorized.authorization.permission_mode = PermissionMode::FullAccess;

        assert!(
            verify_authorized_request(
                &policy,
                &LocalPermissionPolicy::default(),
                &bindings,
                authorized.clone()
            )
            .is_err()
        );

        let local_permission_policy = LocalPermissionPolicy {
            permission_control: AgentPermissionControl::default(),
            active_owner: Arc::new(RwLock::new(None)),
            persistent_full_access_owners: BTreeSet::new(),
            session_full_access_owners: BTreeSet::from([owner_id]),
        };
        assert!(
            verify_authorized_request(&policy, &local_permission_policy, &bindings, authorized)
                .is_ok()
        );
    }

    #[test]
    fn controller_approved_request_does_not_require_agent_full_access_toggle() {
        let policy = DefaultPolicy::default();
        let session_id = SessionId::new();
        let controller_id = ControllerInstanceId::new();
        let mut current_binding = binding(session_id, controller_id);
        current_binding.permission_mode = PermissionMode::ControllerApproved;
        let mut bindings = BTreeMap::new();
        bindings.insert(controller_id, current_binding);
        let mut authorized = authorized_request(
            session_id,
            controller_id,
            RemoteOperation::RunCommand {
                shell: ShellKind::WindowsPowerShell,
                command: "Set-Content -Path status.txt -Value ok".to_owned(),
                readonly: false,
            },
            None,
            ApprovalState::NotRequired,
        );
        authorized.authorization.permission_mode = PermissionMode::ControllerApproved;

        assert!(
            verify_authorized_request(
                &policy,
                &LocalPermissionPolicy::default(),
                &bindings,
                authorized
            )
            .is_ok()
        );
    }

    #[test]
    fn local_permission_control_grants_and_revokes_current_owner() {
        let owner_id = test_owner_id();
        let permission_control = AgentPermissionControl::default();
        let policy =
            LocalPermissionPolicy::from_config(&AgentConfig::default(), permission_control.clone());
        policy.set_active_owner(Some(owner_id));

        assert!(!policy.allows(owner_id, PermissionMode::FullAccess));
        permission_control.set_permission_mode(PermissionMode::FullAccess);
        assert!(policy.allows(owner_id, PermissionMode::FullAccess));
        permission_control.set_permission_mode(PermissionMode::ApprovalRequired);
        assert!(!policy.allows(owner_id, PermissionMode::FullAccess));
    }

    #[test]
    fn loads_persistent_full_access_owner_with_timestamp() {
        let owner_id = test_owner_id();
        let config_file = std::env::temp_dir().join(format!(
            "remoteops-agent-trusted-owner-{}.json",
            RequestId::new()
        ));
        std::fs::write(
            &config_file,
            format!(
                r#"{{"relay":"relay.example.com:7443","trusted_full_access_owners":[{{"owner_id":"{owner_id}","granted_at":"2026-08-06T00:00:00Z"}}]}}"#
            ),
        )
        .expect("应写入测试配置");

        let config = AgentConfig::load_file(Some(&config_file)).expect("应读取可信 Owner 配置");

        assert!(config.trusted_full_access_owners.contains_key(&owner_id));
        let _ = std::fs::remove_file(config_file);
    }

    #[test]
    fn rejects_forged_binding_token_and_source() {
        let policy = DefaultPolicy::default();
        let session_id = SessionId::new();
        let controller_id = remoteops_domain::ControllerInstanceId::new();
        let mut bindings = BTreeMap::new();
        bindings.insert(controller_id, binding(session_id, controller_id));
        let mut forged = authorized_request(
            session_id,
            controller_id,
            RemoteOperation::TestPort {
                host: "127.0.0.1".to_owned(),
                port: 22,
            },
            None,
            ApprovalState::NotRequired,
        );
        forged.authorization.binding_token = "forged".to_owned();
        assert!(
            verify_authorized_request(
                &policy,
                &LocalPermissionPolicy::default(),
                &bindings,
                forged
            )
            .is_err()
        );

        let mut forged = authorized_request(
            session_id,
            controller_id,
            RemoteOperation::TestPort {
                host: "127.0.0.1".to_owned(),
                port: 22,
            },
            None,
            ApprovalState::NotRequired,
        );
        forged.request.source = EventSource::Human;
        assert!(
            verify_authorized_request(
                &policy,
                &LocalPermissionPolicy::default(),
                &bindings,
                forged
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_client_approval_without_relay_approved_claim() {
        let policy = DefaultPolicy::default();
        let session_id = SessionId::new();
        let controller_id = remoteops_domain::ControllerInstanceId::new();
        let mut bindings = BTreeMap::new();
        bindings.insert(controller_id, binding(session_id, controller_id));
        let forged = authorized_request(
            session_id,
            controller_id,
            RemoteOperation::RunCommand {
                shell: ShellKind::WindowsPowerShell,
                command: "Remove-Item C:\\temp\\a.txt".to_owned(),
                readonly: false,
            },
            Some(ApprovalId::new()),
            ApprovalState::NotRequired,
        );

        assert!(
            verify_authorized_request(
                &policy,
                &LocalPermissionPolicy::default(),
                &bindings,
                forged
            )
            .is_err()
        );
    }

    #[test]
    fn tracks_ai_and_human_bindings_for_same_session() {
        let session_id = SessionId::new();
        let ai_id = remoteops_domain::ControllerInstanceId::new();
        let human_id = remoteops_domain::ControllerInstanceId::new();
        let mut bindings = BTreeMap::new();
        let ai_binding = binding(session_id, ai_id);
        let mut human_binding = binding(session_id, human_id);
        human_binding.controller_kind = ControllerKind::Human;
        human_binding.binding_token = "human-binding-token".to_owned();

        bindings.insert(ai_id, ai_binding.clone());
        bindings.insert(human_id, human_binding.clone());
        let local_permission_policy = LocalPermissionPolicy {
            permission_control: AgentPermissionControl::new(PermissionMode::FullAccess),
            ..LocalPermissionPolicy::default()
        };

        assert_eq!(bindings.len(), 2);
        assert_eq!(active_controller_owner_count(&bindings), 1);
        assert!(revoke_controller_binding(
            &mut bindings,
            session_id,
            &ai_binding.binding_token,
        ));
        assert_eq!(bindings.len(), 1);
        assert_eq!(active_controller_owner_count(&bindings), 1);
        assert_eq!(bindings.get(&human_id), Some(&human_binding));
        assert_eq!(
            local_permission_policy.permission_control.permission_mode(),
            PermissionMode::FullAccess
        );

        assert!(revoke_controller_binding(
            &mut bindings,
            session_id,
            &human_binding.binding_token,
        ));
        assert_eq!(
            local_permission_policy.permission_control.permission_mode(),
            PermissionMode::FullAccess
        );
    }

    #[test]
    fn agent_normalizes_structured_serial_query_readonly_claim() {
        let policy = DefaultPolicy::default();
        let operation = RemoteOperation::RunSerialQuery {
            serial_session_id: "serial-1".to_owned(),
            command: "display version".to_owned(),
            line_ending: remoteops_domain::SerialLineEnding::Cr,
            profile: remoteops_domain::SerialTerminalProfile::HuaweiVrp,
            overall_timeout_millis: 30_000,
            idle_timeout_millis: 1_200,
            max_bytes: 128 * 1024,
            max_pages: 50,
            readonly: false,
        };

        assert!(matches!(
            normalize_operation(&policy, operation),
            RemoteOperation::RunSerialQuery { readonly: true, .. }
        ));
    }

    #[test]
    fn persistent_shell_request_must_match_runtime_shell_and_session() {
        let session_id = SessionId::new();
        assert!(
            validate_shell_binding(
                session_id,
                ShellKind::PowerShell,
                session_id,
                ShellKind::PowerShell,
            )
            .is_ok()
        );
        assert!(
            validate_shell_binding(
                session_id,
                ShellKind::PowerShell,
                session_id,
                ShellKind::Cmd,
            )
            .is_err()
        );
        assert!(
            validate_shell_binding(
                session_id,
                ShellKind::PowerShell,
                SessionId::new(),
                ShellKind::PowerShell,
            )
            .is_err()
        );
    }

    #[test]
    fn serial_output_is_redacted_before_remote_event_delivery() {
        let output = serial_output_for_remote_event(
            b"snmp-agent community read private-community\r\npassword cipher super-secret\r\n",
        );

        assert!(!output.contains("private-community"));
        assert!(!output.contains("super-secret"));
        assert!(output.contains("[敏感行已遮盖]"));
        assert_eq!(
            serial_output_for_remote_event(&[0xff, 0xfe]),
            "[binary serial output: 2 bytes]"
        );
    }

    #[derive(Default)]
    struct RecordingSshProvider {
        commands: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl SshProvider for RecordingSshProvider {
        async fn run_command(
            &self,
            _host: &str,
            _port: u16,
            _username: &str,
            _password: Option<&str>,
            _identity_file: Option<&str>,
            _known_hosts_file: Option<&str>,
            command: &str,
            _timeout_seconds: u64,
        ) -> Result<remoteops_device::CommandResult, remoteops_device::DeviceError> {
            self.commands
                .lock()
                .expect("SSH 命令记录锁不应损坏")
                .push(command.to_owned());
            Ok(remoteops_device::CommandResult {
                stdout: "ok".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
            })
        }
    }

    #[tokio::test]
    async fn agent_ssh_gate_uses_relay_verified_permission_and_approval() {
        let policy = DefaultPolicy::default();
        let session_id = SessionId::new();
        let controller_id = ControllerInstanceId::new();
        let operation = RemoteOperation::RunSsh {
            host: "192.0.2.10".to_owned(),
            port: 22,
            username: "operator".to_owned(),
            identity_file: None,
            known_hosts_file: None,
            command: "system-view".to_owned(),
            readonly: false,
        };

        let mut bindings = BTreeMap::new();
        bindings.insert(controller_id, binding(session_id, controller_id));
        let unapproved = authorized_request(
            session_id,
            controller_id,
            operation.clone(),
            None,
            ApprovalState::NotRequired,
        );
        assert!(
            verify_authorized_request(
                &policy,
                &LocalPermissionPolicy::default(),
                &bindings,
                unapproved,
            )
            .is_err()
        );

        let approved = authorized_request(
            session_id,
            controller_id,
            operation.clone(),
            Some(ApprovalId::new()),
            ApprovalState::Approved,
        );
        let approved = verify_authorized_request(
            &policy,
            &LocalPermissionPolicy::default(),
            &bindings,
            approved,
        )
        .expect("Relay 已批准的 SSH 命令应通过 Agent 最终校验");
        let transfer_root = test_state_file("ssh-provider").with_extension("dir");
        let device = SystemDevice::with_transfer_root(&transfer_root).expect("应创建交换目录");
        let ssh_provider = RecordingSshProvider::default();
        let runtime_state = RequestRuntimeState {
            shell_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            serial_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            file_uploads: Arc::new(Mutex::new(BTreeMap::new())),
        };
        let (sender, _receiver) = mpsc::unbounded_channel();
        execute_operation(
            &device,
            &ssh_provider,
            &approved,
            &runtime_state,
            &sender,
            &Arc::new(AtomicU64::new(1)),
        )
        .await
        .expect("Relay 已批准的 SSH 修改命令应到达设备适配器");
        assert_eq!(
            ssh_provider
                .commands
                .lock()
                .expect("SSH 命令记录锁不应损坏")
                .as_slice(),
            ["system-view"]
        );
        let _ = fs::remove_dir_all(transfer_root);

        let mut controller_approved_binding = binding(session_id, controller_id);
        controller_approved_binding.permission_mode = PermissionMode::ControllerApproved;
        bindings.insert(controller_id, controller_approved_binding);
        let mut controller_approved = authorized_request(
            session_id,
            controller_id,
            operation,
            None,
            ApprovalState::NotRequired,
        );
        controller_approved.authorization.permission_mode = PermissionMode::ControllerApproved;
        assert!(
            verify_authorized_request(
                &policy,
                &LocalPermissionPolicy::default(),
                &bindings,
                controller_approved,
            )
            .is_ok()
        );
    }
}
