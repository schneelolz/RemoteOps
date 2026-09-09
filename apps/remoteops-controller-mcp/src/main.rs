#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use clap::{Parser, ValueEnum};
use remoteops_application::{
    ApplicationError, ControllerKind, OperationResult, RelayClient, RelayClientConfig,
};
use remoteops_audit::sha256_bytes;
use remoteops_domain::{
    ApprovalId, Capability, ControllerInstanceId, ControllerOwnerId, EventSource, FileTransferId,
    PairingCode, PermissionMode, PowerAction, RemoteOperation, SerialDataBits, SerialFlowControl,
    SerialLineEnding, SerialParity, SerialSettings, SerialStopBits, SerialTerminalProfile,
    ServiceAction, SessionId, ShellId, ShellKind, VisualTarget,
};
use remoteops_protocol::{
    ControllerControlMode, ControllerControlModeUpdate, CredentialEncryptionContext,
    PROTOCOL_VERSION, seal_credential,
};
use rmcp::{
    Json, RoleServer, ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        BooleanSchema, ElicitRequestParams, ElicitationAction, ElicitationSchema,
        PrimitiveSchemaDefinition, StringSchema,
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
    process::Command,
    sync::Mutex,
    time::timeout,
};
use tracing_subscriber::EnvFilter;
use zeroize::{Zeroize as _, Zeroizing};

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
    /// Enables local-only MCP UI diagnostics when explicitly requested.
    #[arg(long, env = "REMOTEOPS_ENABLE_TEST_UI", default_value_t = false)]
    enable_test_ui: bool,
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
    enable_test_ui: bool,
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
            enable_test_ui: self.enable_test_ui,
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
    enable_test_ui: bool,
    full_access_grants: Arc<Mutex<BTreeMap<SessionId, FullAccessGrant>>>,
    control_modes: Arc<Mutex<BTreeMap<SessionId, ControllerControlMode>>>,
    ssh_credential_cache: Arc<Mutex<BTreeMap<SshCredentialCacheKey, CachedSshCredential>>>,
    credential_prompt_lock: Arc<Mutex<()>>,
    credential_prompt: Arc<dyn CredentialPrompt>,
}

const FULL_ACCESS_IDLE_TIMEOUT: Duration = Duration::from_hours(1);
const SSH_CREDENTIAL_CACHE_TIMEOUT: Duration = Duration::from_mins(10);
const CREDENTIAL_PROMPT_TIMEOUT: Duration = Duration::from_mins(5);
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TestPromptInput {
    title: String,
    message: String,
    default_value: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct TestPromptOutput {
    submitted: bool,
    action: String,
    value: Option<String>,
    value_length: usize,
    sha256: Option<String>,
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
struct VisualObserveInput {
    session_id: String,
    include_screenshot: Option<bool>,
    include_ui_tree: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VisualWaitInput {
    session_id: String,
    condition: String,
    timeout_millis: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VisualActionInput {
    session_id: String,
    target: String,
    action: Option<String>,
    text: Option<String>,
    input: Option<String>,
    approval_id: Option<String>,
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
#[serde(deny_unknown_fields)]
struct SshInput {
    session_id: String,
    host: String,
    port: Option<u16>,
    username: String,
    identity_file: Option<String>,
    known_hosts_file: Option<String>,
    command: String,
    readonly: bool,
    /// 为 true 时由 MCP 本机安全窗口取得密码；密码不属于工具参数。
    use_password: Option<bool>,
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ClearSshCredentialCacheInput {
    session_id: String,
    host: String,
    port: Option<u16>,
    username: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SshCredentialCacheKey {
    session_id: SessionId,
    host: String,
    port: u16,
    username: String,
}

struct CachedSshCredential {
    password: Zeroizing<String>,
    expires_at: Instant,
}

struct CredentialPromptRequest<'a> {
    host: &'a str,
    port: u16,
    username: &'a str,
    command_sha256: &'a str,
}

struct PromptedCredential {
    password: Zeroizing<String>,
    remember: bool,
}

#[async_trait]
trait CredentialPrompt: Send + Sync {
    async fn prompt(
        &self,
        request: CredentialPromptRequest<'_>,
    ) -> Result<Option<PromptedCredential>, String>;
}

struct ProcessCredentialPrompt;

#[derive(Deserialize)]
struct PromptWireResponse {
    action: String,
    password: Option<String>,
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
    mac_address: Option<String>,
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

#[async_trait]
impl CredentialPrompt for ProcessCredentialPrompt {
    async fn prompt(
        &self,
        request: CredentialPromptRequest<'_>,
    ) -> Result<Option<PromptedCredential>, String> {
        let executable = credential_prompt_executable()?;
        let mut output = timeout(
            CREDENTIAL_PROMPT_TIMEOUT,
            Command::new(executable)
                .arg("--host")
                .arg(request.host)
                .arg("--port")
                .arg(request.port.to_string())
                .arg("--username")
                .arg(request.username)
                .arg("--command-sha256")
                .arg(request.command_sha256)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| "SSH 密码安全输入窗口等待超时".to_owned())?
        .map_err(|error| format!("无法启动 SSH 密码安全输入窗口：{error}"))?;
        if !output.status.success() {
            return Err(format!(
                "SSH 密码安全输入窗口失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        if output.stdout.len() > 8192 {
            return Err("SSH 密码安全输入窗口返回数据过大".to_owned());
        }
        let parsed = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("SSH 密码安全输入结果格式无效：{error}"));
        output.stdout.zeroize();
        let mut wire: PromptWireResponse = parsed?;
        match wire.action.as_str() {
            "cancel" => {
                if let Some(password) = wire.password.as_mut() {
                    password.zeroize();
                }
                Ok(None)
            }
            "use_once" | "remember_10_minutes" => {
                let password = Zeroizing::new(
                    wire.password
                        .take()
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| "SSH 密码不能为空".to_owned())?,
                );
                if password.len() > 4096 {
                    return Err("SSH 密码超过 4096 字节上限".to_owned());
                }
                Ok(Some(PromptedCredential {
                    password,
                    remember: wire.action == "remember_10_minutes",
                }))
            }
            _ => Err("SSH 密码安全输入窗口返回了未知操作".to_owned()),
        }
    }
}

fn credential_prompt_executable() -> Result<PathBuf, String> {
    let directory = std::env::current_exe()
        .map_err(|error| format!("无法定位 MCP 程序目录：{error}"))?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "MCP 程序路径缺少父目录".to_owned())?;
    let file_name = if cfg!(windows) {
        "remoteops-credential-prompt.exe"
    } else {
        "remoteops-credential-prompt"
    };
    let executable = directory.join(file_name);
    executable.is_file().then_some(executable).ok_or_else(|| {
        format!("MCP 安装包缺少本机安全输入程序 {file_name}；请重新安装完整 RemoteOps MCP 包")
    })
}

fn cached_ssh_password(
    cache: &mut BTreeMap<SshCredentialCacheKey, CachedSshCredential>,
    key: &SshCredentialCacheKey,
    now: Instant,
) -> Option<Zeroizing<String>> {
    cache.retain(|_, credential| credential.expires_at > now);
    cache.get(key).map(|credential| credential.password.clone())
}

fn remember_ssh_password(
    cache: &mut BTreeMap<SshCredentialCacheKey, CachedSshCredential>,
    key: SshCredentialCacheKey,
    password: Zeroizing<String>,
    now: Instant,
) {
    cache.insert(
        key,
        CachedSshCredential {
            password,
            expires_at: now + SSH_CREDENTIAL_CACHE_TIMEOUT,
        },
    );
}

fn validate_ssh_prompt_target(host: &str, username: &str) -> Result<(), String> {
    if host.trim().is_empty()
        || username.trim().is_empty()
        || host.starts_with('-')
        || host.chars().any(char::is_whitespace)
        || host.chars().any(char::is_control)
        || username.starts_with('-')
        || username.contains('@')
        || username.chars().any(char::is_whitespace)
        || username.chars().any(char::is_control)
    {
        return Err("SSH 主机或用户名格式无效".to_owned());
    }
    Ok(())
}

fn validate_test_prompt(input: &TestPromptInput) -> Result<(), String> {
    let title = input.title.trim();
    let message = input.message.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return Err("测试窗体标题必须包含 1 到 120 个字符".to_owned());
    }
    if message.is_empty() || message.chars().count() > 2_000 {
        return Err("测试窗体说明必须包含 1 到 2000 个字符".to_owned());
    }
    if input
        .default_value
        .as_ref()
        .is_some_and(|value| value.chars().count() > 4_096)
    {
        return Err("测试默认值不能超过 4096 个字符".to_owned());
    }
    Ok(())
}

fn configured_tool_router(enable_test_ui: bool) -> ToolRouter<RemoteOpsMcp> {
    let mut router = RemoteOpsMcp::tool_router();
    if !enable_test_ui {
        router.remove_route("test_prompt_text");
        router.remove_route("test_prompt_password");
    }
    router
}

#[tool_router]
impl RemoteOpsMcp {
    fn new(
        client: RelayClient,
        transfer_root: PathBuf,
        command_mode: CommandMode,
        enable_test_ui: bool,
    ) -> Self {
        let mcp = Self {
            client,
            shells: Arc::new(Mutex::new(BTreeMap::new())),
            transfer_root: Arc::new(transfer_root),
            command_mode,
            enable_test_ui,
            full_access_grants: Arc::new(Mutex::new(BTreeMap::new())),
            control_modes: Arc::new(Mutex::new(BTreeMap::new())),
            ssh_credential_cache: Arc::new(Mutex::new(BTreeMap::new())),
            credential_prompt_lock: Arc::new(Mutex::new(())),
            credential_prompt: Arc::new(ProcessCredentialPrompt),
        };
        let reporter = mcp.clone();
        tokio::spawn(async move { reporter.control_mode_reporter().await });
        mcp
    }

    async fn control_mode_reporter(&self) {
        let mut ticker = tokio::time::interval(Duration::from_secs(15));
        loop {
            ticker.tick().await;
            self.report_current_control_modes().await;
        }
    }

    async fn report_current_control_modes(&self) {
        let connections = self.client.list_connections().await;
        let now = Instant::now();
        let expired = {
            let mut grants = self.full_access_grants.lock().await;
            let expired = grants
                .iter()
                .filter_map(|(session_id, grant)| (!grant.is_active_at(now)).then_some(*session_id))
                .collect::<Vec<_>>();
            for session_id in &expired {
                grants.remove(session_id);
            }
            expired
        };
        if !expired.is_empty() {
            let mut modes = self.control_modes.lock().await;
            for session_id in expired {
                modes.insert(session_id, ControllerControlMode::Expired);
            }
        }
        let modes = self.control_modes.lock().await;
        let fallback = match self.command_mode {
            CommandMode::Readonly => ControllerControlMode::ReadOnly,
            CommandMode::Approval => ControllerControlMode::ExternalApproval,
            CommandMode::FullAccess => ControllerControlMode::FullAccess,
            CommandMode::AgentControlled => ControllerControlMode::StepByStep,
        };
        let updates = connections
            .into_iter()
            .map(|connection| {
                let session_id = connection.session_id;
                let mode = modes.get(&session_id).copied().unwrap_or(fallback);
                ControllerControlModeUpdate { session_id, mode }
            })
            .collect();
        let _ = self.client.report_controller_control_modes(updates).await;
    }

    fn runtime_tool_router(&self) -> ToolRouter<Self> {
        configured_tool_router(self.enable_test_ui)
    }

    async fn run_test_prompt(
        &self,
        context: &RequestContext<RoleServer>,
        input: TestPromptInput,
        password: bool,
    ) -> Result<TestPromptOutput, String> {
        validate_test_prompt(&input)?;
        let mut field = StringSchema::new()
            .title(input.title)
            .description(if password {
                "测试密码只用于验证输入界面，不会返回明文或写入日志"
            } else {
                "输入测试文本并提交"
            })
            .max_length(4_096);
        if !password {
            field = field.with_default(input.default_value.unwrap_or_default());
        }
        let property_name = if password { "password" } else { "value" };
        let schema = ElicitationSchema::builder()
            .required_property(property_name, PrimitiveSchemaDefinition::String(field))
            .build()
            .map_err(str::to_owned)?;
        let response = context
            .peer
            .create_elicitation_with_timeout(
                ElicitRequestParams::FormElicitationParams {
                    meta: None,
                    message: input.message,
                    requested_schema: schema,
                },
                Some(ELICITATION_TIMEOUT),
            )
            .await
            .map_err(|error| elicitation_unavailable_error(&error.to_string()))?;

        if response.action != ElicitationAction::Accept {
            return Ok(TestPromptOutput {
                submitted: false,
                action: match response.action {
                    ElicitationAction::Decline => "decline",
                    ElicitationAction::Cancel => "cancel",
                    _ => "unknown",
                }
                .to_owned(),
                value: None,
                value_length: 0,
                sha256: None,
            });
        }
        let value = response
            .content
            .as_ref()
            .and_then(|content| content.get(property_name))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "测试输入结果缺少 value 字段".to_owned())?;
        let mut value = Zeroizing::new(value.to_owned());
        let output = TestPromptOutput {
            submitted: true,
            action: "accept".to_owned(),
            value: (!password).then(|| value.to_string()),
            value_length: value.chars().count(),
            sha256: Some(sha256_bytes(value.as_bytes())),
        };
        value.zeroize();
        Ok(output)
    }

    #[tool(
        name = "test_prompt_text",
        description = "仅在显式启用测试 UI 时可见。弹出 MCP 标准普通文本输入表单并返回测试内容、长度和 SHA-256。不得用于收集真实敏感信息。",
        annotations(
            title = "测试普通文本输入",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn test_prompt_text(
        &self,
        Parameters(input): Parameters<TestPromptInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<TestPromptOutput>, String> {
        Ok(Json(self.run_test_prompt(&context, input, false).await?))
    }

    #[tool(
        name = "test_prompt_password",
        description = "仅在显式启用测试 UI 时可见。弹出 MCP 标准密码测试表单；只返回是否提交、长度和 SHA-256，绝不返回密码明文。",
        annotations(
            title = "测试密码输入",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn test_prompt_password(
        &self,
        Parameters(input): Parameters<TestPromptInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<TestPromptOutput>, String> {
        Ok(Json(self.run_test_prompt(&context, input, true).await?))
    }

    async fn ssh_password(
        &self,
        key: &SshCredentialCacheKey,
        command_sha256: &str,
    ) -> Result<Zeroizing<String>, String> {
        {
            let mut cache = self.ssh_credential_cache.lock().await;
            if let Some(password) = cached_ssh_password(&mut cache, key, Instant::now()) {
                return Ok(password);
            }
        }
        // Only one native credential window may be visible at a time. Recheck the
        // cache after waiting so a concurrent request can reuse a remembered value.
        let _prompt_guard = self.credential_prompt_lock.lock().await;
        {
            let mut cache = self.ssh_credential_cache.lock().await;
            if let Some(password) = cached_ssh_password(&mut cache, key, Instant::now()) {
                return Ok(password);
            }
        }
        let prompted = self
            .credential_prompt
            .prompt(CredentialPromptRequest {
                host: &key.host,
                port: key.port,
                username: &key.username,
                command_sha256,
            })
            .await?
            .ok_or_else(|| "用户取消了 SSH 密码输入".to_owned())?;
        if prompted.remember {
            let mut cache = self.ssh_credential_cache.lock().await;
            remember_ssh_password(
                &mut cache,
                key.clone(),
                prompted.password.clone(),
                Instant::now(),
            );
        }
        Ok(prompted.password)
    }

    async fn clear_cached_ssh_credentials_for_session(&self, session_id: SessionId) {
        self.ssh_credential_cache
            .lock()
            .await
            .retain(|key, _| key.session_id != session_id);
    }

    async fn encrypted_ssh_payload(
        &self,
        session_id: SessionId,
        operation: &RemoteOperation,
    ) -> Result<String, String> {
        let RemoteOperation::RunSsh {
            host,
            port,
            username,
            command,
            ..
        } = operation
        else {
            return Err("只有 SSH 操作可以携带加密密码".to_owned());
        };
        let connection = self
            .client
            .list_connections()
            .await
            .into_iter()
            .find(|connection| connection.session_id == session_id)
            .ok_or_else(|| "未找到 session_id".to_owned())?;
        if connection.state != remoteops_domain::ConnectionState::Online {
            return Err("Agent 当前不在线，不能请求或发送 SSH 密码".to_owned());
        }
        if connection.credential_encryption_public_key.is_empty()
            || connection.credential_encryption_key_id.is_empty()
        {
            return Err("Agent 未提供 v15 凭据加密公钥；请同步升级 Agent、Relay 和 MCP".to_owned());
        }
        let cache_key = SshCredentialCacheKey {
            session_id,
            host: host.trim().to_ascii_lowercase(),
            port: *port,
            username: username.trim().to_owned(),
        };
        let command_sha256 = sha256_bytes(command.as_bytes());
        let mut password = self.ssh_password(&cache_key, &command_sha256).await?;
        let context = CredentialEncryptionContext {
            protocol_version: PROTOCOL_VERSION,
            agent_instance_id: connection.agent_instance_id,
            session_id,
            envelope_id: remoteops_domain::RequestId::new(),
            host: cache_key.host,
            port: cache_key.port,
            username: cache_key.username,
            command_sha256,
        };
        let encrypted = seal_credential(
            &connection.credential_encryption_public_key,
            &connection.credential_encryption_key_id,
            &context,
            password.as_bytes(),
        )
        .map_err(|error| error.to_string())?;
        password.zeroize();
        let serialized = serde_json::to_vec(&encrypted)
            .map_err(|error| format!("无法编码 SSH 加密凭据：{error}"))?;
        Ok(BASE64.encode(serialized))
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
            self.control_modes
                .lock()
                .await
                .insert(session_id, ControllerControlMode::Expired);
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
                            "在当前 MCP 确认界面点击允许即表示仅确认本次操作；这不是完全控制授权。Agent 端没有授权按钮",
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
            .map_err(|error| elicitation_unavailable_error(&error.to_string()))?;
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
            Err(format!(
                "当前 MCP 确认界面未允许{direction}大文件，操作未执行；不得自动切换到完全控制"
            ))
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
                "当前 MCP 确认界面未允许本次操作：{action}，操作未执行；不得自动切换到完全控制；Agent 端没有授权按钮"
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
        self.report_current_control_modes().await;
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
        self.control_modes
            .lock()
            .await
            .insert(session_id, ControllerControlMode::StepByStep);
        self.report_current_control_modes().await;
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
        description = "按精确 session_id 切换控制模式。默认保持 step_by_step；不得因为逐项确认界面不可用、超时或写操作被拒绝而调用 full_access。只有用户明确要求完全控制时才调用。调用 full_access 时只使用 Codex 对本工具的授权，不再嵌套弹出第二次确认；Agent 端没有完全控制按钮。切回逐项确认立即生效。",
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
                self.control_modes
                    .lock()
                    .await
                    .insert(session_id, ControllerControlMode::StepByStep);
            }
            McpControlMode::FullAccess => {
                self.full_access_grants.lock().await.insert(
                    session_id,
                    FullAccessGrant {
                        last_successful_use: Instant::now(),
                    },
                );
                self.control_modes
                    .lock()
                    .await
                    .insert(session_id, ControllerControlMode::FullAccess);
            }
        }
        self.report_current_control_modes().await;
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

    /// 获取交互式 Windows 桌面的 UIA 状态和可选截图。
    #[tool(
        name = "desktop_capabilities",
        description = "查询现场 Windows 交互式桌面是否可用，以及窗口/UIA/截图/输入能力。",
        annotations(
            title = "查询图形能力",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn desktop_capabilities(
        &self,
        Parameters(input): Parameters<TargetInput>,
    ) -> Result<Json<ActionOutput>, String> {
        let mut output = self
            .execute_simple(
                input.session_id,
                RemoteOperation::VisualObserve {
                    include_screenshot: false,
                    include_ui_tree: true,
                },
                None,
                None,
            )
            .await?;
        if let Some(observation) = output.0.details.take() {
            let uia_available = visual_uia_available(&observation);
            let interactive_desktop = observation
                .get("state")
                .is_some_and(|value| value == "ready");
            let windows_available = observation
                .get("windows")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|windows| !windows.is_empty());
            let displays_available = observation
                .get("displays")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|displays| !displays.is_empty());
            let foreground_available = observation
                .get("active_window_fingerprint")
                .is_some_and(|value| value.as_str().is_some_and(|text| !text.is_empty()));
            let mut unavailable_reasons = Vec::new();
            if !interactive_desktop {
                unavailable_reasons.push("interactive_desktop_unavailable");
            }
            if !windows_available {
                unavailable_reasons.push("window_enumeration_unavailable");
            }
            if !foreground_available {
                unavailable_reasons.push("foreground_window_unavailable");
            }
            if !uia_available {
                unavailable_reasons.push("ui_automation_unavailable");
            }
            output.0.details = Some(serde_json::json!({
                "provider_instance_id": observation.get("provider_instance_id"),
                "state": observation.get("state"),
                "interactive_desktop": interactive_desktop,
                "initialized": interactive_desktop && displays_available,
                "capabilities": {
                    "visual": interactive_desktop && windows_available,
                    "screenshot": interactive_desktop && displays_available,
                    "ui_automation": interactive_desktop && windows_available && foreground_available && uia_available,
                    "control_invoke": interactive_desktop && windows_available && foreground_available && uia_available,
                    "text_input": interactive_desktop && windows_available && foreground_available && uia_available,
                    "synthetic_input": interactive_desktop && windows_available && foreground_available && !uia_available
                },
                "unavailable_reasons": unavailable_reasons,
                "observation": observation
            }));
        }
        Ok(output)
    }

    /// 观察交互式桌面窗口、UIA 树和按需截图。
    #[tool(
        name = "observe_window",
        description = "观察精确 session_id 上的 Windows 窗口和 UI Automation 状态；按需返回截图。",
        annotations(
            title = "观察 Windows 界面",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn observe_window(
        &self,
        Parameters(input): Parameters<VisualObserveInput>,
    ) -> Result<Json<ActionOutput>, String> {
        self.execute_simple(
            input.session_id,
            RemoteOperation::VisualObserve {
                include_screenshot: input.include_screenshot.unwrap_or(true),
                include_ui_tree: input.include_ui_tree.unwrap_or(true),
            },
            None,
            None,
        )
        .await
    }

    /// 等待窗口、文本或 UI 状态变化。
    #[tool(
        name = "wait_for_visual_state",
        description = "等待现场 Windows 桌面满足指定条件，避免 AI 盲目重复截图。",
        annotations(
            title = "等待界面状态",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn wait_for_visual_state(
        &self,
        Parameters(input): Parameters<VisualWaitInput>,
    ) -> Result<Json<ActionOutput>, String> {
        self.execute_simple(
            input.session_id,
            RemoteOperation::VisualWaitFor {
                condition: input.condition,
                timeout_millis: input.timeout_millis.unwrap_or(30_000).min(120_000),
            },
            None,
            None,
        )
        .await
    }

    /// 通过 UIA 语义控件调用低风险动作。
    #[tool(
        name = "invoke_control",
        description = "按 UIA 控件目标调用按钮、菜单、选择或切换动作；AI 默认需要当前用户逐项确认。target 必须是 VisualTarget JSON。",
        annotations(
            title = "操作 Windows 控件",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn invoke_control(
        &self,
        Parameters(input): Parameters<VisualActionInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let target = parse_visual_target(&input.target)?;
        let action = input.action.unwrap_or_else(|| "invoke".to_owned());
        let authorization = self
            .authorize_mutation(
                &context,
                session_id,
                &format!("图形控件动作：{action}"),
                input.approval_id,
            )
            .await?;
        self.execute_authorized(
            session_id,
            RemoteOperation::VisualInvoke { target, action },
            authorization,
            None,
        )
        .await
    }

    /// 向已验证的 UIA 文本控件输入文字。
    #[tool(
        name = "type_text",
        description = "向已验证的 Windows 文本控件输入文字；密码和敏感凭据不能使用此工具。",
        annotations(
            title = "输入 Windows 文本",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn type_text(
        &self,
        Parameters(input): Parameters<VisualActionInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let target = parse_visual_target(&input.target)?;
        let text = input.text.ok_or_else(|| "type_text 缺少 text".to_owned())?;
        let authorization = self
            .authorize_mutation(&context, session_id, "图形文本输入", input.approval_id)
            .await?;
        self.execute_authorized(
            session_id,
            RemoteOperation::VisualTypeText { target, text },
            authorization,
            None,
        )
        .await
    }

    /// UIA 不可用时执行经审批的坐标或键鼠输入回退。
    #[tool(
        name = "send_input",
        description = "在 UIA 不可用时执行坐标或键鼠输入回退；要求交互式桌面和当前前台窗口。",
        annotations(
            title = "发送 Windows 输入",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn send_input(
        &self,
        Parameters(input): Parameters<VisualActionInput>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        let target = parse_visual_target(&input.target)?;
        let input_json = input
            .input
            .ok_or_else(|| "send_input 缺少 input".to_owned())?;
        let authorization = self
            .authorize_mutation(&context, session_id, "图形键鼠输入", input.approval_id)
            .await?;
        self.execute_authorized(
            session_id,
            RemoteOperation::VisualSendInput {
                target,
                input: input_json,
            },
            authorization,
            None,
        )
        .await
    }

    /// 停止图形 Provider 会话。
    #[tool(
        name = "stop_visual_session",
        description = "停止现场 Windows 图形 Provider，撤销当前图形输入租约。",
        annotations(
            title = "停止图形会话",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn stop_visual_session(
        &self,
        Parameters(input): Parameters<TargetInput>,
    ) -> Result<Json<ActionOutput>, String> {
        self.execute_simple(input.session_id, RemoteOperation::VisualStop, None, None)
            .await
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
        description = "由 Agent 连接明确 SSH 目标；设置 use_password=true 时，MCP 在本机安全窗口取得密码并端到端加密，密码不得写入对话或工具参数。非只读命令默认必须审批。",
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
        let port = input.port.unwrap_or(22);
        let use_password = input.use_password.unwrap_or(false);
        validate_ssh_prompt_target(&input.host, &input.username)?;
        if use_password && input.identity_file.is_some() {
            return Err("密码 SSH 不能同时指定 identity_file".to_owned());
        }
        if use_password && input.known_hosts_file.is_some() {
            return Err("密码 SSH 使用 Agent 专用 known_hosts，不能由控制端指定路径".to_owned());
        }
        let authorization = if input.readonly {
            None
        } else {
            Some(
                self.authorize_mutation(&context, session_id, "非只读 SSH 命令", input.approval_id)
                    .await?,
            )
        };
        let operation = RemoteOperation::RunSsh {
            host: input.host,
            port,
            username: input.username,
            identity_file: input.identity_file,
            known_hosts_file: input.known_hosts_file,
            command: input.command,
            readonly: input.readonly,
        };
        let payload = if use_password {
            Some(self.encrypted_ssh_payload(session_id, &operation).await?)
        } else {
            None
        };
        if let Some(authorization) = authorization {
            self.execute_authorized(session_id, operation, authorization, payload)
                .await
        } else {
            self.execute_simple(session_id.to_string(), operation, None, payload)
                .await
        }
    }

    #[tool(
        name = "clear_ssh_credential_cache",
        description = "清除 MCP 本机内存中为精确 session_id、主机、端口和用户名短时保存的 SSH 密码；不会向 Relay 或 Agent 发送密码。",
        annotations(
            title = "清除 SSH 密码缓存",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn clear_ssh_credential_cache(
        &self,
        Parameters(input): Parameters<ClearSshCredentialCacheInput>,
    ) -> Result<Json<ActionOutput>, String> {
        let session_id = parse_session_id(&input.session_id)?;
        self.ensure_connection_exists(session_id).await?;
        let key = SshCredentialCacheKey {
            session_id,
            host: input.host.trim().to_ascii_lowercase(),
            port: input.port.unwrap_or(22),
            username: input.username.trim().to_owned(),
        };
        let cleared = self
            .ssh_credential_cache
            .lock()
            .await
            .remove(&key)
            .is_some();
        Ok(Json(ActionOutput {
            status: "completed".to_owned(),
            session_id: session_id.to_string(),
            request_id: None,
            exit_code: Some(0),
            summary: if cleared {
                "已清除精确目标的 SSH 密码缓存"
            } else {
                "精确目标没有活动的 SSH 密码缓存"
            }
            .to_owned(),
            approval_id: None,
            sha256: None,
            details: None,
        }))
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
        self.control_modes.lock().await.remove(&session_id);
        self.report_current_control_modes().await;
        self.clear_cached_ssh_credentials_for_session(session_id)
            .await;
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

#[allow(unknown_lints)]
#[allow(clippy::unused_async_trait_impl)]
#[tool_handler(
    router = self.runtime_tool_router(),
    name = "remoteops-controller",
    version = "0.2.0-preview.5",
    instructions = "RemoteOps 是控制台与结构化工具驱动的远程诊断，不是远程桌面。仅当用户明确提到 RemoteOps、Relay、RemoteOps Agent、控制码/配对码，或明确要求使用 RemoteOps 时，才接管远程任务；普通服务器、云主机、跳板机、SSH、Shell 或其他远程运维请求不属于本 MCP，不要强制改用 RemoteOps。新 Agent 只需填写 Relay 地址并等待显示九位控制码，不需要入网码或部署级注册 Token。用户提供 RemoteOps 控制码、配对码或 Agent 显示的九位码时，必须先调用 pair_connection；RemoteOps 任务中不要改用 Computer Use、屏幕操作、本机 Shell 或 SSH 直连。配对后默认逐项确认，Agent 端没有逐项确认或完全控制按钮，绝对不要引导用户去 Agent 点击授权。已有连接时先调用 list_connections，再用返回的不可变 session_id 调用 get_target_info 和其他工具，别名只用于核对。检查、分析、判断等请求默认只读，优先使用结构化工具或一次性 Shell 的 run_readonly_command；持久 Shell 保留目录、变量和模块状态，任何命令都必须走 run_command 的逐项确认或完全控制路径。SSH 密码绝不能写入对话、提示词或 MCP 参数；需要密码时对 run_ssh 设置 use_password=true，由本机安全窗口直接向用户获取并端到端加密。修改操作在逐项确认模式下由 MCP 向当前用户确认；如果逐项确认不可用、确认界面不存在、超时或确认未完成，必须视为操作未执行并停止，不得自动切换到完全控制。只有用户明确要求完全控制时才调用一次 set_control_mode，Codex 对该工具的授权就是唯一确认，不得再要求 Agent 或用户执行第二次授权。完全控制按 session_id 独立保存在 MCP 内存，空闲一小时自动失效，成功操作才续期；工具返回 full_access 后立即继续任务。request_action_approval 仅保留给独立 Human Controller 的未来/兼容流程，普通 MCP 首版不依赖它。连接或工具不可用时明确报告，禁止声称已操作远端。不要向用户输出 Token、session_id、approval_id、恢复令牌或任何密码。"
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

    let service = RemoteOpsMcp::new(
        client,
        transfer_root,
        args.command_mode,
        args.enable_test_ui,
    )
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

fn parse_visual_target(value: &str) -> Result<VisualTarget, String> {
    serde_json::from_str(value).map_err(|error| format!("VisualTarget JSON 无效：{error}"))
}

fn elicitation_accepted(action: &ElicitationAction) -> bool {
    matches!(action, ElicitationAction::Accept)
}

fn elicitation_unavailable_error(error: &str) -> String {
    format!(
        "RemoteOps 逐项确认未完成：当前 MCP 客户端未提供可用的确认结果（可能不支持或未显示确认界面），本次操作未执行。请停止并向用户说明原因；不得将用户对具体操作的授权解释为完全控制授权，也不得自动切换控制模式。只有用户明确要求启用完全控制时，才可另行请求该模式。底层错误：{error}"
    )
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

/// 只接受 Provider 明确确认的 UIA 可用状态，错误对象不代表成功。
fn visual_uia_available(observation: &serde_json::Value) -> bool {
    observation
        .get("ui_tree")
        .and_then(|tree| tree.get("available"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

fn connection_output(connection: remoteops_domain::ConnectionDescriptor) -> ConnectionOutput {
    ConnectionOutput {
        display_name: connection.display_name(),
        alias: connection.alias,
        session_id: connection.session_id.to_string(),
        agent_instance_id: connection.agent_instance_id.to_string(),
        hostname: connection.hostname,
        mac_address: connection.mac_address,
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
    #[test]
    fn visual_uia_requires_explicit_success() {
        for observation in [
            serde_json::json!({}),
            serde_json::json!({"ui_tree": null}),
            serde_json::json!({"ui_tree": {}}),
            serde_json::json!({"ui_tree": {"available": false, "reason": "uia_root_unavailable"}}),
        ] {
            assert!(!super::visual_uia_available(&observation));
        }
        assert!(super::visual_uia_available(&serde_json::json!({
            "ui_tree": {"available": true, "name": "Explorer", "children": []}
        })));
    }

    use super::*;

    struct FakeCredentialPrompt {
        response: Mutex<Option<PromptedCredential>>,
    }

    #[async_trait]
    impl CredentialPrompt for FakeCredentialPrompt {
        async fn prompt(
            &self,
            _request: CredentialPromptRequest<'_>,
        ) -> Result<Option<PromptedCredential>, String> {
            Ok(self.response.lock().await.take())
        }
    }

    #[test]
    fn codex_allow_action_confirms_without_boolean_form_value() {
        assert!(elicitation_accepted(&ElicitationAction::Accept));
        assert!(!elicitation_accepted(&ElicitationAction::Decline));
        assert!(!elicitation_accepted(&ElicitationAction::Cancel));
    }

    #[test]
    fn unavailable_elicitation_explicitly_blocks_permission_escalation() {
        let error = elicitation_unavailable_error("client_not_supported");
        assert!(error.contains("操作未执行"));
        assert!(error.contains("不得将用户对具体操作的授权解释为完全控制授权"));
        assert!(error.contains("不得自动切换控制模式"));
        assert!(error.contains("只有用户明确要求启用完全控制时"));
        assert!(error.contains("client_not_supported"));
    }

    #[test]
    fn ssh_tool_input_rejects_inline_passwords() {
        let input = serde_json::json!({
            "session_id": SessionId::new().to_string(),
            "host": "192.0.2.10",
            "port": 22,
            "username": "admin",
            "identity_file": null,
            "known_hosts_file": null,
            "command": "display version",
            "readonly": true,
            "use_password": true,
            "approval_id": null,
            "password": "must-not-enter-mcp-arguments"
        });
        assert!(serde_json::from_value::<SshInput>(input).is_err());

        let schema =
            serde_json::to_value(schemars::schema_for!(SshInput)).expect("SSH 输入架构应可序列化");
        assert!(schema["properties"].get("password").is_none());
        assert!(schema["properties"].get("use_password").is_some());
        assert!(validate_ssh_prompt_target("192.0.2.10", "admin").is_ok());
        assert!(validate_ssh_prompt_target("-oProxyCommand=calc", "admin").is_err());
        assert!(validate_ssh_prompt_target("192.0.2.10", "bad@user").is_err());
    }

    #[test]
    fn test_prompt_input_is_bounded_and_has_no_password_field() {
        let input = TestPromptInput {
            title: "输入测试信息".to_owned(),
            message: "请填写测试内容".to_owned(),
            default_value: Some("default".to_owned()),
        };
        assert!(validate_test_prompt(&input).is_ok());

        let schema = serde_json::to_value(schemars::schema_for!(TestPromptInput))
            .expect("测试窗体输入架构应可序列化");
        assert!(schema["properties"].get("password").is_none());
        assert!(schema["properties"].get("message").is_some());
        assert!(schema["properties"].get("default_value").is_some());
    }

    #[test]
    fn test_prompt_input_rejects_oversized_copy() {
        let input = TestPromptInput {
            title: "标题".to_owned(),
            message: "说明".to_owned(),
            default_value: Some("x".repeat(4_097)),
        };
        assert!(validate_test_prompt(&input).is_err());
    }

    #[test]
    fn password_prompt_result_never_serializes_secret_value() {
        let output = TestPromptOutput {
            submitted: true,
            action: "accept".to_owned(),
            value: None,
            value_length: 12,
            sha256: Some("a".repeat(64)),
        };
        let serialized = serde_json::to_string(&output).expect("测试结果应可序列化");
        assert!(!serialized.contains("password"));
        assert!(!serialized.contains("secret"));
        assert!(serialized.contains("value_length"));
        assert!(serialized.contains("sha256"));
    }

    #[test]
    fn test_prompt_tools_are_hidden_by_default_and_enabled_explicitly() {
        let disabled = configured_tool_router(false);
        assert!(!disabled.has_route("test_prompt_text"));
        assert!(!disabled.has_route("test_prompt_password"));

        let enabled = configured_tool_router(true);
        assert!(enabled.has_route("test_prompt_text"));
        assert!(enabled.has_route("test_prompt_password"));
    }

    #[test]
    fn ssh_credential_cache_is_exact_and_has_fixed_expiry() {
        let now = Instant::now();
        let key = SshCredentialCacheKey {
            session_id: SessionId::new(),
            host: "192.0.2.10".to_owned(),
            port: 22,
            username: "admin".to_owned(),
        };
        let mut cache = BTreeMap::new();
        remember_ssh_password(
            &mut cache,
            key.clone(),
            Zeroizing::new("secret-value".to_owned()),
            now,
        );
        assert_eq!(
            cached_ssh_password(&mut cache, &key, now)
                .expect("精确目标应命中缓存")
                .as_str(),
            "secret-value"
        );

        let mut other = key.clone();
        other.session_id = SessionId::new();
        assert!(cached_ssh_password(&mut cache, &other, now).is_none());
        assert!(
            cached_ssh_password(&mut cache, &key, now + SSH_CREDENTIAL_CACHE_TIMEOUT,).is_none()
        );
        assert!(cache.is_empty());
    }

    #[tokio::test]
    async fn credential_prompt_abstraction_supports_cancel_and_memory_choice() {
        let prompt = FakeCredentialPrompt {
            response: Mutex::new(Some(PromptedCredential {
                password: Zeroizing::new("secret-value".to_owned()),
                remember: true,
            })),
        };
        let prompted = prompt
            .prompt(CredentialPromptRequest {
                host: "192.0.2.10",
                port: 22,
                username: "admin",
                command_sha256: &"a".repeat(64),
            })
            .await
            .expect("假安全窗口应成功")
            .expect("假安全窗口应返回凭据");
        assert!(prompted.remember);
        assert_eq!(prompted.password.as_str(), "secret-value");
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
            enable_test_ui: false,
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
        let started = Instant::now();
        let grant = FullAccessGrant {
            last_successful_use: started,
        };
        // Windows 新启动的 runner 不一定支持将 Instant 向过去回退一小时。
        // 从授权时间向未来推进，避免测试依赖宿主机已运行多久。
        assert!(grant.is_active_at(started));
        assert!(grant.is_active_at(started + Duration::from_secs(3_599)));
        assert!(!grant.is_active_at(started + Duration::from_hours(1)));
        assert!(!grant.is_active_at(started + Duration::from_secs(3_601)));
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
