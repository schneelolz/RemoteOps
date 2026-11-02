//! 原生界面共用的有界、脱敏文本模型；不依赖窗口或后台运行时。

use std::{collections::VecDeque, sync::OnceLock};

use remoteops_audit::Redactor;

/// 后台向界面发送的完整呈现事件；文本不是键盘输入。
#[derive(Debug)]
pub enum Event {
    /// 更新当前运行状态。
    Status(String),
    /// 追加一条完整的命令或返回记录。
    Line(String),
    /// 当前后台任务已经结束。
    Finished,
}

/// 对完整记录应用现有审计与网络设备脱敏规则。
#[must_use]
pub(crate) fn safe_record(text: &str) -> String {
    static REDACTOR: OnceLock<Redactor> = OnceLock::new();
    REDACTOR
        .get_or_init(Redactor::default)
        .redact(&remoteops_serial::redact_serial_text(text))
}

/// 已呈现在观察窗中的记录位置；UTF-16 计数包含每行的 CRLF。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderCursor {
    /// 已呈现到的累计行数。
    pub total_lines: u64,
    /// 已从开头移除的累计行数。
    pub trimmed_lines: u64,
    /// 已从开头移除的累计 UTF-16 单元数。
    pub trimmed_utf16_units: u64,
    /// 此位置仍保留在窗口中的 UTF-16 单元数。
    pub utf16_units: usize,
}

/// 相对于一次已完成呈现的文本更新；先删除前缀，再追加已脱敏文本。
#[derive(Debug)]
pub struct TextPatch {
    /// 历史已全部淘汰或窗口重建时，完整替换现有文本。
    pub replace_all: bool,
    /// 增量更新时，从现有文本开头移除的 UTF-16 单元数。
    pub remove_utf16_units: usize,
    /// 需要追加的 CRLF 文本；完整替换时为所有保留文本。
    pub append: String,
    /// 成功完成本次控件更新后记录的游标。
    pub cursor: RenderCursor,
}

/// 只保留有限行数与 UTF-16 单元的只读观察记录。
pub struct Transcript {
    // 同时限制行数和 UTF-16 字符数，避免 日志控件随压力输入无限增长。
    lines: VecDeque<String>,
    max_lines: usize,
    max_utf16_units: usize,
    utf16_units: usize,
    total_lines: u64,
    trimmed_lines: u64,
    trimmed_utf16_units: u64,
    // 完整 PEM 跨多条日志到达时，正文也不能在后续事件中重新暴露。
    in_private_key: bool,
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new(2_000, 262_144)
    }
}

impl Transcript {
    /// 创建有界记录；最小容量保证一个字符和 CRLF 能完整保留。
    #[must_use]
    pub fn new(max_lines: usize, max_utf16_units: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            max_lines: max_lines.max(1),
            max_utf16_units: max_utf16_units.max(4),
            utf16_units: 0,
            total_lines: 0,
            trimmed_lines: 0,
            trimmed_utf16_units: 0,
            in_private_key: false,
        }
    }

    /// 追加完整记录，先脱敏再截断；流式输出须由后台先组装到完整行。
    pub fn append(&mut self, record: &str) {
        let mut masked = Vec::new();
        let lines: Vec<_> = record.lines().collect();
        for line in if lines.is_empty() { vec![""] } else { lines } {
            let upper = line.to_ascii_uppercase();
            let begins_key = upper.contains("-----BEGIN ") && upper.contains("PRIVATE KEY-----");
            let ends_key = upper.contains("-----END ") && upper.contains("PRIVATE KEY-----");
            if self.in_private_key || begins_key {
                self.in_private_key = !ends_key;
                masked.push("[私钥内容已隐藏]".to_owned());
            } else {
                // NUL 不得提前终止 Windows UTF-16 文本；其余控制字节只保留换行和制表符。
                masked.push(
                    line.chars()
                        .filter(|character| !character.is_control() || *character == '\t')
                        .collect(),
                );
            }
        }
        let safe = safe_record(&masked.join("\n"));
        let safe_lines: Vec<_> = safe.lines().collect();
        for line in if safe_lines.is_empty() {
            vec![""]
        } else {
            safe_lines
        } {
            self.total_lines = self.total_lines.saturating_add(1);
            let line = truncate_utf16(line, self.max_utf16_units - 2);
            let units = line.encode_utf16().count() + 2;
            self.lines.push_back(line);
            self.utf16_units += units;
            while self.lines.len() > self.max_lines || self.utf16_units > self.max_utf16_units {
                if let Some(removed) = self.lines.pop_front() {
                    let removed_units = removed.encode_utf16().count() + 2;
                    self.utf16_units -= removed_units;
                    self.trimmed_lines = self.trimmed_lines.saturating_add(1);
                    self.trimmed_utf16_units = self
                        .trimmed_utf16_units
                        .saturating_add(u64::try_from(removed_units).unwrap_or(u64::MAX));
                }
            }
        }
    }

    /// 返回可直接交给 Unicode 文本控件 的 CRLF 文本。
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.iter().fold(String::new(), |mut text, line| {
            text.push_str(line);
            text.push_str("\r\n");
            text
        })
    }

    /// 返回当前文本位置，用于下一轮增量呈现。
    #[must_use]
    pub const fn cursor(&self) -> RenderCursor {
        RenderCursor {
            total_lines: self.total_lines,
            trimmed_lines: self.trimmed_lines,
            trimmed_utf16_units: self.trimmed_utf16_units,
            utf16_units: self.utf16_units,
        }
    }

    /// 仅生成游标之后的更新；窗口重建或暂停过久时退回完整快照。
    #[must_use]
    pub fn patch_since(&self, previous: Option<RenderCursor>) -> TextPatch {
        let cursor = self.cursor();
        let incremental = previous.and_then(|previous| {
            if previous.total_lines < self.trimmed_lines || previous.total_lines > self.total_lines
            {
                return None;
            }
            let removed_units = usize::try_from(
                self.trimmed_utf16_units
                    .checked_sub(previous.trimmed_utf16_units)?,
            )
            .ok()?;
            if removed_units > previous.utf16_units {
                return None;
            }
            let retained_before =
                usize::try_from(previous.total_lines - self.trimmed_lines).ok()?;
            let append =
                self.lines
                    .iter()
                    .skip(retained_before)
                    .fold(String::new(), |mut text, line| {
                        text.push_str(line);
                        text.push_str("\r\n");
                        text
                    });
            Some((removed_units, append))
        });
        let (replace_all, remove_utf16_units, append) = incremental.map_or_else(
            || (true, 0, self.text()),
            |(removed, append)| (false, removed, append),
        );
        TextPatch {
            replace_all,
            remove_utf16_units,
            append,
            cursor,
        }
    }

    /// 返回已经接收并处理的行数，不包含后台因队列满而丢弃的事件。
    #[must_use]
    pub const fn total_lines(&self) -> u64 {
        self.total_lines
    }

    /// 返回当前仍然保留的行数。
    #[must_use]
    pub fn retained_lines(&self) -> usize {
        self.lines.len()
    }

    /// 返回已经因历史容量限制而淘汰的行数。
    #[must_use]
    pub const fn trimmed_lines(&self) -> u64 {
        self.trimmed_lines
    }
}

fn truncate_utf16(text: &str, limit: usize) -> String {
    if text.encode_utf16().count() <= limit {
        return text.to_owned();
    }
    let mut output = String::new();
    let mut units = 0;
    for character in text.chars() {
        if units + character.len_utf16() >= limit {
            break;
        }
        units += character.len_utf16();
        output.push(character);
    }
    output.push('…');
    output
}

/// 将 96 DPI 逻辑尺寸缩放为像素，并饱和处理异常输入。
#[must_use]
pub fn scaled(value: i32, dpi: u32) -> i32 {
    let dpi = if dpi == 0 { 96 } else { dpi };
    let product = i64::from(value) * i64::from(dpi);
    let rounded = (product + 48 * product.signum()) / 96;
    i32::try_from(rounded).unwrap_or(if rounded < 0 { i32::MIN } else { i32::MAX })
}

/// 创建标明来源的中文压力样例；测试口令用于验证脱敏，不是真实凭据。
#[must_use]
pub fn sample_line(index: u64) -> String {
    if index.is_multiple_of(257) {
        format!("[压力样例 {index:06}] password=原型测试秘密")
    } else {
        format!("[压力样例 {index:06}] 中文状态：接口正常，链路已连接，观察窗只读 🔎")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded_by_lines_and_keeps_latest_output() {
        let mut transcript = Transcript::new(3, 1_024);
        for index in 0..10 {
            transcript.append(&format!("中文第{index}行"));
        }
        assert_eq!(transcript.total_lines(), 10);
        assert_eq!(transcript.retained_lines(), 3);
        assert_eq!(transcript.trimmed_lines(), 7);
        assert!(!transcript.text().contains("第6行"));
        assert!(transcript.text().contains("第9行"));
    }

    #[test]
    fn long_unicode_records_respect_utf16_capacity_without_splitting_surrogates() {
        let mut transcript = Transcript::new(10, 12);
        transcript.append("中文🔎🔎🔎🔎🔎🔎🔎🔎");
        let text = transcript.text();
        assert!(text.encode_utf16().count() <= 12);
        assert!(text.contains('…'));
        assert!(!text.contains('\u{fffd}'));
        assert_eq!(
            String::from_utf16(&text.encode_utf16().collect::<Vec<_>>()).unwrap(),
            text
        );
    }

    #[test]
    fn redaction_precedes_truncation_and_masks_private_keys_across_records() {
        let mut transcript = Transcript::new(20, 200);
        transcript.append("password=原型测试秘密");
        transcript.append("Authorization: Bearer abc.def-secret");
        transcript.append("-----BEGIN PRIVATE KEY-----");
        transcript.append("private-material-that-must-not-be-visible");
        transcript.append("-----END PRIVATE KEY-----");
        transcript.append("恢复中文输出 🔎");
        let text = transcript.text();
        assert!(!text.contains("原型测试秘密"));
        assert!(!text.contains("abc.def-secret"));
        assert!(!text.contains("private-material"));
        assert!(text.contains("恢复中文输出 🔎"));
    }

    #[test]
    fn zero_capacities_and_embedded_nul_do_not_break_windows_text_contract() {
        let mut transcript = Transcript::new(0, 0);
        transcript.append("中文\0第二部分🔎");
        assert_eq!(transcript.retained_lines(), 1);
        assert!(transcript.text().encode_utf16().count() <= 4);
        assert!(!transcript.text().contains('\0'));
    }

    // 对 UTF-16 缓冲应用同样的删除和追加顺序，覆盖真实控件的补丁边界。
    fn apply_patch(buffer: &mut Vec<u16>, patch: &TextPatch) {
        if patch.replace_all {
            buffer.clear();
        } else {
            buffer.drain(..patch.remove_utf16_units);
        }
        buffer.extend(patch.append.encode_utf16());
        assert_eq!(buffer.len(), patch.cursor.utf16_units);
        assert!(String::from_utf16(buffer).is_ok());
    }

    #[test]
    fn patches_append_and_trim_multiple_unicode_batches_without_rewriting_history() {
        let mut transcript = Transcript::new(3, 100);
        let mut cursor = None;
        let mut displayed = Vec::new();
        for batch in 0..8 {
            transcript.append(&format!("批次{batch}第一行 🔎\n批次{batch}第二行"));
            let patch = transcript.patch_since(cursor);
            assert_eq!(patch.replace_all, batch == 0);
            assert_eq!(patch.cursor.trimmed_lines, transcript.trimmed_lines());
            assert_eq!(patch.cursor.total_lines, (batch + 1) * 2);
            if batch > 0 {
                assert!(patch.remove_utf16_units > 0);
                assert_eq!(patch.append.lines().count(), 2);
            }
            apply_patch(&mut displayed, &patch);
            assert_eq!(String::from_utf16(&displayed).unwrap(), transcript.text());
            cursor = Some(patch.cursor);
        }
        let unchanged = transcript.patch_since(cursor);
        assert!(!unchanged.replace_all);
        assert_eq!(unchanged.remove_utf16_units, 0);
        assert!(unchanged.append.is_empty());
    }

    #[test]
    fn paragraph_positions_remove_one_crlf_unit_per_trimmed_line() {
        let mut transcript = Transcript::new(3, 100);
        let mut cursor: Option<RenderCursor> = None;
        let mut internal_text: Vec<u16> = Vec::new();
        for batch in 0..10 {
            transcript.append(&format!("中文{batch} 🔎\n第二行{batch}"));
            let patch = transcript.patch_since(cursor);
            if patch.replace_all {
                internal_text.clear();
            } else {
                let removed_lines = patch.cursor.trimmed_lines - cursor.unwrap().trimmed_lines;
                let removed_positions =
                    patch.remove_utf16_units - usize::try_from(removed_lines).unwrap();
                internal_text.drain(..removed_positions);
            }
            internal_text.extend(patch.append.replace("\r\n", "\r").encode_utf16());
            assert_eq!(
                String::from_utf16(&internal_text).unwrap(),
                transcript.text().replace("\r\n", "\r")
            );
            assert_eq!(
                internal_text.len(),
                patch.cursor.utf16_units - transcript.retained_lines()
            );
            cursor = Some(patch.cursor);
        }
    }

    #[test]
    fn paused_view_resets_when_all_displayed_history_has_expired() {
        let mut transcript = Transcript::new(2, 100);
        transcript.append("暂停时的第一行 🔎");
        let first = transcript.patch_since(None);
        let mut displayed = Vec::new();
        apply_patch(&mut displayed, &first);
        for index in 0..8 {
            transcript.append(&format!("暂停期间新增{index} 🔎"));
        }
        let resumed = transcript.patch_since(Some(first.cursor));
        assert!(resumed.replace_all);
        apply_patch(&mut displayed, &resumed);
        assert_eq!(String::from_utf16(&displayed).unwrap(), transcript.text());
        let recreated = transcript.patch_since(None);
        assert!(recreated.replace_all);
        assert_eq!(recreated.append, resumed.append);
    }

    #[test]
    fn patch_deletes_complete_utf16_lines_before_appending_bounded_long_output() {
        let mut transcript = Transcript::new(8, 16);
        transcript.append("旧 🔎");
        let initial = transcript.patch_since(None);
        let mut displayed = Vec::new();
        apply_patch(&mut displayed, &initial);
        transcript.append("新🔎🔎🔎🔎🔎🔎🔎🔎🔎");
        let patch = transcript.patch_since(Some(initial.cursor));
        assert!(!patch.replace_all);
        assert_eq!(patch.remove_utf16_units, initial.cursor.utf16_units);
        assert!(patch.append.contains('…'));
        apply_patch(&mut displayed, &patch);
        assert!(displayed.len() <= 16);
        assert_eq!(String::from_utf16(&displayed).unwrap(), transcript.text());
    }

    #[test]
    fn incremental_updates_never_expose_credentials_or_private_key_bodies() {
        let mut transcript = Transcript::new(20, 1_024);
        let mut displayed = Vec::new();
        let mut cursor = None;
        for record in [
            "password=原型测试秘密",
            "-----BEGIN PRIVATE KEY-----",
            "sensitive-private-material",
            "-----END PRIVATE KEY-----",
            "中文输出恢复 🔎",
        ] {
            transcript.append(record);
            let patch = transcript.patch_since(cursor);
            assert!(!patch.append.contains("原型测试秘密"));
            assert!(!patch.append.contains("sensitive-private-material"));
            apply_patch(&mut displayed, &patch);
            cursor = Some(patch.cursor);
        }
        assert_eq!(String::from_utf16(&displayed).unwrap(), transcript.text());
        assert!(transcript.text().contains("中文输出恢复 🔎"));
    }

    #[test]
    fn dpi_scaling_covers_common_scales_and_saturates() {
        assert_eq!(scaled(100, 96), 100);
        assert_eq!(scaled(100, 120), 125);
        assert_eq!(scaled(100, 144), 150);
        assert_eq!(scaled(100, 192), 200);
        assert_eq!(scaled(-20, 144), -30);
        assert_eq!(scaled(20, 0), 20);
        assert_eq!(scaled(i32::MAX, u32::MAX), i32::MAX);
    }
}
