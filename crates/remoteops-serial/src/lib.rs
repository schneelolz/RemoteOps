//! 与表现层和具体串口驱动无关的串口交互核心。
#![allow(clippy::missing_errors_doc)]

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
pub use remoteops_domain::{SerialLineEnding, SerialTerminalProfile};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 默认保留的串口记录条数。
pub const DEFAULT_TRANSCRIPT_ENTRIES: usize = 2_000;
/// 默认保留的串口记录字节数。
pub const DEFAULT_TRANSCRIPT_BYTES: usize = 128 * 1024;

/// 串口数据方向。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialDirection {
    /// 设备发往控制端。
    Receive,
    /// 控制端发往设备。
    Transmit,
}

/// 一条带顺序号的串口记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerialTranscriptEntry {
    /// 会话内单调递增顺序号。
    pub sequence: u64,
    /// 数据方向。
    pub direction: SerialDirection,
    /// 发生时间。
    pub occurred_at: DateTime<Utc>,
    /// 原始串口字节。
    pub bytes: Vec<u8>,
}

/// 有界的内存串口记录。
pub struct SerialTranscript {
    entries: VecDeque<SerialTranscriptEntry>,
    byte_count: usize,
    next_sequence: u64,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for SerialTranscript {
    fn default() -> Self {
        Self::with_limits(DEFAULT_TRANSCRIPT_ENTRIES, DEFAULT_TRANSCRIPT_BYTES)
    }
}

impl SerialTranscript {
    /// 使用指定的记录数和字节数上限创建缓冲。
    #[must_use]
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            byte_count: 0,
            next_sequence: 1,
            max_entries: max_entries.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    /// 追加一条记录并返回其顺序号。
    pub fn push(&mut self, direction: SerialDirection, bytes: Vec<u8>) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.byte_count = self.byte_count.saturating_add(bytes.len());
        self.entries.push_back(SerialTranscriptEntry {
            sequence,
            direction,
            occurred_at: Utc::now(),
            bytes,
        });
        while self.entries.len() > self.max_entries || self.byte_count > self.max_bytes {
            if let Some(entry) = self.entries.pop_front() {
                self.byte_count = self.byte_count.saturating_sub(entry.bytes.len());
            }
        }
        sequence
    }

    /// 返回当前最新顺序号；空缓冲返回零。
    #[must_use]
    pub fn latest_sequence(&self) -> u64 {
        self.next_sequence.saturating_sub(1)
    }

    /// 返回指定顺序号之后的记录。
    #[must_use]
    pub fn entries_after(&self, sequence: u64) -> Vec<SerialTranscriptEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.sequence > sequence)
            .cloned()
            .collect()
    }

    /// 从最新记录向前选择不超过指定字节数的记录。
    #[must_use]
    pub fn recent_entries(&self, max_bytes: usize) -> Vec<SerialTranscriptEntry> {
        let max_bytes = max_bytes.max(1);
        let mut selected = Vec::new();
        let mut count = 0_usize;
        for entry in self.entries.iter().rev() {
            if count.saturating_add(entry.bytes.len()) > max_bytes && !selected.is_empty() {
                break;
            }
            count = count.saturating_add(entry.bytes.len());
            selected.push(entry.clone());
            if count >= max_bytes {
                break;
            }
        }
        selected.reverse();
        selected
    }

    /// 清空记录但保留顺序号单调性。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.byte_count = 0;
    }
}

/// 与具体控制台库无关的终端按键。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SerialTerminalKey {
    /// 回车。
    Enter,
    /// Tab。
    Tab,
    /// Shift+Tab。
    BackTab,
    /// 退格。
    Backspace,
    /// 向前删除。
    Delete,
    /// Escape。
    Escape,
    /// 上方向键。
    Up,
    /// 下方向键。
    Down,
    /// 左方向键。
    Left,
    /// 右方向键。
    Right,
    /// Home。
    Home,
    /// End。
    End,
    /// Insert。
    Insert,
    /// `PageUp`。
    PageUp,
    /// `PageDown`。
    PageDown,
}

/// 将终端按键编码为设备需要的字节。
#[must_use]
pub fn encode_terminal_key(
    profile: SerialTerminalProfile,
    key: SerialTerminalKey,
    line_ending: SerialLineEnding,
) -> Vec<u8> {
    match (profile, key) {
        (_, SerialTerminalKey::Enter) => line_ending.bytes().to_vec(),
        (_, SerialTerminalKey::Tab) => vec![b'\t'],
        (_, SerialTerminalKey::BackTab) => b"\x1B[Z".to_vec(),
        (
            SerialTerminalProfile::HuaweiVrp,
            SerialTerminalKey::Backspace | SerialTerminalKey::Delete,
        ) => {
            vec![0x08]
        }
        (SerialTerminalProfile::HuaweiVrp, SerialTerminalKey::Up) => vec![0x10],
        (SerialTerminalProfile::HuaweiVrp, SerialTerminalKey::Down) => vec![0x0E],
        (SerialTerminalProfile::HuaweiVrp, SerialTerminalKey::Left) => vec![0x02],
        (SerialTerminalProfile::HuaweiVrp, SerialTerminalKey::Right) => vec![0x06],
        (SerialTerminalProfile::HuaweiVrp, SerialTerminalKey::Home) => vec![0x01],
        (SerialTerminalProfile::HuaweiVrp, SerialTerminalKey::End) => vec![0x05],
        (_, SerialTerminalKey::Backspace) => vec![0x7F],
        (_, SerialTerminalKey::Delete) => b"\x1B[3~".to_vec(),
        (_, SerialTerminalKey::Escape) => vec![0x1B],
        (_, SerialTerminalKey::Up) => b"\x1B[A".to_vec(),
        (_, SerialTerminalKey::Down) => b"\x1B[B".to_vec(),
        (_, SerialTerminalKey::Right) => b"\x1B[C".to_vec(),
        (_, SerialTerminalKey::Left) => b"\x1B[D".to_vec(),
        (_, SerialTerminalKey::Home) => b"\x1B[H".to_vec(),
        (_, SerialTerminalKey::End) => b"\x1B[F".to_vec(),
        (_, SerialTerminalKey::Insert) => b"\x1B[2~".to_vec(),
        (_, SerialTerminalKey::PageUp) => b"\x1B[5~".to_vec(),
        (_, SerialTerminalKey::PageDown) => b"\x1B[6~".to_vec(),
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EscapeState {
    #[default]
    Text,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

/// 允许表现层安全执行的终端渲染动作。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalRenderAction {
    /// 输出普通文本。
    Text(String),
    /// 光标左移。
    MoveLeft(u16),
    /// 光标右移。
    MoveRight(u16),
    /// 移动到指定零基列。
    MoveToColumn(u16),
    /// 清除光标到行尾。
    ClearUntilNewLine,
    /// 清除当前行。
    ClearCurrentLine,
}

/// 过滤不可信设备控制序列并生成有限终端动作。
#[derive(Default)]
pub struct TerminalDecoder {
    state: EscapeState,
    csi_parameters: String,
}

impl TerminalDecoder {
    /// 解析一个可能被分片的设备输出块。
    pub fn push(&mut self, bytes: &[u8]) -> Vec<TerminalRenderAction> {
        let mut output = Vec::new();
        for value in String::from_utf8_lossy(bytes).chars() {
            match self.state {
                EscapeState::Text => match value {
                    '\u{001B}' => self.state = EscapeState::Escape,
                    '\u{0007}' => {}
                    '\u{0008}' => output.push(TerminalRenderAction::MoveLeft(1)),
                    '\r' | '\n' | '\t' => append_terminal_text(&mut output, value),
                    value if value.is_control() => append_terminal_string(
                        &mut output,
                        format!("\\u{{{:04X}}}", u32::from(value)),
                    ),
                    value => append_terminal_text(&mut output, value),
                },
                EscapeState::Escape => match value {
                    '[' => {
                        self.csi_parameters.clear();
                        self.state = EscapeState::Csi;
                    }
                    ']' => self.state = EscapeState::Osc,
                    '\u{001B}' => {}
                    _ => self.state = EscapeState::Text,
                },
                EscapeState::Csi => {
                    if ('@'..='~').contains(&value) {
                        self.render_safe_csi(value, &mut output);
                        self.csi_parameters.clear();
                        self.state = EscapeState::Text;
                    } else if (value.is_ascii_digit() || matches!(value, ';' | '?'))
                        && self.csi_parameters.len() < 32
                    {
                        self.csi_parameters.push(value);
                    } else {
                        self.csi_parameters.clear();
                        self.state = EscapeState::Text;
                    }
                }
                EscapeState::Osc => match value {
                    '\u{0007}' => self.state = EscapeState::Text,
                    '\u{001B}' => self.state = EscapeState::OscEscape,
                    _ => {}
                },
                EscapeState::OscEscape => match value {
                    '\\' => self.state = EscapeState::Text,
                    '\u{001B}' => {}
                    _ => self.state = EscapeState::Osc,
                },
            }
        }
        output
    }

    fn render_safe_csi(&self, final_byte: char, output: &mut Vec<TerminalRenderAction>) {
        let parameter = first_csi_parameter(&self.csi_parameters, u16::from(final_byte != 'K'));
        match final_byte {
            'D' => output.push(TerminalRenderAction::MoveLeft(parameter.max(1))),
            'C' => output.push(TerminalRenderAction::MoveRight(parameter.max(1))),
            'G' => output.push(TerminalRenderAction::MoveToColumn(
                parameter.max(1).saturating_sub(1),
            )),
            'K' if parameter == 0 => output.push(TerminalRenderAction::ClearUntilNewLine),
            'K' if parameter == 2 => output.push(TerminalRenderAction::ClearCurrentLine),
            _ => {}
        }
    }
}

fn append_terminal_text(output: &mut Vec<TerminalRenderAction>, value: char) {
    let mut encoded = [0_u8; 4];
    append_terminal_string(output, value.encode_utf8(&mut encoded));
}

fn append_terminal_string(output: &mut Vec<TerminalRenderAction>, value: impl AsRef<str>) {
    let value = value.as_ref();
    if let Some(TerminalRenderAction::Text(text)) = output.last_mut() {
        text.push_str(value);
    } else {
        output.push(TerminalRenderAction::Text(value.to_owned()));
    }
}

fn first_csi_parameter(value: &str, default: u16) -> u16 {
    value
        .trim_start_matches('?')
        .split(';')
        .next()
        .and_then(|item| item.parse::<u16>().ok())
        .unwrap_or(default)
}

/// 结构化串口查询的风险等级。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialQueryRisk {
    /// 已知只读查询。
    ReadOnly,
    /// 已知会修改会话或设备状态。
    Mutating,
    /// 无法可靠判断。
    Unknown,
}

/// 根据设备配置和完整命令判断查询风险。
#[must_use]
pub fn classify_serial_query(profile: SerialTerminalProfile, command: &str) -> SerialQueryRisk {
    let command = command.trim();
    if command.is_empty()
        || command
            .chars()
            .any(|value| matches!(value, '\r' | '\n' | '\0'))
    {
        return SerialQueryRisk::Unknown;
    }
    let lower = command.to_ascii_lowercase();
    match profile {
        SerialTerminalProfile::HuaweiVrp if is_huawei_readonly_display(&lower) => {
            SerialQueryRisk::ReadOnly
        }
        SerialTerminalProfile::HuaweiVrp
            if [
                "system-view",
                "sys",
                "save",
                "reboot",
                "reset",
                "undo",
                "quit",
                "return",
                "screen-length",
            ]
            .iter()
            .any(|prefix| lower == *prefix || lower.starts_with(&format!("{prefix} "))) =>
        {
            SerialQueryRisk::Mutating
        }
        _ => SerialQueryRisk::Unknown,
    }
}

fn is_huawei_readonly_display(command: &str) -> bool {
    if !command.starts_with("display ")
        || command
            .chars()
            .any(|value| matches!(value, ';' | '&' | '>' | '<' | '`'))
    {
        return false;
    }
    let mut segments = command.split('|');
    let Some(display) = segments.next() else {
        return false;
    };
    if display.trim().len() <= "display".len() {
        return false;
    }
    segments.all(|segment| {
        let filter = segment.trim_start();
        ["begin ", "exclude ", "include ", "count", "no-more"]
            .iter()
            .any(|prefix| filter == prefix.trim_end() || filter.starts_with(prefix))
    })
}

/// 有界只读授权检查结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SerialGrantDecision {
    /// 已授权并消耗一次额度。
    Authorized { remaining_commands: u32 },
    /// 授权不属于当前串口会话。
    SessionMismatch,
    /// 授权已经过期。
    Expired,
    /// 授权次数已经耗尽。
    Exhausted,
    /// 命令不属于结构化只读查询。
    NotReadOnly,
}

/// 绑定串口会话、设备配置、期限和次数的只读授权。
#[derive(Clone, Debug)]
pub struct SerialReadOnlyGrant {
    session_key: String,
    profile: SerialTerminalProfile,
    expires_at: DateTime<Utc>,
    remaining_commands: u32,
}

impl SerialReadOnlyGrant {
    /// 创建一个只读串口查询授权。
    #[must_use]
    pub fn new(
        session_key: impl Into<String>,
        profile: SerialTerminalProfile,
        expires_at: DateTime<Utc>,
        max_commands: u32,
    ) -> Self {
        Self {
            session_key: session_key.into(),
            profile,
            expires_at,
            remaining_commands: max_commands.max(1),
        }
    }

    /// 检查并消耗一次查询额度。
    pub fn authorize(
        &mut self,
        session_key: &str,
        command: &str,
        now: DateTime<Utc>,
    ) -> SerialGrantDecision {
        if self.session_key != session_key {
            return SerialGrantDecision::SessionMismatch;
        }
        if now >= self.expires_at {
            return SerialGrantDecision::Expired;
        }
        if self.remaining_commands == 0 {
            return SerialGrantDecision::Exhausted;
        }
        if classify_serial_query(self.profile, command) != SerialQueryRisk::ReadOnly {
            return SerialGrantDecision::NotReadOnly;
        }
        self.remaining_commands -= 1;
        SerialGrantDecision::Authorized {
            remaining_commands: self.remaining_commands,
        }
    }

    /// 返回授权到期时间。
    #[must_use]
    pub const fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    /// 返回剩余命令次数。
    #[must_use]
    pub const fn remaining_commands(&self) -> u32 {
        self.remaining_commands
    }
}

/// 串口查询运行参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SerialQueryPlan {
    /// 不含行结束符的完整命令。
    pub command: String,
    /// 命令行结束符。
    pub line_ending: SerialLineEnding,
    /// 设备终端配置。
    pub profile: SerialTerminalProfile,
    /// 总超时毫秒数。
    pub overall_timeout_millis: u64,
    /// 收到部分结果后的空闲完成时间。
    pub idle_timeout_millis: u64,
    /// 最大接收字节数。
    pub max_bytes: usize,
    /// 最多自动翻页次数。
    pub max_pages: u16,
}

impl SerialQueryPlan {
    /// 返回约束到安全范围后的参数。
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.command = self.command.trim().to_owned();
        self.overall_timeout_millis = self.overall_timeout_millis.clamp(500, 120_000);
        self.idle_timeout_millis = self
            .idle_timeout_millis
            .clamp(100, self.overall_timeout_millis);
        self.max_bytes = self.max_bytes.clamp(1, 1024 * 1024);
        self.max_pages = self.max_pages.min(200);
        self
    }
}

/// 查询完成原因。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialQueryCompletion {
    /// 已观察到设备提示符。
    Prompt,
    /// 设备输出停止并达到空闲时间。
    Idle,
    /// 达到总超时。
    Timeout,
    /// 达到最大输出字节数。
    MaxBytes,
    /// 达到最大分页次数。
    MaxPages,
}

/// 一次结构化串口查询结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SerialQueryResult {
    /// 本机内存中的原始文本。
    pub text: String,
    /// 可发送给 AI 的脱敏文本。
    pub redacted_text: String,
    /// 完成原因。
    pub completion: SerialQueryCompletion,
    /// 接收字节数。
    pub received_bytes: usize,
    /// 自动翻页次数。
    pub pages: u16,
    /// 实际耗时毫秒数。
    pub elapsed_millis: u64,
}

/// 查询执行器观察到的一条串口记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerialObservedChunk {
    /// 会话内顺序号。
    pub sequence: u64,
    /// 数据方向。
    pub direction: SerialDirection,
    /// 原始字节。
    pub bytes: Vec<u8>,
}

/// 结构化查询所需的最小串口传输能力。
#[async_trait]
pub trait SerialQueryTransport: Send + Sync {
    /// 完整写入全部字节。
    async fn write_all(&self, bytes: &[u8]) -> Result<(), SerialQueryError>;
    /// 返回当前最新记录顺序号。
    async fn latest_sequence(&self) -> Result<u64, SerialQueryError>;
    /// 等待指定顺序号之后的新记录。
    async fn wait_for_chunks(
        &self,
        after_sequence: u64,
        timeout: Duration,
    ) -> Result<Vec<SerialObservedChunk>, SerialQueryError>;
}

/// 串口查询失败。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SerialQueryError {
    /// 查询命令无效。
    #[error("串口查询命令不能为空")]
    EmptyCommand,
    /// 传输层失败。
    #[error("串口传输失败：{0}")]
    Transport(String),
}

/// 确定性执行写入、等待、分页和提示符识别的查询编排器。
#[derive(Default)]
pub struct SerialQueryRunner;

impl SerialQueryRunner {
    /// 执行一次完整串口查询。
    pub async fn run<T: SerialQueryTransport>(
        &self,
        transport: &T,
        plan: SerialQueryPlan,
    ) -> Result<SerialQueryResult, SerialQueryError> {
        let plan = plan.normalized();
        if plan.command.is_empty() {
            return Err(SerialQueryError::EmptyCommand);
        }
        let started = Instant::now();
        let mut cursor = transport.latest_sequence().await?;
        let mut command = plan.command.as_bytes().to_vec();
        command.extend_from_slice(plan.line_ending.bytes());
        transport.write_all(&command).await?;

        let overall_timeout = Duration::from_millis(plan.overall_timeout_millis);
        let idle_timeout = Duration::from_millis(plan.idle_timeout_millis);
        let mut received = Vec::new();
        let mut pages = 0_u16;
        let mut handled_pagers = 0_usize;

        loop {
            let elapsed = started.elapsed();
            if elapsed >= overall_timeout {
                return Ok(query_result(
                    &received,
                    SerialQueryCompletion::Timeout,
                    pages,
                    elapsed,
                ));
            }
            let wait = idle_timeout.min(overall_timeout.saturating_sub(elapsed));
            let chunks = transport.wait_for_chunks(cursor, wait).await?;
            if chunks.is_empty() {
                return Ok(query_result(
                    &received,
                    SerialQueryCompletion::Idle,
                    pages,
                    started.elapsed(),
                ));
            }
            for chunk in chunks {
                cursor = cursor.max(chunk.sequence);
                if chunk.direction == SerialDirection::Receive {
                    let remaining = plan.max_bytes.saturating_sub(received.len());
                    received.extend(chunk.bytes.into_iter().take(remaining));
                    if received.len() >= plan.max_bytes {
                        return Ok(query_result(
                            &received,
                            SerialQueryCompletion::MaxBytes,
                            pages,
                            started.elapsed(),
                        ));
                    }
                }
            }

            let visible = visible_terminal_text(&received);
            let pager_count = visible.matches("---- More ----").count();
            while handled_pagers < pager_count {
                if pages >= plan.max_pages {
                    return Ok(query_result(
                        &received,
                        SerialQueryCompletion::MaxPages,
                        pages,
                        started.elapsed(),
                    ));
                }
                transport.write_all(b" ").await?;
                pages = pages.saturating_add(1);
                handled_pagers += 1;
            }
            if detects_prompt(plan.profile, &visible) {
                return Ok(query_result(
                    &received,
                    SerialQueryCompletion::Prompt,
                    pages,
                    started.elapsed(),
                ));
            }
        }
    }
}

fn query_result(
    bytes: &[u8],
    completion: SerialQueryCompletion,
    pages: u16,
    elapsed: Duration,
) -> SerialQueryResult {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let redacted_text = redact_serial_text(&text);
    SerialQueryResult {
        received_bytes: bytes.len(),
        text,
        redacted_text,
        completion,
        pages,
        elapsed_millis: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
    }
}

fn visible_terminal_text(bytes: &[u8]) -> String {
    let mut decoder = TerminalDecoder::default();
    decoder
        .push(bytes)
        .into_iter()
        .filter_map(|action| match action {
            TerminalRenderAction::Text(value) => Some(value),
            _ => None,
        })
        .collect()
}

fn detects_prompt(profile: SerialTerminalProfile, visible: &str) -> bool {
    let last = visible
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    match profile {
        SerialTerminalProfile::HuaweiVrp => {
            (last.starts_with('<') && last.ends_with('>'))
                || (last.starts_with('[') && last.ends_with(']'))
        }
        SerialTerminalProfile::Ansi => last.ends_with(['>', '#', '$']) || last.ends_with("% "),
    }
}

/// 遮盖常见网络设备凭据行，保留原始数据只在调用方本机内存中。
#[must_use]
pub fn redact_serial_text(text: &str) -> String {
    let mut output = String::new();
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        let trimmed = lower.trim_start();
        let sensitive = [
            " password ",
            " password=",
            " cipher ",
            " secret ",
            " community ",
            " authentication-key",
            " privacy-key",
            " private-key",
        ]
        .iter()
        .any(|pattern| lower.contains(pattern))
            || [
                "password ",
                "password=",
                "cipher ",
                "secret ",
                "community ",
                "authentication-key",
                "privacy-key",
                "private-key",
            ]
            .iter()
            .any(|prefix| trimmed.starts_with(prefix));
        if sensitive {
            output.push_str("[敏感行已遮盖]");
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    if !text.ends_with(['\r', '\n']) {
        output.pop();
    }
    output
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::Duration as ChronoDuration;

    use super::*;

    #[derive(Default)]
    struct FakeTransport {
        state: Mutex<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        sequence: u64,
        batches: VecDeque<Vec<Vec<u8>>>,
        writes: Vec<Vec<u8>>,
    }

    impl FakeTransport {
        fn with_batches(batches: Vec<Vec<Vec<u8>>>) -> Self {
            Self {
                state: Mutex::new(FakeState {
                    batches: batches.into(),
                    ..FakeState::default()
                }),
            }
        }

        fn writes(&self) -> Vec<Vec<u8>> {
            self.state.lock().expect("测试锁").writes.clone()
        }
    }

    #[async_trait]
    impl SerialQueryTransport for FakeTransport {
        async fn write_all(&self, bytes: &[u8]) -> Result<(), SerialQueryError> {
            self.state
                .lock()
                .expect("测试锁")
                .writes
                .push(bytes.to_vec());
            Ok(())
        }

        async fn latest_sequence(&self) -> Result<u64, SerialQueryError> {
            Ok(self.state.lock().expect("测试锁").sequence)
        }

        async fn wait_for_chunks(
            &self,
            _after_sequence: u64,
            _timeout: Duration,
        ) -> Result<Vec<SerialObservedChunk>, SerialQueryError> {
            let mut state = self.state.lock().expect("测试锁");
            let Some(batch) = state.batches.pop_front() else {
                return Ok(Vec::new());
            };
            Ok(batch
                .into_iter()
                .map(|bytes| {
                    state.sequence += 1;
                    SerialObservedChunk {
                        sequence: state.sequence,
                        direction: SerialDirection::Receive,
                        bytes,
                    }
                })
                .collect())
        }
    }

    fn huawei_plan(command: &str) -> SerialQueryPlan {
        SerialQueryPlan {
            command: command.to_owned(),
            line_ending: SerialLineEnding::Cr,
            profile: SerialTerminalProfile::HuaweiVrp,
            overall_timeout_millis: 5_000,
            idle_timeout_millis: 500,
            max_bytes: 64 * 1024,
            max_pages: 10,
        }
    }

    #[test]
    fn terminal_profile_maps_huawei_line_editing_keys() {
        assert_eq!(
            encode_terminal_key(
                SerialTerminalProfile::HuaweiVrp,
                SerialTerminalKey::Backspace,
                SerialLineEnding::Cr
            ),
            vec![0x08]
        );
        assert_eq!(
            encode_terminal_key(
                SerialTerminalProfile::HuaweiVrp,
                SerialTerminalKey::Left,
                SerialLineEnding::Cr
            ),
            vec![0x02]
        );
        assert_eq!(
            encode_terminal_key(
                SerialTerminalProfile::HuaweiVrp,
                SerialTerminalKey::Right,
                SerialLineEnding::Cr
            ),
            vec![0x06]
        );
    }

    #[test]
    fn decoder_preserves_safe_cursor_actions_and_filters_osc() {
        let mut decoder = TerminalDecoder::default();
        assert_eq!(
            decoder.push(b"abc\x1b[2D!\x1b]52;c;bad\x07ok"),
            vec![
                TerminalRenderAction::Text("abc".to_owned()),
                TerminalRenderAction::MoveLeft(2),
                TerminalRenderAction::Text("!ok".to_owned())
            ]
        );
    }

    #[test]
    fn query_classifier_only_auto_authorizes_full_display_commands() {
        assert_eq!(
            classify_serial_query(SerialTerminalProfile::HuaweiVrp, "display version"),
            SerialQueryRisk::ReadOnly
        );
        assert_eq!(
            classify_serial_query(
                SerialTerminalProfile::HuaweiVrp,
                "display interface | include CRC:.*[1-9]"
            ),
            SerialQueryRisk::ReadOnly
        );
        assert_eq!(
            classify_serial_query(SerialTerminalProfile::HuaweiVrp, "dis version"),
            SerialQueryRisk::Unknown
        );
        for command in [
            "display version; reboot",
            "display version > flash:/version.txt",
            "display version | redirect flash:/version.txt",
        ] {
            assert_eq!(
                classify_serial_query(SerialTerminalProfile::HuaweiVrp, command),
                SerialQueryRisk::Unknown,
                "不受支持的复合 display 命令不得自动授权：{command}"
            );
        }
        assert_eq!(
            classify_serial_query(SerialTerminalProfile::HuaweiVrp, "system-view"),
            SerialQueryRisk::Mutating
        );
    }

    #[test]
    fn readonly_grant_is_session_bound_bounded_and_does_not_cover_unknown_commands() {
        let now = Utc::now();
        let mut grant = SerialReadOnlyGrant::new(
            "COM7",
            SerialTerminalProfile::HuaweiVrp,
            now + ChronoDuration::minutes(10),
            1,
        );
        assert_eq!(
            grant.authorize("COM8", "display version", now),
            SerialGrantDecision::SessionMismatch
        );
        assert_eq!(
            grant.authorize("COM7", "system-view", now),
            SerialGrantDecision::NotReadOnly
        );
        assert_eq!(
            grant.authorize("COM7", "display version", now),
            SerialGrantDecision::Authorized {
                remaining_commands: 0
            }
        );
        assert_eq!(
            grant.authorize("COM7", "display interface brief", now),
            SerialGrantDecision::Exhausted
        );
    }

    #[tokio::test]
    async fn query_runner_handles_fragmented_echo_and_prompt_in_one_tool_execution() {
        let transport = FakeTransport::with_batches(vec![
            vec![b"d".to_vec()],
            vec![b"isplay version\r\nHuawei VRP\r\n".to_vec()],
            vec![b"<HUAWEI>".to_vec()],
        ]);
        let result = SerialQueryRunner
            .run(&transport, huawei_plan("display version"))
            .await
            .expect("查询应完成");
        assert_eq!(result.completion, SerialQueryCompletion::Prompt);
        assert!(result.text.contains("Huawei VRP"));
        assert_eq!(transport.writes(), vec![b"display version\r".to_vec()]);
    }

    #[tokio::test]
    async fn query_runner_handles_pager_without_another_model_round() {
        let transport = FakeTransport::with_batches(vec![
            vec![b"line1\r\n  ---- More ----".to_vec()],
            vec![b"\r                \rline2\r\n<HUAWEI>".to_vec()],
        ]);
        let result = SerialQueryRunner
            .run(&transport, huawei_plan("display interface brief"))
            .await
            .expect("分页查询应完成");
        assert_eq!(result.completion, SerialQueryCompletion::Prompt);
        assert_eq!(result.pages, 1);
        assert_eq!(
            transport.writes(),
            vec![b"display interface brief\r".to_vec(), b" ".to_vec()]
        );
    }

    #[test]
    fn sensitive_configuration_lines_are_redacted_before_ai_use() {
        let text = "sysname HUAWEI\n local-user admin password irreversible-cipher value\npassword plain-value\nsecret another-value\ninterface GE1/0/1";
        let redacted = redact_serial_text(text);
        assert!(redacted.contains("sysname HUAWEI"));
        assert!(redacted.contains("[敏感行已遮盖]"));
        assert!(!redacted.contains("irreversible-cipher value"));
        assert!(!redacted.contains("plain-value"));
        assert!(!redacted.contains("another-value"));
    }
}

