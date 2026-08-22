#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use clap::{Parser, ValueEnum};
use remoteops_application::{
    ApplicationError, ControllerKind, OperationResult, RelayClient, RelayClientConfig,
};
use remoteops_audit::sha256_bytes;
use remoteops_device::SshCredentialStore;
use remoteops_domain::{
    ApprovalId, Capability, ControllerInstanceId, ControllerOwnerId, EventSource, FileTransferId,
    PairingCode, PermissionMode, PowerAction, RemoteOperation, SerialDataBits, SerialFlowControl,
    SerialLineEnding, SerialParity, SerialSettings, SerialStopBits, SerialTerminalProfile,
    ServiceAction, SessionId, ShellId, ShellKind,
};
use rmcp::{
    Json, RoleServer, ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{
        BooleanSchema, ElicitRequestParams, ElicitationAction, ElicitationSchema,
        PrimitiveSchemaDefinition,
    },
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use tracing_subscriber::EnvFilter;

/// `RemoteOps` 本地 STDIO MCP Server。
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// MCP JSON 配置文件；默认读取 `Codex` 用户目录下的 `RemoteOps` 配置。
    #[arg(long, env = "REMOTEOPS_MCP_CONFIG")]
    config: Option<PathBuf>,
    /// Relay TLS 地址。
    #[arg(long, env = "REMOTEOPS_RELAY")]
    relay: Option<String>,
    /// Relay 证书中的 DNS 名称或 IP。
    #[arg(long, env = "REMOTEOPS_SERVER_NAME")]
    server_name: Option<String>,
    /// Relay 自签名 PEM 证书；不提供时使用操作系统可信根。
    #[arg(long, env = "REMOTEOPS_CA_CERT")]
    ca_cert: Option<PathBuf>,
    /// 已由本地用户确认的 Relay 叶证书 SHA-256 指纹。
    #[arg(long, env = "REMOTEOPS_TLS_FINGERPRINT")]
    tls_fingerprint: Option<String>,
    /// Relay 断开后的自动重连间隔。
    #[arg(long, env = "REMOTEOPS_RECONNECT_SECONDS")]
    reconnect_seconds: Option<u64>,
    /// Controller 本地脱敏审计 JSONL 文件。
    #[arg(
        long,
        env = "REMOTEOPS_AUDIT_LOG",
        default_value_os_t = remoteops_audit::default_audit_log_path()
    )]
    audit_log: PathBuf,
    /// MCP 允许读取和写入的控制端文件根目录。
    #[arg(
        long,
        env = "REMOTEOPS_TRANSFER_ROOT",
        default_value_os_t = default_transfer_root_path()
    )]
    transfer_root: PathBuf,
    /// Relay 为 AI Controller 独立配置的认证令牌。
    #[arg(long, env = "REMOTEOPS_CONTROLLER_TOKEN", hide_env_values = true)]
    controller_token: Option<String>,
    /// Human 与 AI Controller 共同使用的稳定 Owner ID。
    #[arg(long, env = "REMOTEOPS_CONTROLLER_OWNER_ID")]
    owner_id: Option<ControllerOwnerId>,
    /// 远程命令模式；默认跟随 Agent 本地权限选择。
    #[arg(
        long,
        env = "REMOTEOPS_COMMAND_MODE",
        value_enum,
        default_value = "agent-controlled"
    )]
    command_mode: CommandMode,
    /// 启动时配对，格式 CODE 或 CODE=别名；环境变量用分号分隔。
    #[arg(
        long = "pair",
        env = "REMOTEOPS_PAIRINGS",
        value_delimiter = ';',
        value_parser = parse_pair_spec
    )]
    pairs: Vec<PairSpec>,
}

/// 可以安全写入普通 JSON 文件的 MCP 连接配置。
#[derive(Clone, Debug, Default, Deserialize)]
struct McpFileConfig {
    /// Relay TLS 地址，例如 `relay.example.com:7443`。
    relay: Option<String>,
    /// TLS 证书中的服务名或 IP；省略时从 Relay 地址推导。
    server_name: Option<String>,
    /// 可选的自签名 CA 证书路径。
    ca_cert: Option<PathBuf>,
    /// 已由本地用户确认的 Relay 叶证书 SHA-256 指纹。
    tls_fingerprint: Option<String>,
    /// Relay 断开后的自动重连间隔。
    reconnect_seconds: Option<u64>,
    /// Human 与 AI Controller 共同使用的稳定 Owner ID；不是凭据。
    owner_id: Option<ControllerOwnerId>,
}

#[derive(Debug)]
struct ResolvedArgs {
    relay: String,
    server_name: String,
    ca_cert: Option<PathBuf>,
    tls_fingerprint: Option<String>,
    audit_log: PathBuf,
    transfer_root: PathBuf,
    controller_token: String,
    owner_id: ControllerOwnerId,
    permission_mode: PermissionMode,
    command_mode: CommandMode,
    pairs: Vec<PairSpec>,
    reconnect_seconds: u64,
}

impl Args {
    fn resolve(self) -> anyhow::Result<ResolvedArgs> {
        let explicit_config = self.config.is_some();
        let config_path = self.config.unwrap_or_else(default_mcp_config_path);
        let config_exists = config_path.is_file();
        let file_config = if config_exists {
            let text = fs::read_to_string(&config_path)
                .with_context(|| format!("无法读取 MCP 配置文件 {}", config_path.display()))?;
            serde_json::from_str::<McpFileConfig>(&text)
                .with_context(|| format!("MCP 配置文件格式无效：{}", config_path.display()))?
        } else {
            McpFileConfig::default()
        };
        let base_directory = config_path.parent().unwrap_or_else(|| Path::new("."));
        let relay = self
            .relay
            .or(file_config.relay)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let controller_token = self
            .controller_token
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let mut missing = Vec::new();
        if explicit_config && !config_exists {
            missing.push("指定的 MCP 配置文件不存在");
        }
        if relay.is_none() {
            missing.push("Relay 地址：配置 relay、--relay 或 REMOTEOPS_RELAY");
        }
        if controller_token.is_none() {
            missing.push("AI Controller Token：设置 REMOTEOPS_CONTROLLER_TOKEN");
        }
        let owner_id = self.owner_id.or(file_config.owner_id);
        if owner_id.is_none() {
            missing.push(
                "Controller Owner ID：配置 owner_id、--owner-id 或 REMOTEOPS_CONTROLLER_OWNER_ID",
            );
        }
        if !missing.is_empty() {
            bail!(mcp_missing_configuration_message(&config_path, &missing));
        }
        let relay = relay.expect("缺失 Relay 已在前面返回错误");
        let controller_token = controller_token.expect("缺失 Token 已在前面返回错误");
        let owner_id = owner_id.expect("缺失 Owner 已在前面返回错误");
        let server_name = self
            .server_name
            .or(file_config.server_name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map_or_else(|| infer_server_name(&relay), Ok)?;
        let ca_cert = self.ca_cert.or(file_config.ca_cert.map(|path| {
            if path.is_absolute() {
                path
            } else {
                base_directory.join(path)
            }
        }));
        let tls_fingerprint = self.tls_fingerprint.or(file_config.tls_fingerprint);
        let reconnect_seconds = self
            .reconnect_seconds
            .or(file_config.reconnect_seconds)
            .unwrap_or(2);
        if reconnect_seconds == 0 {
            bail!("reconnect_seconds 必须大于 0");
        }
        Ok(ResolvedArgs {
            relay,
            server_name,
            ca_cert,
            tls_fingerprint,
            audit_log: self.audit_log,
            transfer_root: self.transfer_root,
            controller_token,
            owner_id,
            permission_mode: self.command_mode.into(),
            command_mode: self.command_mode,
            pairs: self.pairs,
            reconnect_seconds,
        })
    }
}

fn mcp_missing_configuration_message(config_path: &Path, missing: &[&str]) -> String {
    format!(
        "RemoteOps MCP 配置不完整。\n配置文件：{}\n缺少：\n- {}\n处理方法：运行 MCP 安装脚本，或补齐上述配置文件、命令行参数和用户环境变量。设置环境变量后请完全退出并重新打开 Codex。",
        config_path.display(),
        missing.join("\n- ")
    )
}

fn default_mcp_config_path() -> PathBuf {
    if let Some(root) = env::var_os("CODEX_HOME") {
        return PathBuf::from(root)
            .join("remoteops")
            .join("controller-config.json");
    }
    if let Some(root) = env::var_os("USERPROFILE") {
        return PathBuf::from(root)
            .join(".codex")
            .join("remoteops")
            .join("controller-config.json");
    }
    env::var_os("HOME").map_or_else(
        || {
            PathBuf::from(".codex")
                .join("remoteops")
                .join("controller-config.json")
        },
        |root| codex_config_path_from_home(Path::new(&root)),
    )
}

fn codex_config_path_from_home(home: &Path) -> PathBuf {
    home.join(".codex")
        .join("remoteops")
        .join("controller-config.json")
}

fn infer_server_name(relay: &str) -> anyhow::Result<String> {
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

/// MCP 远程命令执行模式。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum CommandMode {
    /// 只开放只读诊断命令。
    Readonly,
    /// 开放经独立人工逐条审批的非只读命令。
    Approval,
    /// 由 Agent 本地权限选择决定是否逐项审批。
    #[default]
    AgentControlled,
    /// 由可信 Human Owner 显式授权后执行，不逐项申请审批。
    FullAccess,
}

impl From<CommandMode> for PermissionMode {
    fn from(value: CommandMode) -> Self {
        match value {
            CommandMode::Readonly => Self::ReadOnly,
            CommandMode::Approval => Self::ApprovalRequired,
            CommandMode::AgentControlled => Self::ControllerApproved,
            CommandMode::FullAccess => Self::FullAccess,
        }
    }
}

#[derive(Clone, Debug)]
struct PairSpec {
    code: PairingCode,
    alias: Option<String>,
}

#[derive(Clone)]
struct RemoteOpsMcp {
    client: RelayClient,
    shells: Arc<Mutex<BTreeMap<ShellId, ShellHandle>>>,
    transfer_root: Arc<PathBuf>,
    command_mode: CommandMode,
    full_access_grants: Arc<Mutex<BTreeMap<SessionId, FullAccessGrant>>>,
    ssh_credentials: SshCredentialStore,
}

const FULL_ACCESS_IDLE_TIMEOUT: Duration = Duration::from_hours(1);
const ELICITATION_TIMEOUT: Duration = Duration::from_mins(2);
const FILE_CHUNK_BYTES: usize = 1024 * 1024;
const LARGE_TRANSFER_CONFIRM_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_TRANSFER_BYTES: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
struct FullAccessGrant {
    last_successful_use: Instant,
}

impl FullAccessGrant {
    fn is_active_at(self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_successful_use) < FULL_ACCESS_IDLE_TIMEOUT
    }
}

#[derive(Clone, Copy, Debug)]
struct WriteAuthorization {
    approval_id: Option<ApprovalId>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum McpControlMode {
    StepByStep,
    FullAccess,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SetControlModeInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// `step_by_step` 为逐项确认，`full_access` 为空闲一小时的完全控制。
    mode: McpControlMode,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ControlModeOutput {
    session_id: String,
    mode: String,
    idle_timeout_seconds: Option<u64>,
    message: String,
}

#[derive(Clone)]
struct ShellHandle {
    session_id: SessionId,
    shell: ShellKind,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TargetInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PairConnectionInput {
    /// Agent 窗口显示的九位临时配对码。
    pairing_code: String,
    /// 可选展示别名，例如“客户 A”。
    alias: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RunReadonlyCommandInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// 用于本次命令的一次性 Shell。
    shell: Option<McpShell>,
    /// 只读诊断命令。策略检测到修改行为时会拒绝。
    command: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RunCommandInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// `open_shell` 返回的 `shell_id`；与 `shell` 二选一。
    shell_id: Option<String>,
    /// 未使用 `shell_id` 时指定 Shell。
    shell: Option<McpShell>,
    /// 经独立人工审批的精确命令。
    command: String,
    /// 由独立人工 Controller 批准的一次性审批标识。
    approval_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum McpShell {
    Cmd,
    WindowsPowerShell,
    PowerShell,
    System,
}

impl From<McpShell> for ShellKind {
    fn from(value: McpShell) -> Self {
        match value {
            McpShell::Cmd => Self::Cmd,
            McpShell::WindowsPowerShell => Self::WindowsPowerShell,
            McpShell::PowerShell => Self::PowerShell,
            McpShell::System => Self::System,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct OpenShellInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// 要使用的 Shell。
    shell: McpShell,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CloseShellInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// `open_shell` 返回的持久 Shell 标识。
    shell_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PortInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// 从 Agent 所在网络访问的目标主机。
    host: String,
    /// TCP 端口。
    port: u16,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RemotePathInput {
    session_id: String,
    remote_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MutatingRemotePathInput {
    session_id: String,
    remote_path: String,
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MoveFileInput {
    session_id: String,
    source_path: String,
    destination_path: String,
    overwrite: bool,
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TcpExchangeInput {
    session_id: String,
    host: String,
    port: u16,
    data_base64: String,
    max_response_bytes: Option<usize>,
    timeout_millis: Option<u64>,
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[allow(clippy::struct_field_names)]
struct ProcessInput {
    session_id: String,
    process_id: u32,
    approval_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ServiceActionInput {
    Start,
    Stop,
    Restart,
}

impl From<ServiceActionInput> for ServiceAction {
    fn from(value: ServiceActionInput) -> Self {
        match value {
            ServiceActionInput::Start => Self::Start,
            ServiceActionInput::Stop => Self::Stop,
            ServiceActionInput::Restart => Self::Restart,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ServiceInput {
    session_id: String,
    service_name: String,
    action: ServiceActionInput,
    approval_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PowerActionInput {
    Restart,
    Shutdown,
}

impl From<PowerActionInput> for PowerAction {
    fn from(value: PowerActionInput) -> Self {
        match value {
            PowerActionInput::Restart => Self::Restart,
            PowerActionInput::Shutdown => Self::Shutdown,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PowerInput {
    session_id: String,
    action: PowerActionInput,
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReadOutputInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// 只返回大于该序号的事件。
    after_sequence: Option<u64>,
    /// 最大事件数，默认 100，最大 500。
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct OpenSerialInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// Windows COM 口或平台串口名。
    port_name: String,
    /// 波特率。
    baud_rate: u32,
    /// 数据位；省略时使用 8。
    data_bits: Option<SerialDataBitsInput>,
    /// 停止位；省略时使用 1。
    stop_bits: Option<SerialStopBitsInput>,
    /// 校验位；省略时不校验。
    parity: Option<SerialParityInput>,
    /// 流控方式；省略时不使用流控。
    flow_control: Option<SerialFlowControlInput>,
    /// AI 默认应为 false；写入需要审批。
    writable: bool,
    /// 由独立人工 Controller 批准的一次性审批标识。
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SerialSessionInput {
    session_id: String,
    serial_session_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WriteSerialInput {
    session_id: String,
    serial_session_id: String,
    data_base64: String,
    approval_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SerialLineEndingInput {
    None,
    Cr,
    Lf,
    CrLf,
}

impl From<SerialLineEndingInput> for SerialLineEnding {
    fn from(value: SerialLineEndingInput) -> Self {
        match value {
            SerialLineEndingInput::None => Self::None,
            SerialLineEndingInput::Cr => Self::Cr,
            SerialLineEndingInput::Lf => Self::Lf,
            SerialLineEndingInput::CrLf => Self::CrLf,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SerialProfileInput {
    Ansi,
    HuaweiVrp,
}

impl From<SerialProfileInput> for SerialTerminalProfile {
    fn from(value: SerialProfileInput) -> Self {
        match value {
            SerialProfileInput::Ansi => Self::Ansi,
            SerialProfileInput::HuaweiVrp => Self::HuaweiVrp,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RunSerialQueryInput {
    session_id: String,
    serial_session_id: String,
    command: String,
    line_ending: Option<SerialLineEndingInput>,
    profile: Option<SerialProfileInput>,
    overall_timeout_millis: Option<u64>,
    idle_timeout_millis: Option<u64>,
    max_bytes: Option<usize>,
    max_pages: Option<u16>,
    readonly: bool,
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SshInput {
    session_id: String,
    host: String,
    port: Option<u16>,
    username: String,
    identity_file: Option<String>,
    known_hosts_file: Option<String>,
    command: String,
    readonly: bool,
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProvisionSshCredentialInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// SSH 目标主机。
    host: String,
    /// SSH 端口，省略时为 22。
    port: Option<u16>,
    /// SSH 用户名。
    username: String,
    /// 控制端本地 DPAPI 凭据库引用，格式为 `ssh://用户名@主机:端口`。
    credential_ref: String,
    /// 完全控制模式下不需要；逐项确认模式可携带一次性审批标识。
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UploadInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// `transfer-root` 内的相对文件路径。
    local_path: String,
    /// Agent 目标文件路径。
    remote_path: String,
    /// 是否覆盖现有文件。
    overwrite: bool,
    /// 由独立人工 Controller 批准的一次性审批标识。
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DownloadInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// Agent 文件路径。
    remote_path: String,
    /// `transfer-root` 内的相对保存路径。
    local_path: String,
    /// 是否覆盖本地文件。
    overwrite_local: bool,
    /// 覆盖本地文件时，由独立人工 Controller 批准的一次性审批标识。
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApprovalRequestInput {
    /// `list_connections` 返回的不可变 `session_id`。
    session_id: String,
    /// 需要人工审批的精确动作及参数。
    action: ApprovalActionInput,
}

/// MCP 可以申请但不能自行决定的动作。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ApprovalActionInput {
    /// 申请执行一条非只读命令。
    RunCommand {
        /// `open_shell` 返回的 `shell_id`；与 `shell` 二选一。
        shell_id: Option<String>,
        /// 未使用 `shell_id` 时指定 Shell。
        shell: Option<McpShell>,
        /// 需要审批的精确命令。
        command: String,
    },
    /// 申请打开可写或只读串口。
    OpenSerial {
        /// Windows COM 口或平台串口名。
        port_name: String,
        /// 波特率。
        baud_rate: u32,
        /// 数据位；省略时使用 8。
        data_bits: Option<SerialDataBitsInput>,
        /// 停止位；省略时使用 1。
        stop_bits: Option<SerialStopBitsInput>,
        /// 校验位；省略时不校验。
        parity: Option<SerialParityInput>,
        /// 流控方式；省略时不使用流控。
        flow_control: Option<SerialFlowControlInput>,
        /// 是否申请写入能力。
        writable: bool,
    },
    /// 申请上传控制端受控目录中的文件。
    UploadFile {
        /// `transfer-root` 内的相对文件路径。
        local_path: String,
        /// Agent 目标文件路径。
        remote_path: String,
        /// 是否覆盖现有文件。
        overwrite: bool,
    },
    /// 申请下载文件并覆盖控制端受控目录中的现有文件。
    DownloadFile {
        /// Agent 文件路径。
        remote_path: String,
        /// 必须为 true 才会触发本地覆盖审批。
        overwrite_local: bool,
    },
    /// 申请移动或重命名文件。
    MoveFile {
        source_path: String,
        destination_path: String,
        overwrite: bool,
    },
    /// 申请删除单个普通文件。
    DeleteFile { remote_path: String },
    /// 申请向指定 TCP 目标发送精确数据。
    TcpExchange {
        host: String,
        port: u16,
        data_base64: String,
        max_response_bytes: Option<usize>,
        timeout_millis: Option<u64>,
    },
    /// 申请写入串口原始数据。
    WriteSerial {
        serial_session_id: String,
        data_base64: String,
    },
    /// 申请执行串口查询。
    RunSerialQuery {
        serial_session_id: String,
        command: String,
        line_ending: Option<SerialLineEndingInput>,
        profile: Option<SerialProfileInput>,
        overall_timeout_millis: Option<u64>,
        idle_timeout_millis: Option<u64>,
        max_bytes: Option<usize>,
        max_pages: Option<u16>,
        readonly: bool,
    },
    /// 申请执行 SSH 命令。
    RunSsh {
        host: String,
        port: Option<u16>,
        username: String,
        identity_file: Option<String>,
        known_hosts_file: Option<String>,
        command: String,
        readonly: bool,
    },
    /// 申请终止进程。
    TerminateProcess { process_id: u32 },
    /// 申请控制服务。
    ControlService {
        service_name: String,
        action: ServiceActionInput,
    },
    /// 申请重启或关机。
    PowerControl { action: PowerActionInput },
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SerialDataBitsInput {
    Five,
    Six,
    Seven,
    Eight,
}

impl From<SerialDataBitsInput> for SerialDataBits {
    fn from(value: SerialDataBitsInput) -> Self {
        match value {
            SerialDataBitsInput::Five => Self::Five,
            SerialDataBitsInput::Six => Self::Six,
            SerialDataBitsInput::Seven => Self::Seven,
            SerialDataBitsInput::Eight => Self::Eight,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SerialStopBitsInput {
    One,
    Two,
}

impl From<SerialStopBitsInput> for SerialStopBits {
    fn from(value: SerialStopBitsInput) -> Self {
        match value {
            SerialStopBitsInput::One => Self::One,
            SerialStopBitsInput::Two => Self::Two,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SerialParityInput {
    None,
    Odd,
    Even,
}

impl From<SerialParityInput> for SerialParity {
    fn from(value: SerialParityInput) -> Self {
        match value {
            SerialParityInput::None => Self::None,
            SerialParityInput::Odd => Self::Odd,
            SerialParityInput::Even => Self::Even,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SerialFlowControlInput {
    None,
    Software,
    Hardware,
}

impl From<SerialFlowControlInput> for SerialFlowControl {
    fn from(value: SerialFlowControlInput) -> Self {
        match value {
            SerialFlowControlInput::None => Self::None,
            SerialFlowControlInput::Software => Self::Software,
            SerialFlowControlInput::Hardware => Self::Hardware,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct ConnectionOutput {
    display_name: String,
    alias: Option<String>,
    session_id: String,
    agent_instance_id: String,
    hostname: String,
    operating_system: String,
    environment: serde_json::Value,
    state: String,
    role: String,
    permission_mode: String,
    control_mode: String,
    transfer_root: String,
    capabilities: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ConnectionsOutput {
    connections: Vec<ConnectionOutput>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ShellOutput {
    session_id: String,
    shell_id: String,
    shell: String,
    message: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ActionOutput {
    status: String,
    session_id: String,
    request_id: Option<String>,
    exit_code: Option<i32>,
    summary: String,
    approval_id: Option<String>,
    sha256: Option<String>,
    details: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct EventsOutput {
    session_id: String,
    events: Vec<serde_json::Value>,
}

#[tool_router]
impl RemoteOpsMcp {
    fn new(
        client: RelayClient,
        transfer_root: PathBuf,
        command_mode: CommandMode,
        ssh_credentials: SshCredentialStore,
    ) -> Self {
        Self {
            client,
            shells: Arc::new(Mutex::new(BTreeMap::new())),
            transfer_root: Arc::new(transfer_root),
            command_mode,
            full_access_grants: Arc::new(Mutex::new(BTreeMap::new())),
            ssh_credentials,
        }
    }

    async fn ensure_connection_exists(&self, session_id: SessionId) -> Result<(), String> {
        self.client
            .list_connections()
            .await
            .into_iter()
            .any(|connection| connection.session_id == session_id)
            .then_some(())
            .ok_or_else(|| "未找到 session_id 对应的连接；请先调用 list_connections".to_owned())
    }

    async fn has_full_access(&self, session_id: SessionId) -> bool {
        let mut grants = self.full_access_grants.lock().await;
        let Some(grant) = grants.get(&session_id) else {
            return false;
        };
        if !grant.is_active_at(Instant::now()) {
            grants.remove(&session_id);
            return false;
        }
        true
    }

    async fn control_mode_name(&self, session_id: SessionId) -> &'static str {
        match self.command_mode {
            CommandMode::Readonly => "readonly",
            CommandMode::Approval => "external_approval",
            CommandMode::FullAccess => "full_access",
            CommandMode::AgentControlled if self.has_full_access(session_id).await => "full_access",
            CommandMode::AgentControlled => "step_by_step",
        }
    }

    async fn control_mode_output(&self, session_id: SessionId) -> ControlModeOutput {
        let mode = self.control_mode_name(session_id).await;
        let full_access = mode == "full_access";
        ControlModeOutput {
            session_id: session_id.to_string(),
            mode: mode.to_owned(),
            idle_timeout_seconds: (self.command_mode == CommandMode::AgentControlled
                && full_access)
                .then_some(FULL_ACCESS_IDLE_TIMEOUT.as_secs()),
            message: match mode {
                "readonly" => "MCP 启动参数固定为只读模式，拒绝修改操作".to_owned(),
                "external_approval" => {
                    "MCP 启动参数固定为外部审批模式，修改操作必须携带 Human Controller approval_id"
                        .to_owned()
                }
                "full_access" if self.command_mode == CommandMode::FullAccess => {
                    "MCP 启动参数固定为完全控制模式，不使用会话空闲过期".to_owned()
                }
                "full_access" => {
                    "完全控制已开启；每次成功的远程操作都会把空闲有效期刷新为一小时".to_owned()
                }
                _ => "当前为逐项确认；只读检查直接执行，修改操作前询问".to_owned(),
            },
        }
    }

    async fn elicit_confirmation(
        &self,
        context: &RequestContext<RoleServer>,
        message: &str,
        title: &'static str,
    ) -> Result<bool, String> {
        let schema = ElicitationSchema::builder()
            .required_property(
                "confirmed",
                PrimitiveSchemaDefinition::Boolean(
                    BooleanSchema::new()
                        .title(title)
                        .description(
                            "在当前 Codex 授权弹窗点击允许即表示确认；Agent 端没有授权按钮",
                        )
                        .with_default(true),
                ),
            )
            .build()
            .map_err(str::to_owned)?;
        let response = context
            .peer
            .create_elicitation_with_timeout(
                ElicitRequestParams::FormElicitationParams {
                    meta: None,
                    message: message.to_owned(),
                    requested_schema: schema,
                },
                Some(ELICITATION_TIMEOUT),
            )
            .await
            .map_err(|error| format!("当前 MCP 客户端无法完成人机确认：{error}"))?;
        Ok(elicitation_accepted(&response.action))
    }

    async fn confirm_large_transfer(
        &self,
        context: &RequestContext<RoleServer>,
        size: u64,
        direction: &str,
    ) -> Result<(), String> {
        if !large_transfer_requires_confirmation(size) {
            return Ok(());
        }
        let confirmed = self
            .elicit_confirmation(
                context,
                &format!(
                    "RemoteOps 将{direction} {size} 字节（超过 1 GiB）的文件。确认后才会开始读取、计算哈希或传输。是否继续？"
                ),
                "允许大文件传输",
            )
            .await?;
        if confirmed {
            Ok(())
        } else {
            Err(format!("当前 Codex 授权弹窗未允许{direction}大文件"))
        }
    }

    async fn authorize_upload(
        &self,
        context: &RequestContext<RoleServer>,
        session_id: SessionId,
        size: u64,
        approval_id: Option<String>,
    ) -> Result<WriteAuthorization, String> {
        if !large_transfer_requires_confirmation(size) {
            return self
                .authorize_mutation(context, session_id, "文件上传", approval_id)
                .await;
        }
        if self.command_mode == CommandMode::Readonly {
            return Err("当前 MCP 使用 readonly 模式，已拒绝文件上传".to_owned());
        }
        let authorization = if self.command_mode == CommandMode::Approval {
            self.authorize_mutation(context, session_id, "文件上传", approval_id)
                .await?
        } else {
            WriteAuthorization { approval_id: None }
        };
        self.confirm_large_transfer(context, size, "上传").await?;
        Ok(authorization)
    }

    async fn execute_raw(
        &self,
        session_id: SessionId,
        operation: RemoteOperation,
        approval_id: Option<ApprovalId>,
        payload_base64: Option<String>,
    ) -> Result<OperationResult, String> {
        let result = self
            .client
            .execute(
                &session_id.to_string(),
                EventSource::Ai,
                operation,
                approval_id,
                payload_base64,
                None,
            )
            .await
            .map_err(application_error)?;
        self.refresh_full_access_after_success(session_id, &result)
            .await;
        Ok(result)
    }

    async fn remote_file_metadata(
        &self,
        session_id: SessionId,
        remote_path: &str,
        include_sha256: bool,
    ) -> Result<(u64, Option<String>, OperationResult), String> {
        let result = self
            .execute_raw(
                session_id,
                RemoteOperation::GetFileMetadata {
                    remote_path: remote_path.to_owned(),
                    include_sha256,
                },
                None,
                None,
            )
            .await?;
        let size = result
            .response
            .details
            .as_ref()
            .and_then(|details| details.get("metadata"))
            .and_then(|metadata| metadata.get("size"))
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "Agent 未返回文件大小".to_owned())?;
        Ok((size, result.response.sha256.clone(), result))
    }

    async fn abort_remote_upload(&self, session_id: SessionId, transfer_id: FileTransferId) {
        let _ = self
            .client
            .execute(
                &session_id.to_string(),
                EventSource::Ai,
                RemoteOperation::AbortUploadFile { transfer_id },
                None,
                None,
                None,
            )
            .await;
    }

    async fn authorize_mutation(
        &self,
        context: &RequestContext<RoleServer>,
        session_id: SessionId,
        action: &str,
        legacy_approval_id: Option<String>,
    ) -> Result<WriteAuthorization, String> {
        if self.command_mode == CommandMode::Readonly {
            return Err("当前 MCP 使用 readonly 模式，已拒绝修改操作".to_owned());
        }
        if self.command_mode == CommandMode::Approval {
            let approval_id = parse_optional_approval(legacy_approval_id)?.ok_or_else(|| {
                format!("{action}必须提供独立 Human Controller 批准的 approval_id")
            })?;
            return Ok(WriteAuthorization {
                approval_id: Some(approval_id),
            });
        }
        if self.command_mode == CommandMode::FullAccess || self.has_full_access(session_id).await {
            return Ok(WriteAuthorization { approval_id: None });
        }
        let confirmed = self
            .elicit_confirmation(
                context,
                &format!("RemoteOps 将在当前 Agent 上执行：{action}。是否允许本次操作？"),
                "允许本次操作",
            )
            .await?;
        if !confirmed {
            return Err(format!(
                "当前 Codex 授权弹窗未允许本次操作：{action}；Agent 端没有授权按钮"
            ));
        }
        Ok(WriteAuthorization { approval_id: None })
    }

    async fn refresh_full_access_after_success(
        &self,
        session_id: SessionId,
        result: &OperationResult,
    ) {
        if result.response.error_code.is_some()
            || result
                .response
                .exit_code
                .is_some_and(|exit_code| exit_code != 0)
        {
            return;
        }
        if let Some(grant) = self.full_access_grants.lock().await.get_mut(&session_id) {
            grant.last_successful_use = Instant::now();
        }
    }

    /// 列出所有已配对连接及其不可变 `session_id`。
    #[tool(
        name = "list_connections",
        description = "列出 RemoteOps 当前连接。后续每个工具必须使用这里返回的不可变 session_id；不要根据别名猜测。",
        annotations(
            title = "列出远程连接",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_connections(&self) -> Json<ConnectionsOutput> {
        let mut connections = Vec::new();
        for connection in self.client.list_connections().await {
            let mut output = connection_output(connection);
            output.transfer_root = self.transfer_root.display().to_string();
            let session_id =
                parse_session_id(&output.session_id).expect("Relay 返回的 session_id 必须有效");
            output.control_mode = self.control_mode_name(session_id).await.to_owned();
            connections.push(output);
        }
        Json(ConnectionsOutput { connections })
    }

    /// 在 MCP 运行期间配对一个新 Agent。
    #[tool(
        name = "pair_connection",
        description = "当用户提供 RemoteOps 控制码、RemoteOps 配对码或 Agent 窗口显示的九位码（例如 123-456-789）时，必须先调用此工具建立远程连接。可同时设置别名；无需修改 MCP 启动参数或重启 Codex。不要改用远程桌面、Computer Use、本机 Shell 或 SSH 直连。",
        annotations(
            title = "配对远程连接",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn pair_connection(
        &self,
        Parameters(input): Parameters<PairConnectionInput>,
    ) -> Result<Json<ConnectionOutput>, String> {
        let pairing_code =
            PairingCode::parse(input.pairing_code).map_err(|error| error.to_string())?;
        let connection = self
            .client
            .pair(pairing_code)
            .await
            .map_err(application_error)?;
        if let Some(alias) = input.alias {
            self.client
                .set_alias(&connection.session_id.to_string(), alias)
                .await
                .map_err(application_error)?;
        }
        let connection = self
            .client
            .list_connections()
            .await
            .into_iter()
            .find(|item| item.session_id == connection.session_id)
            .ok_or_else(|| "配对成功后未找到连接".to_owned())?;
        let session_id = connection.session_id;
        self.full_access_grants.lock().await.remove(&session_id);
        let mut output = connection_output(connection);
        output.transfer_root = self.transfer_root.display().to_string();
        output.control_mode = self.control_mode_name(session_id).await.to_owned();
        Ok(Json(output))
    }

    /// 查询指定 Agent 在本 MCP 进程中的控制模式。
    #[tool(
        name = "get_control_mode",
        description = "查询精确 session_id 当前是逐项确认还是完全控制。完全控制仅保存在本机 MCP 内存，空闲一小时自动失效。",
        annotations(
            title = "查询控制模式",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_control_mode(
        &self,
        Parameters(input): Parameters<TargetInput>,
    ) -> Result<Json<ControlModeOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        self.ensure_connection_exists(session_id).await?;
        Ok(Json(self.control_mode_output(session_id).await))
    }

    /// 更改指定 Agent 在本 MCP 进程中的控制模式。
    #[tool(
        name = "set_control_mode",
        description = "按精确 session_id 切换控制模式。调用 full_access 时只使用 Codex 对本工具的授权，不再嵌套弹出第二次确认；Agent 端没有完全控制按钮。切回逐项确认立即生效。",
        annotations(
            title = "切换控制模式",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn set_control_mode(
        &self,
        Parameters(input): Parameters<SetControlModeInput>,
    ) -> Result<Json<ControlModeOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        self.ensure_connection_exists(session_id).await?;
        if self.command_mode != CommandMode::AgentControlled {
            let requested_full_access = matches!(input.mode, McpControlMode::FullAccess);
            let already_full_access = self.command_mode == CommandMode::FullAccess;
            if requested_full_access != already_full_access {
                return Err(format!(
                    "MCP 启动 command_mode={:?} 固定了有效权限，不能在会话内切换",
                    self.command_mode
                ));
            }
            return Ok(Json(self.control_mode_output(session_id).await));
        }
        match input.mode {
            McpControlMode::StepByStep => {
                self.full_access_grants.lock().await.remove(&session_id);
            }
            McpControlMode::FullAccess => {
                self.full_access_grants.lock().await.insert(
                    session_id,
                    FullAccessGrant {
                        last_successful_use: Instant::now(),
                    },
                );
            }
        }
        Ok(Json(self.control_mode_output(session_id).await))
    }

    /// 读取单个连接的主机、系统、能力和状态。
    #[tool(
        name = "get_target_info",
        description = "按精确 session_id 获取目标详情。别名只用于展示，不能代替 session_id。",
        annotations(
            title = "读取目标信息",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_target_info(
        &self,
        Parameters(input): Parameters<TargetInput>,
    ) -> Result<Json<ConnectionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let connection = self
            .client
            .list_connections()
            .await
            .into_iter()
            .find(|connection| connection.session_id == session_id)
            .ok_or_else(|| "未找到 session_id".to_owned())?;
        let mut output = connection_output(connection);
        output.transfer_root = self.transfer_root.display().to_string();
        output.control_mode = self.control_mode_name(session_id).await.to_owned();
        Ok(Json(output))
    }

    /// 为指定连接创建受控 Shell 句柄。
    #[tool(
        name = "open_shell",
        description = "按 session_id 打开一个逻辑 Shell 句柄。持久 Shell 保留进程状态，后续命令必须通过审批模式下的 run_command 调用；只读诊断请使用一次性 Shell。",
        annotations(
            title = "打开远程 Shell",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn open_shell(
        &self,
        Parameters(input): Parameters<OpenShellInput>,
    ) -> Result<Json<ShellOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let shell: ShellKind = input.shell.into();
        ensure_shell_capability(&self.client, session_id, shell).await?;
        let result = self
            .client
            .execute(
                &session_id.to_string(),
                EventSource::Ai,
                RemoteOperation::OpenShell { shell },
                None,
                None,
                None,
            )
            .await
            .map_err(application_error)?;
        let shell_id = result
            .response
            .details
            .as_ref()
            .and_then(|details| details.get("shell_id"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Agent 未返回 shell_id".to_owned())?
            .parse::<ShellId>()
            .map_err(|error| error.to_string())?;
        self.shells
            .lock()
            .await
            .insert(shell_id, ShellHandle { session_id, shell });
        Ok(Json(ShellOutput {
            session_id: session_id.to_string(),
            shell_id: shell_id.to_string(),
            shell: format!("{shell:?}"),
            message: "真实持久 Shell 已打开；后续命令仍受策略检查和 session_id 绑定".to_owned(),
        }))
    }

    /// 关闭指定持久 Shell 并删除本地句柄。
    #[tool(
        name = "close_shell",
        description = "关闭 open_shell 返回的持久 Shell；shell_id 必须属于精确 session_id。",
        annotations(
            title = "关闭远程 Shell",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn close_shell(
        &self,
        Parameters(input): Parameters<CloseShellInput>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let shell_id = input
            .shell_id
            .parse::<ShellId>()
            .map_err(|error| error.to_string())?;
        {
            let handles = self.shells.lock().await;
            let handle = handles
                .get(&shell_id)
                .ok_or_else(|| "shell_id 不存在".to_owned())?;
            if handle.session_id != session_id {
                return Err("shell_id 不属于指定 session_id".to_owned());
            }
        }
        let result = self
            .client
            .execute(
                &session_id.to_string(),
                EventSource::Ai,
                RemoteOperation::CloseShell { shell_id },
                None,
                None,
                None,
            )
            .await
            .map_err(application_error)?;
        self.shells.lock().await.remove(&shell_id);
        self.refresh_full_access_after_success(session_id, &result)
            .await;
        Ok(Json(action_output(session_id, result)))
    }

    /// 执行只读诊断命令。
    #[tool(
        name = "run_readonly_command",
        description = "在精确 session_id 上执行只读命令。检测到删除、写文件、服务修改、重启或执行策略修改时拒绝。",
        annotations(
            title = "执行只读命令",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn run_readonly_command(
        &self,
        Parameters(input): Parameters<RunReadonlyCommandInput>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let shell = self.resolve_shell(session_id, None, input.shell).await?;
        let operation = command_operation(shell, input.command, true);
        let result = self
            .client
            .execute(
                &session_id.to_string(),
                EventSource::Ai,
                operation,
                None,
                None,
                None,
            )
            .await
            .map_err(application_error)?;
        Ok(Json(action_output(session_id, result)))
    }

    /// 执行一条由 Agent 当前权限模式约束的非只读命令。
    #[tool(
        name = "run_command",
        description = "执行非只读命令。默认逐项确认时 MCP 会向当前用户确认本次完整命令；完全控制时不再逐项询问。显式 approval 启动模式仍兼容独立 Human Controller 的 approval_id。",
        annotations(
            title = "执行审批命令",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn run_command(
        &self,
        Parameters(input): Parameters<RunCommandInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let shell = self
            .resolve_shell(session_id, input.shell_id.as_deref(), input.shell)
            .await?;
        let action = format!("非只读命令：{}", input.command);
        let authorization = self
            .authorize_mutation(&context, session_id, &action, input.approval_id)
            .await?;
        let persistent_shell_id = match shell {
            ResolvedShell::Persistent(shell_id, _) => Some(shell_id),
            ResolvedShell::OneShot(_) => None,
        };
        let result = self
            .execute_raw(
                session_id,
                command_operation(shell, input.command, false),
                authorization.approval_id,
                None,
            )
            .await?;
        if result
            .response
            .details
            .as_ref()
            .and_then(|details| details.get("shell_closed"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && let Some(shell_id) = persistent_shell_id
        {
            self.shells.lock().await.remove(&shell_id);
        }
        Ok(Json(action_output(session_id, result)))
    }

    /// 从 Agent 所在网络测试 TCP 端口。
    #[tool(
        name = "test_port",
        description = "按精确 session_id 从现场 Agent 所在网络探测 TCP 端口，仅执行连接测试。",
        annotations(
            title = "探测现场端口",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn test_port(
        &self,
        Parameters(input): Parameters<PortInput>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let result = self
            .client
            .execute(
                &session_id.to_string(),
                EventSource::Ai,
                RemoteOperation::TestPort {
                    host: input.host,
                    port: input.port,
                },
                None,
                None,
                None,
            )
            .await
            .map_err(application_error)?;
        Ok(Json(action_output(session_id, result)))
    }

    #[tool(
        name = "tcp_exchange",
        description = "仅连接用户明确指定的单个主机和端口，发送 Base64 数据并返回最多 1 MiB 响应；不提供扫描、代理或端口转发。",
        annotations(
            title = "指定 TCP 收发",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn tcp_exchange(
        &self,
        Parameters(input): Parameters<TcpExchangeInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let payload = BASE64
            .decode(&input.data_base64)
            .map_err(|error| format!("TCP data_base64 无效：{error}"))?;
        let authorization = self
            .authorize_mutation(&context, session_id, "TCP 发送", input.approval_id)
            .await?;
        let operation = RemoteOperation::TcpExchange {
            host: input.host,
            port: input.port,
            byte_count: payload.len(),
            request_sha256: sha256_bytes(&payload),
            max_response_bytes: input.max_response_bytes.unwrap_or(64 * 1024),
            timeout_millis: input.timeout_millis.unwrap_or(5_000),
        };
        self.execute_authorized(
            session_id,
            operation,
            authorization,
            Some(BASE64.encode(payload)),
        )
        .await
    }

    #[tool(
        name = "get_file_metadata",
        description = "读取 Agent transfer-root 内普通文件的大小、修改时间和小文件 SHA-256。",
        annotations(
            title = "读取文件元数据",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_file_metadata(
        &self,
        Parameters(input): Parameters<RemotePathInput>,
    ) -> Result<Json<ActionOutput>, String> {
        self.execute_simple(
            input.session_id,
            RemoteOperation::GetFileMetadata {
                remote_path: input.remote_path,
                include_sha256: true,
            },
            None,
            None,
        )
        .await
    }

    #[tool(
        name = "move_file",
        description = "在 Agent transfer-root 内移动或重命名普通文件；写操作在默认模式下必须审批。",
        annotations(
            title = "移动远程文件",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn move_file(
        &self,
        Parameters(input): Parameters<MoveFileInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let authorization = self
            .authorize_mutation(&context, session_id, "文件移动", input.approval_id)
            .await?;
        self.execute_authorized(
            session_id,
            RemoteOperation::MoveFile {
                source_path: input.source_path,
                destination_path: input.destination_path,
                overwrite: input.overwrite,
            },
            authorization,
            None,
        )
        .await
    }

    #[tool(
        name = "delete_file",
        description = "删除 Agent transfer-root 内单个普通文件，不支持目录或递归删除；默认模式下必须审批。",
        annotations(
            title = "删除远程文件",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn delete_file(
        &self,
        Parameters(input): Parameters<MutatingRemotePathInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let authorization = self
            .authorize_mutation(&context, session_id, "文件删除", input.approval_id)
            .await?;
        self.execute_authorized(
            session_id,
            RemoteOperation::DeleteFile {
                remote_path: input.remote_path,
            },
            authorization,
            None,
        )
        .await
    }

    #[tool(
        name = "list_processes",
        description = "读取 Agent 当前进程列表。",
        annotations(
            title = "列出进程",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_processes(
        &self,
        Parameters(input): Parameters<TargetInput>,
    ) -> Result<Json<ActionOutput>, String> {
        self.execute_simple(input.session_id, RemoteOperation::ListProcesses, None, None)
            .await
    }

    #[tool(
        name = "terminate_process",
        description = "终止指定 PID 的进程树；禁止终止 Agent 自身，默认模式下必须审批。",
        annotations(
            title = "终止进程",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn terminate_process(
        &self,
        Parameters(input): Parameters<ProcessInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let authorization = self
            .authorize_mutation(&context, session_id, "终止进程", input.approval_id)
            .await?;
        self.execute_authorized(
            session_id,
            RemoteOperation::TerminateProcess {
                process_id: input.process_id,
            },
            authorization,
            None,
        )
        .await
    }

    #[tool(
        name = "list_services",
        description = "读取 Windows Service 或平台服务列表。",
        annotations(
            title = "列出系统服务",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_services(
        &self,
        Parameters(input): Parameters<TargetInput>,
    ) -> Result<Json<ActionOutput>, String> {
        self.execute_simple(input.session_id, RemoteOperation::ListServices, None, None)
            .await
    }

    #[tool(
        name = "control_service",
        description = "启动、停止或重启指定服务；属于高风险操作，默认模式下必须审批。",
        annotations(
            title = "控制系统服务",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn control_service(
        &self,
        Parameters(input): Parameters<ServiceInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let authorization = self
            .authorize_mutation(&context, session_id, "服务控制", input.approval_id)
            .await?;
        self.execute_authorized(
            session_id,
            RemoteOperation::ControlService {
                service_name: input.service_name,
                action: input.action.into(),
            },
            authorization,
            None,
        )
        .await
    }

    #[tool(
        name = "power_control",
        description = "重启或关闭 Agent 操作系统；属于最高风险操作，默认模式下必须审批，执行后连接会中断。",
        annotations(
            title = "重启或关机",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn power_control(
        &self,
        Parameters(input): Parameters<PowerInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let authorization = self
            .authorize_mutation(&context, session_id, "重启或关机", input.approval_id)
            .await?;
        self.execute_authorized(
            session_id,
            RemoteOperation::PowerControl {
                action: input.action.into(),
            },
            authorization,
            None,
        )
        .await
    }

    /// 读取统一事件流和远程输出。
    #[tool(
        name = "read_output",
        description = "读取指定 session_id 的人工、AI、系统与 Agent 统一事件流，可按 sequence 增量读取。",
        annotations(
            title = "读取远程输出",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn read_output(
        &self,
        Parameters(input): Parameters<ReadOutputInput>,
    ) -> Result<Json<EventsOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let events = self
            .client
            .read_events(session_id, input.after_sequence, input.limit.unwrap_or(100))
            .await
            .into_iter()
            .map(|event| serde_json::to_value(event).unwrap_or(serde_json::Value::Null))
            .collect();
        Ok(Json(EventsOutput {
            session_id: session_id.to_string(),
            events,
        }))
    }

    /// 打开串口会话。
    #[tool(
        name = "open_serial",
        description = "按 session_id 打开现场串口。AI 应默认 writable=false；可写模式在逐项确认下会由 MCP 向当前用户确认。",
        annotations(
            title = "打开现场串口",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn open_serial(
        &self,
        Parameters(input): Parameters<OpenSerialInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let authorization = if input.writable {
            Some(
                self.authorize_mutation(&context, session_id, "打开可写串口", input.approval_id)
                    .await?,
            )
        } else {
            None
        };
        match self
            .client
            .execute(
                &session_id.to_string(),
                EventSource::Ai,
                RemoteOperation::OpenSerial {
                    port_name: input.port_name,
                    settings: serial_settings(
                        input.baud_rate,
                        input.data_bits,
                        input.stop_bits,
                        input.parity,
                        input.flow_control,
                    ),
                    writable: input.writable,
                },
                authorization.and_then(|value| value.approval_id),
                None,
                None,
            )
            .await
        {
            Ok(result) => {
                self.refresh_full_access_after_success(session_id, &result)
                    .await;
                Ok(Json(action_output(session_id, result)))
            }
            Err(error) => Ok(Json(error_action_output(session_id, error))),
        }
    }

    #[tool(
        name = "list_serial_ports",
        description = "枚举 Agent 当前可见串口。",
        annotations(
            title = "列出串口",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_serial_ports(
        &self,
        Parameters(input): Parameters<TargetInput>,
    ) -> Result<Json<ActionOutput>, String> {
        self.execute_simple(input.session_id, RemoteOperation::ListSerial, None, None)
            .await
    }

    #[tool(
        name = "write_serial",
        description = "向已用 writable=true 打开的串口发送 Base64 原始字节；审批绑定字节数和 SHA-256。",
        annotations(
            title = "写入串口",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn write_serial(
        &self,
        Parameters(input): Parameters<WriteSerialInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let payload = BASE64
            .decode(&input.data_base64)
            .map_err(|error| format!("串口 data_base64 无效：{error}"))?;
        let session_id = parse_session_id(&input.session_id)?;
        let authorization = self
            .authorize_mutation(&context, session_id, "串口写入", input.approval_id)
            .await?;
        self.execute_authorized(
            session_id,
            RemoteOperation::WriteSerial {
                serial_session_id: input.serial_session_id,
                byte_count: payload.len(),
                sha256: sha256_bytes(&payload),
            },
            authorization,
            Some(BASE64.encode(payload)),
        )
        .await
    }

    #[tool(
        name = "run_serial_query",
        description = "执行一次有界串口查询，支持华为 VRP 自动翻页、超时、最大字节数和脱敏输出；非只读命令默认必须审批。",
        annotations(
            title = "执行串口查询",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn run_serial_query(
        &self,
        Parameters(input): Parameters<RunSerialQueryInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let authorization = if input.readonly {
            None
        } else {
            Some(
                self.authorize_mutation(&context, session_id, "非只读串口查询", input.approval_id)
                    .await?,
            )
        };
        let operation = RemoteOperation::RunSerialQuery {
            serial_session_id: input.serial_session_id,
            command: input.command,
            line_ending: input.line_ending.map_or(SerialLineEnding::Cr, Into::into),
            profile: input
                .profile
                .map_or(SerialTerminalProfile::HuaweiVrp, Into::into),
            overall_timeout_millis: input.overall_timeout_millis.unwrap_or(30_000),
            idle_timeout_millis: input.idle_timeout_millis.unwrap_or(1_200),
            max_bytes: input.max_bytes.unwrap_or(128 * 1024),
            max_pages: input.max_pages.unwrap_or(50),
            readonly: input.readonly,
        };
        if let Some(authorization) = authorization {
            self.execute_authorized(session_id, operation, authorization, None)
                .await
        } else {
            self.execute_simple(session_id.to_string(), operation, None, None)
                .await
        }
    }

    #[tool(
        name = "close_serial",
        description = "关闭指定串口会话并释放设备。",
        annotations(
            title = "关闭串口",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn close_serial(
        &self,
        Parameters(input): Parameters<SerialSessionInput>,
    ) -> Result<Json<ActionOutput>, String> {
        self.execute_simple(
            input.session_id,
            RemoteOperation::CloseSerial {
                serial_session_id: input.serial_session_id,
            },
            None,
            None,
        )
        .await
    }

    #[tool(
        name = "run_ssh",
        description = "由 Agent 使用本地受控密钥和 known_hosts 连接明确 SSH 目标；非只读命令默认必须审批。",
        annotations(
            title = "执行 SSH 命令",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn run_ssh(
        &self,
        Parameters(input): Parameters<SshInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let operation = RemoteOperation::RunSsh {
            host: input.host,
            port: input.port.unwrap_or(22),
            username: input.username,
            identity_file: input.identity_file,
            known_hosts_file: input.known_hosts_file,
            command: input.command,
            readonly: input.readonly,
        };
        if input.readonly {
            self.execute_simple(session_id.to_string(), operation, None, None)
                .await
        } else {
            let authorization = self
                .authorize_mutation(&context, session_id, "非只读 SSH 命令", input.approval_id)
                .await?;
            self.execute_authorized(session_id, operation, authorization, None)
                .await
        }
    }

    /// 将控制端本地 DPAPI 凭据按精确目标绑定注入 Agent，密码不进入 MCP 参数或审计。
    #[tool(
        name = "provision_ssh_credential",
        description = "从控制端当前 Windows 用户的 DPAPI SSH 凭据库读取 credential_ref，并通过受控链路注入精确 Agent；密码不会写入配置、日志或工具结果。credential_ref 必须匹配 ssh://用户名@主机:端口。",
        annotations(
            title = "注入 SSH 凭据",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn provision_ssh_credential(
        &self,
        Parameters(input): Parameters<ProvisionSshCredentialInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let port = input.port.unwrap_or(22);
        let expected_ref = SshCredentialStore::credential_ref(&input.host, port, &input.username);
        if input.credential_ref != expected_ref {
            return Err("credential_ref 与主机、端口或用户名不匹配".to_owned());
        }
        let password = self
            .ssh_credentials
            .get(&input.host, port, &input.username)
            .ok_or_else(|| "控制端 DPAPI 凭据库中不存在该 credential_ref".to_owned())?;
        let operation = RemoteOperation::ProvisionSshCredential {
            host: input.host,
            port,
            username: input.username,
            credential_ref: input.credential_ref,
        };
        let authorization = self
            .authorize_mutation(&context, session_id, "注入 SSH 凭据", input.approval_id)
            .await?;
        let payload = BASE64.encode(password.as_bytes());
        drop(password);
        self.execute_authorized(session_id, operation, authorization, Some(payload))
            .await
    }

    /// 上传控制端本机文件到 Agent。
    #[tool(
        name = "upload_file",
        description = "从受控 transfer-root 以 1 MiB 分块上传文件到精确 session_id，并校验分块及完整 SHA-256；超过 1 GiB 时在读取文件前单独确认。",
        annotations(
            title = "上传诊断文件",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn upload_file(
        &self,
        Parameters(input): Parameters<UploadInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let local_path =
            resolve_existing_transfer_file(&self.transfer_root, &input.local_path).await?;
        let size = tokio::fs::metadata(&local_path)
            .await
            .map_err(|error| format!("读取控制端文件信息失败：{error}"))?
            .len();
        if size > MAX_TRANSFER_BYTES {
            return Err(format!("上传文件超过 {MAX_TRANSFER_BYTES} 字节限制"));
        }
        let authorization = self
            .authorize_upload(&context, session_id, size, input.approval_id)
            .await?;
        let hash = sha256_local_file(&local_path).await?;
        let transfer_id = FileTransferId::new();
        let begin_result = self
            .client
            .execute(
                &session_id.to_string(),
                EventSource::Ai,
                RemoteOperation::BeginUploadFile {
                    transfer_id,
                    remote_path: input.remote_path,
                    size,
                    sha256: hash.clone(),
                    overwrite: input.overwrite,
                },
                authorization.approval_id,
                None,
                None,
            )
            .await;
        match begin_result {
            Ok(result) => {
                self.refresh_full_access_after_success(session_id, &result)
                    .await;
            }
            Err(error) => return Ok(Json(error_action_output(session_id, error))),
        }

        let transfer_result = async {
            let mut file = tokio::fs::File::open(&local_path)
                .await
                .map_err(|error| format!("打开控制端上传文件失败：{error}"))?;
            let mut buffer = vec![0_u8; FILE_CHUNK_BYTES];
            let mut offset = 0_u64;
            loop {
                let read = file
                    .read(&mut buffer)
                    .await
                    .map_err(|error| format!("读取控制端上传文件失败：{error}"))?;
                if read == 0 {
                    break;
                }
                let chunk = &buffer[..read];
                self.execute_raw(
                    session_id,
                    RemoteOperation::UploadFileChunk {
                        transfer_id,
                        offset,
                        size: read as u64,
                        sha256: sha256_bytes(chunk),
                    },
                    None,
                    Some(BASE64.encode(chunk)),
                )
                .await?;
                offset = offset
                    .checked_add(read as u64)
                    .ok_or_else(|| "上传文件偏移溢出".to_owned())?;
            }
            if offset != size {
                return Err(format!(
                    "上传期间文件大小发生变化：期望 {size}，读取 {offset}"
                ));
            }
            self.execute_raw(
                session_id,
                RemoteOperation::CompleteUploadFile { transfer_id },
                None,
                None,
            )
            .await
        }
        .await;
        let result = match transfer_result {
            Ok(result) => result,
            Err(error) => {
                self.abort_remote_upload(session_id, transfer_id).await;
                return Err(error);
            }
        };
        let mut output = action_output(session_id, result);
        output.summary = format!("已分块上传 {size} 字节");
        output.sha256 = Some(hash);
        output.details = Some(serde_json::json!({
            "size": size,
            "chunk_size_bytes": FILE_CHUNK_BYTES,
        }));
        Ok(Json(output))
    }

    /// 下载 Agent 文件到控制端本机。
    #[tool(
        name = "download_file",
        description = "从精确 session_id 以 1 MiB 分块下载文件，仅写入受控 transfer-root；逐块和整文件校验后原子提交，超过 1 GiB 时在远端哈希和读取前单独确认。",
        annotations(
            title = "下载诊断文件",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn download_file(
        &self,
        Parameters(input): Parameters<DownloadInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let relative_path = parse_transfer_relative_path(&input.local_path)?;
        let (size, _, _) = self
            .remote_file_metadata(session_id, &input.remote_path, false)
            .await?;
        if size > MAX_TRANSFER_BYTES {
            return Err(format!("下载文件超过 {MAX_TRANSFER_BYTES} 字节限制"));
        }
        self.confirm_large_transfer(&context, size, "下载").await?;
        let local_path =
            resolve_transfer_destination(&self.transfer_root, &relative_path.to_string_lossy())
                .await?;
        if local_path.exists() && !input.overwrite_local {
            return Err("控制端目标文件已存在，未设置 overwrite_local".to_owned());
        }
        let overwrite_authorization = if input.overwrite_local {
            let authorization = if large_transfer_requires_confirmation(size)
                && self.command_mode != CommandMode::Approval
            {
                if self.command_mode == CommandMode::Readonly {
                    return Err("当前 MCP 使用 readonly 模式，已拒绝覆盖控制端下载文件".to_owned());
                }
                WriteAuthorization { approval_id: None }
            } else {
                self.authorize_mutation(
                    &context,
                    session_id,
                    "覆盖控制端下载文件",
                    input.approval_id,
                )
                .await?
            };
            Some(authorization)
        } else {
            None
        };
        if let Some(authorization) = overwrite_authorization {
            let authorization_result = self
                .client
                .execute(
                    &session_id.to_string(),
                    EventSource::Ai,
                    RemoteOperation::AuthorizeDownloadFile {
                        remote_path: input.remote_path.clone(),
                        overwrite_local: true,
                    },
                    authorization.approval_id,
                    None,
                    None,
                )
                .await;
            match authorization_result {
                Ok(result) => {
                    self.refresh_full_access_after_success(session_id, &result)
                        .await;
                }
                Err(error) => return Ok(Json(error_action_output(session_id, error))),
            }
        }
        let (temporary_path, mut temporary_file) = create_local_download_file(&local_path).await?;
        let download_result = async {
            let mut offset = 0_u64;
            let mut hasher = Sha256::new();
            while offset < size {
                let result = self
                    .execute_raw(
                        session_id,
                        RemoteOperation::DownloadFileChunk {
                            remote_path: input.remote_path.clone(),
                            offset,
                            max_bytes: FILE_CHUNK_BYTES as u64,
                        },
                        None,
                        None,
                    )
                    .await?;
                let payload = result
                    .response
                    .payload_base64
                    .as_deref()
                    .ok_or_else(|| "Agent 未返回下载分块".to_owned())?;
                let bytes = BASE64
                    .decode(payload)
                    .map_err(|error| format!("下载分块 Base64 无效：{error}"))?;
                if bytes.is_empty() || bytes.len() > FILE_CHUNK_BYTES {
                    return Err("Agent 返回了无效下载分块大小".to_owned());
                }
                let chunk_hash = sha256_bytes(&bytes);
                if result.response.sha256.as_deref() != Some(chunk_hash.as_str()) {
                    return Err("下载分块 SHA-256 校验失败".to_owned());
                }
                let next_offset = offset
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| "下载文件偏移溢出".to_owned())?;
                if next_offset > size {
                    return Err("Agent 返回的下载分块超过声明文件大小".to_owned());
                }
                temporary_file
                    .write_all(&bytes)
                    .await
                    .map_err(|error| format!("写入控制端临时文件失败：{error}"))?;
                hasher.update(&bytes);
                offset = next_offset;
            }
            temporary_file
                .flush()
                .await
                .map_err(|error| format!("刷新控制端临时文件失败：{error}"))?;
            temporary_file
                .sync_all()
                .await
                .map_err(|error| format!("同步控制端临时文件失败：{error}"))?;
            drop(temporary_file);
            let local_hash = format!("{:x}", hasher.finalize());
            let (final_size, remote_hash, metadata_result) = self
                .remote_file_metadata(session_id, &input.remote_path, true)
                .await?;
            if final_size != size {
                return Err("远端文件在下载期间大小发生变化".to_owned());
            }
            if remote_hash.as_deref() != Some(local_hash.as_str()) {
                return Err("下载文件完整 SHA-256 校验失败".to_owned());
            }
            Ok((local_hash, metadata_result))
        }
        .await;
        let (local_hash, result) = match download_result {
            Ok(value) => value,
            Err(error) => {
                let _ = tokio::fs::remove_file(&temporary_path).await;
                return Err(error);
            }
        };
        commit_local_download(&temporary_path, &local_path, input.overwrite_local).await?;
        let mut output = action_output(session_id, result);
        output.summary = format!("已分块下载并校验 {size} 字节");
        output.sha256 = Some(local_hash);
        output.details = Some(serde_json::json!({
            "size": size,
            "chunk_size_bytes": FILE_CHUNK_BYTES,
            "local_path": input.local_path,
        }));
        Ok(Json(output))
    }

    /// 为一项精确远程动作申请人工审批。
    #[tool(
        name = "request_action_approval",
        description = "只向 Relay 提交精确动作的审批申请并返回 approval_id；本 MCP 是 AI Controller，不能批准或拒绝审批。工程师必须通过独立的 Human Controller 作出决定。",
        annotations(
            title = "申请人工审批",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn request_action_approval(
        &self,
        Parameters(input): Parameters<ApprovalRequestInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let operation = self
            .approval_operation(&context, session_id, input.action)
            .await?;
        let result = self
            .client
            .request_approval(&session_id.to_string(), operation)
            .await
            .map_err(application_error)?;
        Ok(Json(ActionOutput {
            status: format!("{:?}", result.state).to_lowercase(),
            session_id: session_id.to_string(),
            request_id: Some(result.request_id.to_string()),
            exit_code: None,
            summary: result.reason,
            approval_id: result
                .approval_id
                .map(|approval_id| approval_id.to_string()),
            sha256: None,
            details: Some(serde_json::json!({
                "expires_at": result.expires_at,
                "operation": result.operation,
                "decision_channel": "independent_human_controller"
            })),
        }))
    }

    /// 关闭当前控制端的连接。
    #[tool(
        name = "close_connection",
        description = "按精确 session_id 关闭当前 Controller 会话并从本地连接列表移除。",
        annotations(
            title = "关闭远程连接",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn close_connection(
        &self,
        Parameters(input): Parameters<TargetInput>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let result = self
            .client
            .close_connection(&session_id.to_string())
            .await
            .map_err(application_error)?;
        self.shells
            .lock()
            .await
            .retain(|_, handle| handle.session_id != session_id);
        self.full_access_grants.lock().await.remove(&session_id);
        Ok(Json(action_output(session_id, result)))
    }

    async fn execute_simple(
        &self,
        session_id: String,
        operation: RemoteOperation,
        approval_id: Option<ApprovalId>,
        payload_base64: Option<String>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&session_id)?;
        match self
            .client
            .execute(
                &session_id.to_string(),
                EventSource::Ai,
                operation,
                approval_id,
                payload_base64,
                None,
            )
            .await
        {
            Ok(result) => {
                self.refresh_full_access_after_success(session_id, &result)
                    .await;
                Ok(Json(action_output(session_id, result)))
            }
            Err(error) => Ok(Json(error_action_output(session_id, error))),
        }
    }

    async fn execute_authorized(
        &self,
        session_id: SessionId,
        operation: RemoteOperation,
        authorization: WriteAuthorization,
        payload_base64: Option<String>,
    ) -> Result<Json<ActionOutput>, String> {
        match self
            .client
            .execute(
                &session_id.to_string(),
                EventSource::Ai,
                operation,
                authorization.approval_id,
                payload_base64,
                None,
            )
            .await
        {
            Ok(result) => {
                self.refresh_full_access_after_success(session_id, &result)
                    .await;
                Ok(Json(action_output(session_id, result)))
            }
            Err(error) => Ok(Json(error_action_output(session_id, error))),
        }
    }

    async fn resolve_shell(
        &self,
        session_id: SessionId,
        shell_id: Option<&str>,
        shell: Option<McpShell>,
    ) -> Result<ResolvedShell, String> {
        match (shell_id, shell) {
            (Some(shell_id), None) => {
                let shell_id = shell_id
                    .parse::<ShellId>()
                    .map_err(|error| error.to_string())?;
                let handles = self.shells.lock().await;
                let handle = handles
                    .get(&shell_id)
                    .ok_or_else(|| "shell_id 不存在".to_owned())?;
                if handle.session_id != session_id {
                    return Err("shell_id 不属于指定 session_id".to_owned());
                }
                Ok(ResolvedShell::Persistent(shell_id, handle.shell))
            }
            (None, Some(shell)) => {
                let shell = shell.into();
                ensure_shell_capability(&self.client, session_id, shell).await?;
                Ok(ResolvedShell::OneShot(shell))
            }
            _ => Err("shell_id 与 shell 必须二选一".to_owned()),
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn approval_operation(
        &self,
        context: &RequestContext<RoleServer>,
        session_id: SessionId,
        action: ApprovalActionInput,
    ) -> Result<RemoteOperation, String> {
        match action {
            ApprovalActionInput::RunCommand {
                shell_id,
                shell,
                command,
            } => {
                ensure_approval_command_mode(self.command_mode)?;
                let shell = self
                    .resolve_shell(session_id, shell_id.as_deref(), shell)
                    .await?;
                Ok(command_operation(shell, command, false))
            }
            ApprovalActionInput::OpenSerial {
                port_name,
                baud_rate,
                data_bits,
                stop_bits,
                parity,
                flow_control,
                writable,
            } => Ok(RemoteOperation::OpenSerial {
                port_name,
                settings: serial_settings(baud_rate, data_bits, stop_bits, parity, flow_control),
                writable,
            }),
            ApprovalActionInput::UploadFile {
                local_path,
                remote_path,
                overwrite,
            } => {
                let local_path =
                    resolve_existing_transfer_file(&self.transfer_root, &local_path).await?;
                let size = tokio::fs::metadata(&local_path)
                    .await
                    .map_err(|error| format!("读取控制端文件信息失败：{error}"))?
                    .len();
                if size > MAX_TRANSFER_BYTES {
                    return Err(format!("上传文件超过 {MAX_TRANSFER_BYTES} 字节限制"));
                }
                self.confirm_large_transfer(context, size, "为上传审批预检")
                    .await?;
                let sha256 = sha256_local_file(&local_path).await?;
                Ok(RemoteOperation::UploadFile {
                    remote_path,
                    size,
                    sha256,
                    overwrite,
                })
            }
            ApprovalActionInput::DownloadFile {
                remote_path,
                overwrite_local,
            } => Ok(RemoteOperation::DownloadFile {
                remote_path,
                overwrite_local,
            }),
            ApprovalActionInput::MoveFile {
                source_path,
                destination_path,
                overwrite,
            } => Ok(RemoteOperation::MoveFile {
                source_path,
                destination_path,
                overwrite,
            }),
            ApprovalActionInput::DeleteFile { remote_path } => {
                Ok(RemoteOperation::DeleteFile { remote_path })
            }
            ApprovalActionInput::TcpExchange {
                host,
                port,
                data_base64,
                max_response_bytes,
                timeout_millis,
            } => {
                let payload = BASE64
                    .decode(data_base64)
                    .map_err(|error| format!("TCP data_base64 无效：{error}"))?;
                Ok(RemoteOperation::TcpExchange {
                    host,
                    port,
                    byte_count: payload.len(),
                    request_sha256: sha256_bytes(&payload),
                    max_response_bytes: max_response_bytes.unwrap_or(64 * 1024),
                    timeout_millis: timeout_millis.unwrap_or(5_000),
                })
            }
            ApprovalActionInput::WriteSerial {
                serial_session_id,
                data_base64,
            } => {
                let payload = BASE64
                    .decode(data_base64)
                    .map_err(|error| format!("串口 data_base64 无效：{error}"))?;
                Ok(RemoteOperation::WriteSerial {
                    serial_session_id,
                    byte_count: payload.len(),
                    sha256: sha256_bytes(&payload),
                })
            }
            ApprovalActionInput::RunSerialQuery {
                serial_session_id,
                command,
                line_ending,
                profile,
                overall_timeout_millis,
                idle_timeout_millis,
                max_bytes,
                max_pages,
                readonly,
            } => Ok(RemoteOperation::RunSerialQuery {
                serial_session_id,
                command,
                line_ending: line_ending.map_or(SerialLineEnding::Cr, Into::into),
                profile: profile.map_or(SerialTerminalProfile::HuaweiVrp, Into::into),
                overall_timeout_millis: overall_timeout_millis.unwrap_or(30_000),
                idle_timeout_millis: idle_timeout_millis.unwrap_or(1_200),
                max_bytes: max_bytes.unwrap_or(128 * 1024),
                max_pages: max_pages.unwrap_or(50),
                readonly,
            }),
            ApprovalActionInput::RunSsh {
                host,
                port,
                username,
                identity_file,
                known_hosts_file,
                command,
                readonly,
            } => Ok(RemoteOperation::RunSsh {
                host,
                port: port.unwrap_or(22),
                username,
                identity_file,
                known_hosts_file,
                command,
                readonly,
            }),
            ApprovalActionInput::TerminateProcess { process_id } => {
                Ok(RemoteOperation::TerminateProcess { process_id })
            }
            ApprovalActionInput::ControlService {
                service_name,
                action,
            } => Ok(RemoteOperation::ControlService {
                service_name,
                action: action.into(),
            }),
            ApprovalActionInput::PowerControl { action } => Ok(RemoteOperation::PowerControl {
                action: action.into(),
            }),
        }
    }
}

fn serial_settings(
    baud_rate: u32,
    data_bits: Option<SerialDataBitsInput>,
    stop_bits: Option<SerialStopBitsInput>,
    parity: Option<SerialParityInput>,
    flow_control: Option<SerialFlowControlInput>,
) -> SerialSettings {
    SerialSettings {
        baud_rate,
        data_bits: data_bits.map_or(SerialDataBits::Eight, Into::into),
        stop_bits: stop_bits.map_or(SerialStopBits::One, Into::into),
        parity: parity.map_or(SerialParity::None, Into::into),
        flow_control: flow_control.map_or(SerialFlowControl::None, Into::into),
    }
}

#[derive(Clone, Copy)]
enum ResolvedShell {
    Persistent(ShellId, ShellKind),
    OneShot(ShellKind),
}

fn command_operation(shell: ResolvedShell, command: String, readonly: bool) -> RemoteOperation {
    match shell {
        ResolvedShell::Persistent(shell_id, shell) => RemoteOperation::RunShellCommand {
            shell_id,
            shell,
            command,
            readonly,
        },
        ResolvedShell::OneShot(shell) => RemoteOperation::RunCommand {
            shell,
            command,
            readonly,
        },
    }
}

fn ensure_approval_command_mode(command_mode: CommandMode) -> Result<(), String> {
    if command_mode == CommandMode::Readonly {
        return Err(
            "非只读命令已关闭；请使用 run_readonly_command，或由操作员以 command-mode=agent-controlled/approval/full-access 启动 MCP"
                .to_owned(),
        );
    }
    Ok(())
}

#[tool_handler(
    name = "remoteops-controller",
    version = "0.2.0-preview.1",
    instructions = "RemoteOps 是控制台与结构化工具驱动的远程诊断，不是远程桌面。新 Agent 只需填写 Relay 地址并等待显示九位控制码，不需要入网码或部署级注册 Token。用户提供 RemoteOps 控制码、配对码或 Agent 显示的九位码时，必须先调用 pair_connection；禁止改用 Computer Use、屏幕操作、本机 Shell 或 SSH 直连。配对后默认逐项确认，Agent 端没有逐项确认或完全控制按钮，绝对不要引导用户去 Agent 点击授权。已有连接时先调用 list_connections，再用返回的不可变 session_id 调用 get_target_info 和其他工具，别名只用于核对。检查、分析、判断等请求默认只读，优先使用结构化工具或一次性 Shell 的 run_readonly_command；持久 Shell 保留目录、变量和模块状态，任何命令都必须走 run_command 的逐项确认或完全控制路径。修改操作在逐项确认模式下由 MCP 向当前用户确认；用户明确要求完全控制时只调用一次 set_control_mode，Codex 对该工具的授权就是唯一确认，不得再要求 Agent 或用户执行第二次授权。完全控制按 session_id 独立保存在 MCP 内存，空闲一小时自动失效，成功操作才续期；工具返回 full_access 后立即继续任务。request_action_approval 仅保留给独立 Human Controller 的未来/兼容流程，普通 MCP 首版不依赖它。连接或工具不可用时明确报告，禁止声称已操作远端。不要向用户输出 Token、session_id、approval_id 或恢复令牌。"
)]
impl ServerHandler for RemoteOpsMcp {}

#[tokio::main]
async fn main() {
    if let Err(error) = run_mcp().await {
        eprintln!("RemoteOps MCP 启动失败：{error:#}");
        std::process::exit(2);
    }
}

async fn run_mcp() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("remoteops_controller_mcp=info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse().resolve()?;
    tokio::fs::create_dir_all(&args.transfer_root)
        .await
        .with_context(|| format!("无法创建 MCP 文件交换目录 {}", args.transfer_root.display()))?;
    let transfer_root = tokio::fs::canonicalize(&args.transfer_root)
        .await
        .with_context(|| format!("无法解析 MCP 文件交换目录 {}", args.transfer_root.display()))?;
    let relay_label = args.relay.clone();
    let client = RelayClient::connect(
        RelayClientConfig {
            relay_address: args.relay,
            server_name: args.server_name,
            ca_certificate: args.ca_cert,
            tls_fingerprint: args.tls_fingerprint,
            audit_log: Some(args.audit_log),
            controller_kind: ControllerKind::Ai,
            owner_id: args.owner_id,
            permission_mode: args.permission_mode,
            authentication_token: args.controller_token,
            reconnect_delay: std::time::Duration::from_secs(args.reconnect_seconds),
        },
        ControllerInstanceId::new(),
    )
    .await
    .with_context(|| {
        format!(
            "MCP 无法连接 Relay {relay_label}。请检查网络、端口和 TLS 配置；自签名证书必须配置 ca_cert 或已人工确认的 tls_fingerprint"
        )
    })?;
    for pair in args.pairs {
        let connection = client
            .pair(pair.code)
            .await
            .context("MCP 配对 Agent 失败")?;
        if let Some(alias) = pair.alias {
            client
                .set_alias(&connection.session_id.to_string(), alias)
                .await
                .context("MCP 设置连接别名失败")?;
        }
    }

    let ssh_credentials = SshCredentialStore::load_persisted()
        .context("无法加载控制端本地 DPAPI SSH 凭据；请检查当前 Windows 用户凭据库")?;
    let service = RemoteOpsMcp::new(client, transfer_root, args.command_mode, ssh_credentials)
        .serve(stdio())
        .await
        .context("启动 STDIO MCP 服务失败")?;
    service.waiting().await?;
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

fn parse_session_id(value: &str) -> Result<SessionId, String> {
    value
        .parse::<SessionId>()
        .map_err(|error| error.to_string())
}

fn elicitation_accepted(action: &ElicitationAction) -> bool {
    matches!(action, ElicitationAction::Accept)
}

fn large_transfer_requires_confirmation(size: u64) -> bool {
    size > LARGE_TRANSFER_CONFIRM_BYTES
}

fn parse_optional_approval(value: Option<String>) -> Result<Option<ApprovalId>, String> {
    value
        .map(|value| {
            value
                .parse::<ApprovalId>()
                .map_err(|error| error.to_string())
        })
        .transpose()
}

fn default_transfer_root_path() -> PathBuf {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("RemoteOps")
            .join("transfers");
    }
    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(user_profile)
            .join(".remoteops")
            .join("transfers");
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("RemoteOps")
            .join("transfers");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("remoteops")
            .join("transfers");
    }
    std::env::temp_dir().join("remoteops").join("transfers")
}

fn parse_transfer_relative_path(value: &str) -> Result<PathBuf, String> {
    if value.trim().is_empty() {
        return Err("文件相对路径不能为空".to_owned());
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err("MCP 文件路径必须是 transfer-root 内的相对路径".to_owned());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("MCP 文件路径不能包含父目录或根目录跳转".to_owned());
            }
        }
    }
    if normalized.as_os_str().is_empty() || normalized.file_name().is_none() {
        return Err("MCP 文件相对路径无效".to_owned());
    }
    Ok(normalized)
}

async fn sha256_local_file(path: &Path) -> Result<String, String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| format!("打开控制端文件失败：{error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; FILE_CHUNK_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| format!("读取控制端文件失败：{error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

async fn create_local_download_file(
    destination: &Path,
) -> Result<(PathBuf, tokio::fs::File), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "MCP 下载目标缺少父目录".to_owned())?;
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
            Err(error) => return Err(format!("创建控制端下载临时文件失败：{error}")),
        }
    }
    Err("无法创建唯一的控制端下载临时文件".to_owned())
}

async fn commit_local_download(
    temporary: &Path,
    destination: &Path,
    overwrite: bool,
) -> Result<(), String> {
    let temporary = temporary.to_path_buf();
    let destination = destination.to_path_buf();
    let cleanup = temporary.clone();
    let result = tokio::task::spawn_blocking(move || {
        if !destination.exists() {
            return std::fs::rename(&temporary, &destination)
                .map_err(|error| format!("提交控制端下载文件失败：{error}"));
        }
        if !overwrite {
            return Err("控制端目标文件已存在，未设置 overwrite_local".to_owned());
        }
        let metadata = std::fs::symlink_metadata(&destination)
            .map_err(|error| format!("读取控制端下载目标失败：{error}"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("控制端下载目标必须是非链接普通文件".to_owned());
        }
        let parent = destination
            .parent()
            .ok_or_else(|| "控制端下载目标缺少父目录".to_owned())?;
        let file_name = destination
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("download");
        let backup = parent.join(format!(
            ".{file_name}.remoteops-{}.backup",
            FileTransferId::new()
        ));
        std::fs::rename(&destination, &backup)
            .map_err(|error| format!("备份控制端原文件失败：{error}"))?;
        if let Err(error) = std::fs::rename(&temporary, &destination) {
            let restore_error = std::fs::rename(&backup, &destination).err();
            return Err(match restore_error {
                Some(restore_error) => {
                    format!("提交下载文件失败：{error}；恢复原文件也失败：{restore_error}")
                }
                None => format!("提交下载文件失败，已恢复原文件：{error}"),
            });
        }
        std::fs::remove_file(&backup)
            .map_err(|error| format!("下载文件已提交，但清理原文件备份失败：{error}"))
    })
    .await
    .map_err(|error| format!("提交控制端下载文件任务失败：{error}"))?;
    if result.is_err() {
        let _ = tokio::fs::remove_file(cleanup).await;
    }
    result
}

async fn resolve_existing_transfer_file(root: &Path, value: &str) -> Result<PathBuf, String> {
    let relative = parse_transfer_relative_path(value)?;
    let candidate = tokio::fs::canonicalize(root.join(relative))
        .await
        .map_err(|error| format!("无法读取 transfer-root 内文件：{error}"))?;
    if !candidate.starts_with(root) {
        return Err("文件路径通过链接逃逸了 transfer-root".to_owned());
    }
    let metadata = tokio::fs::metadata(&candidate)
        .await
        .map_err(|error| format!("无法读取控制端文件信息：{error}"))?;
    if !metadata.is_file() {
        return Err("控制端上传源必须是普通文件".to_owned());
    }
    Ok(candidate)
}

async fn resolve_transfer_destination(root: &Path, value: &str) -> Result<PathBuf, String> {
    let relative = parse_transfer_relative_path(value)?;
    let candidate = root.join(relative);
    let parent = candidate
        .parent()
        .ok_or_else(|| "MCP 下载目标缺少父目录".to_owned())?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| format!("无法创建 transfer-root 下载目录：{error}"))?;
    let canonical_parent = tokio::fs::canonicalize(parent)
        .await
        .map_err(|error| format!("无法解析 transfer-root 下载目录：{error}"))?;
    if !canonical_parent.starts_with(root) {
        return Err("下载目录通过链接逃逸了 transfer-root".to_owned());
    }
    let file_name = candidate
        .file_name()
        .ok_or_else(|| "MCP 下载目标缺少文件名".to_owned())?;
    let destination = canonical_parent.join(file_name);
    if destination.exists() {
        let canonical_destination = tokio::fs::canonicalize(&destination)
            .await
            .map_err(|error| format!("无法解析已有下载目标：{error}"))?;
        if !canonical_destination.starts_with(root) {
            return Err("下载目标通过链接逃逸了 transfer-root".to_owned());
        }
        let metadata = tokio::fs::metadata(&canonical_destination)
            .await
            .map_err(|error| format!("无法读取下载目标信息：{error}"))?;
        if !metadata.is_file() {
            return Err("MCP 下载目标必须是普通文件".to_owned());
        }
        return Ok(canonical_destination);
    }
    Ok(destination)
}

async fn ensure_shell_capability(
    client: &RelayClient,
    session_id: SessionId,
    shell: ShellKind,
) -> Result<(), String> {
    let connection = client
        .list_connections()
        .await
        .into_iter()
        .find(|connection| connection.session_id == session_id)
        .ok_or_else(|| "未找到 session_id".to_owned())?;
    let capability = match shell {
        ShellKind::Cmd | ShellKind::System => Capability::Cmd,
        ShellKind::WindowsPowerShell => Capability::WindowsPowerShell,
        ShellKind::PowerShell => Capability::PowerShell,
    };
    if !connection.capabilities.contains(capability) {
        return Err(format!("目标不支持 {shell:?}"));
    }
    Ok(())
}

fn connection_output(connection: remoteops_domain::ConnectionDescriptor) -> ConnectionOutput {
    ConnectionOutput {
        display_name: connection.display_name(),
        alias: connection.alias,
        session_id: connection.session_id.to_string(),
        agent_instance_id: connection.agent_instance_id.to_string(),
        hostname: connection.hostname,
        operating_system: connection.operating_system,
        environment: serde_json::to_value(connection.environment)
            .unwrap_or_else(|_| serde_json::json!({})),
        state: format!("{:?}", connection.state).to_lowercase(),
        role: format!("{:?}", connection.role).to_lowercase(),
        permission_mode: format!("{:?}", connection.permission_mode).to_lowercase(),
        control_mode: "step_by_step".to_owned(),
        transfer_root: String::new(),
        capabilities: connection
            .capabilities
            .iter()
            .map(|capability| format!("{capability:?}").to_lowercase())
            .collect(),
    }
}

fn action_output(session_id: SessionId, result: OperationResult) -> ActionOutput {
    ActionOutput {
        status: "completed".to_owned(),
        session_id: session_id.to_string(),
        request_id: Some(result.response.request_id.to_string()),
        exit_code: result.response.exit_code,
        summary: result.response.summary,
        approval_id: None,
        sha256: result.response.sha256,
        details: result.response.details,
    }
}

fn error_action_output(session_id: SessionId, error: ApplicationError) -> ActionOutput {
    match error {
        ApplicationError::ApprovalRequired {
            approval_id,
            reason,
        } => ActionOutput {
            status: "approval_required".to_owned(),
            session_id: session_id.to_string(),
            request_id: None,
            exit_code: None,
            summary: reason,
            approval_id: Some(approval_id.to_string()),
            sha256: None,
            details: None,
        },
        other => ActionOutput {
            status: "failed".to_owned(),
            session_id: session_id.to_string(),
            request_id: None,
            exit_code: None,
            summary: other.to_string(),
            approval_id: None,
            sha256: None,
            details: None,
        },
    }
}

#[allow(clippy::needless_pass_by_value)]
fn application_error(error: ApplicationError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_allow_action_confirms_without_boolean_form_value() {
        assert!(elicitation_accepted(&ElicitationAction::Accept));
        assert!(!elicitation_accepted(&ElicitationAction::Decline));
        assert!(!elicitation_accepted(&ElicitationAction::Cancel));
    }

    fn test_args(config: PathBuf) -> Args {
        Args {
            config: Some(config),
            relay: None,
            server_name: None,
            ca_cert: None,
            tls_fingerprint: None,
            reconnect_seconds: None,
            audit_log: PathBuf::from("audit.jsonl"),
            transfer_root: PathBuf::from("transfers"),
            controller_token: None,
            owner_id: None,
            command_mode: CommandMode::Approval,
            pairs: Vec::new(),
        }
    }

    #[test]
    fn missing_mcp_configuration_reports_path_and_remediation() {
        let config = std::env::temp_dir().join(format!(
            "remoteops-missing-mcp-config-{}.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&config);

        let error = test_args(config.clone())
            .resolve()
            .expect_err("缺少 MCP 配置必须失败");
        let message = error.to_string();

        assert!(message.contains(&config.display().to_string()));
        assert!(message.contains("指定的 MCP 配置文件不存在"));
        assert!(message.contains("REMOTEOPS_CONTROLLER_TOKEN"));
        assert!(message.contains("REMOTEOPS_CONTROLLER_OWNER_ID"));
        assert!(message.contains("完全退出并重新打开 Codex"));
    }

    #[test]
    fn missing_mcp_owner_error_does_not_disclose_token() {
        let config = std::env::temp_dir().join(format!(
            "remoteops-missing-owner-config-{}.json",
            std::process::id()
        ));
        let token = "sensitive-test-token-that-must-never-appear";
        let mut args = test_args(config);
        args.relay = Some("relay.example.com:7443".to_owned());
        args.controller_token = Some(token.to_owned());

        let message = args.resolve().expect_err("缺少 Owner 必须失败").to_string();

        assert!(message.contains("REMOTEOPS_CONTROLLER_OWNER_ID"));
        assert!(!message.contains(token));
    }

    #[test]
    fn owner_id_can_be_loaded_from_non_secret_file_config() {
        let config = std::env::temp_dir().join(format!(
            "remoteops-owner-file-config-{}.json",
            ControllerOwnerId::new()
        ));
        let owner_id = ControllerOwnerId::new();
        fs::write(
            &config,
            serde_json::json!({
                "relay": "relay.example.com:7443",
                "owner_id": owner_id,
                "reconnect_seconds": 2
            })
            .to_string(),
        )
        .expect("应写入 MCP 测试配置");
        let mut args = test_args(config.clone());
        args.controller_token = Some("test-token-that-is-long-enough-for-mcp".to_owned());

        let resolved = args.resolve().expect("应从 JSON 读取非敏感 Owner ID");

        assert_eq!(resolved.owner_id, owner_id);
        let _ = fs::remove_file(config);
    }

    #[test]
    fn server_name_is_inferred_from_relay_address() {
        assert_eq!(
            infer_server_name("relay.example.com:7443").expect("应推导服务名"),
            "relay.example.com"
        );
        assert_eq!(
            infer_server_name("[2001:db8::1]:7443").expect("应推导 IPv6 服务名"),
            "2001:db8::1"
        );
    }

    #[test]
    fn home_directory_is_a_supported_codex_config_fallback() {
        let home = PathBuf::from("home-directory");
        let expected = home
            .join(".codex")
            .join("remoteops")
            .join("controller-config.json");

        assert_eq!(codex_config_path_from_home(&home), expected);
    }

    #[test]
    fn command_mode_defaults_to_agent_and_parses_full_access() {
        assert_eq!(CommandMode::default(), CommandMode::AgentControlled);
        assert_eq!(
            CommandMode::from_str("agent-controlled", true).expect("应解析 Agent 跟随模式"),
            CommandMode::AgentControlled
        );
        assert_eq!(
            CommandMode::from_str("approval", true).expect("应解析审批模式"),
            CommandMode::Approval
        );
        assert_eq!(
            CommandMode::from_str("full-access", true).expect("应解析完全授权模式"),
            CommandMode::FullAccess
        );
        assert!(CommandMode::from_str("unrestricted", true).is_err());
    }

    #[test]
    fn full_access_grant_expires_after_one_hour_of_inactivity() {
        let now = Instant::now();
        assert!(
            FullAccessGrant {
                last_successful_use: now
                    .checked_sub(Duration::from_secs(3_599))
                    .expect("测试时间应可回退"),
            }
            .is_active_at(now)
        );
        assert!(
            !FullAccessGrant {
                last_successful_use: now
                    .checked_sub(Duration::from_hours(1))
                    .expect("测试时间应可回退"),
            }
            .is_active_at(now)
        );
    }

    #[test]
    fn large_transfer_confirmation_starts_above_one_gibibyte() {
        assert!(!large_transfer_requires_confirmation(
            LARGE_TRANSFER_CONFIRM_BYTES
        ));
        assert!(large_transfer_requires_confirmation(
            LARGE_TRANSFER_CONFIRM_BYTES + 1
        ));
    }

    #[tokio::test]
    async fn local_download_commit_replaces_recoverably_and_preserves_without_overwrite() {
        let root = std::env::temp_dir().join(format!(
            "remoteops-mcp-download-{}-{}",
            std::process::id(),
            FileTransferId::new()
        ));
        tokio::fs::create_dir_all(&root)
            .await
            .expect("应创建测试目录");
        let destination = root.join("result.bin");
        tokio::fs::write(&destination, b"original")
            .await
            .expect("应创建原文件");
        let (temporary, mut file) = create_local_download_file(&destination)
            .await
            .expect("应创建临时文件");
        file.write_all(b"replacement")
            .await
            .expect("应写入临时文件");
        file.sync_all().await.expect("应同步临时文件");
        drop(file);

        commit_local_download(&temporary, &destination, true)
            .await
            .expect("应可恢复替换原文件");
        assert_eq!(
            tokio::fs::read(&destination).await.expect("应读取替换文件"),
            b"replacement"
        );

        let (temporary, mut file) = create_local_download_file(&destination)
            .await
            .expect("应创建第二个临时文件");
        file.write_all(b"rejected")
            .await
            .expect("应写入第二个临时文件");
        drop(file);
        assert!(
            commit_local_download(&temporary, &destination, false)
                .await
                .is_err()
        );
        assert_eq!(
            tokio::fs::read(&destination)
                .await
                .expect("拒绝覆盖后原文件仍应存在"),
            b"replacement"
        );
        assert!(!temporary.exists());
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[test]
    fn approved_command_operation_is_not_marked_readonly() {
        let operation = command_operation(
            ResolvedShell::OneShot(ShellKind::PowerShell),
            "Set-Content -Path status.txt -Value ok".to_owned(),
            false,
        );

        assert!(matches!(
            operation,
            RemoteOperation::RunCommand {
                shell: ShellKind::PowerShell,
                readonly: false,
                ..
            }
        ));
    }

    #[test]
    fn rejects_paths_outside_transfer_root() {
        assert!(parse_transfer_relative_path("").is_err());
        assert!(parse_transfer_relative_path("../secret.txt").is_err());
        assert!(parse_transfer_relative_path("/etc/passwd").is_err());
        #[cfg(windows)]
        {
            assert!(parse_transfer_relative_path("..\\secret.txt").is_err());
            assert!(parse_transfer_relative_path("C:\\Windows\\win.ini").is_err());
        }
        assert_eq!(
            parse_transfer_relative_path("tools/diag.exe").expect("相对路径应有效"),
            PathBuf::from("tools").join("diag.exe")
        );
    }

    #[tokio::test]
    async fn resolves_only_files_below_transfer_root() {
        let test_root =
            std::env::temp_dir().join(format!("remoteops-mcp-path-{}", SessionId::new()));
        tokio::fs::create_dir_all(&test_root)
            .await
            .expect("应创建测试目录");
        tokio::fs::write(test_root.join("upload.txt"), b"remoteops")
            .await
            .expect("应创建上传文件");
        let canonical_root = tokio::fs::canonicalize(&test_root)
            .await
            .expect("应解析测试目录");

        let upload = resolve_existing_transfer_file(&canonical_root, "upload.txt")
            .await
            .expect("应解析上传文件");
        let download = resolve_transfer_destination(&canonical_root, "nested/download.txt")
            .await
            .expect("应解析下载路径");

        assert!(upload.starts_with(&canonical_root));
        assert!(download.starts_with(&canonical_root));
        assert_eq!(
            download.file_name().and_then(|value| value.to_str()),
            Some("download.txt")
        );
        let _ = tokio::fs::remove_dir_all(test_root).await;
    }

    #[test]
    fn approval_request_input_cannot_include_a_decision() {
        let input = serde_json::json!({
            "session_id": SessionId::new().to_string(),
            "action": {
                "type": "open_serial",
                "port_name": "COM3",
                "baud_rate": 9_600,
                "writable": true
            },
            "approved": true
        });

        assert!(
            serde_json::from_value::<ApprovalRequestInput>(input).is_err(),
            "MCP 审批申请输入不得接受批准或拒绝字段"
        );
    }

    #[test]
    fn command_approval_input_cannot_include_a_decision() {
        let input = serde_json::json!({
            "session_id": SessionId::new().to_string(),
            "action": {
                "type": "run_command",
                "shell": "power_shell",
                "command": "Set-Content -Path status.txt -Value ok",
                "approved": true
            }
        });

        assert!(
            serde_json::from_value::<ApprovalRequestInput>(input).is_err(),
            "命令审批申请不得夹带批准或拒绝决定"
        );
    }
}
