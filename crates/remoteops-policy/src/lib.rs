//! 远程操作风险分类、审批和人工接管策略。

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use remoteops_domain::{
    ApprovalId, ApprovalState, EventSource, PermissionMode, RemoteOperation, SessionId, ShellKind,
};
use remoteops_serial::{SerialQueryRisk, classify_serial_query};
use serde::{Deserialize, Serialize};

/// 操作风险等级。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// 信息读取和连通性检查。
    ReadOnly,
    /// 可能改变远程状态。
    Mutating,
    /// 执行程序、重启、服务或安全策略等高风险动作。
    High,
}

/// 策略评估结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyDecision {
    /// 可以立即执行。
    Allow,
    /// 必须由人工批准。
    RequireApproval {
        /// 风险等级。
        risk: RiskLevel,
        /// 面向人工的原因。
        reason: String,
    },
    /// 当前策略明确拒绝。
    Deny {
        /// 拒绝原因。
        reason: String,
    },
}

/// 判断一次已批准操作是否与待执行操作具有相同安全语义。
///
/// 分块上传的 `transfer_id` 只用于关联后续分块，不改变人工审批所关心的路径、
/// 大小、完整哈希和覆盖行为，因此允许旧上传审批与新的开始上传操作互相匹配。
#[must_use]
pub fn approval_operations_match(approved: &RemoteOperation, requested: &RemoteOperation) -> bool {
    match (approved, requested) {
        (
            RemoteOperation::UploadFile {
                remote_path: approved_path,
                size: approved_size,
                sha256: approved_sha256,
                overwrite: approved_overwrite,
            },
            RemoteOperation::BeginUploadFile {
                remote_path: requested_path,
                size: requested_size,
                sha256: requested_sha256,
                overwrite: requested_overwrite,
                ..
            },
        )
        | (
            RemoteOperation::BeginUploadFile {
                remote_path: approved_path,
                size: approved_size,
                sha256: approved_sha256,
                overwrite: approved_overwrite,
                ..
            },
            RemoteOperation::UploadFile {
                remote_path: requested_path,
                size: requested_size,
                sha256: requested_sha256,
                overwrite: requested_overwrite,
            },
        ) => {
            approved_path == requested_path
                && approved_size == requested_size
                && approved_sha256.eq_ignore_ascii_case(requested_sha256)
                && approved_overwrite == requested_overwrite
        }
        (
            RemoteOperation::DownloadFile {
                remote_path: approved_path,
                overwrite_local: approved_overwrite,
            },
            RemoteOperation::AuthorizeDownloadFile {
                remote_path: requested_path,
                overwrite_local: requested_overwrite,
            },
        )
        | (
            RemoteOperation::AuthorizeDownloadFile {
                remote_path: approved_path,
                overwrite_local: approved_overwrite,
            },
            RemoteOperation::DownloadFile {
                remote_path: requested_path,
                overwrite_local: requested_overwrite,
            },
        ) => approved_path == requested_path && approved_overwrite == requested_overwrite,
        _ => approved == requested,
    }
}

/// 审批记录。
#[derive(Clone, Debug)]
pub struct ApprovalRecord {
    /// 审批标识。
    pub approval_id: ApprovalId,
    /// 目标会话。
    pub session_id: SessionId,
    /// 被审批操作。
    pub operation: RemoteOperation,
    /// 当前状态。
    pub state: ApprovalState,
    /// 到期时间。
    pub expires_at: DateTime<Utc>,
}

/// 首版默认操作策略。
pub struct DefaultPolicy {
    high_risk_patterns: Vec<Regex>,
}

impl Default for DefaultPolicy {
    fn default() -> Self {
        Self {
            high_risk_patterns: compile_patterns(&[
                r"(?i)\b(shutdown|restart-computer|stop-computer|reboot)\b",
                r"(?i)\b(sc(\.exe)?\s+(delete|stop|config)|set-service|stop-service)\b",
                r"(?i)\b(reg(\.exe)?\s+(add|delete)|set-executionpolicy)\b",
                r"(?i)\b(format|diskpart|bcdedit|cipher\s+/w)\b",
                r"(?i)\b(invoke-expression|iex|start-process)\b",
            ]),
        }
    }
}

impl DefaultPolicy {
    /// 评估远程操作是否可以立即执行。
    ///
    /// 返回值只描述策略结果，不执行操作，也不代表网络端已经接受请求。调用
    /// 方仍需使用 Session、Owner 和 Relay 的认证结果完成后续校验。
    #[must_use]
    pub fn evaluate(&self, source: EventSource, operation: &RemoteOperation) -> PolicyDecision {
        self.evaluate_with_mode(PermissionMode::ApprovalRequired, source, operation)
    }

    /// 按会话权限模式评估远程操作。
    ///
    /// 只读声明会重新根据具体命令计算；命令与白名单不匹配时，即使调用方
    /// 声明 `readonly` 也会被拒绝。高风险操作通常返回需要审批的决定。
    #[must_use]
    pub fn evaluate_with_mode(
        &self,
        permission_mode: PermissionMode,
        source: EventSource,
        operation: &RemoteOperation,
    ) -> PolicyDecision {
        let risk = self.classify(operation);

        if matches!(
            operation,
            RemoteOperation::RunCommand {
                readonly: true,
                shell,
                command,
                ..
            } if self.classify_command(Some(*shell), command) != RiskLevel::ReadOnly
        ) {
            return PolicyDecision::Deny {
                reason: "命令未匹配结构化只读白名单，已拒绝执行".to_owned(),
            };
        }
        if matches!(
            operation,
            RemoteOperation::RunSerialQuery {
                readonly: true,
                profile,
                command,
                ..
            } if classify_serial_query(*profile, command) != SerialQueryRisk::ReadOnly
        ) {
            return PolicyDecision::Deny {
                reason: "串口命令未匹配设备只读白名单，已拒绝执行".to_owned(),
            };
        }
        if matches!(
            operation,
            RemoteOperation::RunShellCommand { readonly: true, .. }
        ) {
            return PolicyDecision::Deny {
                reason: "持久 Shell 保留可变状态，不能声明为免确认只读命令".to_owned(),
            };
        }
        if matches!(
            operation,
            RemoteOperation::RunSsh {
                readonly: true,
                command,
                ..
            } if !is_network_device_readonly_command(command)
        ) {
            return PolicyDecision::Deny {
                reason: "SSH 命令未匹配结构化只读白名单，已拒绝执行".to_owned(),
            };
        }

        if matches!(
            operation,
            RemoteOperation::OpenSerial { writable: true, .. }
                | RemoteOperation::WriteSerial { .. }
        ) {
            return match permission_mode {
                PermissionMode::ReadOnly => PolicyDecision::Deny {
                    reason: "当前会话处于只读模式，已拒绝可写串口操作".to_owned(),
                },
                PermissionMode::ControllerApproved => PolicyDecision::Allow,
                PermissionMode::ApprovalRequired | PermissionMode::FullAccess => {
                    PolicyDecision::RequireApproval {
                        risk: RiskLevel::Mutating,
                        reason: "可写串口操作始终需要人工确认".to_owned(),
                    }
                }
            };
        }

        match permission_mode {
            PermissionMode::ReadOnly if risk != RiskLevel::ReadOnly => PolicyDecision::Deny {
                reason: "当前会话处于只读模式，已拒绝非只读操作".to_owned(),
            },
            PermissionMode::ControllerApproved
            | PermissionMode::FullAccess
            | PermissionMode::ReadOnly => PolicyDecision::Allow,
            PermissionMode::ApprovalRequired => match (source, risk) {
                (_, RiskLevel::High) => PolicyDecision::RequireApproval {
                    risk,
                    reason: "该操作可能影响服务、系统启动、安全策略或执行未知程序".to_owned(),
                },
                (EventSource::Ai, RiskLevel::Mutating) => PolicyDecision::RequireApproval {
                    risk,
                    reason: "AI 发起的状态修改必须由人工确认".to_owned(),
                },
                _ => PolicyDecision::Allow,
            },
        }
    }

    /// 返回操作风险等级。
    #[must_use]
    pub fn classify(&self, operation: &RemoteOperation) -> RiskLevel {
        match operation {
            RemoteOperation::RunCommand { shell, command, .. } => {
                self.classify_command(Some(*shell), command)
            }
            RemoteOperation::OpenShell { .. }
            | RemoteOperation::CloseShell { .. }
            | RemoteOperation::TestPort { .. }
            | RemoteOperation::GetFileMetadata { .. }
            | RemoteOperation::UploadFileChunk { .. }
            | RemoteOperation::CompleteUploadFile { .. }
            | RemoteOperation::AbortUploadFile { .. }
            | RemoteOperation::DownloadFileChunk { .. }
            | RemoteOperation::ListProcesses
            | RemoteOperation::ListServices
            | RemoteOperation::ListSerial
            | RemoteOperation::CloseSerial { .. }
            | RemoteOperation::CloseConnection
            | RemoteOperation::HumanTakeover
            | RemoteOperation::ReleaseHumanTakeover
            | RemoteOperation::EmergencyStop
            | RemoteOperation::CancelRequest { .. }
            | RemoteOperation::VisualObserve { .. }
            | RemoteOperation::VisualWaitFor { .. }
            | RemoteOperation::VisualStop => RiskLevel::ReadOnly,
            RemoteOperation::VisualInvoke { .. } | RemoteOperation::VisualTypeText { .. } => {
                RiskLevel::Mutating
            }
            RemoteOperation::VisualSendInput { .. } => RiskLevel::High,
            RemoteOperation::RunSsh {
                command, readonly, ..
            } => {
                let risk = if is_network_device_readonly_command(command) {
                    RiskLevel::ReadOnly
                } else {
                    RiskLevel::High
                };
                if *readonly && risk != RiskLevel::ReadOnly {
                    RiskLevel::High
                } else {
                    risk
                }
            }
            RemoteOperation::OpenSerial { writable, .. } => {
                if *writable {
                    RiskLevel::Mutating
                } else {
                    RiskLevel::ReadOnly
                }
            }
            RemoteOperation::UploadFile { overwrite, .. }
            | RemoteOperation::BeginUploadFile { overwrite, .. }
            | RemoteOperation::MoveFile { overwrite, .. } => {
                if *overwrite {
                    RiskLevel::High
                } else {
                    RiskLevel::Mutating
                }
            }
            RemoteOperation::DownloadFile {
                overwrite_local, ..
            }
            | RemoteOperation::AuthorizeDownloadFile {
                overwrite_local, ..
            } => {
                if *overwrite_local {
                    RiskLevel::Mutating
                } else {
                    RiskLevel::ReadOnly
                }
            }
            RemoteOperation::RunShellCommand { .. }
            | RemoteOperation::DeleteFile { .. }
            | RemoteOperation::TcpExchange { .. }
            | RemoteOperation::TerminateProcess { .. }
            | RemoteOperation::WriteSerial { .. } => RiskLevel::Mutating,
            RemoteOperation::ControlService { .. } | RemoteOperation::PowerControl { .. } => {
                RiskLevel::High
            }
            RemoteOperation::RunSerialQuery {
                command, profile, ..
            } => match classify_serial_query(*profile, command) {
                SerialQueryRisk::ReadOnly => RiskLevel::ReadOnly,
                SerialQueryRisk::Mutating => RiskLevel::Mutating,
                SerialQueryRisk::Unknown => RiskLevel::High,
            },
        }
    }

    fn classify_command(&self, shell: Option<ShellKind>, command: &str) -> RiskLevel {
        if self
            .high_risk_patterns
            .iter()
            .any(|pattern| pattern.is_match(command))
        {
            return RiskLevel::High;
        }
        if is_structured_readonly_command(shell, command) {
            RiskLevel::ReadOnly
        } else {
            RiskLevel::Mutating
        }
    }
}

fn is_structured_readonly_command(shell: Option<ShellKind>, command: &str) -> bool {
    let command = command.trim();
    if command.is_empty()
        || command.len() > 4_096
        || command.contains(['\r', '\n', ';', '|', '&', '>', '<', '`'])
        || command.contains("$(")
        || command.contains("@(")
    {
        return false;
    }

    let Some(program) = command.split_whitespace().next() else {
        return false;
    };
    let program = program
        .trim_matches('"')
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    match shell {
        Some(ShellKind::Cmd) => is_cmd_readonly(&program, command),
        Some(ShellKind::WindowsPowerShell | ShellKind::PowerShell) => {
            is_literal_read(command)
                || is_environment_read(command)
                || is_powershell_readonly(&program, command)
        }
        Some(ShellKind::System) => is_posix_readonly(&program, command),
        None => false,
    }
}

fn is_literal_read(command: &str) -> bool {
    command.len() >= 2
        && ((command.starts_with('\'') && command.ends_with('\''))
            || (command.starts_with('"')
                && command.ends_with('"')
                && !command.contains('$')
                && !command.contains('%')))
}

fn is_environment_read(command: &str) -> bool {
    let Some(name) = command.strip_prefix("$env:") else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn is_cmd_readonly(program: &str, command: &str) -> bool {
    match program {
        "echo" | "ver" | "hostname" | "whoami" | "tasklist" | "systeminfo" | "netstat" | "arp"
        | "nslookup" | "ping" | "tracert" | "pathping" => true,
        "ipconfig" => command.split_whitespace().skip(1).all(|argument| {
            matches!(
                argument.to_ascii_lowercase().as_str(),
                "/all" | "/displaydns"
            )
        }),
        "route" => command
            .split_whitespace()
            .nth(1)
            .is_some_and(|argument| argument.eq_ignore_ascii_case("print")),
        "sc" | "sc.exe" => command.split_whitespace().nth(1).is_some_and(|argument| {
            matches!(
                argument.to_ascii_lowercase().as_str(),
                "query" | "queryex" | "qc" | "enumdepend" | "getkeyname" | "getdisplayname"
            )
        }),
        "appcmd" | "appcmd.exe" => is_appcmd_readonly(command),
        _ => false,
    }
}

fn is_appcmd_readonly(command: &str) -> bool {
    let mut arguments = command.split_whitespace().skip(1);
    if !arguments
        .next()
        .is_some_and(|action| action.eq_ignore_ascii_case("list"))
    {
        return false;
    }
    arguments.next().is_some_and(|object| {
        matches!(
            object.trim_matches('"').to_ascii_lowercase().as_str(),
            "site" | "app" | "apppool" | "vdir" | "config" | "wp" | "request" | "module" | "backup"
        )
    })
}

fn is_powershell_readonly(program: &str, command: &str) -> bool {
    matches!(
        program,
        "get-process"
            | "get-service"
            | "get-computerinfo"
            | "get-nettcpconnection"
            | "get-netudpendpoint"
            | "get-netipaddress"
            | "get-netipconfiguration"
            | "get-netroute"
            | "get-ciminstance"
            | "get-wmiobject"
            | "get-childitem"
            | "get-item"
            | "get-content"
            | "get-filehash"
            | "get-acl"
            | "get-location"
            | "get-date"
            | "get-command"
            | "get-host"
            | "get-variable"
            | "test-path"
            | "test-netconnection"
            | "resolve-dnsname"
            | "select-string"
            | "write-output"
    ) && is_simple_powershell_invocation(command)
}

fn is_simple_powershell_invocation(command: &str) -> bool {
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(character) = chars.next() {
        match quote {
            Some('\'') if character == '\'' => {
                if chars.peek() == Some(&'\'') {
                    let _ = chars.next();
                } else {
                    quote = None;
                }
            }
            Some('"') if character == '"' => quote = None,
            Some('"') if matches!(character, '$' | '`') => return false,
            None if matches!(character, '\'' | '"') => quote = Some(character),
            None if matches!(
                character,
                '(' | ')' | '{' | '}' | '[' | ']' | '$' | '@' | '='
            ) =>
            {
                return false;
            }
            None if character == ':' && chars.peek() == Some(&':') => return false,
            Some(_) | None => {}
        }
    }
    if quote.is_some() {
        return false;
    }

    !command.split_whitespace().any(|argument| {
        let parameter = argument
            .split_once(':')
            .map_or(argument, |(name, _)| name)
            .to_ascii_lowercase();
        matches!(
            parameter.as_str(),
            "-outvariable"
                | "-ov"
                | "-errorvariable"
                | "-ev"
                | "-warningvariable"
                | "-wv"
                | "-informationvariable"
                | "-iv"
                | "-pipelinevariable"
                | "-pv"
        )
    })
}

fn is_posix_readonly(program: &str, command: &str) -> bool {
    match program {
        "hostname" | "date" => command.split_whitespace().count() == 1,
        "printf" | "echo" | "uname" | "whoami" | "id" | "uptime" | "pwd" | "ls" | "head"
        | "tail" | "grep" | "ps" | "ss" | "netstat" | "df" => true,
        "cat" => command
            .split_whitespace()
            .skip(1)
            .all(|argument| !argument.starts_with('-')),
        "ip" => is_posix_ip_readonly(command),
        _ => false,
    }
}

fn is_posix_ip_readonly(command: &str) -> bool {
    let arguments = command
        .split_whitespace()
        .skip(1)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let Some(object_index) = arguments
        .iter()
        .position(|argument| matches!(argument.as_str(), "address" | "addr" | "route" | "link"))
    else {
        return false;
    };
    !arguments[object_index + 1..].iter().any(|argument| {
        matches!(
            argument.as_str(),
            "add" | "append" | "change" | "delete" | "del" | "flush" | "replace" | "set"
        )
    })
}

/// 判断命令是否属于受约束的网络设备只读查询。
///
/// 该门禁供 Agent 在旧 Relay 尚未识别设备命令时执行最终本地校验，不改变协议风险分类。
#[must_use]
pub fn is_network_device_readonly_command(command: &str) -> bool {
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty()
        || normalized.len() > 512
        || normalized.contains(['\r', '\n', ';', '|', '&', '>', '<', '`'])
    {
        return false;
    }
    let lower = normalized.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "display version"
            | "display current-configuration"
            | "display saved-configuration"
            | "display device"
            | "display interface brief"
            | "display ip interface brief"
            | "display vlan"
            | "display mac-address"
            | "display arp"
            | "display lldp neighbor brief"
            | "display clock"
            | "display users"
            | "display cpu-usage"
            | "display memory-usage"
            | "show version"
            | "show running-config"
            | "show startup-config"
            | "show interfaces status"
            | "show ip interface brief"
            | "show vlan brief"
            | "show mac address-table"
            | "show arp"
            | "show clock"
    )
}

fn compile_patterns(patterns: &[&str]) -> Vec<Regex> {
    patterns
        .iter()
        .map(|pattern| Regex::new(pattern).expect("内置策略正则必须有效"))
        .collect()
}

/// 内存审批存储；未来壳子可以替换为持久化实现。
#[derive(Default)]
pub struct ApprovalStore {
    records: BTreeMap<ApprovalId, ApprovalRecord>,
}

impl ApprovalStore {
    /// 创建等待人工处理的审批。
    pub fn create(
        &mut self,
        session_id: SessionId,
        operation: RemoteOperation,
        now: DateTime<Utc>,
        lifetime: Duration,
    ) -> ApprovalRecord {
        let record = ApprovalRecord {
            approval_id: ApprovalId::new(),
            session_id,
            operation,
            state: ApprovalState::Pending,
            expires_at: now + lifetime,
        };
        self.records.insert(record.approval_id, record.clone());
        record
    }

    /// 为匹配的会话和完整操作消费一次已经批准的审批。
    ///
    /// 返回 `Approved` 时记录已从存储中移除，后续不能再次使用；过期记录也会在
    /// 本次检查后移除。会话或操作不匹配时返回 `None`，且不会消耗合法审批。
    pub fn consume(
        &mut self,
        session_id: SessionId,
        approval_id: ApprovalId,
        operation: &RemoteOperation,
        now: DateTime<Utc>,
    ) -> Option<ApprovalRecord> {
        let mut record = self.records.get(&approval_id)?.clone();
        if record.session_id != session_id
            || !approval_operations_match(&record.operation, operation)
        {
            return None;
        }
        match record.state {
            ApprovalState::Approved if now < record.expires_at => {
                self.records.remove(&approval_id);
                Some(record)
            }
            ApprovalState::Approved | ApprovalState::Pending if now >= record.expires_at => {
                record.state = ApprovalState::Expired;
                self.records.remove(&approval_id);
                Some(record)
            }
            _ => Some(record),
        }
    }

    /// 只在审批属于指定会话时批准或拒绝。
    pub fn decide_for_session(
        &mut self,
        session_id: SessionId,
        approval_id: ApprovalId,
        approved: bool,
        now: DateTime<Utc>,
    ) -> Option<ApprovalRecord> {
        let mut record = self.records.get(&approval_id)?.clone();
        if record.session_id != session_id {
            return None;
        }
        if now >= record.expires_at {
            record.state = ApprovalState::Expired;
            self.records.remove(&approval_id);
            return Some(record);
        }
        if record.state != ApprovalState::Pending {
            return Some(record);
        }
        record.state = if approved {
            ApprovalState::Approved
        } else {
            ApprovalState::Rejected
        };
        if approved {
            self.records.insert(approval_id, record.clone());
        } else {
            self.records.remove(&approval_id);
        }
        Some(record)
    }
}

#[cfg(test)]
mod tests {
    use remoteops_domain::{SerialLineEnding, SerialSettings, SerialTerminalProfile, ShellKind};

    use super::*;

    #[test]
    fn ai_readonly_command_is_allowed() {
        let policy = DefaultPolicy::default();
        let operation = RemoteOperation::RunCommand {
            shell: ShellKind::WindowsPowerShell,
            command: "Get-Process".to_owned(),
            readonly: true,
        };

        assert_eq!(
            policy.evaluate(EventSource::Ai, &operation),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn false_readonly_declaration_is_denied() {
        let policy = DefaultPolicy::default();
        let operation = RemoteOperation::RunCommand {
            shell: ShellKind::WindowsPowerShell,
            command: "Remove-Item C:\\temp\\a.txt".to_owned(),
            readonly: true,
        };

        assert!(matches!(
            policy.evaluate(EventSource::Ai, &operation),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn unknown_readonly_commands_are_denied() {
        let policy = DefaultPolicy::default();
        for command in [
            "Set-ItemProperty HKCU:\\Software\\RemoteOps Enabled 1",
            "Clear-Content C:\\temp\\remoteops.txt",
            "net user remoteops Temp123! /add",
            "schtasks /create /tn RemoteOps /tr calc.exe /sc once /st 23:59",
            "Invoke-WebRequest https://example.test/tool.exe -OutFile C:\\temp\\tool.exe",
        ] {
            let operation = RemoteOperation::RunCommand {
                shell: ShellKind::WindowsPowerShell,
                command: command.to_owned(),
                readonly: true,
            };
            assert!(
                matches!(
                    policy.evaluate(EventSource::Ai, &operation),
                    PolicyDecision::Deny { .. }
                ),
                "未知命令必须拒绝：{command}"
            );
        }
    }

    #[test]
    fn structured_readonly_commands_are_allowed() {
        let policy = DefaultPolicy::default();
        for (shell, command) in [
            (ShellKind::WindowsPowerShell, "Get-NetTCPConnection"),
            (
                ShellKind::WindowsPowerShell,
                "Get-CimInstance Win32_OperatingSystem",
            ),
            (ShellKind::WindowsPowerShell, "Get-Host"),
            (ShellKind::WindowsPowerShell, "$env:COMPUTERNAME"),
            (ShellKind::Cmd, "ipconfig /all"),
            (ShellKind::Cmd, "echo %COMPUTERNAME%"),
            (ShellKind::Cmd, "appcmd list site"),
            (
                ShellKind::Cmd,
                "C:\\Windows\\System32\\inetsrv\\appcmd.exe list apppool /text:name",
            ),
            (ShellKind::System, "printf REMOTEOPS_OK"),
        ] {
            let operation = RemoteOperation::RunCommand {
                shell,
                command: command.to_owned(),
                readonly: true,
            };
            assert_eq!(
                policy.evaluate(EventSource::Ai, &operation),
                PolicyDecision::Allow,
                "白名单命令应允许：{command}"
            );
        }
    }

    #[test]
    fn appcmd_only_allows_constrained_list_queries() {
        let policy = DefaultPolicy::default();
        for command in [
            "appcmd add site /name:test",
            "appcmd delete site test",
            "appcmd set site test /serverAutoStart:false",
            "appcmd start site test",
            "appcmd stop site test",
            "appcmd recycle apppool test",
            "appcmd list site > C:\\temp\\sites.txt",
            "appcmd list site & whoami",
        ] {
            let operation = RemoteOperation::RunCommand {
                shell: ShellKind::Cmd,
                command: command.to_owned(),
                readonly: true,
            };
            assert!(
                matches!(
                    policy.evaluate(EventSource::Ai, &operation),
                    PolicyDecision::Deny { .. }
                ),
                "非受约束 appcmd 查询必须拒绝：{command}"
            );
        }
    }

    #[test]
    fn shell_specific_literal_and_environment_reads_do_not_cross_interpreters() {
        let policy = DefaultPolicy::default();
        for (shell, command) in [
            (ShellKind::Cmd, "\"calc.exe\""),
            (ShellKind::Cmd, "$env:COMPUTERNAME"),
            (ShellKind::System, "'touch /tmp/remoteops-policy-test'"),
        ] {
            let operation = RemoteOperation::RunCommand {
                shell,
                command: command.to_owned(),
                readonly: true,
            };
            assert!(matches!(
                policy.evaluate(EventSource::Ai, &operation),
                PolicyDecision::Deny { .. }
            ));
        }
    }

    #[test]
    fn powershell_readonly_commands_reject_nested_expressions_and_side_effect_parameters() {
        let policy = DefaultPolicy::default();
        for command in [
            "Write-Output (Set-Content -LiteralPath C:\\temp\\a.txt -Value bypass)",
            "Get-Item (Remove-Item -LiteralPath C:\\temp\\a.txt -PassThru)",
            "Get-Process ([System.IO.File]::WriteAllText('C:\\temp\\a.txt','bypass'))",
            "Get-Process -OutVariable captured",
            "Get-Process -OutVariable:captured",
            "Get-Process $pid",
        ] {
            let operation = RemoteOperation::RunCommand {
                shell: ShellKind::WindowsPowerShell,
                command: command.to_owned(),
                readonly: true,
            };
            assert!(
                matches!(
                    policy.evaluate(EventSource::Ai, &operation),
                    PolicyDecision::Deny { .. }
                ),
                "包含表达式或变量副作用的命令必须拒绝：{command}"
            );
        }
    }

    #[test]
    fn persistent_shell_classification_uses_bound_shell_kind() {
        let policy = DefaultPolicy::default();
        let operation = RemoteOperation::RunShellCommand {
            shell_id: remoteops_domain::ShellId::new(),
            shell: ShellKind::PowerShell,
            command: "sc query C:\\temp\\unexpected.txt".to_owned(),
            readonly: true,
        };

        assert!(matches!(
            policy.evaluate(EventSource::Ai, &operation),
            PolicyDecision::Deny { .. }
        ));

        let stateful_read = RemoteOperation::RunShellCommand {
            shell_id: remoteops_domain::ShellId::new(),
            shell: ShellKind::PowerShell,
            command: "Get-Process".to_owned(),
            readonly: true,
        };
        assert!(matches!(
            policy.evaluate(EventSource::Ai, &stateful_read),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn structured_huawei_display_query_is_allowed_without_raw_write_approval() {
        let policy = DefaultPolicy::default();
        let operation = RemoteOperation::RunSerialQuery {
            serial_session_id: "serial-1".to_owned(),
            command: "display interface brief".to_owned(),
            line_ending: SerialLineEnding::Cr,
            profile: SerialTerminalProfile::HuaweiVrp,
            overall_timeout_millis: 30_000,
            idle_timeout_millis: 1_200,
            max_bytes: 128 * 1024,
            max_pages: 50,
            readonly: true,
        };
        assert_eq!(
            policy.evaluate(EventSource::Ai, &operation),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn network_device_readonly_commands_are_strictly_bounded() {
        for command in [
            "display version",
            "display current-configuration",
            "show running-config",
            "show ip interface brief",
        ] {
            assert!(is_network_device_readonly_command(command), "{command}");
        }
        for command in [
            "system-view",
            "reboot",
            "display version | save flash:/version.txt",
            "show running-config; reload",
            "undo interface GigabitEthernet1/0/1",
        ] {
            assert!(!is_network_device_readonly_command(command), "{command}");
        }
    }

    #[test]
    fn ssh_policy_uses_only_network_device_readonly_allowlist() {
        let policy = DefaultPolicy::default();
        for (command, expected) in [
            ("display version", RiskLevel::ReadOnly),
            ("hostname", RiskLevel::High),
            ("ip route", RiskLevel::High),
            ("ip link set eth0 down", RiskLevel::High),
        ] {
            let operation = RemoteOperation::RunSsh {
                host: "192.0.2.10".to_owned(),
                port: 22,
                username: "admin".to_owned(),
                identity_file: None,
                known_hosts_file: None,
                command: command.to_owned(),
                readonly: false,
            };
            assert_eq!(policy.classify(&operation), expected, "{command}");
        }
    }

    #[test]
    fn posix_readonly_classifier_rejects_mutating_arguments() {
        let policy = DefaultPolicy::default();
        for command in [
            "hostname new-name",
            "date -s 2030-01-01",
            "ip link set eth0 down",
            "ip route add default via 192.0.2.1",
            "cat --output=/tmp/remoteops-policy-test /etc/hosts",
        ] {
            let operation = RemoteOperation::RunCommand {
                shell: ShellKind::System,
                command: command.to_owned(),
                readonly: true,
            };
            assert!(
                matches!(
                    policy.evaluate(EventSource::Ai, &operation),
                    PolicyDecision::Deny { .. }
                ),
                "包含修改参数的 POSIX 命令必须拒绝：{command}"
            );
        }
    }

    #[test]
    fn false_readonly_serial_query_is_denied_and_mutating_query_requires_approval() {
        let policy = DefaultPolicy::default();
        let readonly_lie = RemoteOperation::RunSerialQuery {
            serial_session_id: "serial-1".to_owned(),
            command: "system-view".to_owned(),
            line_ending: SerialLineEnding::Cr,
            profile: SerialTerminalProfile::HuaweiVrp,
            overall_timeout_millis: 30_000,
            idle_timeout_millis: 1_200,
            max_bytes: 128 * 1024,
            max_pages: 50,
            readonly: true,
        };
        assert!(matches!(
            policy.evaluate(EventSource::Ai, &readonly_lie),
            PolicyDecision::Deny { .. }
        ));

        let mutating = RemoteOperation::RunSerialQuery {
            serial_session_id: "serial-1".to_owned(),
            command: "system-view".to_owned(),
            line_ending: SerialLineEnding::Cr,
            profile: SerialTerminalProfile::HuaweiVrp,
            overall_timeout_millis: 30_000,
            idle_timeout_millis: 1_200,
            max_bytes: 128 * 1024,
            max_pages: 50,
            readonly: false,
        };
        assert!(matches!(
            policy.evaluate(EventSource::Ai, &mutating),
            PolicyDecision::RequireApproval {
                risk: RiskLevel::Mutating,
                ..
            }
        ));
    }

    #[test]
    fn writable_serial_always_requires_approval_outside_readonly_mode() {
        let policy = DefaultPolicy::default();
        let operation = RemoteOperation::WriteSerial {
            serial_session_id: "serial-1".to_owned(),
            byte_count: 2,
            sha256: "00".repeat(32),
        };

        for permission_mode in [PermissionMode::ApprovalRequired, PermissionMode::FullAccess] {
            for source in [EventSource::Human, EventSource::Ai] {
                assert!(matches!(
                    policy.evaluate_with_mode(permission_mode, source, &operation),
                    PolicyDecision::RequireApproval {
                        risk: RiskLevel::Mutating,
                        ..
                    }
                ));
            }
        }
        assert!(matches!(
            policy.evaluate_with_mode(PermissionMode::ReadOnly, EventSource::Human, &operation),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn high_risk_human_command_still_requires_approval() {
        let policy = DefaultPolicy::default();
        let operation = RemoteOperation::RunCommand {
            shell: ShellKind::WindowsPowerShell,
            command: "Restart-Computer".to_owned(),
            readonly: false,
        };

        assert!(matches!(
            policy.evaluate(EventSource::Human, &operation),
            PolicyDecision::RequireApproval {
                risk: RiskLevel::High,
                ..
            }
        ));
    }

    #[test]
    fn permission_modes_enforce_readonly_approval_and_full_access() {
        let policy = DefaultPolicy::default();
        let operation = RemoteOperation::RunCommand {
            shell: ShellKind::WindowsPowerShell,
            command: "Set-Content -Path C:\\temp\\remoteops.txt -Value ok".to_owned(),
            readonly: false,
        };

        assert!(matches!(
            policy.evaluate_with_mode(PermissionMode::ReadOnly, EventSource::Ai, &operation),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            policy.evaluate_with_mode(
                PermissionMode::ApprovalRequired,
                EventSource::Ai,
                &operation
            ),
            PolicyDecision::RequireApproval { .. }
        ));
        assert_eq!(
            policy.evaluate_with_mode(
                PermissionMode::ControllerApproved,
                EventSource::Ai,
                &operation
            ),
            PolicyDecision::Allow
        );
        assert_eq!(
            policy.evaluate_with_mode(PermissionMode::FullAccess, EventSource::Ai, &operation),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn ssh_permission_modes_enforce_readonly_approval_and_full_access() {
        let policy = DefaultPolicy::default();
        let operation = RemoteOperation::RunSsh {
            host: "192.0.2.10".to_owned(),
            port: 22,
            username: "operator".to_owned(),
            identity_file: None,
            known_hosts_file: None,
            command: "system-view".to_owned(),
            readonly: false,
        };

        assert!(matches!(
            policy.evaluate_with_mode(PermissionMode::ReadOnly, EventSource::Ai, &operation),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            policy.evaluate_with_mode(
                PermissionMode::ApprovalRequired,
                EventSource::Ai,
                &operation,
            ),
            PolicyDecision::RequireApproval {
                risk: RiskLevel::High,
                ..
            }
        ));
        assert_eq!(
            policy.evaluate_with_mode(
                PermissionMode::ControllerApproved,
                EventSource::Ai,
                &operation,
            ),
            PolicyDecision::Allow
        );
        assert_eq!(
            policy.evaluate_with_mode(PermissionMode::FullAccess, EventSource::Ai, &operation),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn controller_approved_allows_writable_serial_after_local_confirmation() {
        let operation = RemoteOperation::WriteSerial {
            serial_session_id: "serial-1".to_owned(),
            byte_count: 2,
            sha256: "00".repeat(32),
        };

        assert_eq!(
            DefaultPolicy::default().evaluate_with_mode(
                PermissionMode::ControllerApproved,
                EventSource::Ai,
                &operation
            ),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn ai_local_download_overwrite_requires_approval() {
        let policy = DefaultPolicy::default();
        let safe_download = RemoteOperation::DownloadFile {
            remote_path: "logs/diagnostic.txt".to_owned(),
            overwrite_local: false,
        };
        let overwrite = RemoteOperation::DownloadFile {
            remote_path: "logs/diagnostic.txt".to_owned(),
            overwrite_local: true,
        };

        assert_eq!(
            policy.evaluate(EventSource::Ai, &safe_download),
            PolicyDecision::Allow
        );
        assert!(matches!(
            policy.evaluate(EventSource::Ai, &overwrite),
            PolicyDecision::RequireApproval {
                risk: RiskLevel::Mutating,
                ..
            }
        ));
    }

    #[test]
    fn approved_record_is_checked_for_expiry_and_consumed_once() {
        let mut approvals = ApprovalStore::default();
        let session_id = SessionId::new();
        let now = Utc::now();
        let operation = RemoteOperation::RunCommand {
            shell: ShellKind::WindowsPowerShell,
            command: "Restart-Computer".to_owned(),
            readonly: false,
        };
        let record = approvals.create(session_id, operation.clone(), now, Duration::minutes(5));

        assert_eq!(
            approvals
                .consume(session_id, record.approval_id, &operation, now)
                .expect("待审批记录应存在")
                .state,
            ApprovalState::Pending
        );
        assert_eq!(
            approvals
                .decide_for_session(session_id, record.approval_id, true, now)
                .expect("人工审批应成功")
                .state,
            ApprovalState::Approved
        );
        assert_eq!(
            approvals
                .consume(session_id, record.approval_id, &operation, now)
                .expect("首次消费应成功")
                .state,
            ApprovalState::Approved
        );
        assert!(
            approvals
                .consume(session_id, record.approval_id, &operation, now)
                .is_none()
        );
    }

    #[test]
    fn approved_record_cannot_be_consumed_after_expiry() {
        let mut approvals = ApprovalStore::default();
        let session_id = SessionId::new();
        let now = Utc::now();
        let operation = RemoteOperation::RunCommand {
            shell: ShellKind::WindowsPowerShell,
            command: "Restart-Computer".to_owned(),
            readonly: false,
        };
        let record = approvals.create(session_id, operation.clone(), now, Duration::seconds(1));
        approvals
            .decide_for_session(session_id, record.approval_id, true, now)
            .expect("人工审批应成功");

        assert_eq!(
            approvals
                .consume(
                    session_id,
                    record.approval_id,
                    &operation,
                    now + Duration::seconds(2),
                )
                .expect("过期状态应返回")
                .state,
            ApprovalState::Expired
        );
        assert!(
            approvals
                .consume(
                    session_id,
                    record.approval_id,
                    &operation,
                    now + Duration::seconds(2),
                )
                .is_none()
        );
    }

    #[test]
    fn upload_approval_matches_chunked_begin_only_for_same_security_fields() {
        let approved = RemoteOperation::UploadFile {
            remote_path: "nested/file.bin".to_owned(),
            size: 123,
            sha256: "a".repeat(64),
            overwrite: true,
        };
        let requested = RemoteOperation::BeginUploadFile {
            transfer_id: remoteops_domain::FileTransferId::new(),
            remote_path: "nested/file.bin".to_owned(),
            size: 123,
            sha256: "A".repeat(64),
            overwrite: true,
        };

        assert!(approval_operations_match(&approved, &requested));
        let mut different = requested.clone();
        if let RemoteOperation::BeginUploadFile { size, .. } = &mut different {
            *size += 1;
        }
        assert!(!approval_operations_match(&approved, &different));
    }

    #[test]
    fn expired_pending_record_cannot_be_approved_later() {
        let mut approvals = ApprovalStore::default();
        let session_id = SessionId::new();
        let now = Utc::now();
        let operation = RemoteOperation::UploadFile {
            remote_path: "C:\\temp\\remoteops.txt".to_owned(),
            size: 9,
            sha256: "a".repeat(64),
            overwrite: false,
        };
        let record = approvals.create(session_id, operation.clone(), now, Duration::seconds(1));

        assert_eq!(
            approvals
                .decide_for_session(
                    session_id,
                    record.approval_id,
                    true,
                    now + Duration::seconds(1),
                )
                .expect("首次触达过期审批时应返回过期状态")
                .state,
            ApprovalState::Expired
        );
        assert!(
            approvals
                .decide_for_session(
                    session_id,
                    record.approval_id,
                    true,
                    now + Duration::seconds(2),
                )
                .is_none(),
            "过期审批从存储移除后不得再次批准"
        );
        assert!(
            approvals
                .consume(
                    session_id,
                    record.approval_id,
                    &operation,
                    now + Duration::seconds(2),
                )
                .is_none(),
            "过期审批不得用于执行"
        );
    }

    #[test]
    fn rejected_record_is_terminal_and_cannot_be_reused() {
        let mut approvals = ApprovalStore::default();
        let session_id = SessionId::new();
        let now = Utc::now();
        let operation = RemoteOperation::OpenSerial {
            port_name: "COM3".to_owned(),
            settings: SerialSettings::default(),
            writable: true,
        };
        let record = approvals.create(session_id, operation.clone(), now, Duration::minutes(5));

        assert_eq!(
            approvals
                .decide_for_session(session_id, record.approval_id, false, now)
                .expect("拒绝决定应成功")
                .state,
            ApprovalState::Rejected
        );
        assert!(
            approvals
                .decide_for_session(session_id, record.approval_id, true, now)
                .is_none(),
            "被拒绝的审批不得改为批准"
        );
        assert!(
            approvals
                .consume(session_id, record.approval_id, &operation, now)
                .is_none(),
            "被拒绝的审批不得用于执行"
        );
    }

    #[test]
    fn mismatched_target_does_not_consume_valid_approval() {
        let mut approvals = ApprovalStore::default();
        let session_id = SessionId::new();
        let now = Utc::now();
        let operation = RemoteOperation::UploadFile {
            remote_path: "C:\\temp\\remoteops.txt".to_owned(),
            size: 9,
            sha256: "a".repeat(64),
            overwrite: false,
        };
        let record = approvals.create(session_id, operation.clone(), now, Duration::minutes(5));
        approvals
            .decide_for_session(session_id, record.approval_id, true, now)
            .expect("人工审批应成功");
        let different_operation = RemoteOperation::UploadFile {
            remote_path: "C:\\temp\\other.txt".to_owned(),
            size: 9,
            sha256: "a".repeat(64),
            overwrite: false,
        };

        assert!(
            approvals
                .consume(SessionId::new(), record.approval_id, &operation, now,)
                .is_none(),
            "不同 session_id 不得消费审批"
        );
        assert!(
            approvals
                .consume(session_id, record.approval_id, &different_operation, now,)
                .is_none(),
            "不同操作不得消费审批"
        );
        assert_eq!(
            approvals
                .consume(session_id, record.approval_id, &operation, now)
                .expect("匹配的审批仍应可以消费")
                .state,
            ApprovalState::Approved
        );
    }

    #[test]
    fn visual_observation_is_read_only_but_input_requires_approval() {
        let policy = DefaultPolicy::default();
        let observe = RemoteOperation::VisualObserve {
            include_screenshot: true,
            include_ui_tree: true,
        };
        assert_eq!(
            policy.evaluate(EventSource::Ai, &observe),
            PolicyDecision::Allow
        );

        let input = RemoteOperation::VisualSendInput {
            target: remoteops_domain::VisualTarget::Coordinate {
                window_fingerprint: "window".to_owned(),
                display_id: "display-0".to_owned(),
                x: 1,
                y: 1,
                screenshot_scale_percent: 100,
                end_x: None,
                end_y: None,
            },
            input: "click".to_owned(),
        };
        assert!(matches!(
            policy.evaluate(EventSource::Ai, &input),
            PolicyDecision::RequireApproval {
                risk: RiskLevel::High,
                ..
            }
        ));
    }
}
