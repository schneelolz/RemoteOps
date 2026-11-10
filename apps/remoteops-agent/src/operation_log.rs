//! 将 Agent 已执行的操作和输出转为有界、脱敏的本地日志，不改变远程协议。

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use chrono::{DateTime, Utc};
use remoteops_audit::Redactor;
use remoteops_domain::{
    EventPayload, RemoteEvent, RemoteOperation, RequestId, SerialSessionId, SessionId, ShellId,
    ShellKind,
};
use remoteops_protocol::{RemoteRequest, RemoteResponse, WireMessage};

#[cfg(test)]
use crate::AgentEventSender;

use crate::{
    AgentEvent, AgentEventSink, AgentLogLevel, AgentOperationKind, AgentOperationLog,
    AgentOperationState, AgentOutputRepetition, AgentPresentationRole, emit_agent_event,
};

/// 单行输出超过此长度时整行隐藏，避免截断后泄漏部分凭据。
const MAX_LINE_BYTES: usize = 8192;
/// 每个请求、每种输出流最多展示的行数。
const MAX_OUTPUT_LINES: usize = 128;
/// 同时缓冲的输出流数量上限。
const MAX_STREAMS: usize = 64;
/// 连接结束前保留的中断请求墓碑数量，防止迟到的输出重新创建缓冲流。
const MAX_INTERRUPTED_REQUESTS: usize = 128;
/// 资源关闭后保留少量打开请求标识，避免迟到 wire 回放重复展示。
const MAX_CLOSED_SERIAL_REQUESTS: usize = 128;
/// 超长行只需保留边界附近的少量文本来识别跨分片 PEM 标记。
const MARKER_CONTEXT_BYTES: usize = 256;

static NEXT_SERIAL_RECORD: AtomicU64 = AtomicU64::new(1);

enum ResponsePresentation {
    Metadata,
    Result {
        summary: Option<String>,
        status: Option<&'static str>,
    },
}

fn shell_response_presentation(
    operation: &RemoteOperation,
    response: &RemoteResponse,
    payload: &EventPayload,
) -> Option<ResponsePresentation> {
    if !matches!(
        operation,
        RemoteOperation::OpenShell { .. }
            | RemoteOperation::RunShellCommand { .. }
            | RemoteOperation::CloseShell { .. }
    ) {
        return None;
    }
    match payload {
        EventPayload::OutputChunk { stderr: false, .. } if response.details.is_some() => {
            Some(ResponsePresentation::Metadata)
        }
        EventPayload::OperationCompleted { .. } | EventPayload::OperationFailed { .. } => {
            let succeeded = response.error_code.is_none() && response.exit_code == Some(0);
            let summary = match operation {
                RemoteOperation::OpenShell { shell } if succeeded => {
                    let name = match shell {
                        ShellKind::Cmd => "CMD",
                        ShellKind::WindowsPowerShell => "Windows PowerShell",
                        ShellKind::PowerShell => "PowerShell",
                        ShellKind::System => "Shell",
                    };
                    Some(format!("{name} 持久终端已打开"))
                }
                RemoteOperation::CloseShell { .. } if succeeded => Some("持久终端已关闭".into()),
                _ => None,
            };
            // 正常命令已有真实退出状态；只在终端确实结束时补充后续操作提示。
            let status = matches!(operation, RemoteOperation::RunShellCommand { .. })
                .then(|| response.details.as_ref()?.get("shell_closed")?.as_bool())
                .flatten()
                .filter(|closed| *closed)
                .map(|_| "持久终端已结束；后续命令请重新打开终端");
            Some(ResponsePresentation::Result { summary, status })
        }
        _ => None,
    }
}

/// 合并跨协议分片的输出行，并记住私钥块的隐藏状态。
#[derive(Default)]
struct OutputPolicy {
    // 有界订阅可以持续输出，旧宿主保留每流行数限制。
    unlimited: bool,
    // 网络设备配置需要额外敏感行遮盖。
    serial: bool,
}

struct OutputLine {
    text: String,
    repeatable: bool,
    display: bool,
}

#[derive(Default, PartialEq, Eq)]
enum CompletedLineKind {
    #[default]
    Opaque,
    Repeatable,
    Empty,
}

#[derive(Default)]
struct OutputBuffer {
    // 在原始完整行脱敏前计算，避免不同输入映射到相同占位文本后误合并。
    last_line_kind: CompletedLineKind,
    /// 尚未遇到换行符的文本。
    pending: String,
    /// 当前行是否已超过长度上限。
    oversized: bool,
    /// 是否正在跳过跨行私钥内容。
    private_key: bool,
    /// 超长行被丢弃后保留的末尾上下文，用于跨分片识别 PEM 边界。
    oversized_tail: String,
    /// 已展示的输出行数。
    emitted: usize,
    /// 是否已给出数量上限提示。
    limit_reported: bool,
    /// 输出限额与设备配置脱敏策略独立于逐行解析状态。
    policy: OutputPolicy,
}

impl OutputBuffer {
    /// 缓冲完整行，凭据被拆到多个协议分片时仍可整体脱敏。
    fn push(&mut self, text: &str, redactor: &Redactor) -> Vec<String> {
        self.push_lines(text, redactor)
            .into_iter()
            .filter(|line| line.display)
            .map(|line| line.text)
            .collect()
    }

    fn push_lines(&mut self, text: &str, redactor: &Redactor) -> Vec<OutputLine> {
        let mut lines = Vec::new();
        for piece in text.split_inclusive('\n') {
            let was_oversized = self.oversized;
            let oversized = self.pending.len() + piece.len() > MAX_LINE_BYTES;
            if oversized {
                self.oversized = true;
            }
            if self.oversized {
                // 当前行会被丢弃，仍要保留 PEM 边界状态，防止后续行泄漏私钥内容。
                let base = if was_oversized {
                    self.oversized_tail.clone()
                } else {
                    tail_text(&self.pending)
                };
                let context = marker_context(&base, piece);
                let has_begin = piece.contains("-----BEGIN ") && piece.contains("PRIVATE KEY-----")
                    || context.contains("-----BEGIN ") && context.contains("PRIVATE KEY-----");
                let has_end = piece.contains("-----END ") && piece.contains("PRIVATE KEY-----")
                    || context.contains("-----END ") && context.contains("PRIVATE KEY-----");
                if has_begin {
                    self.private_key = !has_end;
                } else if self.private_key && has_end {
                    self.private_key = false;
                }
                self.oversized_tail = append_tail(&base, piece);
            }
            if !self.oversized {
                self.pending.push_str(piece);
            }
            if piece.ends_with('\n') {
                if let Some(line) = self.finish_line(redactor) {
                    lines.push(OutputLine {
                        text: line,
                        repeatable: self.last_line_kind == CompletedLineKind::Repeatable,
                        display: true,
                    });
                } else if self.last_line_kind == CompletedLineKind::Empty {
                    // 空行不进入既有远程输出，但结束本地连续重复段。
                    lines.push(OutputLine {
                        text: String::new(),
                        repeatable: false,
                        display: false,
                    });
                }
            }
        }
        lines
    }

    /// 返回一条已脱敏的完整行；被隐藏的内容不会进入 GUI 事件通道。
    fn finish_line(&mut self, redactor: &Redactor) -> Option<String> {
        self.last_line_kind = CompletedLineKind::Opaque;
        let line = std::mem::take(&mut self.pending);
        let oversized = std::mem::take(&mut self.oversized);
        if oversized {
            self.oversized_tail.clear();
        }
        if !self.policy.unlimited && self.emitted >= MAX_OUTPUT_LINES {
            if std::mem::replace(&mut self.limit_reported, true) {
                return None;
            }
            return Some("[后续输出已省略，仍会显示执行结果]".to_owned());
        }
        if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
            self.private_key = !line.contains("-----END ");
            self.emitted += 1;
            return Some("[REDACTED PRIVATE KEY]".to_owned());
        }
        if self.private_key {
            if line.contains("-----END ") && line.contains("PRIVATE KEY-----") {
                self.private_key = false;
            }
            return None;
        }
        if oversized {
            self.emitted += 1;
            return Some("[单行输出过长，内容已隐藏]".to_owned());
        }
        let original = line.strip_suffix('\n').unwrap_or(&line);
        let original = original.strip_suffix('\r').unwrap_or(original);
        let no_terminal_controls = !original.chars().any(char::is_control);
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            self.last_line_kind = CompletedLineKind::Empty;
            return None;
        }
        self.emitted += 1;
        let text = redactor.redact(&if self.policy.serial {
            remoteops_serial::redact_serial_text(line)
        } else {
            line.to_owned()
        });
        if no_terminal_controls && text == line && repeatable_serial_line(&text) {
            self.last_line_kind = CompletedLineKind::Repeatable;
        }
        Some(text)
    }
}

/// 返回尚未完成行的尾部和当前分片的头部，避免为超长输出复制整行。
fn marker_context(base: &str, piece: &str) -> String {
    let mut piece_end = piece.len().min(MARKER_CONTEXT_BYTES);
    while piece_end > 0 && !piece.is_char_boundary(piece_end) {
        piece_end -= 1;
    }
    let mut context = String::with_capacity(base.len() + piece_end);
    context.push_str(base);
    context.push_str(&piece[..piece_end]);
    context
}

/// 保留不超过上下文上限的 UTF-8 尾部。
fn tail_text(text: &str) -> String {
    let mut start = text.len().saturating_sub(MARKER_CONTEXT_BYTES);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

/// 将新分片追加到有界尾部，避免复制整块超长输出。
fn append_tail(previous: &str, piece: &str) -> String {
    if piece.len() >= MARKER_CONTEXT_BYTES {
        return tail_text(piece);
    }
    let mut combined = String::with_capacity(previous.len() + piece.len());
    combined.push_str(previous);
    combined.push_str(piece);
    tail_text(&combined)
}

/// 旁路观察已有协议事件；不影响请求执行、授权和网络消息。
struct SerialRepetition {
    session: SessionId,
    resource: SerialSessionId,
    request: RequestId,
    text: String,
    value: AgentOutputRepetition,
}

// 脱敏占位、截断及终端控制行无法证明正文相同，因此始终单独显示。
fn repeatable_serial_line(text: &str) -> bool {
    !text.is_empty()
        && text.chars().count() <= 2048
        && !text.chars().any(char::is_control)
        && !["[REDACTED", "已隐藏", "已遮盖", "已截断", "已省略"]
            .iter()
            .any(|marker| text.contains(marker))
}

pub(super) struct LogObserver {
    // 只保存最后一条可靠串口完整行，任何其他显示来源都会结束连续段。
    repetition: RefCell<Option<SerialRepetition>>,
    epoch: Option<u64>,
    /// 可选的本地表现层订阅端。
    sender: Option<AgentEventSink>,
    /// 重用现有审计层的脱敏规则。
    redactor: Redactor,
    /// 按会话、请求和输出流保存不完整文本。
    streams: BTreeMap<(SessionId, RequestId, bool), OutputBuffer>,
    /// 已写入中断终态的请求，丢弃其之后迟到的协议事件。
    interrupted_requests: BTreeSet<(SessionId, RequestId)>,
    /// 达到流数量上限而被丢弃的输出流，直到请求终态都不重新捕获。
    discarded_streams: BTreeSet<(SessionId, RequestId, bool)>,
    /// 串口行缓冲按资源而非打开请求的完成事件存活。
    serial_streams: BTreeMap<(SessionId, SerialSessionId), SerialLogStream>,
    /// 仅按真实会话/请求标识识别重复串口 wire 事件，不检查输出内容前缀。
    closed_serial_requests: VecDeque<(SessionId, RequestId)>,
    /// 串口查询的详情已通过资源流展示，避免重复回放 JSON 文本。
    serial_queries: BTreeSet<(SessionId, RequestId)>,
    /// SSH 输出同样按完整设备配置行遮盖凭据。
    device_requests: BTreeSet<(SessionId, RequestId)>,
    // 仅保留仍执行或持有串口资源的可展示请求，内部诊断不进入终端。
    presentation_requests: BTreeSet<RequestId>,
    // 只登记生产方明确标注的响应事件，随同一writer队列消费，不按正文JSON形状猜测。
    response_presentations: BTreeMap<(SessionId, RequestId, u64), ResponsePresentation>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SerialPromptHint {
    Authentication,
    Command,
}
impl SerialPromptHint {
    fn message(self) -> &'static str {
        match self {
            Self::Authentication => "设备等待身份认证输入，输入隐藏",
            Self::Command => "设备已返回命令提示符",
        }
    }
}

/// UTF-8 尾部至多保留三个字节，完整行再执行设备配置和 PEM 脱敏。
struct SerialLogStream {
    request_id: RequestId,
    output: OutputBuffer,
    utf8_pending: Vec<u8>,
    // 原始输入一律不回显；短期保留用于遮盖设备回显，资源关闭时清零。
    hidden_echo: Vec<zeroize::Zeroizing<String>>,
    suppress_output: bool,
    prompt_tail: String,
    authentication_prompt: bool,
    // 提示仅表示状态；永不携带原始提示符或邻近文本。
    last_hint: Option<SerialPromptHint>,
    pending_hint: Option<SerialPromptHint>,
    shown: usize,
    omission_reported: bool,
}
impl SerialLogStream {
    fn remember_hidden(&mut self, text: &str) {
        if text.lines().count() > 1 {
            for line in text.lines() {
                self.remember_hidden(line.trim());
            }
            return;
        }
        if text.is_empty() {
            return;
        }
        if self.hidden_echo.len() >= 16 || text.len() > MAX_LINE_BYTES {
            self.suppress_output = true;
            return;
        }
        self.hidden_echo
            .push(zeroize::Zeroizing::new(text.to_owned()));
    }
    fn sanitize_echo(&self, line: String) -> String {
        if self.suppress_output
            || self
                .hidden_echo
                .iter()
                .any(|hidden| line.contains(hidden.as_str()))
        {
            "[身份认证输入回显已隐藏]".to_owned()
        } else {
            line
        }
    }
    fn local_line(
        &mut self,
        line: OutputLine,
        unlimited: bool,
        repeated: bool,
    ) -> Option<OutputLine> {
        if unlimited || self.shown < MAX_OUTPUT_LINES || repeated {
            if !repeated {
                self.shown += 1;
            }
            Some(line)
        } else if !std::mem::replace(&mut self.omission_reported, true) {
            Some(OutputLine {
                text: "[后续输出已省略，仍会显示执行结果]".to_owned(),
                repeatable: false,
                display: true,
            })
        } else {
            None
        }
    }
    fn update_prompt_hint(&mut self, input: &[u8]) {
        let visible = String::from_utf8_lossy(input);
        self.prompt_tail.push_str(&visible);
        self.prompt_tail = tail_text(&self.prompt_tail);
        let last = self
            .prompt_tail
            .rsplit(['\r', '\n'])
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let authentication = [
            "password",
            "passwd",
            "login",
            "username",
            "passphrase",
            "secret",
        ]
        .iter()
        .any(|word| last.contains(word));
        let hint = if authentication {
            self.authentication_prompt = true;
            last.ends_with([':', '：'])
                .then_some(SerialPromptHint::Authentication)
        } else if last.ends_with(['>', '#', '$']) {
            self.authentication_prompt = false;
            Some(SerialPromptHint::Command)
        } else {
            None
        };
        // PEM 和超长/未知字节行不参与提示符识别，不改变它们的脱敏状态。
        if !self.output.private_key
            && !self.output.oversized
            && let Some(hint) = hint
            && self.last_hint != Some(hint)
        {
            self.last_hint = Some(hint);
            self.pending_hint = Some(hint);
        }
    }
    #[cfg(test)]
    fn push(&mut self, bytes: &[u8], redactor: &Redactor) -> Vec<String> {
        self.push_lines(bytes, redactor)
            .into_iter()
            .filter(|line| line.display)
            .map(|line| line.text)
            .collect()
    }

    fn push_lines(&mut self, bytes: &[u8], redactor: &Redactor) -> Vec<OutputLine> {
        let mut input = std::mem::take(&mut self.utf8_pending);
        input.extend_from_slice(bytes);
        let mut lines = Vec::new();
        let mut remaining = input.as_slice();
        while !remaining.is_empty() {
            match std::str::from_utf8(remaining) {
                Ok(text) => {
                    lines.extend(self.output.push_lines(text, redactor));
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        lines.extend(self.output.push_lines(
                            std::str::from_utf8(&remaining[..valid]).expect("UTF-8 已验证前缀"),
                            redactor,
                        ));
                    }
                    if let Some(length) = error.error_len() {
                        lines.push(OutputLine {
                            text: "[二进制串口内容已隐藏]".to_owned(),
                            repeatable: false,
                            display: true,
                        });
                        // 非文本字节不能作为凭据续行的一部分展示。
                        self.output.oversized = true;
                        remaining = &remaining[valid + length..];
                    } else {
                        self.utf8_pending.extend_from_slice(&remaining[valid..]);
                        break;
                    }
                }
            }
        }
        for line in &mut lines {
            let sanitized = self.sanitize_echo(line.text.clone());
            line.repeatable &= sanitized == line.text;
            line.text = sanitized;
        }
        self.update_prompt_hint(&input);
        lines
    }
}

impl LogObserver {
    /// 创建本地日志观察器。
    #[cfg(test)]
    pub(super) fn new(sender: Option<AgentEventSender>) -> Self {
        Self::with_sink(sender.map(AgentEventSink::Legacy))
    }

    pub(super) fn with_sink(sender: Option<AgentEventSink>) -> Self {
        Self {
            repetition: RefCell::new(None),
            epoch: None,
            sender,
            redactor: Redactor::default(),
            streams: BTreeMap::new(),
            interrupted_requests: BTreeSet::new(),
            discarded_streams: BTreeSet::new(),
            serial_streams: BTreeMap::new(),
            closed_serial_requests: VecDeque::new(),
            serial_queries: BTreeSet::new(),
            device_requests: BTreeSet::new(),
            presentation_requests: BTreeSet::new(),
            response_presentations: BTreeMap::new(),
        }
    }

    pub(super) fn resource_progress(
        &self,
        request_id: RequestId,
        p: &remoteops_resources::PreparationProgress,
    ) {
        let phase = match p.stage.as_str() {
            "waiting" => "等待准备",
            "download" => "下载中",
            "verify" => "校验SHA-256",
            "extract" => "解压检查",
            "cached" => "使用已校验缓存",
            "starting" => "启动及健康检查",
            "ready" => "已就绪",
            "cancelled" => "已取消",
            _ => "准备失败",
        };
        let text = if p.stage == "download" {
            format!(
                "{}：{} {}/{} MiB（{}%），{} KiB/s",
                p.tool_id,
                phase,
                p.completed / 1_048_576,
                p.total.unwrap_or(0) / 1_048_576,
                p.total
                    .filter(|t| *t > 0)
                    .map_or(0, |t| p.completed.saturating_mul(100) / t),
                p.bytes_per_second / 1024
            )
        } else {
            format!("{}：{}", p.tool_id, phase)
        };
        emit_scoped(
            self.epoch,
            self.sender.as_ref(),
            AgentEvent::OperationPresentation {
                request_id,
                role: AgentPresentationRole::Progress,
                text,
                terminal: false,
            },
        );
    }

    pub(super) fn ssh_connecting(&self, request_id: RequestId) {
        self.emit(
            Some(request_id),
            Utc::now(),
            AgentLogLevel::Running,
            "正在建立SSH连接，身份认证信息已隐藏",
        );
    }

    pub(super) fn record_request(&mut self, request: &RemoteRequest, shell: Option<ShellKind>) {
        self.repetition.replace(None);
        if presented_operation(&request.operation) {
            self.presentation_requests.insert(request.request_id);
        }
        if matches!(request.operation, RemoteOperation::RunSsh { .. }) {
            self.device_requests
                .insert((request.session_id, request.request_id));
        }
        if matches!(request.operation, RemoteOperation::RunSerialQuery { .. }) {
            self.serial_queries
                .insert((request.session_id, request.request_id));
        }
        let mut hidden = false;
        if let RemoteOperation::RunSerialQuery {
            serial_session_id,
            command,
            ..
        } = &request.operation
        {
            let stream = serial_session_id
                .parse::<SerialSessionId>()
                .ok()
                .and_then(|resource| self.serial_streams.get_mut(&(request.session_id, resource)));
            hidden = stream
                .as_ref()
                .is_none_or(|stream| stream.authentication_prompt);
            if hidden && let Some(stream) = stream {
                stream.remember_hidden(command.trim());
            }
        }
        record_request(self.sender.as_ref(), request, self.epoch, hidden, shell);
    }

    /// 只在真实 Shell 创建成功后补资源关联，不为失败的打开操作虚构会话。
    pub(super) fn open_shell(&self, request: &RemoteRequest, resource: ShellId, shell: ShellKind) {
        emit_scoped(
            self.epoch,
            self.sender.as_ref(),
            AgentEvent::OperationContext {
                request_id: request.request_id,
                kind: AgentOperationKind::Shell,
                source: request.source,
                resource_id: Some(resource.to_string()),
                shell: Some(normalize_observed_shell(shell)),
            },
        );
    }

    pub(super) fn open_serial(
        &mut self,
        session: SessionId,
        resource: SerialSessionId,
        request_id: RequestId,
        source: remoteops_domain::EventSource,
    ) {
        self.repetition.replace(None);
        emit_scoped(
            self.epoch,
            self.sender.as_ref(),
            AgentEvent::OperationContext {
                request_id,
                kind: AgentOperationKind::Serial,
                source,
                resource_id: Some(resource.to_string()),
                shell: None,
            },
        );
        self.serial_streams.insert(
            (session, resource),
            SerialLogStream {
                request_id,
                output: OutputBuffer {
                    policy: OutputPolicy {
                        unlimited: true,
                        serial: true,
                    },
                    ..OutputBuffer::default()
                },
                utf8_pending: Vec::new(),
                hidden_echo: Vec::new(),
                suppress_output: false,
                prompt_tail: String::new(),
                authentication_prompt: true,
                last_hint: None,
                pending_hint: None,
                shown: 0,
                omission_reported: false,
            },
        );
    }

    pub(super) fn serial_output(
        &mut self,
        session: SessionId,
        resource: SerialSessionId,
        bytes: &[u8],
    ) -> Vec<String> {
        let unlimited = self.sender.as_ref().is_some_and(AgentEventSink::bounded);
        let Some(stream) = self.serial_streams.get_mut(&(session, resource)) else {
            return Vec::new();
        };
        let request = stream.request_id;
        let lines = stream.push_lines(bytes, &self.redactor);
        let hint = stream.pending_hint.take();
        if let Some(hint) = hint {
            self.emit(
                Some(request),
                Utc::now(),
                AgentLogLevel::Info,
                hint.message(),
            );
        }
        let raw = lines
            .iter()
            .filter(|line| line.display)
            .map(|line| line.text.clone())
            .collect();
        for line in lines {
            if !line.display {
                self.repetition.replace(None);
                continue;
            }
            let repeated = line.repeatable
                && self.repetition.borrow().as_ref().is_some_and(|previous| {
                    previous.session == session
                        && previous.resource == resource
                        && previous.request == request
                        && previous.text == line.text
                });
            let line = self
                .serial_streams
                .get_mut(&(session, resource))
                .and_then(|stream| stream.local_line(line, unlimited, repeated));
            if let Some(line) = line {
                self.emit_serial_line(session, resource, request, &line.text, line.repeatable);
            } else {
                self.repetition.replace(None);
            }
        }
        raw
    }

    pub(super) fn close_serial(
        &mut self,
        session: SessionId,
        resource: SerialSessionId,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(mut stream) = self.serial_streams.remove(&(session, resource)) {
            if self.closed_serial_requests.len() >= MAX_CLOSED_SERIAL_REQUESTS {
                self.closed_serial_requests.pop_front();
            }
            self.closed_serial_requests
                .push_back((session, stream.request_id));
            if let Some(line) = stream.output.finish_line(&self.redactor) {
                lines.push(stream.sanitize_echo(line));
            }
            for line in &lines {
                let line = OutputLine {
                    text: line.clone(),
                    repeatable: false,
                    display: true,
                };
                if let Some(line) = stream.local_line(
                    line,
                    self.sender.as_ref().is_some_and(AgentEventSink::bounded),
                    false,
                ) {
                    // 关闭时冲刷的是未完成行，不参与完整行重复合并。
                    self.emit(
                        Some(stream.request_id),
                        Utc::now(),
                        AgentLogLevel::Info,
                        &line.text,
                    );
                }
            }
            self.repetition.replace(None);
            self.presentation_requests.remove(&stream.request_id);
        }
        lines
    }

    pub(super) fn serial_result(
        &self,
        session: SessionId,
        resource: SerialSessionId,
        text: &str,
    ) -> String {
        let mut buffer = OutputBuffer {
            policy: OutputPolicy {
                unlimited: true,
                serial: true,
            },
            ..OutputBuffer::default()
        };
        let mut lines = buffer.push(text, &self.redactor);
        if let Some(line) = buffer.finish_line(&self.redactor) {
            lines.push(line);
        }
        if let Some(stream) = self.serial_streams.get(&(session, resource)) {
            lines = lines
                .into_iter()
                .map(|line| stream.sanitize_echo(line))
                .collect();
        }
        lines.join("\n")
    }

    pub(super) fn prepare_serial_write(&mut self, request: &RemoteRequest, bytes: &[u8]) {
        // 必须先登记再开始 I/O；驱动可能在 write 尚未返回时就让 reader 收到回显。
        if let RemoteOperation::WriteSerial {
            serial_session_id, ..
        } = &request.operation
            && let Ok(resource) = serial_session_id.parse::<SerialSessionId>()
            && let Some(stream) = self.serial_streams.get_mut(&(request.session_id, resource))
        {
            let text = String::from_utf8_lossy(bytes);
            // 控制字节和非文本字节不展示；可读片段仍登记，避免失败/部分写入的回显泄漏。
            for piece in text.split(|ch: char| ch.is_control() || ch == char::REPLACEMENT_CHARACTER)
            {
                stream.remember_hidden(piece.trim());
            }
        }
    }

    pub(super) fn serial_write(&self, request: &RemoteRequest, bytes: &[u8]) {
        // 只有真实写入成功才发布摘要；原始输入始终不回显。
        let text = std::str::from_utf8(bytes).ok().filter(|text| {
            text.chars()
                .all(|ch| !ch.is_control() || matches!(ch, '\r' | '\n' | '\t'))
        });
        let message = if text.is_some() {
            "[串口原始输入已隐藏；使用结构化串口查询展示命令]".to_owned()
        } else {
            format!("串口已发送 {} 字节，二进制内容不展示", bytes.len())
        };
        self.emit(
            Some(request.request_id),
            Utc::now(),
            AgentLogLevel::Info,
            &message,
        );
    }

    pub(super) fn with_epoch(mut self, epoch: Option<u64>) -> Self {
        self.repetition.replace(None);
        self.epoch = epoch;
        self
    }

    /// 在入队前标注响应事件；真实stdout从不经过此入口，远程消息本身不变。
    pub(super) fn annotate_response_event(
        &mut self,
        request: &RemoteRequest,
        response: &RemoteResponse,
        event: &RemoteEvent,
    ) {
        if event.session_id != request.session_id
            || event.request_id != Some(request.request_id)
            || response.session_id != request.session_id
            || response.request_id != request.request_id
            || self
                .interrupted_requests
                .contains(&(request.session_id, request.request_id))
        {
            return;
        }
        if let Some(presentation) =
            shell_response_presentation(&request.operation, response, &event.payload)
        {
            self.response_presentations.insert(
                (request.session_id, request.request_id, event.sequence),
                presentation,
            );
        }
    }

    /// 发送失败或请求结束时释放尚未消费的本地标注，不影响其他请求。
    pub(super) fn discard_response_events(&mut self, session: SessionId, request: RequestId) {
        self.response_presentations
            .retain(|(owner, id, _), _| *owner != session || *id != request);
    }

    /// 只读取操作事件和失败响应，绝不读取认证或文件二进制载荷。
    pub(super) fn observe(&mut self, message: &WireMessage) {
        // 即使没有表现层，也维护串口资源/查询的安全状态与终态回收。
        match message {
            WireMessage::RemoteEvent(event) => self.observe_event(event),
            WireMessage::RemoteResponse(response)
                if response.error_code.is_some()
                    && response.error_code.as_deref() != Some("agent_operation_failed") =>
            {
                self.flush(response.session_id, response.request_id, Utc::now());
                self.emit_terminal(
                    Some(response.request_id),
                    Utc::now(),
                    AgentLogLevel::Error,
                    &response.summary,
                );
                self.emit_state(response.request_id, AgentOperationState::Failed);
            }
            _ => {}
        }
    }

    /// 将执行阶段和输出映射到本地日志。
    fn observe_event(&mut self, event: &RemoteEvent) {
        let Some(request_id) = event.request_id else {
            return;
        };
        let presentation =
            self.response_presentations
                .remove(&(event.session_id, request_id, event.sequence));
        let (summary_override, shell_status) = match presentation {
            Some(ResponsePresentation::Metadata) => return,
            Some(ResponsePresentation::Result { summary, status }) => (summary, status),
            None => (None, None),
        };
        match &event.payload {
            _payload
                if self
                    .interrupted_requests
                    .contains(&(event.session_id, request_id)) => {}
            EventPayload::OperationStarted => {
                self.emit_diagnostic(
                    Some(request_id),
                    event.occurred_at,
                    AgentLogLevel::Running,
                    "开始执行",
                );
                self.emit_state(request_id, AgentOperationState::Running);
            }
            EventPayload::OutputChunk { stderr, text } => {
                // 串口原始事件保留给现有远程协议；本地只订阅资源级、安全解码后的流。
                let serial_request = (event.session_id, request_id);
                let is_serial_resource =
                    self.serial_streams.iter().any(|((session, _), stream)| {
                        *session == event.session_id && stream.request_id == request_id
                    });
                if is_serial_resource
                    || self.closed_serial_requests.contains(&serial_request)
                    || self.serial_queries.contains(&serial_request)
                {
                    return;
                }
                let unlimited = self.sender.as_ref().is_some_and(AgentEventSink::bounded);
                let serial = self
                    .device_requests
                    .contains(&(event.session_id, request_id));
                let key = (event.session_id, request_id, *stderr);
                if self
                    .interrupted_requests
                    .contains(&(event.session_id, request_id))
                    || self.discarded_streams.contains(&key)
                {
                    return;
                }
                if !self.streams.contains_key(&key) && self.streams.len() >= MAX_STREAMS {
                    // 保持丢弃状态直到请求终态，避免容量释放后从中途开始而绕过脱敏。
                    self.discarded_streams.insert(key);
                    return;
                }
                let lines = self
                    .streams
                    .entry(key)
                    .or_insert_with(|| OutputBuffer {
                        policy: OutputPolicy { unlimited, serial },
                        ..OutputBuffer::default()
                    })
                    .push(text, &self.redactor);
                for line in lines {
                    self.emit(
                        Some(request_id),
                        event.occurred_at,
                        if *stderr {
                            AgentLogLevel::Error
                        } else {
                            AgentLogLevel::Info
                        },
                        &line,
                    );
                }
            }
            EventPayload::OperationCompleted { exit_code, summary } => {
                self.complete_operation(
                    event,
                    request_id,
                    *exit_code,
                    summary_override.as_deref().unwrap_or(summary),
                    shell_status,
                );
            }
            EventPayload::OperationFailed { message, .. } => {
                self.fail_operation(event, request_id, message, shell_status);
            }
            EventPayload::OperationCancelled => {
                self.flush(event.session_id, request_id, event.occurred_at);
                self.remember_interrupted(event.session_id, request_id);
                self.emit_terminal(
                    Some(request_id),
                    event.occurred_at,
                    AgentLogLevel::Error,
                    "操作已中断",
                );
                self.emit_state(request_id, AgentOperationState::Cancelled);
            }
            _ => {}
        }
    }

    fn fail_operation(
        &mut self,
        event: &RemoteEvent,
        request_id: RequestId,
        message: &str,
        shell_status: Option<&str>,
    ) {
        self.flush(event.session_id, request_id, event.occurred_at);
        if let Some(status) = shell_status {
            self.emit(
                Some(request_id),
                event.occurred_at,
                AgentLogLevel::Info,
                status,
            );
        }
        self.emit_terminal(
            Some(request_id),
            event.occurred_at,
            AgentLogLevel::Error,
            &format!("执行失败：{message}"),
        );
        self.emit_state(request_id, AgentOperationState::Failed);
    }

    /// 先提交剩余输出，再根据真实退出码发布完成日志和执行终态。
    fn complete_operation(
        &mut self,
        event: &RemoteEvent,
        request_id: RequestId,
        exit_code: Option<i32>,
        summary: &str,
        shell_status: Option<&str>,
    ) {
        let streamed = self.flush(event.session_id, request_id, event.occurred_at);
        if !streamed && !summary.is_empty() {
            self.emit(
                Some(request_id),
                event.occurred_at,
                AgentLogLevel::Info,
                summary,
            );
        }
        if let Some(status) = shell_status {
            self.emit(
                Some(request_id),
                event.occurred_at,
                AgentLogLevel::Info,
                status,
            );
        }
        let failed = exit_code.is_some_and(|code| code != 0);
        let message = exit_code.map_or_else(
            || "执行完成".to_owned(),
            |code| format!("执行完成 · 退出码 {code}"),
        );
        self.emit_terminal(
            Some(request_id),
            event.occurred_at,
            if failed {
                AgentLogLevel::Error
            } else {
                AgentLogLevel::Success
            },
            &message,
        );
        self.emit_state(
            request_id,
            if failed {
                AgentOperationState::Failed
            } else {
                AgentOperationState::Succeeded
            },
        );
    }

    /// 记录 Agent 终止在途请求，并丢弃迟到的输出分片。
    pub(super) fn interrupt(
        &mut self,
        session_id: SessionId,
        request_id: RequestId,
        occurred_at: DateTime<Utc>,
        message: &str,
    ) {
        self.flush(session_id, request_id, occurred_at);
        self.remember_interrupted(session_id, request_id);
        self.emit_terminal(Some(request_id), occurred_at, AgentLogLevel::Error, message);
        self.emit_state(request_id, AgentOperationState::Interrupted);
    }

    fn remember_interrupted(&mut self, session_id: SessionId, request_id: RequestId) {
        if self.interrupted_requests.len() >= MAX_INTERRUPTED_REQUESTS
            && let Some(oldest) = self.interrupted_requests.iter().next().copied()
        {
            self.interrupted_requests.remove(&oldest);
        }
        self.interrupted_requests.insert((session_id, request_id));
    }

    /// 只在真实执行阶段变化时通知本地表现层，标准错误输出不改变执行状态。
    fn emit_state(&mut self, request_id: RequestId, state: AgentOperationState) {
        self.repetition.replace(None);
        emit_scoped(
            self.epoch,
            self.sender.as_ref(),
            AgentEvent::OperationStateChanged { request_id, state },
        );
        if state != AgentOperationState::Running
            && !self
                .serial_streams
                .values()
                .any(|stream| stream.request_id == request_id)
        {
            self.presentation_requests.remove(&request_id);
        }
    }

    /// 请求结束时提交最后一行并释放两个输出流的缓冲。
    fn flush(&mut self, session_id: SessionId, request_id: RequestId, time: DateTime<Utc>) -> bool {
        self.discard_response_events(session_id, request_id);
        self.device_requests.remove(&(session_id, request_id));
        let mut streamed = self.serial_queries.remove(&(session_id, request_id));
        for stderr in [false, true] {
            if let Some(mut buffer) = self.streams.remove(&(session_id, request_id, stderr)) {
                streamed = true;
                if let Some(line) = buffer.finish_line(&self.redactor) {
                    self.emit(
                        Some(request_id),
                        time,
                        if stderr {
                            AgentLogLevel::Error
                        } else {
                            AgentLogLevel::Info
                        },
                        &line,
                    );
                }
            }
        }
        self.discarded_streams
            .retain(|(session, request, _)| *session != session_id || *request != request_id);
        streamed
    }

    /// 限制文本长度后发布；完整脱敏总是在截断之前执行。
    fn emit(
        &self,
        request_id: Option<RequestId>,
        occurred_at: DateTime<Utc>,
        level: AgentLogLevel,
        message: &str,
    ) {
        self.emit_display(request_id, occurred_at, level, message, false);
    }

    // 仅真实完成/失败/中断分支调用，标准错误始终走普通 emit。
    fn emit_terminal(
        &self,
        request_id: Option<RequestId>,
        occurred_at: DateTime<Utc>,
        level: AgentLogLevel,
        message: &str,
    ) {
        self.emit_display(request_id, occurred_at, level, message, true);
    }

    // 重复的“开始执行”只进入诊断，执行状态仍由关键事件单独发布。
    fn emit_serial_line(
        &self,
        session: SessionId,
        resource: SerialSessionId,
        request: RequestId,
        line: &str,
        reliable: bool,
    ) {
        if !reliable || !repeatable_serial_line(line) {
            self.emit(Some(request), Utc::now(), AgentLogLevel::Info, line);
            return;
        }
        let repetition = {
            let mut previous = self.repetition.borrow_mut();
            if let Some(previous) = previous.as_mut()
                && previous.session == session
                && previous.resource == resource
                && previous.request == request
                && previous.text == line
            {
                previous.value.total = previous.value.total.saturating_add(1);
                previous.value
            } else {
                let value = AgentOutputRepetition {
                    record_id: NEXT_SERIAL_RECORD.fetch_add(1, Ordering::Relaxed),
                    total: 1,
                };
                *previous = Some(SerialRepetition {
                    session,
                    resource,
                    request,
                    text: line.to_owned(),
                    value,
                });
                value
            }
        };
        let message = if repetition.total > 1 {
            format!("{line} × {}", repetition.total)
        } else {
            line.to_owned()
        };
        emit_scoped(
            self.epoch,
            self.sender.as_ref(),
            AgentEvent::OperationLog(AgentOperationLog {
                repetition: Some(repetition),
                request_id: Some(request),
                occurred_at: Utc::now(),
                level: AgentLogLevel::Info,
                message,
            }),
        );
        emit_scoped(
            self.epoch,
            self.sender.as_ref(),
            AgentEvent::OperationOutputUpdate {
                request_id: request,
                text: line.to_owned(),
                repetition,
            },
        );
    }

    fn emit_diagnostic(
        &self,
        request_id: Option<RequestId>,
        occurred_at: DateTime<Utc>,
        level: AgentLogLevel,
        message: &str,
    ) {
        self.repetition.replace(None);
        emit_scoped(
            self.epoch,
            self.sender.as_ref(),
            AgentEvent::OperationLog(AgentOperationLog {
                repetition: None,
                request_id,
                occurred_at,
                level,
                message: safe_text(&self.redactor, message),
            }),
        );
    }

    fn emit_display(
        &self,
        request_id: Option<RequestId>,
        occurred_at: DateTime<Utc>,
        level: AgentLogLevel,
        message: &str,
        terminal: bool,
    ) {
        self.repetition.replace(None);
        let text = safe_text(&self.redactor, message);
        let presentation = request_id
            .filter(|request| self.presentation_requests.contains(request))
            .map(|request_id| AgentEvent::OperationPresentation {
                request_id,
                role: match level {
                    AgentLogLevel::Success => AgentPresentationRole::Success,
                    AgentLogLevel::Error => AgentPresentationRole::Error,
                    AgentLogLevel::Info => AgentPresentationRole::Output,
                    AgentLogLevel::Running => AgentPresentationRole::Info,
                },
                text: text.clone(),
                terminal,
            });
        emit_scoped(
            self.epoch,
            self.sender.as_ref(),
            AgentEvent::OperationLog(AgentOperationLog {
                repetition: None,
                request_id,
                occurred_at,
                level,
                message: text,
            }),
        );
        if let Some(event) = presentation {
            emit_scoped(self.epoch, self.sender.as_ref(), event);
        }
    }
}

/// 格式化已授权的操作名称，只读取操作本身，不读取请求附带的凭据载荷。
/// System 在 Windows 执行的是 cmd；其他平台保持系统 Shell，不声称具体 bash。
pub(super) const fn normalize_observed_shell(shell: ShellKind) -> ShellKind {
    if cfg!(windows) && matches!(shell, ShellKind::System) {
        ShellKind::Cmd
    } else {
        shell
    }
}

pub(super) fn record_request(
    sender: Option<&AgentEventSink>,
    request: &RemoteRequest,
    epoch: Option<u64>,
    hide_command: bool,
    shell: Option<ShellKind>,
) {
    static REDACTOR: OnceLock<Redactor> = OnceLock::new();
    if sender.is_none() {
        return;
    }
    let redactor = REDACTOR.get_or_init(Redactor::default);
    let operation = &request.operation;
    let (kind, resource_id) = match operation {
        RemoteOperation::RunCommand { .. } | RemoteOperation::OpenShell { .. } => {
            (AgentOperationKind::Shell, None)
        }
        RemoteOperation::RunShellCommand { shell_id, .. }
        | RemoteOperation::CloseShell { shell_id } => {
            (AgentOperationKind::Shell, Some(shell_id.to_string()))
        }
        RemoteOperation::RunSsh { .. } => (AgentOperationKind::Ssh, None),
        RemoteOperation::WriteSerial {
            serial_session_id, ..
        }
        | RemoteOperation::RunSerialQuery {
            serial_session_id, ..
        }
        | RemoteOperation::CloseSerial { serial_session_id } => {
            (AgentOperationKind::Serial, Some(serial_session_id.clone()))
        }
        RemoteOperation::OpenSerial { .. } | RemoteOperation::ListSerial => {
            (AgentOperationKind::Serial, None)
        }
        _ => (AgentOperationKind::Other, None),
    };
    emit_scoped(
        epoch,
        sender,
        AgentEvent::OperationContext {
            request_id: request.request_id,
            kind,
            source: request.source,
            resource_id,
            shell: match operation {
                RemoteOperation::RunCommand { shell, .. }
                | RemoteOperation::OpenShell { shell } => Some(normalize_observed_shell(*shell)),
                RemoteOperation::RunShellCommand { .. } | RemoteOperation::CloseShell { .. } => {
                    shell.map(normalize_observed_shell)
                }
                _ => None,
            },
        },
    );
    let message = match operation {
        RemoteOperation::RunCommand { command, .. }
        | RemoteOperation::RunShellCommand { command, .. }
        | RemoteOperation::RunSerialQuery { command, .. }
        | RemoteOperation::RunSsh { command, .. } => {
            if hide_command {
                format!(
                    "收到{}：[身份认证阶段输入已隐藏]",
                    operation_label(operation)
                )
            } else {
                let command = if matches!(
                    operation,
                    RemoteOperation::RunSerialQuery { .. } | RemoteOperation::RunSsh { .. }
                ) {
                    remoteops_serial::redact_serial_text(command)
                } else {
                    command.to_owned()
                };
                format!("收到{}：{command}", operation_label(operation))
            }
        }
        _ => format!("收到操作：{}", operation_label(operation)),
    };
    emit_scoped(
        epoch,
        sender,
        AgentEvent::OperationLog(AgentOperationLog {
            repetition: None,
            request_id: Some(request.request_id),
            occurred_at: Utc::now(),
            level: AgentLogLevel::Info,
            message: safe_text(redactor, &message),
        }),
    );
    record_presentation_request(sender, request, epoch, hide_command, redactor);
}

// 与详细诊断分开构造终端命令，调用同一脱敏边界，不读取认证载荷。
fn record_presentation_request(
    sender: Option<&AgentEventSink>,
    request: &RemoteRequest,
    epoch: Option<u64>,
    hide_command: bool,
    redactor: &Redactor,
) {
    let operation = &request.operation;
    if presented_operation(operation) {
        let text = match operation {
            RemoteOperation::RunCommand { command, .. }
            | RemoteOperation::RunShellCommand { command, .. }
            | RemoteOperation::RunSerialQuery { command, .. }
            | RemoteOperation::RunSsh { command, .. } => {
                if hide_command {
                    "[身份认证阶段输入已隐藏]".to_owned()
                } else if matches!(
                    operation,
                    RemoteOperation::RunSerialQuery { .. } | RemoteOperation::RunSsh { .. }
                ) {
                    remoteops_serial::redact_serial_text(command)
                } else {
                    command.to_owned()
                }
            }
            _ => operation_label(operation).to_owned(),
        };
        emit_scoped(
            epoch,
            sender,
            AgentEvent::OperationPresentation {
                request_id: request.request_id,
                role: AgentPresentationRole::Command,
                text: safe_text(redactor, &text),
                terminal: false,
            },
        );
    }
}

// 按真实操作类型筛选；用户命令返回 JSON 时仍是命令输出，不受正文内容影响。
fn presented_operation(operation: &RemoteOperation) -> bool {
    matches!(
        operation,
        RemoteOperation::RunCommand { .. }
            | RemoteOperation::OpenShell { .. }
            | RemoteOperation::RunShellCommand { .. }
            | RemoteOperation::CloseShell { .. }
            | RemoteOperation::RunSsh { .. }
            | RemoteOperation::OpenSerial { .. }
            | RemoteOperation::ListSerial
            | RemoteOperation::WriteSerial { .. }
            | RemoteOperation::RunSerialQuery { .. }
            | RemoteOperation::CloseSerial { .. }
            | RemoteOperation::UploadFile { .. }
            | RemoteOperation::BeginUploadFile { .. }
            | RemoteOperation::CompleteUploadFile { .. }
            | RemoteOperation::DownloadFile { .. }
            | RemoteOperation::MoveFile { .. }
            | RemoteOperation::DeleteFile { .. }
    )
}

fn emit_scoped(epoch: Option<u64>, sender: Option<&AgentEventSink>, event: AgentEvent) {
    let event = if let Some(epoch) = epoch {
        AgentEvent::ScopedOperation {
            epoch,
            event: Box::new(event),
        }
    } else {
        event
    };
    emit_agent_event(sender, event);
}

/// 对可能很长的诊断文本先脱敏，再保留可展示的开头部分。
fn safe_text(redactor: &Redactor, message: &str) -> String {
    let text = if message.contains("-----BEGIN ")
        && message.contains("PRIVATE KEY-----")
        && !message.contains("-----END ")
    {
        "[REDACTED PRIVATE KEY]".to_owned()
    } else {
        redactor.redact(message)
    };
    if let Some((index, _)) = text.char_indices().nth(2048) {
        format!("{}\n[内容已截断]", &text[..index])
    } else {
        text
    }
}

/// 将核心操作映射为现场人员可理解的类型名称。
fn operation_label(operation: &RemoteOperation) -> &'static str {
    match operation {
        RemoteOperation::GetTargetContext => "读取目标上下文",
        RemoteOperation::ValidateResourceTool { .. } => "受控验证资源工具",
        RemoteOperation::PrepareBrowserPassword { .. } => "核验网页密码目标",
        RemoteOperation::FillBrowserPassword { .. } => "安全填写网页密码",
        RemoteOperation::ListResourceTools => "列出资源工具",
        RemoteOperation::StartResourceTool { .. } => "启动资源工具",
        RemoteOperation::CallResourceTool { .. } => "调用资源工具",
        RemoteOperation::StopResourceTool { .. } => "停止资源工具",
        RemoteOperation::OpenShell { .. } => "打开 Shell",
        RemoteOperation::RunShellCommand { .. } | RemoteOperation::RunCommand { .. } => "命令",
        RemoteOperation::CloseShell { .. } => "关闭 Shell",
        RemoteOperation::TestPort { .. } => "端口探测",
        RemoteOperation::UploadFile { .. }
        | RemoteOperation::BeginUploadFile { .. }
        | RemoteOperation::UploadFileChunk { .. }
        | RemoteOperation::CompleteUploadFile { .. }
        | RemoteOperation::AbortUploadFile { .. } => "文件上传",
        RemoteOperation::DownloadFile { .. }
        | RemoteOperation::AuthorizeDownloadFile { .. }
        | RemoteOperation::DownloadFileChunk { .. } => "文件下载",
        RemoteOperation::GetFileMetadata { .. } => "文件信息查询",
        RemoteOperation::MoveFile { .. } | RemoteOperation::DeleteFile { .. } => "文件管理",
        RemoteOperation::CreateTcpForward { .. } => "创建持续 TCP 转发",
        RemoteOperation::TcpExchange { .. } => "TCP 通信",
        RemoteOperation::ListProcesses | RemoteOperation::TerminateProcess { .. } => "进程管理",
        RemoteOperation::ListServices | RemoteOperation::ControlService { .. } => "服务管理",
        RemoteOperation::PowerControl { .. } => "电源控制",
        RemoteOperation::OpenSerial { .. }
        | RemoteOperation::ListSerial
        | RemoteOperation::WriteSerial { .. }
        | RemoteOperation::RunSerialQuery { .. }
        | RemoteOperation::CloseSerial { .. } => "串口操作",
        RemoteOperation::RunSsh { .. } => "SSH 命令",
        RemoteOperation::CloseConnection => "关闭连接",
        RemoteOperation::HumanTakeover => "人工接管",
        RemoteOperation::ReleaseHumanTakeover => "释放人工接管",
        RemoteOperation::EmergencyStop => "紧急停止",
        RemoteOperation::CancelRequest { .. } => "取消请求",
        RemoteOperation::VisualObserve { .. } => "图形观察",
        RemoteOperation::VisualWaitFor { .. } => "等待图形状态",
        RemoteOperation::VisualInvoke { .. } => "图形控件操作",
        RemoteOperation::VisualTypeText { .. } => "图形文本输入",
        RemoteOperation::VisualSendInput { .. } => "图形键鼠输入",
        RemoteOperation::VisualStop => "停止图形会话",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remoteops_domain::{ApprovalState, EventSource};

    fn observe(
        observer: &mut LogObserver,
        session_id: SessionId,
        request_id: RequestId,
        payload: EventPayload,
    ) {
        observer.observe(&WireMessage::RemoteEvent(RemoteEvent {
            sequence: 1,
            session_id,
            request_id: Some(request_id),
            source: EventSource::Human,
            approval: ApprovalState::NotRequired,
            occurred_at: Utc::now(),
            payload,
        }));
    }

    fn metadata_request(operation: RemoteOperation) -> RemoteRequest {
        RemoteRequest {
            request_id: RequestId::new(),
            session_id: SessionId::new(),
            source: EventSource::Human,
            operation,
            approval_id: None,
            payload_base64: None,
            control_proof: None,
        }
    }

    fn presentation(event: AgentEvent) -> Option<(AgentPresentationRole, String, bool)> {
        match event {
            AgentEvent::ScopedOperation { event, .. } => presentation(*event),
            AgentEvent::OperationPresentation {
                role,
                text,
                terminal,
                ..
            } => Some((role, text, terminal)),
            _ => None,
        }
    }

    fn shell_response(
        request: &RemoteRequest,
        exit_code: Option<i32>,
        summary: &str,
        details: Option<serde_json::Value>,
    ) -> RemoteResponse {
        RemoteResponse {
            request_id: request.request_id,
            session_id: request.session_id,
            exit_code,
            summary: summary.into(),
            error_code: None,
            payload_base64: None,
            sha256: None,
            details,
        }
    }

    fn response_event(
        request: &RemoteRequest,
        sequence: u64,
        payload: EventPayload,
    ) -> RemoteEvent {
        RemoteEvent {
            sequence,
            session_id: request.session_id,
            request_id: Some(request.request_id),
            source: request.source,
            approval: ApprovalState::NotRequired,
            occurred_at: Utc::now(),
            payload,
        }
    }

    #[test]
    fn shell_open_and_close_present_readable_status_without_resource_ids() {
        let resource = ShellId::new();
        for (operation, expected) in [
            (
                RemoteOperation::OpenShell {
                    shell: ShellKind::WindowsPowerShell,
                },
                "Windows PowerShell 持久终端已打开",
            ),
            (
                RemoteOperation::CloseShell { shell_id: resource },
                "持久终端已关闭",
            ),
        ] {
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut observer = LogObserver::new(Some(sender));
            let request = metadata_request(operation);
            observer.record_request(&request, Some(ShellKind::WindowsPowerShell));
            let details =
                serde_json::json!({"shell_id":resource.to_string(),"shell":"WindowsPowerShell"});
            let response = shell_response(
                &request,
                Some(0),
                &format!("持久 Shell {resource} 已处理"),
                Some(details.clone()),
            );
            for event in [
                response_event(&request, 10, output(&details.to_string())),
                response_event(
                    &request,
                    11,
                    EventPayload::OperationCompleted {
                        exit_code: response.exit_code,
                        summary: response.summary.clone(),
                    },
                ),
            ] {
                observer.annotate_response_event(&request, &response, &event);
                observer.observe(&WireMessage::RemoteEvent(event));
            }
            let rows: Vec<_> = receiver.try_iter().filter_map(presentation).collect();
            assert!(rows.iter().any(|(_, text, _)| text == expected));
            assert!(
                rows.iter().all(|(_, text, _)| !text.contains("shell_id")
                    && !text.contains(&resource.to_string()))
            );
            assert!(
                rows.iter().any(
                    |(role, _, terminal)| *role == AgentPresentationRole::Success && *terminal
                )
            );
            assert!(observer.response_presentations.is_empty());
            assert_eq!(response.details, Some(details));
        }
    }

    #[test]
    fn shell_metadata_does_not_consume_genuine_json_or_split_unterminated_stdout() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let request = metadata_request(RemoteOperation::RunShellCommand {
            shell_id: ShellId::new(),
            shell: ShellKind::PowerShell,
            command: "产生中文JSON，不带末尾换行".into(),
            readonly: true,
        });
        observer.record_request(&request, Some(ShellKind::PowerShell));
        let literal = r#"{"shell_id":"user-output","shell_closed":false,"中文":"正常"}"#;
        let response = shell_response(
            &request,
            Some(0),
            literal,
            Some(serde_json::json!({"shell_id":"internal-resource","shell_closed":false})),
        );
        let metadata = response_event(
            &request,
            12,
            output(&response.details.as_ref().unwrap().to_string()),
        );
        let completed = response_event(
            &request,
            13,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: literal.into(),
            },
        );
        // 与writer真正排队相同：先标注响应，但前面的stdout尚未消费。
        observer.annotate_response_event(&request, &response, &metadata);
        observer.annotate_response_event(&request, &response, &completed);
        for event in [
            response_event(&request, 10, output(&literal[..20])),
            response_event(&request, 11, output(&literal[20..])),
            metadata,
            completed,
        ] {
            observer.observe(&WireMessage::RemoteEvent(event));
        }
        let rows: Vec<_> = receiver.try_iter().filter_map(presentation).collect();
        assert_eq!(
            rows.iter().filter(|(_, text, _)| text == literal).count(),
            1
        );
        assert!(
            rows.iter()
                .all(|(_, text, _)| !text.contains("internal-resource"))
        );
        assert!(observer.streams.is_empty() && observer.response_presentations.is_empty());
    }

    #[test]
    fn closed_shell_keeps_real_stderr_exit_and_redaction_before_status() {
        for failed in [false, true] {
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut observer = LogObserver::new(Some(sender));
            let request = metadata_request(RemoteOperation::RunShellCommand {
                shell_id: ShellId::new(),
                shell: ShellKind::PowerShell,
                command: "测试终端结束".into(),
                readonly: false,
            });
            observer.record_request(&request, Some(ShellKind::PowerShell));
            observe(
                &mut observer,
                request.session_id,
                request.request_id,
                output("中文 password=sec"),
            );
            observe(
                &mut observer,
                request.session_id,
                request.request_id,
                output("ret"),
            );
            observe(
                &mut observer,
                request.session_id,
                request.request_id,
                EventPayload::OutputChunk {
                    stderr: true,
                    text: "真实标准错误\n".into(),
                },
            );
            let mut response = shell_response(
                &request,
                Some(7),
                "原始汇总 password=secret",
                Some(serde_json::json!({"shell_closed":true})),
            );
            let payload = if failed {
                response.error_code = Some("agent_operation_failed".into());
                EventPayload::OperationFailed {
                    code: "agent_operation_failed".into(),
                    message: "中文失败 password=secret".into(),
                }
            } else {
                EventPayload::OperationCompleted {
                    exit_code: Some(7),
                    summary: response.summary.clone(),
                }
            };
            let event = response_event(&request, 3, payload);
            observer.annotate_response_event(&request, &response, &event);
            observer.observe(&WireMessage::RemoteEvent(event));
            let rows: Vec<_> = receiver.try_iter().filter_map(presentation).collect();
            assert!(rows.iter().any(|(role, text, terminal)| *role
                == AgentPresentationRole::Error
                && !terminal
                && text == "真实标准错误"));
            assert!(
                rows.iter()
                    .any(|(_, text, _)| text.contains("持久终端已结束"))
            );
            assert!(rows.iter().any(|(role, text, terminal)| *role
                == AgentPresentationRole::Error
                && *terminal
                && text.contains(if failed {
                    "中文失败"
                } else {
                    "退出码 7"
                })));
            assert!(
                rows.iter()
                    .all(|(role, text, _)| *role != AgentPresentationRole::Success
                        && !text.contains("secret"))
            );
        }
    }

    #[test]
    fn non_shell_response_metadata_keeps_existing_diagnostic_json() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let request = metadata_request(RemoteOperation::GetTargetContext);
        observer.record_request(&request, None);
        let literal = r#"{"shell_id":"not-a-shell-result","中文":"正常"}"#;
        let response = shell_response(&request, Some(0), "", None);
        let event = response_event(&request, 9, output(&format!("{literal}\n")));
        observer.annotate_response_event(&request, &response, &event);
        assert!(observer.response_presentations.is_empty());
        observer.observe(&WireMessage::RemoteEvent(event));
        assert!(
            receiver.try_iter().any(
                |event| matches!(event, AgentEvent::OperationLog(log) if log.message == literal)
            )
        );
    }

    #[test]
    fn interrupted_or_failed_enqueue_releases_response_annotations() {
        let mut observer = LogObserver::new(None);
        let request = metadata_request(RemoteOperation::OpenShell {
            shell: ShellKind::Cmd,
        });
        let response = shell_response(
            &request,
            Some(0),
            "",
            Some(serde_json::json!({"shell_id":"internal"})),
        );
        let event = response_event(&request, 20, output("internal metadata"));
        observer.annotate_response_event(&request, &response, &event);
        assert_eq!(observer.response_presentations.len(), 1);
        observer.discard_response_events(request.session_id, request.request_id);
        assert!(observer.response_presentations.is_empty());
        observer.annotate_response_event(&request, &response, &event);
        observer.interrupt(
            request.session_id,
            request.request_id,
            Utc::now(),
            "连接已断开",
        );
        assert!(observer.response_presentations.is_empty());
        observer.annotate_response_event(&request, &response, &event);
        observer.observe(&WireMessage::RemoteEvent(event));
        assert!(observer.response_presentations.is_empty() && observer.streams.is_empty());
    }

    #[test]
    fn presentation_is_structured_and_internal_context_json_remains_only_in_diagnostics() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let internal = metadata_request(RemoteOperation::GetTargetContext);
        observer.record_request(&internal, None);
        observe(
            &mut observer,
            internal.session_id,
            internal.request_id,
            output(
                r#"{"diagnostic":true}
"#,
            ),
        );
        observe(
            &mut observer,
            internal.session_id,
            internal.request_id,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: String::new(),
            },
        );
        let events: Vec<_> = receiver.try_iter().collect();
        assert!(events.iter().any(|event| matches!(event, AgentEvent::OperationLog(log) if log.message.contains("diagnostic"))));
        assert!(events.into_iter().find_map(presentation).is_none());
        let request = metadata_request(RemoteOperation::RunCommand {
            shell: ShellKind::Cmd,
            command: "echo command".into(),
            readonly: true,
        });
        observer.record_request(&request, None);
        observe(
            &mut observer,
            request.session_id,
            request.request_id,
            EventPayload::OperationStarted,
        );
        observe(
            &mut observer,
            request.session_id,
            request.request_id,
            output("{\"error\":\"ordinary data\"}\n"),
        );
        observe(
            &mut observer,
            request.session_id,
            request.request_id,
            EventPayload::OutputChunk {
                stderr: true,
                text: "warning\n".into(),
            },
        );
        observe(
            &mut observer,
            request.session_id,
            request.request_id,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: String::new(),
            },
        );
        let rows: Vec<_> = receiver.try_iter().filter_map(presentation).collect();
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows[0],
            (AgentPresentationRole::Command, "echo command".into(), false)
        );
        assert_eq!(
            rows[1],
            (
                AgentPresentationRole::Output,
                r#"{"error":"ordinary data"}"#.into(),
                false
            )
        );
        assert_eq!(
            rows[2],
            (AgentPresentationRole::Error, "warning".into(), false)
        );
        assert_eq!(
            rows[3],
            (
                AgentPresentationRole::Success,
                "执行完成 · 退出码 0".into(),
                true
            )
        );
        assert!(observer.presentation_requests.is_empty());
    }

    #[test]
    fn presentation_reuses_split_secret_and_private_key_redaction() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let request = metadata_request(RemoteOperation::RunCommand {
            shell: ShellKind::Cmd,
            command: "echo password=command-secret".into(),
            readonly: true,
        });
        observer.record_request(&request, None);
        observe(
            &mut observer,
            request.session_id,
            request.request_id,
            output("password=split-"),
        );
        observe(
            &mut observer,
            request.session_id,
            request.request_id,
            output(
                "secret\n-----BEGIN PRIVATE KEY-----\nkey-material\n-----END PRIVATE KEY-----\n",
            ),
        );
        observe(
            &mut observer,
            request.session_id,
            request.request_id,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: String::new(),
            },
        );
        let rows: Vec<_> = receiver.try_iter().filter_map(presentation).collect();
        let text = rows
            .iter()
            .map(|(_, text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for secret in ["command-secret", "split-secret", "key-material"] {
            assert!(!text.contains(secret));
        }
        assert!(text.contains("[REDACTED]"));
        assert!(text.contains("[REDACTED PRIVATE KEY]"));
    }

    fn metadata_context(
        event: AgentEvent,
    ) -> Option<(RequestId, Option<ShellKind>, Option<String>)> {
        match event {
            AgentEvent::ScopedOperation { event, .. } => metadata_context(*event),
            AgentEvent::OperationContext {
                request_id,
                shell,
                resource_id,
                ..
            } => Some((request_id, shell, resource_id)),
            _ => None,
        }
    }

    #[test]
    fn shell_metadata_uses_structured_kind_and_ignores_other_shell_names_in_text() {
        for shell in [
            ShellKind::Cmd,
            ShellKind::WindowsPowerShell,
            ShellKind::PowerShell,
            ShellKind::System,
        ] {
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut observer = LogObserver::new(Some(sender));
            let request = metadata_request(RemoteOperation::RunCommand {
                shell,
                command: "echo PowerShell bash cmd.exe".into(),
                readonly: true,
            });
            observer.record_request(&request, None);
            let contexts: Vec<_> = receiver.try_iter().filter_map(metadata_context).collect();
            assert_eq!(
                contexts,
                vec![(
                    request.request_id,
                    Some(normalize_observed_shell(shell)),
                    None
                )]
            );
        }
        assert_eq!(
            normalize_observed_shell(ShellKind::System),
            if cfg!(windows) {
                ShellKind::Cmd
            } else {
                ShellKind::System
            }
        );
    }

    #[test]
    fn persistent_shell_metadata_inherits_runtime_kind_not_claimed_request_kind() {
        let resource = ShellId::new();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let request = metadata_request(RemoteOperation::RunShellCommand {
            shell_id: resource,
            shell: ShellKind::Cmd,
            command: "echo requested CMD but actual PowerShell".into(),
            readonly: true,
        });
        observer.record_request(&request, Some(ShellKind::WindowsPowerShell));
        assert_eq!(
            receiver.try_iter().find_map(metadata_context),
            Some((
                request.request_id,
                Some(ShellKind::WindowsPowerShell),
                Some(resource.to_string())
            ))
        );
        let unknown = metadata_request(RemoteOperation::CloseShell {
            shell_id: ShellId::new(),
        });
        observer.record_request(&unknown, None);
        assert_eq!(
            receiver.try_iter().find_map(metadata_context),
            Some((
                unknown.request_id,
                None,
                match unknown.operation {
                    RemoteOperation::CloseShell { shell_id } => Some(shell_id.to_string()),
                    _ => unreachable!(),
                }
            ))
        );
    }

    #[test]
    fn opened_shell_metadata_keeps_resource_and_epoch_and_serial_has_no_shell() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let observer = LogObserver::new(Some(sender)).with_epoch(Some(17));
        let request = metadata_request(RemoteOperation::OpenShell {
            shell: ShellKind::PowerShell,
        });
        let resource = ShellId::new();
        observer.open_shell(&request, resource, ShellKind::PowerShell);
        let event = receiver.recv().unwrap();
        assert!(matches!(
            &event,
            AgentEvent::ScopedOperation { epoch: 17, .. }
        ));
        assert_eq!(
            metadata_context(event),
            Some((
                request.request_id,
                Some(ShellKind::PowerShell),
                Some(resource.to_string())
            ))
        );
        let mut other = LogObserver::new(None).with_epoch(Some(18));
        // 另一代观察器和另一会话不继承任何资源信息；串口始终保留独立主类。
        other.record_request(
            &metadata_request(RemoteOperation::CloseShell { shell_id: resource }),
            None,
        );
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut serial = LogObserver::new(Some(sender));
        serial.open_serial(
            SessionId::new(),
            SerialSessionId::new(),
            RequestId::new(),
            EventSource::Human,
        );
        assert!(
            receiver
                .try_iter()
                .filter_map(metadata_context)
                .all(|(_, shell, _)| shell.is_none())
        );
    }

    fn output(text: &str) -> EventPayload {
        EventPayload::OutputChunk {
            stderr: false,
            text: text.to_owned(),
        }
    }

    /// 仅收集执行阶段，验证输出日志不会被误判成终态。
    fn states(
        receiver: &std::sync::mpsc::Receiver<AgentEvent>,
    ) -> Vec<(RequestId, AgentOperationState)> {
        receiver
            .try_iter()
            .filter_map(|event| match event {
                AgentEvent::OperationStateChanged { request_id, state } => {
                    Some((request_id, state))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn stderr_output_keeps_request_running_until_real_completion() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let session = SessionId::new();
        let request = RequestId::new();
        observe(
            &mut observer,
            session,
            request,
            EventPayload::OperationStarted,
        );
        assert_eq!(states(&receiver), [(request, AgentOperationState::Running)]);
        observe(
            &mut observer,
            session,
            request,
            EventPayload::OutputChunk {
                stderr: true,
                text: "warning\npending warning".to_owned(),
            },
        );
        let events: Vec<_> = receiver.try_iter().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::OperationLog(AgentOperationLog {
                level: AgentLogLevel::Error,
                ..
            })
        )));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::OperationStateChanged { .. }))
        );
        observe(
            &mut observer,
            session,
            request,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: String::new(),
            },
        );
        assert_eq!(
            states(&receiver),
            [(request, AgentOperationState::Succeeded)]
        );
    }

    #[test]
    fn terminal_states_cover_failure_cancellation_and_interruption() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let session = SessionId::new();
        let first = RequestId::new();
        let second = RequestId::new();
        observe(
            &mut observer,
            session,
            first,
            EventPayload::OperationStarted,
        );
        observe(
            &mut observer,
            session,
            second,
            EventPayload::OperationStarted,
        );
        assert_eq!(
            states(&receiver),
            [
                (first, AgentOperationState::Running),
                (second, AgentOperationState::Running),
            ]
        );
        observe(
            &mut observer,
            session,
            first,
            EventPayload::OperationCompleted {
                exit_code: Some(2),
                summary: String::new(),
            },
        );
        assert_eq!(states(&receiver), [(first, AgentOperationState::Failed)]);
        observe(
            &mut observer,
            session,
            second,
            EventPayload::OperationCancelled,
        );
        assert_eq!(
            states(&receiver),
            [(second, AgentOperationState::Cancelled)]
        );
        let failed = RequestId::new();
        observe(
            &mut observer,
            session,
            failed,
            EventPayload::OperationFailed {
                code: "failed".to_owned(),
                message: "failed".to_owned(),
            },
        );
        assert_eq!(states(&receiver), [(failed, AgentOperationState::Failed)]);
        let interrupted = RequestId::new();
        observer.interrupt(session, interrupted, Utc::now(), "连接中断");
        assert_eq!(
            states(&receiver),
            [(interrupted, AgentOperationState::Interrupted)]
        );
        observe(
            &mut observer,
            session,
            interrupted,
            EventPayload::OperationStarted,
        );
        observe(
            &mut observer,
            session,
            interrupted,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: String::new(),
            },
        );
        assert_eq!(
            states(&receiver),
            Vec::<(remoteops_domain::RequestId, AgentOperationState)>::new()
        );
    }

    #[test]
    fn rejected_response_reports_failure_without_duplicate_execution_failure() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let session = SessionId::new();
        let request = RequestId::new();
        for error_code in ["agent_request_limit_reached", "agent_operation_failed"] {
            observer.observe(&WireMessage::RemoteResponse(
                remoteops_protocol::RemoteResponse {
                    request_id: request,
                    session_id: session,
                    exit_code: None,
                    summary: "请求被拒绝".to_owned(),
                    error_code: Some(error_code.to_owned()),
                    payload_base64: None,
                    sha256: None,
                    details: None,
                },
            ));
            let expected = if error_code == "agent_operation_failed" {
                Vec::new()
            } else {
                vec![(request, AgentOperationState::Failed)]
            };
            assert_eq!(states(&receiver), expected);
        }
    }

    #[test]
    fn combines_split_secrets_and_flushes_last_line_on_completion() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let session = SessionId::new();
        let request = RequestId::new();
        observe(&mut observer, session, request, output("password=sec"));
        assert!(receiver.try_recv().is_err());
        observe(&mut observer, session, request, output("ret\nresult: ok"));
        observe(
            &mut observer,
            session,
            request,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: "duplicate".to_owned(),
            },
        );
        let logs: Vec<_> = receiver
            .try_iter()
            .filter_map(|event| match event {
                AgentEvent::OperationLog(log) => Some(log),
                _ => None,
            })
            .collect();
        assert_eq!(logs.len(), 3);
        assert!(!logs[0].message.contains("secret"));
        assert!(logs[0].message.contains("[REDACTED]"));
        assert_eq!(logs[1].message, "result: ok");
        assert_eq!(logs[2].level, AgentLogLevel::Success);
        assert_eq!(logs[2].request_id, Some(request));
        assert!(observer.streams.is_empty());
    }

    #[test]
    fn hides_private_keys_across_output_chunks_and_bounds_long_lines() {
        let redactor = Redactor::default();
        let mut buffer = OutputBuffer::default();
        assert_eq!(
            buffer.push("-----BEGIN PRIVATE KEY-----\nmaterial\n", &redactor),
            ["[REDACTED PRIVATE KEY]"]
        );
        assert_eq!(
            buffer.push("more material\n-----END PRIVATE KEY-----\n", &redactor),
            Vec::<String>::new()
        );
        assert_eq!(
            buffer.push(&"界".repeat(MAX_LINE_BYTES), &redactor),
            Vec::<String>::new()
        );
        assert!(buffer.pending.len() <= MAX_LINE_BYTES);
        assert_eq!(buffer.push("\n", &redactor), ["[单行输出过长，内容已隐藏]"]);
        assert_eq!(buffer.push("next\n", &redactor), ["next"]);
    }

    #[test]
    fn oversized_private_key_marker_suppresses_following_material() {
        let redactor = Redactor::default();
        let mut buffer = OutputBuffer::default();
        let oversized_marker = format!(
            "{}-----BEGIN PRIVATE KEY-----\n",
            "x".repeat(MAX_LINE_BYTES)
        );
        assert_eq!(
            buffer.push(&oversized_marker, &redactor),
            Vec::<String>::new()
        );
        assert_eq!(
            buffer.push("secret material\n", &redactor),
            Vec::<String>::new()
        );
        assert_eq!(
            buffer.push("-----END PRIVATE KEY-----\nvisible\n", &redactor),
            ["visible"]
        );
        assert_eq!(buffer.push("next\n", &redactor), ["next"]);
    }

    #[test]
    fn split_oversized_private_key_marker_keeps_boundary_state() {
        let redactor = Redactor::default();
        let mut buffer = OutputBuffer::default();
        assert_eq!(
            buffer.push("-----BEGIN PRIVATE ", &redactor),
            Vec::<String>::new()
        );
        let oversized_tail = format!("KEY-----{}\n", "x".repeat(MAX_LINE_BYTES));
        assert_eq!(
            buffer.push(&oversized_tail, &redactor),
            Vec::<String>::new()
        );
        assert_eq!(
            buffer.push("secret material\n", &redactor),
            Vec::<String>::new()
        );
        assert_eq!(
            buffer.push("-----END PRIVATE KEY-----\nnext\n", &redactor),
            ["next"]
        );
    }

    #[test]
    fn oversized_tail_tracks_begin_marker_across_later_small_chunks() {
        let redactor = Redactor::default();
        let mut buffer = OutputBuffer::default();
        assert_eq!(
            buffer.push(&"x".repeat(MAX_LINE_BYTES + 1), &redactor),
            Vec::<String>::new()
        );
        assert_eq!(buffer.push("-----BEGIN ", &redactor), Vec::<String>::new());
        assert_eq!(
            buffer.push(
                "PRIVATE KEY-----\nsecret material\n-----END PRIVATE KEY-----\n",
                &redactor,
            ),
            Vec::<String>::new()
        );
        assert_eq!(buffer.push("next\n", &redactor), ["next"]);
    }

    #[test]
    fn pending_tail_is_replaced_by_oversized_tail_for_split_marker() {
        let redactor = Redactor::default();
        let mut buffer = OutputBuffer::default();
        assert_eq!(
            buffer.push(&"x".repeat(MAX_LINE_BYTES), &redactor),
            Vec::<String>::new()
        );
        assert_eq!(buffer.push("-----BEGIN ", &redactor), Vec::<String>::new());
        assert_eq!(
            buffer.push(
                "PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----\n",
                &redactor,
            ),
            Vec::<String>::new()
        );
        assert_eq!(buffer.push("visible\n", &redactor), ["visible"]);
    }

    #[test]
    fn capped_stream_is_discarded_until_completion() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let session = SessionId::new();
        let occupied_request = RequestId::new();
        observe(&mut observer, session, occupied_request, output("occupied"));
        for _ in 0..(MAX_STREAMS - 1) {
            observe(&mut observer, session, RequestId::new(), output("occupied"));
        }
        let target = RequestId::new();
        observe(
            &mut observer,
            session,
            target,
            output("-----BEGIN PRIVATE KEY-----\n"),
        );
        observe(
            &mut observer,
            session,
            occupied_request,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: String::new(),
            },
        );
        observe(&mut observer, session, target, output("secret material\n"));
        observe(
            &mut observer,
            session,
            target,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: String::new(),
            },
        );
        let logs: Vec<_> = receiver
            .try_iter()
            .filter_map(|event| match event {
                AgentEvent::OperationLog(log) => Some(log.message),
                _ => None,
            })
            .collect();
        assert!(
            !logs
                .iter()
                .any(|message| message.contains("secret material"))
        );
        assert!(observer.discarded_streams.is_empty());
    }

    #[test]
    fn interruption_flushes_stream_and_ignores_late_events() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let session = SessionId::new();
        let request = RequestId::new();
        observe(&mut observer, session, request, output("partial"));
        observer.interrupt(session, request, Utc::now(), "连接已断开，操作已中断");
        observe(&mut observer, session, request, output("late secret\n"));
        let logs: Vec<_> = receiver
            .try_iter()
            .filter_map(|event| match event {
                AgentEvent::OperationLog(log) => Some(log),
                _ => None,
            })
            .collect();
        assert!(logs.iter().any(|log| log.message == "partial"));
        assert!(logs.iter().any(|log| log.level == AgentLogLevel::Error));
        assert!(!logs.iter().any(|log| log.message.contains("late secret")));
    }

    #[test]
    fn output_cap_keeps_terminal_result_and_reports_nonzero_exit_as_error() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let session = SessionId::new();
        let request = RequestId::new();
        observe(
            &mut observer,
            session,
            request,
            output(&"line\n".repeat(500)),
        );
        observe(
            &mut observer,
            session,
            request,
            EventPayload::OperationCompleted {
                exit_code: Some(2),
                summary: String::new(),
            },
        );
        let logs: Vec<_> = receiver
            .try_iter()
            .filter(|event| matches!(event, AgentEvent::OperationLog(_)))
            .collect();
        assert_eq!(logs.len(), MAX_OUTPUT_LINES + 2);
        assert!(matches!(
            logs.last(),
            Some(AgentEvent::OperationLog(AgentOperationLog {
                level: AgentLogLevel::Error,
                ..
            }))
        ));
    }

    #[test]
    fn concurrent_requests_do_not_mix_incomplete_output() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let session = SessionId::new();
        let first = RequestId::new();
        let second = RequestId::new();
        observe(&mut observer, session, first, output("first"));
        observe(&mut observer, session, second, output("second\n"));
        observe(&mut observer, session, first, output(" line\n"));
        let messages: Vec<_> = receiver
            .try_iter()
            .filter_map(|event| {
                if let AgentEvent::OperationLog(log) = event {
                    Some(log.message)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(messages, ["second", "first line"]);
    }
    fn messages(receiver: &crate::BoundedAgentEventReceiver) -> Vec<String> {
        let mut lines = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            let event = match event {
                AgentEvent::ScopedOperation { event, .. } => *event,
                event => event,
            };
            if let AgentEvent::OperationLog(log) = event {
                lines.push(log.message);
            }
        }
        lines
    }
    #[test]
    fn bounded_subscriber_continues_past_128_without_silent_stop() {
        let (sender, receiver) = crate::bounded_agent_events(1024);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let request = RequestId::new();
        observe(
            &mut observer,
            session,
            request,
            output(&"line\n".repeat(500)),
        );
        observe(
            &mut observer,
            session,
            request,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: String::new(),
            },
        );
        let lines = messages(&receiver);
        assert_eq!(
            lines.iter().filter(|line| line.as_str() == "line").count(),
            500
        );
        assert!(lines.last().unwrap().contains("退出码 0"));
        assert_eq!(receiver.dropped_log_lines(), 0);
    }
    fn serial_logs(receiver: &crate::BoundedAgentEventReceiver) -> Vec<AgentOperationLog> {
        let mut logs = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            let event = match event {
                AgentEvent::ScopedOperation { event, .. } => *event,
                other => other,
            };
            if let AgentEvent::OperationLog(log) = event {
                logs.push(log);
            }
        }
        logs
    }

    #[test]
    fn serial_repetition_counts_survive_slow_consumer_without_changing_raw_lines() {
        for count in [1, 128, 10_000] {
            let (sender, receiver) = crate::bounded_agent_events(4);
            let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
            let session = SessionId::new();
            let resource = SerialSessionId::new();
            observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
            for _ in 0..count {
                assert_eq!(
                    observer.serial_output(session, resource, b"same line\r\n"),
                    ["same line"]
                );
            }
            let logs = serial_logs(&receiver);
            assert_eq!(logs.len(), 1);
            let repetition = logs[0].repetition.unwrap();
            assert_eq!(repetition.total, count);
            assert_eq!(
                logs[0].message,
                if count == 1 {
                    "same line".into()
                } else {
                    format!("same line × {count}")
                }
            );
            assert_eq!(receiver.dropped_log_lines(), 0);
        }
    }

    #[test]
    fn serial_repetition_only_merges_adjacent_complete_identical_lines() {
        let (sender, receiver) = crate::bounded_agent_events(32);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
        assert_eq!(
            observer.serial_output(session, resource, b"A\nA\nB\nA\n"),
            ["A", "A", "B", "A"]
        );
        let logs = serial_logs(&receiver);
        assert_eq!(
            logs.iter()
                .map(|log| log.message.as_str())
                .collect::<Vec<_>>(),
            ["A × 2", "B", "A"]
        );
        assert_ne!(
            logs[0].repetition.unwrap().record_id,
            logs[2].repetition.unwrap().record_id
        );
    }

    #[test]
    fn blank_lines_break_repetition_without_changing_remote_output() {
        let (sender, receiver) = crate::bounded_agent_events(16);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
        assert_eq!(
            observer.serial_output(session, resource, b"A\n\nA\n"),
            ["A", "A"]
        );
        let logs = serial_logs(&receiver);
        assert_eq!(logs.len(), 2);
        assert!(logs.iter().all(|log| log.repetition.unwrap().total == 1));
        assert_ne!(
            logs[0].repetition.unwrap().record_id,
            logs[1].repetition.unwrap().record_id
        );
    }

    #[test]
    fn legacy_limit_counts_new_records_after_other_resources_interrupt_repetition() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut observer = LogObserver::new(Some(sender));
        let session = SessionId::new();
        let first = SerialSessionId::new();
        let second = SerialSessionId::new();
        observer.open_serial(session, first, RequestId::new(), EventSource::Human);
        observer.open_serial(session, second, RequestId::new(), EventSource::Ai);
        for _ in 0..MAX_OUTPUT_LINES {
            observer.serial_output(session, first, b"A\n");
            observer.serial_output(session, second, b"B\n");
        }
        observer.serial_output(session, first, b"A\n");
        assert_eq!(
            observer.serial_streams[&(session, first)].shown,
            MAX_OUTPUT_LINES
        );
        let logs = receiver
            .try_iter()
            .filter_map(|event| {
                if let AgentEvent::OperationLog(log) = event {
                    Some(log)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(logs.len(), MAX_OUTPUT_LINES * 2 + 1);
        assert!(logs.last().unwrap().message.contains("后续输出已省略"));
        assert!(
            logs.iter()
                .filter_map(|log| log.repetition)
                .all(|value| value.total == 1)
        );
    }

    #[test]
    fn serial_repetition_joins_utf8_and_crlf_across_reads() {
        let (sender, receiver) = crate::bounded_agent_events(8);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
        let bytes = "中文\r\n中文\r\n".as_bytes();
        let mut raw = Vec::new();
        for byte in bytes {
            raw.extend(observer.serial_output(session, resource, &[*byte]));
        }
        assert_eq!(raw, ["中文", "中文"]);
        let logs = serial_logs(&receiver);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].repetition.unwrap().total, 2);
    }

    #[test]
    fn serial_repetition_does_not_merge_masked_truncated_or_terminal_lines() {
        let (sender, receiver) = crate::bounded_agent_events(64);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
        for line in [
            "password=secret\n",
            "local-user admin password cipher value\n",
            "A\u{1b}[0m\n",
            "A\r\r\n",
        ] {
            observer.serial_output(session, resource, line.as_bytes());
            observer.serial_output(session, resource, line.as_bytes());
        }
        let long = format!("{}\n", "x".repeat(2200));
        observer.serial_output(session, resource, long.as_bytes());
        observer.serial_output(session, resource, long.as_bytes());
        let logs = serial_logs(&receiver);
        assert_eq!(logs.len(), 10);
        assert!(logs.iter().all(|log| log.repetition.is_none()));
        assert!(logs.iter().all(|log| !log.message.contains("secret") && !log.message.contains("cipher value")));
    }

    #[test]
    fn serial_repetition_breaks_on_resource_session_source_and_epoch_changes() {
        let (sender, receiver) = crate::bounded_agent_events(64);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let other_session = SessionId::new();
        let resource = SerialSessionId::new();
        let other_resource = SerialSessionId::new();
        observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
        observer.open_serial(session, other_resource, RequestId::new(), EventSource::Ai);
        observer.open_serial(
            other_session,
            resource,
            RequestId::new(),
            EventSource::Human,
        );
        for (session, resource) in [
            (session, resource),
            (session, other_resource),
            (session, resource),
            (other_session, resource),
        ] {
            observer.serial_output(session, resource, b"A\n");
        }
        observer.emit(None, Utc::now(), AgentLogLevel::Info, "different source");
        observer.serial_output(other_session, resource, b"A\n");
        observer = observer.with_epoch(Some(2));
        observer.serial_output(other_session, resource, b"A\n");
        let logs = serial_logs(&receiver);
        let repeats = logs
            .iter()
            .filter_map(|log| log.repetition)
            .collect::<Vec<_>>();
        assert_eq!(repeats.len(), 6);
        assert!(repeats.iter().all(|value| value.total == 1));
        assert_eq!(
            repeats
                .iter()
                .map(|value| value.record_id)
                .collect::<BTreeSet<_>>()
                .len(),
            6
        );
    }

    #[test]
    fn serial_repetition_cumulative_update_survives_missing_initial_event() {
        let (sender, receiver) = crate::bounded_agent_events(2);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
        receiver.try_recv().unwrap();
        observer.serial_output(session, resource, b"A\n");
        let initial = serial_logs(&receiver);
        let record_id = initial[0].repetition.unwrap().record_id;
        for _ in 1..10_000 {
            observer.serial_output(session, resource, b"A\n");
        }
        let logs = serial_logs(&receiver);
        assert_eq!(logs.len(), 1);
        assert_eq!(
            logs[0].repetition,
            Some(AgentOutputRepetition {
                record_id,
                total: 10_000
            })
        );
    }

    #[test]
    fn serial_utf8_and_sensitive_lines_are_joined_before_redaction() {
        let (sender, receiver) = crate::bounded_agent_events(128);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
        let bytes = "中文\nlocal-user admin pass".as_bytes();
        observer.serial_output(session, resource, &bytes[..1]);
        observer.serial_output(session, resource, &bytes[1..]);
        observer.serial_output(session, resource, b"word cipher hidden-value\n");
        observer.close_serial(session, resource);
        let lines = messages(&receiver);
        assert!(lines.contains(&"中文".to_owned()));
        assert!(lines.iter().any(|line| line.contains("敏感行已遮盖")));
        assert!(!lines.iter().any(|line| line.contains("hidden-value")));
    }
    #[test]
    fn serial_open_completion_does_not_reset_pem_state() {
        let (sender, receiver) = crate::bounded_agent_events(128);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        let request = RequestId::new();
        observer.open_serial(session, resource, request, EventSource::Human);
        observer.serial_output(session, resource, b"-----BEGIN PRIV");
        observe(
            &mut observer,
            session,
            request,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: "opened".to_owned(),
            },
        );
        observer.serial_output(session, resource, b"ATE KEY-----\nprivate material\n");
        observer.serial_output(session, resource, b"-----END PRIVATE KEY-----\nvisible\n");
        observer.close_serial(session, resource);
        observer.serial_output(session, resource, b"late material\n");
        let lines = messages(&receiver);
        assert!(lines.iter().any(|line| line == "visible"));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("private material") || line.contains("late material"))
        );
    }
    #[test]
    fn serial_resources_keep_independent_partial_and_close_flushes() {
        let (sender, receiver) = crate::bounded_agent_events(128);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let first = SerialSessionId::new();
        let second = SerialSessionId::new();
        observer.open_serial(session, first, RequestId::new(), EventSource::Human);
        observer.open_serial(session, second, RequestId::new(), EventSource::Human);
        observer.serial_output(session, first, b"first");
        observer.serial_output(session, second, b"second\n");
        observer.close_serial(session, first);
        observer.close_serial(session, second);
        assert_eq!(messages(&receiver), ["second", "first"]);
        assert!(observer.serial_streams.is_empty());
    }
    #[test]
    fn hidden_echo_is_hidden_on_partial_close_and_multiple_inputs() {
        let mut stream = SerialLogStream {
            request_id: RequestId::new(),
            output: OutputBuffer {
                policy: OutputPolicy {
                    unlimited: true,
                    serial: true,
                },
                ..OutputBuffer::default()
            },
            utf8_pending: Vec::new(),
            hidden_echo: Vec::new(),
            suppress_output: false,
            prompt_tail: String::new(),
            authentication_prompt: true,
            last_hint: None,
            pending_hint: None,
            shown: 0,
            omission_reported: false,
        };
        stream.remember_hidden("first-input");
        stream.remember_hidden("second-input");
        assert_eq!(
            stream.push(b"first-input\nsecond-input\n", &Redactor::default()),
            ["[身份认证输入回显已隐藏]", "[身份认证输入回显已隐藏]"]
        );
        assert_eq!(
            stream.sanitize_echo("first-input".to_owned()),
            "[身份认证输入回显已隐藏]"
        );
        for _ in 0..16 {
            stream.remember_hidden("more");
        }
        assert!(stream.suppress_output);
        assert_eq!(
            stream.sanitize_echo("unknown echo".to_owned()),
            "[身份认证输入回显已隐藏]"
        );
    }
    #[test]
    fn incomplete_summary_key_is_never_published_raw() {
        assert_eq!(
            safe_text(
                &Redactor::default(),
                "-----BEGIN PRIVATE KEY-----\nmaterial without end"
            ),
            "[REDACTED PRIVATE KEY]"
        );
    }
    #[test]
    fn raw_serial_input_is_hidden_even_when_text_and_device_prompt_looks_ready() {
        let (sender, receiver) = crate::bounded_agent_events(128);
        let mut observer =
            LogObserver::with_sink(Some(AgentEventSink::Bounded(sender))).with_epoch(Some(11));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        let request_id = RequestId::new();
        observer.open_serial(session, resource, request_id, EventSource::Human);
        observer.serial_output(session, resource, b"device#");
        let request = RemoteRequest {
            request_id,
            session_id: session,
            source: EventSource::Human,
            operation: RemoteOperation::WriteSerial {
                serial_session_id: resource.to_string(),
                byte_count: 19,
                sha256: String::new(),
            },
            approval_id: None,
            payload_base64: None,
            control_proof: None,
        };
        observer.record_request(&request, None);
        observer.prepare_serial_write(&request, b"first-input\nsecond-input\n");
        observer.serial_write(&request, b"first-input\nsecond-input\n");
        observer.serial_output(session, resource, b"first-input\nsecond-input");
        observer.close_serial(session, resource);
        let lines = messages(&receiver);
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("first-input") || line.contains("second-input"))
        );
        assert!(lines.iter().any(|line| line.contains("输入已隐藏")));
    }
    #[test]
    fn serial_query_authentication_input_and_echo_are_not_displayed() {
        let (sender, receiver) = crate::bounded_agent_events(128);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
        observer.serial_output(session, resource, b"Password:");
        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id: session,
            source: EventSource::Human,
            operation: RemoteOperation::RunSerialQuery {
                serial_session_id: resource.to_string(),
                command: "private-input".to_owned(),
                line_ending: remoteops_domain::SerialLineEnding::Cr,
                profile: remoteops_domain::SerialTerminalProfile::HuaweiVrp,
                overall_timeout_millis: 1000,
                idle_timeout_millis: 100,
                max_bytes: 4096,
                max_pages: 1,
                readonly: true,
            },
            approval_id: None,
            payload_base64: None,
            control_proof: None,
        };
        observer.record_request(&request, None);
        observer.serial_output(session, resource, b"private-input\n");
        let summary = observer.serial_result(session, resource, "private-input\n");
        observer.close_serial(session, resource);
        assert!(!summary.contains("private-input"));
        assert!(
            !messages(&receiver)
                .iter()
                .any(|line| line.contains("private-input"))
        );
    }
    #[test]
    fn headless_observer_still_releases_query_and_device_state() {
        let mut observer = LogObserver::with_sink(None);
        let session = SessionId::new();
        let request = RequestId::new();
        observer.device_requests.insert((session, request));
        observer.serial_queries.insert((session, request));
        observe(
            &mut observer,
            session,
            request,
            EventPayload::OperationCompleted {
                exit_code: Some(0),
                summary: String::new(),
            },
        );
        assert!(observer.device_requests.is_empty());
        assert!(observer.serial_queries.is_empty());
    }
    #[test]
    fn cancellation_discards_late_secret_suffix_after_partial_flush() {
        let (sender, receiver) = crate::bounded_agent_events(128);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let request = RequestId::new();
        observe(&mut observer, session, request, output("password=partial"));
        observe(
            &mut observer,
            session,
            request,
            EventPayload::OperationCancelled,
        );
        observe(&mut observer, session, request, output("late-suffix\n"));
        assert!(
            !messages(&receiver)
                .iter()
                .any(|line| line.contains("partial") || line.contains("late-suffix"))
        );
        assert!(observer.streams.is_empty());
    }
    #[test]
    fn fragment_authentication_and_command_prompts_emit_only_fixed_deduplicated_hints() {
        let (sender, receiver) = crate::bounded_agent_events(128);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
        observer
            .serial_streams
            .get_mut(&(session, resource))
            .unwrap()
            .remember_hidden("nearby-private-value");
        observer.serial_output(session, resource, b"nearby-private-value Pass");
        assert_eq!(messages(&receiver), Vec::<String>::new());
        observer.serial_output(session, resource, b"word:");
        assert_eq!(messages(&receiver), ["设备等待身份认证输入，输入隐藏"]);
        observer.serial_output(session, resource, b" ");
        assert_eq!(messages(&receiver), Vec::<String>::new());
        observer.serial_output(session, resource, b"\r\nrouter");
        let lines = messages(&receiver);
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("nearby-private-value"))
        );
        observer.serial_output(session, resource, b"#");
        assert_eq!(messages(&receiver), ["设备已返回命令提示符"]);
        observer.serial_output(session, resource, b" ");
        assert_eq!(messages(&receiver), Vec::<String>::new());
        observer.serial_output(session, resource, b"\r\nUsername:");
        let lines = messages(&receiver);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.as_str() == "设备等待身份认证输入，输入隐藏")
                .count(),
            1
        );
    }
    #[test]
    fn prompt_hints_do_not_unmask_private_key_content() {
        let (sender, receiver) = crate::bounded_agent_events(128);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
        observer.serial_output(session, resource, b"-----BEGIN PRIVATE KEY-----\n");
        messages(&receiver);
        observer.serial_output(session, resource, b"private-material Password:");
        assert_eq!(messages(&receiver), Vec::<String>::new());
        observer.serial_output(session, resource, b"\n-----END PRIVATE KEY-----\nrouter#");
        let lines = messages(&receiver);
        assert_eq!(lines, ["设备已返回命令提示符"]);
        observer.close_serial(session, resource);
        assert!(
            !messages(&receiver)
                .iter()
                .any(|line| line.contains("private-material"))
        );
    }
    #[test]
    fn ssh_preparation_hint_does_not_claim_success_or_include_credentials() {
        let (sender, receiver) = crate::bounded_agent_events(128);
        let observer =
            LogObserver::with_sink(Some(AgentEventSink::Bounded(sender))).with_epoch(Some(9));
        observer.ssh_connecting(RequestId::new());
        assert_eq!(messages(&receiver), ["正在建立SSH连接，身份认证信息已隐藏"]);
    }
    #[test]
    fn ordinary_shell_serial_looking_prefix_is_not_suppressed() {
        let (sender, receiver) = crate::bounded_agent_events(128);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        observe(
            &mut observer,
            SessionId::new(),
            RequestId::new(),
            output("[serial:example] ordinary Shell output\n"),
        );
        assert_eq!(
            messages(&receiver),
            ["[serial:example] ordinary Shell output"]
        );
    }
    #[test]
    fn real_serial_wire_output_is_deduplicated_by_session_and_open_request() {
        let (sender, receiver) = crate::bounded_agent_events(128);
        let mut observer = LogObserver::with_sink(Some(AgentEventSink::Bounded(sender)));
        let session = SessionId::new();
        let resource = SerialSessionId::new();
        let request = RequestId::new();
        observer.open_serial(session, resource, request, EventSource::Human);
        observer.serial_output(session, resource, b"real serial output\n");
        observe(
            &mut observer,
            session,
            request,
            output("wire copy without any special prefix\n"),
        );
        assert_eq!(messages(&receiver), ["real serial output"]);
        observer.close_serial(session, resource);
        observe(&mut observer, session, request, output("late wire copy\n"));
        assert_eq!(messages(&receiver), Vec::<String>::new());
        observe(
            &mut observer,
            SessionId::new(),
            request,
            output("[serial:example] another session\n"),
        );
        observe(
            &mut observer,
            session,
            RequestId::new(),
            output("[serial:example] another request\n"),
        );
        assert_eq!(
            messages(&receiver),
            [
                "[serial:example] another session",
                "[serial:example] another request"
            ]
        );
    }
    #[test]
    fn closed_serial_request_tombstones_are_bounded() {
        let mut observer = LogObserver::with_sink(None);
        let session = SessionId::new();
        for _ in 0..MAX_CLOSED_SERIAL_REQUESTS + 20 {
            let resource = SerialSessionId::new();
            observer.open_serial(session, resource, RequestId::new(), EventSource::Human);
            observer.close_serial(session, resource);
        }
        assert_eq!(
            observer.closed_serial_requests.len(),
            MAX_CLOSED_SERIAL_REQUESTS
        );
        assert!(observer.serial_streams.is_empty());
    }
}
