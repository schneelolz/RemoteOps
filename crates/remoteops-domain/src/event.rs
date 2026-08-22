use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    ApprovalId, ControllerInstanceId, ControllerOwnerId, FileTransferId, PermissionMode, RequestId,
    SessionId, ShellId, ShellKind,
};

/// 事件来源。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    /// 人工操作。
    Human,
    /// AI 工具调用。
    Ai,
    /// Agent、Relay 或 Controller 系统事件。
    System,
}

/// 审批状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    /// 不需要审批。
    NotRequired,
    /// 等待人工审批。
    Pending,
    /// 已批准。
    Approved,
    /// 已拒绝。
    Rejected,
    /// 审批已过期。
    Expired,
}

/// 串口数据位。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialDataBits {
    /// 5 数据位。
    Five,
    /// 6 数据位。
    Six,
    /// 7 数据位。
    Seven,
    /// 8 数据位。
    #[default]
    Eight,
}

/// 串口停止位。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialStopBits {
    /// 1 停止位。
    #[default]
    One,
    /// 2 停止位。
    Two,
}

/// 串口校验位。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialParity {
    /// 无校验。
    #[default]
    None,
    /// 奇校验。
    Odd,
    /// 偶校验。
    Even,
}

/// 串口流控方式。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialFlowControl {
    /// 不使用流控。
    #[default]
    None,
    /// XON/XOFF 软件流控。
    Software,
    /// RTS/CTS 硬件流控。
    Hardware,
}

/// 打开串口时使用的完整通信参数。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SerialSettings {
    /// 波特率。
    pub baud_rate: u32,
    /// 数据位。
    pub data_bits: SerialDataBits,
    /// 停止位。
    pub stop_bits: SerialStopBits,
    /// 校验位。
    pub parity: SerialParity,
    /// 流控方式。
    pub flow_control: SerialFlowControl,
}

impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            baud_rate: 9_600,
            data_bits: SerialDataBits::Eight,
            stop_bits: SerialStopBits::One,
            parity: SerialParity::None,
            flow_control: SerialFlowControl::None,
        }
    }
}

/// 串口命令行结束符。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialLineEnding {
    /// 不追加结束符。
    None,
    /// 追加回车。
    #[default]
    Cr,
    /// 追加换行。
    Lf,
    /// 追加回车和换行。
    CrLf,
}

impl SerialLineEnding {
    /// 返回对应原始字节。
    #[must_use]
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::None => b"",
            Self::Cr => b"\r",
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
        }
    }
}

/// 串口终端交互配置。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialTerminalProfile {
    /// 通用 ANSI 终端。
    Ansi,
    /// 华为 VRP Console。
    #[default]
    HuaweiVrp,
}

/// Windows Service 或平台服务操作。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    /// 启动服务。
    Start,
    /// 停止服务。
    Stop,
    /// 重启服务。
    Restart,
}

/// 被控端电源操作。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    /// 重启操作系统。
    Restart,
    /// 关闭操作系统。
    Shutdown,
}

/// 可以进入策略和审计层的远程操作。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteOperation {
    /// 打开持久交互式 Shell。
    OpenShell {
        /// Shell 类型。
        shell: ShellKind,
    },
    /// 在已经打开的持久 Shell 中执行命令。
    RunShellCommand {
        /// 持久 Shell 会话标识。
        shell_id: ShellId,
        /// 打开该会话时绑定的 Shell 类型。
        shell: ShellKind,
        /// 原始命令。
        command: String,
        /// 是否由调用者声明为只读。
        readonly: bool,
    },
    /// 关闭持久交互式 Shell。
    CloseShell {
        /// 持久 Shell 会话标识。
        shell_id: ShellId,
    },
    /// 执行一次命令。
    RunCommand {
        /// Shell 类型。
        shell: ShellKind,
        /// 原始命令。
        command: String,
        /// 是否由调用者声明为只读。
        readonly: bool,
    },
    /// 探测 TCP 端口。
    TestPort {
        /// 目标主机。
        host: String,
        /// 目标端口。
        port: u16,
    },
    /// 上传文件。
    UploadFile {
        /// Agent 目标路径。
        remote_path: String,
        /// 文件大小。
        size: u64,
        /// SHA-256 哈希。
        sha256: String,
        /// 是否允许覆盖。
        overwrite: bool,
    },
    /// 开始一次分块文件上传。
    BeginUploadFile {
        /// 分块传输标识。
        transfer_id: FileTransferId,
        /// Agent 目标路径。
        remote_path: String,
        /// 文件总大小。
        size: u64,
        /// 完整文件 SHA-256。
        sha256: String,
        /// 是否允许覆盖。
        overwrite: bool,
    },
    /// 写入一次文件上传分块。
    UploadFileChunk {
        /// 分块传输标识。
        transfer_id: FileTransferId,
        /// 分块起始偏移。
        offset: u64,
        /// 分块字节数。
        size: u64,
        /// 当前分块 SHA-256。
        sha256: String,
    },
    /// 校验并完成一次分块文件上传。
    CompleteUploadFile {
        /// 分块传输标识。
        transfer_id: FileTransferId,
    },
    /// 取消一次未完成的分块文件上传。
    AbortUploadFile {
        /// 分块传输标识。
        transfer_id: FileTransferId,
    },
    /// 下载文件。
    DownloadFile {
        /// Agent 文件路径。
        remote_path: String,
        /// Controller 是否要覆盖 transfer-root 内的现有本地文件。
        overwrite_local: bool,
    },
    /// 在分块下载前验证控制端本地覆盖授权，不返回文件内容。
    AuthorizeDownloadFile {
        /// Agent 文件路径。
        remote_path: String,
        /// Controller 是否要覆盖 transfer-root 内的现有本地文件。
        overwrite_local: bool,
    },
    /// 分块读取 Agent 文件。
    DownloadFileChunk {
        /// Agent 文件路径。
        remote_path: String,
        /// 分块起始偏移。
        offset: u64,
        /// 本次最多读取的字节数。
        max_bytes: u64,
    },
    /// 读取文件元数据和 SHA-256。
    GetFileMetadata {
        /// Agent 文件路径。
        remote_path: String,
        /// 是否同时流式计算完整文件 SHA-256。
        include_sha256: bool,
    },
    /// 移动或重命名文件。
    MoveFile {
        /// Agent 源文件路径。
        source_path: String,
        /// Agent 目标文件路径。
        destination_path: String,
        /// 是否允许覆盖目标文件。
        overwrite: bool,
    },
    /// 删除单个普通文件。
    DeleteFile {
        /// Agent 文件路径。
        remote_path: String,
    },
    /// 向用户明确指定的单个 TCP 目标发送并接收有界数据。
    TcpExchange {
        /// 目标主机。
        host: String,
        /// 目标端口。
        port: u16,
        /// 发送字节数。
        byte_count: usize,
        /// 发送内容 SHA-256。
        request_sha256: String,
        /// 最多接收字节数。
        max_response_bytes: usize,
        /// 连接和读取超时毫秒数。
        timeout_millis: u64,
    },
    /// 查询当前进程列表。
    ListProcesses,
    /// 终止指定进程树。
    TerminateProcess {
        /// 操作系统进程标识。
        process_id: u32,
    },
    /// 查询系统服务列表。
    ListServices,
    /// 控制指定系统服务。
    ControlService {
        /// 服务短名称。
        service_name: String,
        /// 服务动作。
        action: ServiceAction,
    },
    /// 执行重启或关机。
    PowerControl {
        /// 电源动作。
        action: PowerAction,
    },
    /// 打开串口。
    OpenSerial {
        /// 串口名称。
        port_name: String,
        /// 完整串口通信参数。
        settings: SerialSettings,
        /// 是否允许写入。
        writable: bool,
    },
    /// 枚举 Agent 可见串口。
    ListSerial,
    /// 向串口写入字节。
    WriteSerial {
        /// 串口会话标识。
        serial_session_id: String,
        /// 写入字节数。
        byte_count: usize,
        /// 写入内容 SHA-256。
        sha256: String,
    },
    /// 执行一条有界的结构化串口查询。
    RunSerialQuery {
        /// 串口会话标识。
        serial_session_id: String,
        /// 不含结束符的完整命令。
        command: String,
        /// 行结束符。
        line_ending: SerialLineEnding,
        /// 终端配置。
        profile: SerialTerminalProfile,
        /// 总超时毫秒数。
        overall_timeout_millis: u64,
        /// 空闲完成毫秒数。
        idle_timeout_millis: u64,
        /// 最大接收字节数。
        max_bytes: usize,
        /// 最大自动翻页次数。
        max_pages: u16,
        /// 是否声明为只读查询。
        readonly: bool,
    },
    /// 关闭串口会话。
    CloseSerial {
        /// 串口会话标识。
        serial_session_id: String,
    },
    /// 执行 SSH 命令。
    RunSsh {
        /// SSH 目标。
        host: String,
        /// SSH 端口。
        port: u16,
        /// SSH 用户名。
        username: String,
        /// Agent 本地私钥路径。
        identity_file: Option<String>,
        /// Agent 本地 `known_hosts` 路径。
        known_hosts_file: Option<String>,
        /// 远程命令。
        command: String,
        /// 是否声明为只读命令。
        readonly: bool,
    },
    /// 将控制端本地凭据库中的 SSH 密码注入 Agent 的本地凭据库。
    ProvisionSshCredential {
        /// SSH 目标主机。
        host: String,
        /// SSH 目标端口。
        port: u16,
        /// SSH 用户名。
        username: String,
        /// 控制端本地凭据引用，不是密码。
        credential_ref: String,
    },
    /// 关闭连接。
    CloseConnection,
    /// 人工接管写入权。
    HumanTakeover,
    /// 释放人工接管，让 AI 恢复按策略工作。
    ReleaseHumanTakeover,
    /// 立即中止当前会话中的全部在途操作和交互资源。
    EmergencyStop,
    /// 中断尚未完成的远程请求。
    CancelRequest {
        /// 被中断的请求标识。
        request_id: RequestId,
    },
}

/// 统一事件负载。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    /// 会话已建立。
    SessionOpened,
    /// 会话传输中断。
    SessionDisconnected,
    /// 会话恢复。
    SessionResumed,
    /// 会话关闭。
    SessionClosed,
    /// 操作已请求。
    OperationRequested {
        /// 远程操作。
        operation: RemoteOperation,
    },
    /// 操作需要审批。
    ApprovalRequired {
        /// 审批标识。
        approval_id: ApprovalId,
        /// 审批原因。
        reason: String,
    },
    /// 操作开始。
    OperationStarted,
    /// 标准输出或标准错误分片。
    OutputChunk {
        /// 是否来自标准错误。
        stderr: bool,
        /// UTF-8 文本；非 UTF-8 数据由设备层编码。
        text: String,
    },
    /// 操作完成。
    OperationCompleted {
        /// 退出码；非进程操作可以为空。
        exit_code: Option<i32>,
        /// 结果摘要。
        summary: String,
    },
    /// 操作失败。
    OperationFailed {
        /// 稳定错误码。
        code: String,
        /// 可安全展示的错误信息。
        message: String,
    },
    /// 操作已取消。
    OperationCancelled,
}

/// 人工、AI 和系统共用的会话事件。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteEvent {
    /// 事件序号。
    pub sequence: u64,
    /// 目标会话。
    pub session_id: SessionId,
    /// 可选请求标识。
    pub request_id: Option<RequestId>,
    /// 事件来源。
    pub source: EventSource,
    /// 审批状态。
    pub approval: ApprovalState,
    /// 事件负载。
    pub payload: EventPayload,
    /// 事件时间。
    pub occurred_at: DateTime<Utc>,
}

/// 可持久化或导出的脱敏审计事件。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// 目标会话。
    pub session_id: SessionId,
    /// 可选请求标识。
    pub request_id: Option<RequestId>,
    /// 事件所属的稳定 Controller Owner；旧审计记录可能为空。
    #[serde(default)]
    pub owner_id: Option<ControllerOwnerId>,
    /// 产生该审计记录的 Controller 实例；旧审计记录可能为空。
    #[serde(default)]
    pub controller_instance_id: Option<ControllerInstanceId>,
    /// 事件来源。
    pub source: EventSource,
    /// 脱敏后的操作说明。
    pub action: String,
    /// 脱敏后的结果摘要。
    pub result: String,
    /// 审批状态。
    pub approval: ApprovalState,
    /// 事件发生时生效的权限模式。
    #[serde(default)]
    pub permission_mode: PermissionMode,
    /// 记录时间。
    pub occurred_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_settings_round_trip_with_all_parameters() {
        let operation = RemoteOperation::OpenSerial {
            port_name: "COM7".to_owned(),
            settings: SerialSettings {
                baud_rate: 115_200,
                data_bits: SerialDataBits::Seven,
                stop_bits: SerialStopBits::Two,
                parity: SerialParity::Even,
                flow_control: SerialFlowControl::Hardware,
            },
            writable: true,
        };
        let json = serde_json::to_string(&operation).expect("串口操作应能序列化");
        let restored =
            serde_json::from_str::<RemoteOperation>(&json).expect("串口操作应能反序列化");

        assert_eq!(restored, operation);
        assert!(json.contains("\"baud_rate\":115200"));
        assert!(json.contains("\"data_bits\":\"seven\""));
        assert!(json.contains("\"stop_bits\":\"two\""));
        assert!(json.contains("\"parity\":\"even\""));
        assert!(json.contains("\"flow_control\":\"hardware\""));
    }

    #[test]
    fn structured_serial_query_round_trips_without_losing_bounds() {
        let operation = RemoteOperation::RunSerialQuery {
            serial_session_id: "serial-session-7".to_owned(),
            command: "display interface brief".to_owned(),
            line_ending: SerialLineEnding::Cr,
            profile: SerialTerminalProfile::HuaweiVrp,
            overall_timeout_millis: 30_000,
            idle_timeout_millis: 1_200,
            max_bytes: 128 * 1024,
            max_pages: 50,
            readonly: true,
        };

        let json = serde_json::to_string(&operation).expect("结构化串口查询应能序列化");
        let restored =
            serde_json::from_str::<RemoteOperation>(&json).expect("结构化串口查询应能反序列化");

        assert_eq!(restored, operation);
        assert!(json.contains("\"kind\":\"run_serial_query\""));
        assert!(json.contains("\"profile\":\"huawei_vrp\""));
        assert!(json.contains("\"max_pages\":50"));
    }
}
