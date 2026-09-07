//! 将 Agent 已执行的操作和输出转为有界、脱敏的本地日志，不改变远程协议。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

use chrono::{DateTime, Utc};
use remoteops_audit::Redactor;
use remoteops_domain::{EventPayload, RemoteEvent, RemoteOperation, RequestId, SessionId};
use remoteops_protocol::{RemoteRequest, WireMessage};

use crate::{AgentEvent, AgentEventSender, AgentLogLevel, AgentOperationLog, emit_agent_event};

/// 单行输出超过此长度时整行隐藏，避免截断后泄漏部分凭据。
const MAX_LINE_BYTES: usize = 8192;
/// 每个请求、每种输出流最多展示的行数。
const MAX_OUTPUT_LINES: usize = 128;
/// 同时缓冲的输出流数量上限。
const MAX_STREAMS: usize = 64;
/// 连接结束前保留的中断请求墓碑数量，防止迟到的输出重新创建缓冲流。
const MAX_INTERRUPTED_REQUESTS: usize = 128;
/// 超长行只需保留边界附近的少量文本来识别跨分片 PEM 标记。
const MARKER_CONTEXT_BYTES: usize = 256;

/// 合并跨协议分片的输出行，并记住私钥块的隐藏状态。
#[derive(Default)]
struct OutputBuffer {
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
}

impl OutputBuffer {
    /// 缓冲完整行，凭据被拆到多个协议分片时仍可整体脱敏。
    fn push(&mut self, text: &str, redactor: &Redactor) -> Vec<String> {
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
            if piece.ends_with('\n')
                && let Some(line) = self.finish_line(redactor)
            {
                lines.push(line);
            }
        }
        lines
    }

    /// 返回一条已脱敏的完整行；被隐藏的内容不会进入 GUI 事件通道。
    fn finish_line(&mut self, redactor: &Redactor) -> Option<String> {
        let line = std::mem::take(&mut self.pending);
        let oversized = std::mem::take(&mut self.oversized);
        if oversized {
            self.oversized_tail.clear();
        }
        if self.emitted >= MAX_OUTPUT_LINES {
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
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            return None;
        }
        self.emitted += 1;
        Some(redactor.redact(line))
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
pub(super) struct LogObserver {
    /// 可选的本地表现层订阅端。
    sender: Option<AgentEventSender>,
    /// 重用现有审计层的脱敏规则。
    redactor: Redactor,
    /// 按会话、请求和输出流保存不完整文本。
    streams: BTreeMap<(SessionId, RequestId, bool), OutputBuffer>,
    /// 已写入中断终态的请求，丢弃其之后迟到的协议事件。
    interrupted_requests: BTreeSet<(SessionId, RequestId)>,
    /// 达到流数量上限而被丢弃的输出流，直到请求终态都不重新捕获。
    discarded_streams: BTreeSet<(SessionId, RequestId, bool)>,
}

impl LogObserver {
    /// 创建本地日志观察器。
    pub(super) fn new(sender: Option<AgentEventSender>) -> Self {
        Self {
            sender,
            redactor: Redactor::default(),
            streams: BTreeMap::new(),
            interrupted_requests: BTreeSet::new(),
            discarded_streams: BTreeSet::new(),
        }
    }

    /// 只读取操作事件和失败响应，绝不读取认证或文件二进制载荷。
    pub(super) fn observe(&mut self, message: &WireMessage) {
        if self.sender.is_none() {
            return;
        }
        match message {
            WireMessage::RemoteEvent(event) => self.observe_event(event),
            WireMessage::RemoteResponse(response)
                if response.error_code.is_some()
                    && response.error_code.as_deref() != Some("agent_operation_failed") =>
            {
                self.flush(response.session_id, response.request_id, Utc::now());
                self.emit(
                    Some(response.request_id),
                    Utc::now(),
                    AgentLogLevel::Error,
                    &response.summary,
                );
            }
            _ => {}
        }
    }

    /// 将执行阶段和输出映射到本地日志。
    fn observe_event(&mut self, event: &RemoteEvent) {
        let Some(request_id) = event.request_id else {
            return;
        };
        match &event.payload {
            _payload
                if self
                    .interrupted_requests
                    .contains(&(event.session_id, request_id)) => {}
            EventPayload::OperationStarted => self.emit(
                Some(request_id),
                event.occurred_at,
                AgentLogLevel::Running,
                "开始执行",
            ),
            EventPayload::OutputChunk { stderr, text } => {
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
                    .or_default()
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
                let streamed = self.flush(event.session_id, request_id, event.occurred_at);
                if !streamed && !summary.is_empty() {
                    self.emit(
                        Some(request_id),
                        event.occurred_at,
                        AgentLogLevel::Info,
                        summary,
                    );
                }
                let failed = exit_code.is_some_and(|code| code != 0);
                let message = exit_code.map_or_else(
                    || "执行完成".to_owned(),
                    |code| format!("执行完成 · 退出码 {code}"),
                );
                self.emit(
                    Some(request_id),
                    event.occurred_at,
                    if failed {
                        AgentLogLevel::Error
                    } else {
                        AgentLogLevel::Success
                    },
                    &message,
                );
            }
            EventPayload::OperationFailed { message, .. } => {
                self.flush(event.session_id, request_id, event.occurred_at);
                self.emit(
                    Some(request_id),
                    event.occurred_at,
                    AgentLogLevel::Error,
                    &format!("执行失败：{message}"),
                );
            }
            EventPayload::OperationCancelled => {
                self.flush(event.session_id, request_id, event.occurred_at);
                self.emit(
                    Some(request_id),
                    event.occurred_at,
                    AgentLogLevel::Error,
                    "操作已中断",
                );
            }
            _ => {}
        }
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
        if self.interrupted_requests.len() >= MAX_INTERRUPTED_REQUESTS
            && let Some(oldest) = self.interrupted_requests.iter().next().copied()
        {
            self.interrupted_requests.remove(&oldest);
        }
        self.interrupted_requests.insert((session_id, request_id));
        self.emit(Some(request_id), occurred_at, AgentLogLevel::Error, message);
    }

    /// 请求结束时提交最后一行并释放两个输出流的缓冲。
    fn flush(&mut self, session_id: SessionId, request_id: RequestId, time: DateTime<Utc>) -> bool {
        let mut streamed = false;
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
        emit_agent_event(
            self.sender.as_ref(),
            AgentEvent::OperationLog(AgentOperationLog {
                request_id,
                occurred_at,
                level,
                message: safe_text(&self.redactor, message),
            }),
        );
    }
}

/// 格式化已授权的操作名称，只读取操作本身，不读取请求附带的凭据载荷。
pub(super) fn record_request(sender: Option<&AgentEventSender>, request: &RemoteRequest) {
    static REDACTOR: OnceLock<Redactor> = OnceLock::new();
    if sender.is_none() {
        return;
    }
    let redactor = REDACTOR.get_or_init(Redactor::default);
    let operation = &request.operation;
    let message = match operation {
        RemoteOperation::RunCommand { command, .. }
        | RemoteOperation::RunShellCommand { command, .. }
        | RemoteOperation::RunSerialQuery { command, .. }
        | RemoteOperation::RunSsh { command, .. } => {
            format!("收到{}：{command}", operation_label(operation))
        }
        _ => format!("收到操作：{}", operation_label(operation)),
    };
    emit_agent_event(
        sender,
        AgentEvent::OperationLog(AgentOperationLog {
            request_id: Some(request.request_id),
            occurred_at: Utc::now(),
            level: AgentLogLevel::Info,
            message: safe_text(redactor, &message),
        }),
    );
}

/// 对可能很长的诊断文本先脱敏，再保留可展示的开头部分。
fn safe_text(redactor: &Redactor, message: &str) -> String {
    let text = redactor.redact(message);
    if let Some((index, _)) = text.char_indices().nth(2048) {
        format!("{}\n[内容已截断]", &text[..index])
    } else {
        text
    }
}

/// 将核心操作映射为现场人员可理解的类型名称。
fn operation_label(operation: &RemoteOperation) -> &'static str {
    match operation {
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

    fn output(text: &str) -> EventPayload {
        EventPayload::OutputChunk {
            stderr: false,
            text: text.to_owned(),
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
            .map(|event| match event {
                AgentEvent::OperationLog(log) => log,
                _ => panic!("应只包含日志"),
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
        assert!(
            buffer
                .push("more material\n-----END PRIVATE KEY-----\n", &redactor)
                .is_empty()
        );
        assert!(
            buffer
                .push(&"界".repeat(MAX_LINE_BYTES), &redactor)
                .is_empty()
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
        assert!(buffer.push(&oversized_marker, &redactor).is_empty());
        assert!(buffer.push("secret material\n", &redactor).is_empty());
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
        assert!(buffer.push("-----BEGIN PRIVATE ", &redactor).is_empty());
        let oversized_tail = format!("KEY-----{}\n", "x".repeat(MAX_LINE_BYTES));
        assert!(buffer.push(&oversized_tail, &redactor).is_empty());
        assert!(buffer.push("secret material\n", &redactor).is_empty());
        assert_eq!(
            buffer.push("-----END PRIVATE KEY-----\nnext\n", &redactor),
            ["next"]
        );
    }

    #[test]
    fn oversized_tail_tracks_begin_marker_across_later_small_chunks() {
        let redactor = Redactor::default();
        let mut buffer = OutputBuffer::default();
        assert!(
            buffer
                .push(&"x".repeat(MAX_LINE_BYTES + 1), &redactor)
                .is_empty()
        );
        assert!(buffer.push("-----BEGIN ", &redactor).is_empty());
        assert!(
            buffer
                .push(
                    "PRIVATE KEY-----\nsecret material\n-----END PRIVATE KEY-----\n",
                    &redactor,
                )
                .is_empty()
        );
        assert_eq!(buffer.push("next\n", &redactor), ["next"]);
    }

    #[test]
    fn pending_tail_is_replaced_by_oversized_tail_for_split_marker() {
        let redactor = Redactor::default();
        let mut buffer = OutputBuffer::default();
        assert!(
            buffer
                .push(&"x".repeat(MAX_LINE_BYTES), &redactor)
                .is_empty()
        );
        assert!(buffer.push("-----BEGIN ", &redactor).is_empty());
        assert!(
            buffer
                .push(
                    "PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----\n",
                    &redactor,
                )
                .is_empty()
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
        let logs: Vec<_> = receiver.try_iter().collect();
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
}
