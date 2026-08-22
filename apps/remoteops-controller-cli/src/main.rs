use std::{
    io::{self, Write},
    path::PathBuf,
    str::FromStr,
    time::Duration,
};

use anyhow::{Context, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use clap::{Parser, Subcommand, ValueEnum};
use remoteops_application::{ApplicationError, OperationResult, RelayClient, RelayClientConfig};
use remoteops_audit::sha256_bytes;
use remoteops_domain::{
    ApprovalId, ApprovalState, ControllerInstanceId, ControllerOwnerId, EventPayload, EventSource,
    FileTransferId, PairingCode, PermissionMode, PowerAction, RemoteOperation, RequestId,
    SerialDataBits, SerialFlowControl, SerialLineEnding, SerialParity, SerialSettings,
    SerialStopBits, SerialTerminalProfile, ServiceAction, SessionId, ShellId, ShellKind,
};
use remoteops_protocol::ControllerKind;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const FILE_CHUNK_BYTES: usize = 1024 * 1024;
const LARGE_TRANSFER_CONFIRM_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_TRANSFER_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// `RemoteOps` 多连接控制端 CLI。
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Relay TLS 地址。
    #[arg(long, env = "REMOTEOPS_RELAY")]
    relay: String,
    /// Relay 证书中的 DNS 名称或 IP。
    #[arg(long, env = "REMOTEOPS_SERVER_NAME")]
    server_name: String,
    /// Relay 自签名 CA/证书；不提供时使用操作系统可信根。
    #[arg(long, env = "REMOTEOPS_CA_CERT")]
    ca_cert: Option<PathBuf>,
    /// 启动时配对，格式为 CODE 或 CODE=别名；可以重复。
    #[arg(long = "pair", value_parser = parse_pair_spec)]
    pairs: Vec<PairSpec>,
    /// 输出机器可读 JSON。
    #[arg(long)]
    json: bool,
    /// Controller 本地脱敏审计 JSONL 文件。
    #[arg(
        long,
        env = "REMOTEOPS_AUDIT_LOG",
        default_value_os_t = remoteops_audit::default_audit_log_path()
    )]
    audit_log: PathBuf,
    /// Relay 为人工 Controller 配置的独立认证令牌。
    #[arg(long, env = "REMOTEOPS_HUMAN_CONTROLLER_TOKEN", hide_env_values = true)]
    controller_token: String,
    /// Human 与 AI Controller 共同使用的稳定 Owner ID。
    #[arg(long, env = "REMOTEOPS_CONTROLLER_OWNER_ID")]
    owner_id: ControllerOwnerId,
    /// 首次配对或重新配对时请求的会话权限。
    #[arg(long, env = "REMOTEOPS_PERMISSION_MODE", value_enum, default_value_t = PermissionModeArg::ApprovalRequired)]
    permission_mode: PermissionModeArg,
    /// Relay 断开后的自动重连间隔。
    #[arg(long, env = "REMOTEOPS_RECONNECT_SECONDS", default_value_t = 2)]
    reconnect_seconds: u64,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Debug)]
struct PairSpec {
    code: PairingCode,
    alias: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 列出已配对连接。
    List,
    /// 修改连接别名。
    Alias {
        /// 会话 UUID、连接编号或现有别名。
        target: String,
        /// 新别名。
        alias: String,
    },
    /// 执行 CMD、Windows PowerShell、PowerShell 7 或系统 Shell。
    Run {
        target: String,
        #[arg(long, value_enum, default_value_t = ShellArg::WindowsPowerShell)]
        shell: ShellArg,
        #[arg(long, value_enum, default_value_t = SourceArg::Human)]
        source: SourceArg,
        /// 声明命令只读取信息。
        #[arg(long)]
        readonly: bool,
        /// 对需要审批的操作自动执行一次显式人工批准。
        #[arg(long)]
        approve: bool,
        /// 原始命令。
        command: String,
    },
    /// 在 Agent 上打开真实持久 Shell。
    ShellOpen {
        target: String,
        #[arg(long, value_enum, default_value_t = ShellArg::WindowsPowerShell)]
        shell: ShellArg,
    },
    /// 在真实持久 Shell 中执行命令。
    ShellRun {
        target: String,
        shell_id: ShellId,
        /// 打开该持久 Shell 时使用的类型。
        #[arg(long, value_enum)]
        shell: ShellArg,
        #[arg(long, value_enum, default_value_t = SourceArg::Human)]
        source: SourceArg,
        /// 对需要审批的操作自动执行一次显式人工批准。
        #[arg(long)]
        approve: bool,
        command: String,
    },
    /// 关闭真实持久 Shell。
    ShellClose { target: String, shell_id: ShellId },
    /// 探测远程 Agent 可访问的 TCP 端口。
    TestPort {
        target: String,
        host: String,
        port: u16,
    },
    /// 分块上传文件并校验哈希。
    Upload {
        target: String,
        local_path: PathBuf,
        remote_path: String,
        #[arg(long)]
        overwrite: bool,
        #[arg(long)]
        approve: bool,
    },
    /// 分块下载文件并校验哈希。
    Download {
        target: String,
        remote_path: String,
        local_path: PathBuf,
        /// 明确允许覆盖现有本地文件。
        #[arg(long)]
        overwrite: bool,
        /// 明确允许下载超过 1 GiB 的文件。
        #[arg(long)]
        approve_large: bool,
    },
    /// 读取远程文件元数据和小文件哈希。
    FileMetadata { target: String, remote_path: String },
    /// 移动或重命名远程文件。
    FileMove {
        target: String,
        source_path: String,
        destination_path: String,
        #[arg(long)]
        overwrite: bool,
        #[arg(long)]
        approve: bool,
    },
    /// 删除单个远程普通文件。
    FileDelete {
        target: String,
        remote_path: String,
        #[arg(long)]
        approve: bool,
    },
    /// 向明确 TCP 目标发送 Base64 数据并返回有界响应。
    TcpExchange {
        target: String,
        host: String,
        port: u16,
        data_base64: String,
        #[arg(long, default_value_t = 64 * 1024)]
        max_response_bytes: usize,
        #[arg(long, default_value_t = 5_000)]
        timeout_millis: u64,
        #[arg(long)]
        approve: bool,
    },
    /// 列出远程进程。
    ProcessList { target: String },
    /// 终止远程进程树。
    ProcessTerminate {
        target: String,
        process_id: u32,
        #[arg(long)]
        approve: bool,
    },
    /// 列出远程服务。
    ServiceList { target: String },
    /// 启动、停止或重启远程服务。
    ServiceControl {
        target: String,
        service_name: String,
        #[arg(value_enum)]
        action: ServiceActionArg,
        #[arg(long)]
        approve: bool,
    },
    /// 重启或关闭远程操作系统。
    Power {
        target: String,
        #[arg(value_enum)]
        action: PowerActionArg,
        #[arg(long)]
        approve: bool,
    },
    /// 执行有界串口查询。
    SerialQuery {
        target: String,
        serial_session_id: String,
        command: String,
        #[arg(long, value_enum, default_value_t = SerialLineEndingArg::Cr)]
        line_ending: SerialLineEndingArg,
        #[arg(long, value_enum, default_value_t = SerialProfileArg::HuaweiVrp)]
        profile: SerialProfileArg,
        #[arg(long, default_value_t = 30_000)]
        overall_timeout_millis: u64,
        #[arg(long, default_value_t = 1_200)]
        idle_timeout_millis: u64,
        #[arg(long, default_value_t = 128 * 1024)]
        max_bytes: usize,
        #[arg(long, default_value_t = 50)]
        max_pages: u16,
        #[arg(long)]
        readonly: bool,
        #[arg(long)]
        approve: bool,
    },
    /// 枚举远程 Agent 可见串口。
    SerialList { target: String },
    /// 打开持续串口会话。
    SerialOpen {
        target: String,
        port_name: String,
        #[arg(long, default_value_t = 9600)]
        baud_rate: u32,
        #[arg(long, value_enum, default_value_t = SerialDataBitsArg::Eight)]
        data_bits: SerialDataBitsArg,
        #[arg(long, value_enum, default_value_t = SerialStopBitsArg::One)]
        stop_bits: SerialStopBitsArg,
        #[arg(long, value_enum, default_value_t = SerialParityArg::None)]
        parity: SerialParityArg,
        #[arg(long, value_enum, default_value_t = SerialFlowControlArg::None)]
        flow_control: SerialFlowControlArg,
        #[arg(long)]
        writable: bool,
        #[arg(long)]
        approve: bool,
    },
    /// 向已打开的可写串口发送文本或十六进制字节。
    SerialWrite {
        target: String,
        serial_session_id: String,
        data: String,
        #[arg(long)]
        hex: bool,
    },
    /// 关闭串口会话。
    SerialClose {
        target: String,
        serial_session_id: String,
    },
    /// 通过 Agent 本机 OpenSSH 客户端执行密钥认证命令。
    Ssh {
        target: String,
        host: String,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long)]
        identity_file: Option<String>,
        #[arg(long)]
        known_hosts_file: Option<String>,
        #[arg(long)]
        readonly: bool,
        username: String,
        command: String,
    },
    /// 人工接管连接写入权。
    Takeover { target: String },
    /// 释放人工接管，让 AI 恢复按策略工作。
    ReleaseTakeover { target: String },
    /// 读取指定连接的统一事件流。
    Events {
        target: String,
        #[arg(long)]
        after_sequence: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// 中断指定远程请求。
    Cancel {
        target: String,
        request_id: RequestId,
    },
    /// 启动一个 AI 只读命令，并在指定延迟后由人工中断。
    CancelAfter {
        target: String,
        #[arg(long, value_enum, default_value_t = ShellArg::WindowsPowerShell)]
        shell: ShellArg,
        #[arg(long, default_value_t = 1_000)]
        delay_millis: u64,
        command: String,
    },
    /// 关闭远程连接。
    Close { target: String },
    /// 由人工立即停止目标会话的全部任务和交互资源。
    EmergencyStop { target: String },
    /// 对 AI Controller 申请的精确操作作出人工审批决定。
    ApprovalDecide {
        /// 会话 UUID、连接编号或别名。
        target: String,
        /// MCP 返回的审批标识。
        approval_id: ApprovalId,
        /// MCP 返回的完整 `RemoteOperation` JSON。
        operation_json: String,
        /// 拒绝该操作；默认表示批准。
        #[arg(long)]
        reject: bool,
    },
    /// 导出指定会话的脱敏审计记录。
    AuditExport { target: String, output: PathBuf },
    /// 进入一对多交互模式。
    Interactive,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ShellArg {
    Cmd,
    WindowsPowerShell,
    PowerShell,
    System,
}

impl From<ShellArg> for ShellKind {
    fn from(value: ShellArg) -> Self {
        match value {
            ShellArg::Cmd => Self::Cmd,
            ShellArg::WindowsPowerShell => Self::WindowsPowerShell,
            ShellArg::PowerShell => Self::PowerShell,
            ShellArg::System => Self::System,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SourceArg {
    Human,
    Ai,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ServiceActionArg {
    Start,
    Stop,
    Restart,
}

impl From<ServiceActionArg> for ServiceAction {
    fn from(value: ServiceActionArg) -> Self {
        match value {
            ServiceActionArg::Start => Self::Start,
            ServiceActionArg::Stop => Self::Stop,
            ServiceActionArg::Restart => Self::Restart,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PowerActionArg {
    Restart,
    Shutdown,
}

impl From<PowerActionArg> for PowerAction {
    fn from(value: PowerActionArg) -> Self {
        match value {
            PowerActionArg::Restart => Self::Restart,
            PowerActionArg::Shutdown => Self::Shutdown,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SerialLineEndingArg {
    None,
    Cr,
    Lf,
    CrLf,
}

impl From<SerialLineEndingArg> for SerialLineEnding {
    fn from(value: SerialLineEndingArg) -> Self {
        match value {
            SerialLineEndingArg::None => Self::None,
            SerialLineEndingArg::Cr => Self::Cr,
            SerialLineEndingArg::Lf => Self::Lf,
            SerialLineEndingArg::CrLf => Self::CrLf,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SerialProfileArg {
    Ansi,
    HuaweiVrp,
}

impl From<SerialProfileArg> for SerialTerminalProfile {
    fn from(value: SerialProfileArg) -> Self {
        match value {
            SerialProfileArg::Ansi => Self::Ansi,
            SerialProfileArg::HuaweiVrp => Self::HuaweiVrp,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PermissionModeArg {
    ReadOnly,
    ApprovalRequired,
    FullAccess,
}

impl From<PermissionModeArg> for PermissionMode {
    fn from(value: PermissionModeArg) -> Self {
        match value {
            PermissionModeArg::ReadOnly => Self::ReadOnly,
            PermissionModeArg::ApprovalRequired => Self::ApprovalRequired,
            PermissionModeArg::FullAccess => Self::FullAccess,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SerialDataBitsArg {
    #[value(name = "5")]
    Five,
    #[value(name = "6")]
    Six,
    #[value(name = "7")]
    Seven,
    #[value(name = "8")]
    Eight,
}

impl From<SerialDataBitsArg> for SerialDataBits {
    fn from(value: SerialDataBitsArg) -> Self {
        match value {
            SerialDataBitsArg::Five => Self::Five,
            SerialDataBitsArg::Six => Self::Six,
            SerialDataBitsArg::Seven => Self::Seven,
            SerialDataBitsArg::Eight => Self::Eight,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SerialStopBitsArg {
    #[value(name = "1")]
    One,
    #[value(name = "2")]
    Two,
}

impl From<SerialStopBitsArg> for SerialStopBits {
    fn from(value: SerialStopBitsArg) -> Self {
        match value {
            SerialStopBitsArg::One => Self::One,
            SerialStopBitsArg::Two => Self::Two,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SerialParityArg {
    None,
    Odd,
    Even,
}

impl From<SerialParityArg> for SerialParity {
    fn from(value: SerialParityArg) -> Self {
        match value {
            SerialParityArg::None => Self::None,
            SerialParityArg::Odd => Self::Odd,
            SerialParityArg::Even => Self::Even,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SerialFlowControlArg {
    None,
    Software,
    Hardware,
}

impl From<SerialFlowControlArg> for SerialFlowControl {
    fn from(value: SerialFlowControlArg) -> Self {
        match value {
            SerialFlowControlArg::None => Self::None,
            SerialFlowControlArg::Software => Self::Software,
            SerialFlowControlArg::Hardware => Self::Hardware,
        }
    }
}

impl From<SourceArg> for EventSource {
    fn from(value: SourceArg) -> Self {
        match value {
            SourceArg::Human => Self::Human,
            SourceArg::Ai => Self::Ai,
        }
    }
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let client = RelayClient::connect(
        RelayClientConfig {
            relay_address: args.relay,
            server_name: args.server_name,
            ca_certificate: args.ca_cert,
            tls_fingerprint: None,
            audit_log: Some(args.audit_log),
            controller_kind: ControllerKind::Human,
            owner_id: args.owner_id,
            permission_mode: args.permission_mode.into(),
            authentication_token: args.controller_token,
            reconnect_delay: Duration::from_secs(args.reconnect_seconds.max(1)),
        },
        ControllerInstanceId::new(),
    )
    .await
    .context("连接 Relay 失败")?;
    pair_all(&client, &args.pairs).await?;
    spawn_event_printer(client.clone(), args.json);

    match args.command {
        Command::List => print_connections(&client, args.json).await?,
        Command::Alias { target, alias } => {
            client.set_alias(&target, alias).await?;
            print_connections(&client, args.json).await?;
        }
        Command::Run {
            target,
            shell,
            source,
            readonly,
            approve,
            command,
        } => {
            let operation = RemoteOperation::RunCommand {
                shell: shell.into(),
                command,
                readonly,
            };
            let result =
                execute_with_approval(&client, &target, source.into(), operation, approve, None)
                    .await?;
            print_result(&result, args.json)?;
        }
        Command::ShellOpen { target, shell } => {
            let result = client
                .execute(
                    &target,
                    EventSource::Human,
                    RemoteOperation::OpenShell {
                        shell: shell.into(),
                    },
                    None,
                    None,
                    None,
                )
                .await?;
            print_result(&result, args.json)?;
        }
        Command::ShellRun {
            target,
            shell_id,
            shell,
            source,
            approve,
            command,
        } => {
            let result = execute_with_approval(
                &client,
                &target,
                source.into(),
                RemoteOperation::RunShellCommand {
                    shell_id,
                    shell: shell.into(),
                    command,
                    readonly: false,
                },
                approve,
                None,
            )
            .await?;
            print_result(&result, args.json)?;
        }
        Command::ShellClose { target, shell_id } => {
            let result = client
                .execute(
                    &target,
                    EventSource::Human,
                    RemoteOperation::CloseShell { shell_id },
                    None,
                    None,
                    None,
                )
                .await?;
            print_result(&result, args.json)?;
        }
        Command::TestPort { target, host, port } => {
            let result = client
                .execute(
                    &target,
                    EventSource::Human,
                    RemoteOperation::TestPort { host, port },
                    None,
                    None,
                    None,
                )
                .await?;
            print_result(&result, args.json)?;
        }
        Command::Upload {
            target,
            local_path,
            remote_path,
            overwrite,
            approve,
        } => {
            let size = tokio::fs::metadata(&local_path)
                .await
                .with_context(|| format!("读取本地文件信息失败：{}", local_path.display()))?
                .len();
            if size > MAX_TRANSFER_BYTES {
                bail!("上传文件超过 {MAX_TRANSFER_BYTES} 字节限制");
            }
            if size > LARGE_TRANSFER_CONFIRM_BYTES && !approve {
                bail!("上传超过 1 GiB 的文件必须显式提供 --approve");
            }
            let hash = sha256_local_file(&local_path).await?;
            let transfer_id = FileTransferId::new();
            execute_with_approval(
                &client,
                &target,
                EventSource::Human,
                RemoteOperation::BeginUploadFile {
                    transfer_id,
                    remote_path,
                    size,
                    sha256: hash,
                    overwrite,
                },
                approve,
                None,
            )
            .await?;
            let transfer_result: anyhow::Result<OperationResult> = async {
                let mut file = tokio::fs::File::open(&local_path)
                    .await
                    .with_context(|| format!("打开本地文件失败：{}", local_path.display()))?;
                let mut buffer = vec![0_u8; FILE_CHUNK_BYTES];
                let mut offset = 0_u64;
                loop {
                    let read = file
                        .read(&mut buffer)
                        .await
                        .with_context(|| format!("读取本地文件失败：{}", local_path.display()))?;
                    if read == 0 {
                        break;
                    }
                    let chunk = &buffer[..read];
                    client
                        .execute(
                            &target,
                            EventSource::Human,
                            RemoteOperation::UploadFileChunk {
                                transfer_id,
                                offset,
                                size: read as u64,
                                sha256: sha256_bytes(chunk),
                            },
                            None,
                            Some(BASE64.encode(chunk)),
                            None,
                        )
                        .await?;
                    offset = offset
                        .checked_add(read as u64)
                        .context("上传文件偏移溢出")?;
                }
                if offset != size {
                    bail!("上传期间文件大小发生变化：期望 {size}，读取 {offset}");
                }
                Ok(client
                    .execute(
                        &target,
                        EventSource::Human,
                        RemoteOperation::CompleteUploadFile { transfer_id },
                        None,
                        None,
                        None,
                    )
                    .await?)
            }
            .await;
            let result = match transfer_result {
                Ok(result) => result,
                Err(error) => {
                    let _ = client
                        .execute(
                            &target,
                            EventSource::Human,
                            RemoteOperation::AbortUploadFile { transfer_id },
                            None,
                            None,
                            None,
                        )
                        .await;
                    return Err(error);
                }
            };
            print_result(&result, args.json)?;
        }
        Command::Download {
            target,
            remote_path,
            local_path,
            overwrite,
            approve_large,
        } => {
            if local_path.exists() && !overwrite {
                bail!("本地目标文件已存在，未设置 --overwrite");
            }
            let metadata = client
                .execute(
                    &target,
                    EventSource::Human,
                    RemoteOperation::GetFileMetadata {
                        remote_path: remote_path.clone(),
                        include_sha256: false,
                    },
                    None,
                    None,
                    None,
                )
                .await?;
            let size = metadata
                .response
                .details
                .as_ref()
                .and_then(|details| details.get("metadata"))
                .and_then(|metadata| metadata.get("size"))
                .and_then(serde_json::Value::as_u64)
                .context("Agent 未返回文件大小")?;
            if size > MAX_TRANSFER_BYTES {
                bail!("下载文件超过 {MAX_TRANSFER_BYTES} 字节限制");
            }
            if size > LARGE_TRANSFER_CONFIRM_BYTES && !approve_large {
                bail!("下载超过 1 GiB 的文件必须显式提供 --approve-large");
            }
            let (temporary_path, mut temporary_file) = create_download_file(&local_path).await?;
            let transfer_result: anyhow::Result<(String, OperationResult)> = async {
                let mut offset = 0_u64;
                let mut hasher = Sha256::new();
                while offset < size {
                    let chunk = client
                        .execute(
                            &target,
                            EventSource::Human,
                            RemoteOperation::DownloadFileChunk {
                                remote_path: remote_path.clone(),
                                offset,
                                max_bytes: FILE_CHUNK_BYTES as u64,
                            },
                            None,
                            None,
                            None,
                        )
                        .await?;
                    let payload = chunk
                        .response
                        .payload_base64
                        .as_deref()
                        .context("Agent 未返回下载分块")?;
                    let bytes = BASE64.decode(payload).context("下载分块 Base64 无效")?;
                    if bytes.is_empty() || bytes.len() > FILE_CHUNK_BYTES {
                        bail!("Agent 返回了无效下载分块大小");
                    }
                    let chunk_hash = sha256_bytes(&bytes);
                    if chunk.response.sha256.as_deref() != Some(chunk_hash.as_str()) {
                        bail!("下载分块 SHA-256 校验失败");
                    }
                    offset = offset
                        .checked_add(bytes.len() as u64)
                        .context("下载文件偏移溢出")?;
                    if offset > size {
                        bail!("Agent 返回的下载分块超过声明文件大小");
                    }
                    temporary_file.write_all(&bytes).await?;
                    hasher.update(&bytes);
                }
                temporary_file.flush().await?;
                temporary_file.sync_all().await?;
                drop(temporary_file);
                let local_hash = format!("{:x}", hasher.finalize());
                let result = client
                    .execute(
                        &target,
                        EventSource::Human,
                        RemoteOperation::GetFileMetadata {
                            remote_path,
                            include_sha256: true,
                        },
                        None,
                        None,
                        None,
                    )
                    .await?;
                if result.response.sha256.as_deref() != Some(local_hash.as_str()) {
                    bail!("下载文件完整 SHA-256 校验失败");
                }
                Ok((local_hash, result))
            }
            .await;
            let (_, result) = match transfer_result {
                Ok(value) => value,
                Err(error) => {
                    let _ = tokio::fs::remove_file(&temporary_path).await;
                    return Err(error);
                }
            };
            commit_download_file(&temporary_path, &local_path, overwrite).await?;
            print_result(&result, args.json)?;
        }
        Command::FileMetadata {
            target,
            remote_path,
        } => {
            let result = client
                .execute(
                    &target,
                    EventSource::Human,
                    RemoteOperation::GetFileMetadata {
                        remote_path,
                        include_sha256: true,
                    },
                    None,
                    None,
                    None,
                )
                .await?;
            print_result(&result, args.json)?;
        }
        Command::FileMove {
            target,
            source_path,
            destination_path,
            overwrite,
            approve,
        } => {
            let result = execute_with_approval(
                &client,
                &target,
                EventSource::Human,
                RemoteOperation::MoveFile {
                    source_path,
                    destination_path,
                    overwrite,
                },
                approve,
                None,
            )
            .await?;
            print_result(&result, args.json)?;
        }
        Command::FileDelete {
            target,
            remote_path,
            approve,
        } => {
            let result = execute_with_approval(
                &client,
                &target,
                EventSource::Human,
                RemoteOperation::DeleteFile { remote_path },
                approve,
                None,
            )
            .await?;
            print_result(&result, args.json)?;
        }
        Command::TcpExchange {
            target,
            host,
            port,
            data_base64,
            max_response_bytes,
            timeout_millis,
            approve,
        } => {
            let payload = BASE64
                .decode(&data_base64)
                .context("TCP data_base64 无效")?;
            let result = execute_with_approval(
                &client,
                &target,
                EventSource::Human,
                RemoteOperation::TcpExchange {
                    host,
                    port,
                    byte_count: payload.len(),
                    request_sha256: sha256_bytes(&payload),
                    max_response_bytes,
                    timeout_millis,
                },
                approve,
                Some(BASE64.encode(payload)),
            )
            .await?;
            print_result(&result, args.json)?;
        }
        Command::ProcessList { target } => {
            let result = client
                .execute(
                    &target,
                    EventSource::Human,
                    RemoteOperation::ListProcesses,
                    None,
                    None,
                    None,
                )
                .await?;
            print_result(&result, args.json)?;
        }
        Command::ProcessTerminate {
            target,
            process_id,
            approve,
        } => {
            let result = execute_with_approval(
                &client,
                &target,
                EventSource::Human,
                RemoteOperation::TerminateProcess { process_id },
                approve,
                None,
            )
            .await?;
            print_result(&result, args.json)?;
        }
        Command::ServiceList { target } => {
            let result = client
                .execute(
                    &target,
                    EventSource::Human,
                    RemoteOperation::ListServices,
                    None,
                    None,
                    None,
                )
                .await?;
            print_result(&result, args.json)?;
        }
        Command::ServiceControl {
            target,
            service_name,
            action,
            approve,
        } => {
            let result = execute_with_approval(
                &client,
                &target,
                EventSource::Human,
                RemoteOperation::ControlService {
                    service_name,
                    action: action.into(),
                },
                approve,
                None,
            )
            .await?;
            print_result(&result, args.json)?;
        }
        Command::Power {
            target,
            action,
            approve,
        } => {
            let result = execute_with_approval(
                &client,
                &target,
                EventSource::Human,
                RemoteOperation::PowerControl {
                    action: action.into(),
                },
                approve,
                None,
            )
            .await?;
            print_result(&result, args.json)?;
        }
        Command::SerialQuery {
            target,
            serial_session_id,
            command,
            line_ending,
            profile,
            overall_timeout_millis,
            idle_timeout_millis,
            max_bytes,
            max_pages,
            readonly,
            approve,
        } => {
            let result = execute_with_approval(
                &client,
                &target,
                EventSource::Human,
                RemoteOperation::RunSerialQuery {
                    serial_session_id,
                    command,
                    line_ending: line_ending.into(),
                    profile: profile.into(),
                    overall_timeout_millis,
                    idle_timeout_millis,
                    max_bytes,
                    max_pages,
                    readonly,
                },
                approve,
                None,
            )
            .await?;
            print_result(&result, args.json)?;
        }
        Command::SerialList { target } => {
            let result = client
                .execute(
                    &target,
                    EventSource::Human,
                    RemoteOperation::ListSerial,
                    None,
                    None,
                    None,
                )
                .await?;
            print_result(&result, args.json)?;
        }
        Command::SerialOpen {
            target,
            port_name,
            baud_rate,
            data_bits,
            stop_bits,
            parity,
            flow_control,
            writable,
            approve,
        } => {
            let result = execute_with_approval(
                &client,
                &target,
                EventSource::Human,
                RemoteOperation::OpenSerial {
                    port_name,
                    settings: SerialSettings {
                        baud_rate,
                        data_bits: data_bits.into(),
                        stop_bits: stop_bits.into(),
                        parity: parity.into(),
                        flow_control: flow_control.into(),
                    },
                    writable,
                },
                approve,
                None,
            )
            .await?;
            print_result(&result, args.json)?;
        }
        Command::SerialWrite {
            target,
            serial_session_id,
            data,
            hex,
        } => {
            let bytes = if hex {
                hex::decode(data).context("十六进制串口数据无效")?
            } else {
                data.into_bytes()
            };
            let result = client
                .execute(
                    &target,
                    EventSource::Human,
                    RemoteOperation::WriteSerial {
                        serial_session_id,
                        byte_count: bytes.len(),
                        sha256: sha256_bytes(&bytes),
                    },
                    None,
                    Some(BASE64.encode(bytes)),
                    None,
                )
                .await?;
            print_result(&result, args.json)?;
        }
        Command::SerialClose {
            target,
            serial_session_id,
        } => {
            let result = client
                .execute(
                    &target,
                    EventSource::Human,
                    RemoteOperation::CloseSerial { serial_session_id },
                    None,
                    None,
                    None,
                )
                .await?;
            print_result(&result, args.json)?;
        }
        Command::Ssh {
            target,
            host,
            port,
            identity_file,
            known_hosts_file,
            readonly,
            username,
            command,
        } => {
            let result = client
                .execute(
                    &target,
                    EventSource::Human,
                    RemoteOperation::RunSsh {
                        host,
                        port,
                        username,
                        identity_file,
                        known_hosts_file,
                        command,
                        readonly,
                    },
                    None,
                    None,
                    None,
                )
                .await?;
            print_result(&result, args.json)?;
        }
        Command::Takeover { target } => {
            let result = client.human_takeover(&target).await?;
            print_result(&result, args.json)?;
        }
        Command::ReleaseTakeover { target } => {
            let result = client.release_human_takeover(&target).await?;
            print_result(&result, args.json)?;
        }
        Command::Events {
            target,
            after_sequence,
            limit,
        } => {
            let session_id = client.resolve_target(&target).await?;
            let events = client.read_events(session_id, after_sequence, limit).await;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&events)?);
            } else {
                for event in events {
                    println!(
                        "{} | {} | {:?} | {:?}",
                        event.sequence, event.occurred_at, event.source, event.payload
                    );
                }
            }
        }
        Command::Cancel { target, request_id } => {
            let result = client.cancel(&target, request_id).await?;
            print_result(&result, args.json)?;
        }
        Command::CancelAfter {
            target,
            shell,
            delay_millis,
            command,
        } => {
            let pending = client
                .start_execute(
                    &target,
                    EventSource::Ai,
                    RemoteOperation::RunCommand {
                        shell: shell.into(),
                        command,
                        readonly: true,
                    },
                    None,
                    None,
                    None,
                )
                .await?;
            let request_id = pending.request_id;
            tokio::time::sleep(std::time::Duration::from_millis(delay_millis.max(1))).await;
            let cancel = client.cancel(&target, request_id).await?;
            let cancelled = matches!(
                pending.wait().await,
                Err(ApplicationError::Remote { ref code, .. }) if code == "cancelled"
            );
            if !cancelled {
                bail!("目标请求没有以 cancelled 结束");
            }
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "request_id": request_id,
                        "cancel_request_id": cancel.response.request_id,
                        "cancelled": true,
                    }))?
                );
            } else {
                println!(
                    "请求 {request_id} 已由人工中断；取消请求 {} 已确认",
                    cancel.response.request_id
                );
            }
        }
        Command::Close { target } => {
            let result = client.close_connection(&target).await?;
            print_result(&result, args.json)?;
        }
        Command::EmergencyStop { target } => {
            let result = client.emergency_stop(&target).await?;
            print_result(&result, args.json)?;
        }
        Command::ApprovalDecide {
            target,
            approval_id,
            operation_json,
            reject,
        } => {
            let session_id = match target.parse::<SessionId>() {
                Ok(session_id) => session_id,
                Err(_) => client.resolve_target(&target).await?,
            };
            let operation = serde_json::from_str::<RemoteOperation>(&operation_json)
                .context("operation-json 不是有效的 RemoteOperation JSON")?;
            let result = client
                .decide_approval(session_id, operation, approval_id, !reject)
                .await?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "审批 {}：{:?}，{}",
                    result
                        .approval_id
                        .map_or_else(|| "无".to_owned(), |value| value.to_string()),
                    result.state,
                    result.reason
                );
            }
        }
        Command::AuditExport { target, output } => {
            let session_id = client.resolve_target(&target).await?;
            let count = client.export_audit(session_id, &output)?;
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "session_id": session_id,
                        "output": output,
                        "event_count": count,
                    })
                );
            } else {
                println!("已导出 {count} 条审计事件到 {}", output.display());
            }
        }
        Command::Interactive => interactive(client).await?,
    }
    Ok(())
}

async fn pair_all(client: &RelayClient, pairs: &[PairSpec]) -> anyhow::Result<()> {
    for pair in pairs {
        let connection = client.pair(pair.code.clone()).await?;
        if let Some(alias) = &pair.alias {
            client
                .set_alias(&connection.session_id.to_string(), alias.clone())
                .await?;
        }
    }
    Ok(())
}

async fn print_connections(client: &RelayClient, json: bool) -> anyhow::Result<()> {
    let connections = client.list_connections().await;
    if json {
        println!("{}", serde_json::to_string_pretty(&connections)?);
    } else if connections.is_empty() {
        println!("当前没有连接");
    } else {
        for connection in connections {
            println!(
                "{} | {} | {} | {:?} | {}",
                connection.display_name(),
                connection.session_id,
                connection.hostname,
                connection.state,
                connection.operating_system
            );
        }
    }
    Ok(())
}

fn print_result(result: &OperationResult, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&result.response)?);
    } else {
        println!(
            "请求 {}，退出码 {:?}",
            result.response.request_id, result.response.exit_code
        );
        if !result.response.summary.trim().is_empty() {
            println!("{}", result.response.summary.trim_end());
        }
        if let Some(details) = &result.response.details {
            println!("{}", serde_json::to_string_pretty(details)?);
        }
        if let Some(hash) = &result.response.sha256 {
            println!("SHA-256：{hash}");
        }
    }
    Ok(())
}

async fn execute_with_approval(
    client: &RelayClient,
    target: &str,
    source: EventSource,
    operation: RemoteOperation,
    auto_approve: bool,
    payload_base64: Option<String>,
) -> Result<OperationResult, ApplicationError> {
    let approval = client.request_approval(target, operation.clone()).await?;
    match approval.state {
        ApprovalState::NotRequired => {
            client
                .execute(target, source, operation, None, payload_base64, None)
                .await
        }
        ApprovalState::Pending if auto_approve => {
            let approval_id = approval
                .approval_id
                .ok_or(ApplicationError::ApprovalNotGranted)?;
            eprintln!("人工审批：{approval_id}，原因：{}", approval.reason);
            let session_id = client.resolve_target(target).await?;
            let decision = client
                .decide_approval(session_id, operation.clone(), approval_id, true)
                .await?;
            if decision.state != ApprovalState::Approved {
                return Err(ApplicationError::ApprovalNotGranted);
            }
            client
                .execute(
                    target,
                    source,
                    operation,
                    Some(approval_id),
                    payload_base64,
                    None,
                )
                .await
        }
        ApprovalState::Pending => Err(ApplicationError::ApprovalRequired {
            approval_id: approval
                .approval_id
                .ok_or(ApplicationError::ApprovalNotGranted)?,
            reason: approval.reason,
        }),
        ApprovalState::Rejected => Err(ApplicationError::PolicyDenied(approval.reason)),
        ApprovalState::Expired | ApprovalState::Approved => {
            Err(ApplicationError::ApprovalNotGranted)
        }
    }
}

fn spawn_event_printer(client: RelayClient, json: bool) {
    let mut receiver = client.subscribe_events();
    tokio::spawn(async move {
        while let Ok(event) = receiver.recv().await {
            if json {
                eprintln!(
                    "{}",
                    serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_owned())
                );
                continue;
            }
            let target = client
                .list_connections()
                .await
                .into_iter()
                .find(|connection| connection.session_id == event.session_id)
                .map_or_else(
                    || event.session_id.to_string(),
                    |connection| connection.display_name(),
                );
            match event.payload {
                EventPayload::OutputChunk { stderr, text } => {
                    eprintln!(
                        "[{target}][{}] {}",
                        if stderr { "stderr" } else { "stdout" },
                        text.trim_end()
                    );
                }
                payload => {
                    eprintln!(
                        "[{target}][{:?}][{:?}] {:?}",
                        event.source, event.approval, payload
                    );
                }
            }
        }
    });
}

#[allow(clippy::too_many_lines)]
async fn interactive(client: RelayClient) -> anyhow::Result<()> {
    println!(
        "交互模式命令：list、alias、cmd、powershell、pwsh、shell-open、shell-run、shell-close、port、serial-list、events、cancel、takeover、release-takeover、close、quit"
    );
    loop {
        print!("remoteops> ");
        io::stdout().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            break;
        }
        let parts = split_command_line(line.trim());
        let Some(command) = parts.first().map(String::as_str) else {
            continue;
        };
        match command {
            "quit" | "exit" => break,
            "list" => print_connections(&client, false).await?,
            "alias" if parts.len() >= 3 => {
                client.set_alias(&parts[1], parts[2..].join(" ")).await?;
            }
            "cmd" | "powershell" | "pwsh" if parts.len() >= 3 => {
                let shell = match command {
                    "cmd" => ShellKind::Cmd,
                    "powershell" => ShellKind::WindowsPowerShell,
                    _ => ShellKind::PowerShell,
                };
                let operation = RemoteOperation::RunCommand {
                    shell,
                    command: parts[2..].join(" "),
                    readonly: false,
                };
                match execute_with_approval(
                    &client,
                    &parts[1],
                    EventSource::Human,
                    operation,
                    false,
                    None,
                )
                .await
                {
                    Ok(result) => print_result(&result, false)?,
                    Err(ApplicationError::ApprovalRequired {
                        approval_id,
                        reason,
                    }) => {
                        println!("需要审批：{approval_id}，{reason}");
                        println!("请改用一次性命令并添加 --approve 明确批准。");
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            "shell-open" if parts.len() == 3 => {
                let shell = parse_shell_name(&parts[2])?;
                let result = client
                    .execute(
                        &parts[1],
                        EventSource::Human,
                        RemoteOperation::OpenShell { shell },
                        None,
                        None,
                        None,
                    )
                    .await?;
                print_result(&result, false)?;
            }
            "shell-run" if parts.len() >= 5 => {
                let shell_id = parts[2].parse::<ShellId>()?;
                let shell = parse_shell_name(&parts[3])?;
                let result = client
                    .execute(
                        &parts[1],
                        EventSource::Human,
                        RemoteOperation::RunShellCommand {
                            shell_id,
                            shell,
                            command: parts[4..].join(" "),
                            readonly: false,
                        },
                        None,
                        None,
                        None,
                    )
                    .await?;
                print_result(&result, false)?;
            }
            "shell-close" if parts.len() == 3 => {
                let shell_id = parts[2].parse::<ShellId>()?;
                let result = client
                    .execute(
                        &parts[1],
                        EventSource::Human,
                        RemoteOperation::CloseShell { shell_id },
                        None,
                        None,
                        None,
                    )
                    .await?;
                print_result(&result, false)?;
            }
            "port" if parts.len() == 4 => {
                let port = parts[3].parse::<u16>().context("端口必须是数字")?;
                let result = client
                    .execute(
                        &parts[1],
                        EventSource::Human,
                        RemoteOperation::TestPort {
                            host: parts[2].clone(),
                            port,
                        },
                        None,
                        None,
                        None,
                    )
                    .await?;
                print_result(&result, false)?;
            }
            "serial-list" if parts.len() == 2 => {
                let result = client
                    .execute(
                        &parts[1],
                        EventSource::Human,
                        RemoteOperation::ListSerial,
                        None,
                        None,
                        None,
                    )
                    .await?;
                print_result(&result, false)?;
            }
            "takeover" if parts.len() == 2 => {
                let result = client.human_takeover(&parts[1]).await?;
                print_result(&result, false)?;
            }
            "release-takeover" if parts.len() == 2 => {
                let result = client.release_human_takeover(&parts[1]).await?;
                print_result(&result, false)?;
            }
            "events" if parts.len() == 2 => {
                let session_id = client.resolve_target(&parts[1]).await?;
                for event in client.read_events(session_id, None, 100).await {
                    println!(
                        "{} | {} | {:?} | {:?}",
                        event.sequence, event.occurred_at, event.source, event.payload
                    );
                }
            }
            "cancel" if parts.len() == 3 => {
                let request_id = parts[2].parse::<RequestId>()?;
                let result = client.cancel(&parts[1], request_id).await?;
                print_result(&result, false)?;
            }
            "close" if parts.len() == 2 => {
                let result = client.close_connection(&parts[1]).await?;
                print_result(&result, false)?;
            }
            _ => println!("命令格式不正确"),
        }
    }
    Ok(())
}

fn parse_pair_spec(value: &str) -> Result<PairSpec, String> {
    let (code, alias) = value
        .split_once('=')
        .map_or((value, None), |(code, alias)| {
            (code, Some(alias.to_owned()))
        });
    let code = PairingCode::parse(code).map_err(|error| error.to_string())?;
    Ok(PairSpec { code, alias })
}

fn split_command_line(line: &str) -> Vec<String> {
    line.split_whitespace().map(ToOwned::to_owned).collect()
}

fn parse_shell_name(value: &str) -> anyhow::Result<ShellKind> {
    match value.to_ascii_lowercase().as_str() {
        "cmd" => Ok(ShellKind::Cmd),
        "powershell" | "windows-powershell" => Ok(ShellKind::WindowsPowerShell),
        "pwsh" => Ok(ShellKind::PowerShell),
        "system" => Ok(ShellKind::System),
        _ => bail!("Shell 必须是 cmd、powershell、pwsh 或 system"),
    }
}

async fn sha256_local_file(path: &std::path::Path) -> anyhow::Result<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("打开本地文件失败：{}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; FILE_CHUNK_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .with_context(|| format!("读取本地文件失败：{}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

async fn create_download_file(
    destination: &std::path::Path,
) -> anyhow::Result<(PathBuf, tokio::fs::File)> {
    let parent = destination.parent().context("下载目标缺少父目录")?;
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    for _ in 0..100 {
        let temporary = parent.join(format!(
            ".{file_name}.remoteops-{}.part",
            FileTransferId::new()
        ));
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("创建下载临时文件失败"),
        }
    }
    bail!("无法创建唯一的下载临时文件")
}

async fn commit_download_file(
    temporary: &std::path::Path,
    destination: &std::path::Path,
    overwrite: bool,
) -> anyhow::Result<()> {
    let temporary = temporary.to_path_buf();
    let destination = destination.to_path_buf();
    let cleanup = temporary.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        if !destination.exists() {
            std::fs::rename(&temporary, &destination)
                .with_context(|| format!("提交下载文件失败：{}", destination.display()))?;
            return Ok(());
        }
        if !overwrite {
            bail!("本地目标文件已存在，未设置 --overwrite");
        }
        let metadata = std::fs::symlink_metadata(&destination)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("下载目标必须是非链接普通文件");
        }
        let parent = destination.parent().context("下载目标缺少父目录")?;
        let file_name = destination
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("download");
        let backup = parent.join(format!(
            ".{file_name}.remoteops-{}.backup",
            FileTransferId::new()
        ));
        std::fs::rename(&destination, &backup).context("备份本地原文件失败")?;
        if let Err(error) = std::fs::rename(&temporary, &destination) {
            let restore_error = std::fs::rename(&backup, &destination).err();
            match restore_error {
                Some(restore_error) => {
                    bail!("提交下载文件失败：{error}；恢复原文件也失败：{restore_error}")
                }
                None => bail!("提交下载文件失败，已恢复原文件：{error}"),
            }
        }
        std::fs::remove_file(&backup).context("下载已提交，但清理原文件备份失败")?;
        Ok(())
    })
    .await
    .context("提交下载文件任务失败")?;
    if result.is_err() {
        let _ = tokio::fs::remove_file(cleanup).await;
    }
    result
}

#[allow(dead_code)]
fn parse_approval_id(value: &str) -> anyhow::Result<ApprovalId> {
    ApprovalId::from_str(value).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ca_uses_system_trust_by_default() {
        let args = Args::try_parse_from([
            "remoteops-controller-cli",
            "--relay",
            "relay.example.com:7443",
            "--server-name",
            "relay.example.com",
            "--controller-token",
            "01234567890123456789012345678901",
            "--owner-id",
            "00000000-0000-0000-0000-000000000001",
            "list",
        ])
        .expect("公网 CA 场景不应要求 --ca-cert");

        assert!(args.ca_cert.is_none());
    }

    #[test]
    fn explicit_private_ca_remains_supported() {
        let args = Args::try_parse_from([
            "remoteops-controller-cli",
            "--relay",
            "relay.example.com:7443",
            "--server-name",
            "relay.example.com",
            "--ca-cert",
            "relay-cert.pem",
            "--controller-token",
            "01234567890123456789012345678901",
            "--owner-id",
            "00000000-0000-0000-0000-000000000001",
            "list",
        ])
        .expect("私有 CA 参数应继续支持");

        assert_eq!(args.ca_cert, Some(PathBuf::from("relay-cert.pem")));
    }

    #[tokio::test]
    async fn download_commit_requires_explicit_overwrite_and_preserves_original_on_failure() {
        let path = std::env::temp_dir().join(format!(
            "remoteops-download-{}.txt",
            remoteops_domain::RequestId::new()
        ));
        tokio::fs::write(&path, b"existing")
            .await
            .expect("应创建测试文件");

        let replacement = b"replacement";
        let (temporary, mut file) = create_download_file(&path).await.expect("应创建临时文件");
        file.write_all(replacement).await.expect("应写入临时文件");
        drop(file);
        assert!(
            commit_download_file(&temporary, &path, false)
                .await
                .is_err()
        );
        assert_eq!(
            tokio::fs::read(&path).await.expect("应读取原文件"),
            b"existing"
        );
        let (temporary, mut file) = create_download_file(&path)
            .await
            .expect("应创建覆盖临时文件");
        file.write_all(replacement)
            .await
            .expect("应写入覆盖临时文件");
        file.sync_all().await.expect("应同步覆盖临时文件");
        drop(file);
        commit_download_file(&temporary, &path, true)
            .await
            .expect("显式覆盖应成功");
        assert_eq!(
            tokio::fs::read(&path).await.expect("应读取替换文件"),
            replacement
        );
        tokio::fs::remove_file(path).await.expect("应清理测试文件");
    }
}

