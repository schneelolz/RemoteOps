//! 为系统文本控件划分有界 UTF-16 呈现片段，不改变记录内容。

/// 返回从 `offset` 开始的片段结束位置，同时限制 UTF-16 单元数和换行数。
///
/// 输入应为有效 UTF-16，`offset` 应来自文本开头或本函数返回的边界。
/// 代理对和 CRLF 始终一起输出；容量至少为两个单元、一行，确保非空片段前进。
/// 超出文本的起点返回文本末尾；超长单行可跨多个片段。
#[must_use]
pub(crate) fn chunk_end(units: &[u16], offset: usize, max_units: usize, max_lines: usize) -> usize {
    if offset >= units.len() {
        return units.len();
    }
    let limit = offset.saturating_add(max_units.max(2)).min(units.len());
    let line_limit = max_lines.max(1);
    let mut end = offset;
    let mut lines = 0usize;
    while end < limit {
        let first = units[end];
        let paired = units.get(end + 1).is_some_and(|second| {
            (matches!(first, 0xd800..=0xdbff) && matches!(*second, 0xdc00..=0xdfff))
                || (first == 13 && *second == 10)
        });
        let next = end + if paired { 2 } else { 1 };
        if next > limit {
            break;
        }
        end = next;
        if first == 13 || first == 10 {
            lines += 1;
            if lines == line_limit {
                break;
            }
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_breaks(units: &[u16]) -> usize {
        units
            .iter()
            .enumerate()
            .filter(|&(index, unit)| {
                *unit == 13 || (*unit == 10 && (index == 0 || units[index - 1] != 13))
            })
            .count()
    }

    fn check_chunks(text: &str, max_units: usize, max_lines: usize) -> Vec<Vec<u16>> {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut offset = 0;
        let mut chunks = Vec::new();
        while offset < units.len() {
            let end = chunk_end(&units, offset, max_units, max_lines);
            assert!(end > offset && end <= units.len());
            let chunk = units[offset..end].to_vec();
            assert!(chunk.len() <= max_units.max(2));
            assert!(line_breaks(&chunk) <= max_lines.max(1));
            assert!(String::from_utf16(&chunk).is_ok());
            if end < units.len() {
                assert!(
                    !(matches!(units[end - 1], 0xd800..=0xdbff)
                        && matches!(units[end], 0xdc00..=0xdfff))
                );
                assert!(!(units[end - 1] == 13 && units[end] == 10));
            }
            chunks.push(chunk);
            offset = end;
        }
        let joined: Vec<u16> = chunks.iter().flatten().copied().collect();
        assert_eq!(joined, units);
        assert_eq!(String::from_utf16(&joined).unwrap(), text);
        chunks
    }

    #[test]
    fn chinese_emoji_and_line_endings_reassemble_under_small_limits() {
        let text = "中文🔎第一行\r\n第二行🔎\n第三行\r末尾";
        for max_units in [0, 1, 2, 3, 4, 7, 2048] {
            for max_lines in [0, 1, 2, 64] {
                check_chunks(text, max_units, max_lines);
            }
        }
    }

    #[test]
    fn one_or_two_unit_limits_do_not_split_surrogate_pairs() {
        let units: Vec<u16> = "中🔎文".encode_utf16().collect();
        for max_units in [0, 1, 2] {
            assert_eq!(chunk_end(&units, 0, max_units, 1), 1);
            assert_eq!(chunk_end(&units, 1, max_units, 1), 3);
            assert_eq!(chunk_end(&units, 3, max_units, 1), 4);
        }
        assert_eq!(check_chunks("🔎", 1, 1).len(), 1);
    }

    #[test]
    fn crlf_stays_together_when_the_unit_budget_ends_at_cr() {
        let units: Vec<u16> = "中\r\n文".encode_utf16().collect();
        assert_eq!(chunk_end(&units, 0, 2, 1), 1);
        assert_eq!(chunk_end(&units, 1, 2, 1), 3);
        assert_eq!(chunk_end(&units, 3, 2, 1), 4);
        assert_eq!(check_chunks("\r\n\r\n", 1, 0).len(), 2);
    }

    #[test]
    fn empty_input_and_eof_return_the_end_without_indexing() {
        assert_eq!(chunk_end(&[], 0, 0, 0), 0);
        assert_eq!(chunk_end(&[], usize::MAX, usize::MAX, usize::MAX), 0);
        let units: Vec<u16> = "中文🔎".encode_utf16().collect();
        assert_eq!(chunk_end(&units, units.len(), 1, 1), units.len());
        assert_eq!(chunk_end(&units, usize::MAX, 1, 1), units.len());
        assert_eq!(check_chunks("", 0, 0), Vec::<Vec<u16>>::new());
    }

    #[test]
    fn an_unbounded_budget_uses_saturating_addition() {
        let units: Vec<u16> = "首🔎尾".encode_utf16().collect();
        assert_eq!(chunk_end(&units, 0, usize::MAX, usize::MAX), units.len());
        assert_eq!(chunk_end(&units, 1, usize::MAX, usize::MAX), units.len());
        check_chunks("中文\r\n🔎\n尾", usize::MAX, usize::MAX);
    }

    #[test]
    fn long_single_lines_make_bounded_progress_without_modifying_text() {
        let text = "中文".repeat(10_000) + &"🔎".repeat(511);
        let chunks = check_chunks(&text, 17, 1);
        assert!(chunks.len() > 1_000);
        assert!(chunks.iter().all(|chunk| line_breaks(chunk) == 0));
    }

    #[test]
    fn sixty_four_lines_stop_at_the_exact_line_boundary() {
        use std::fmt::Write;
        let lines = |count| {
            (0..count).fold(String::new(), |mut text, line| {
                write!(text, "中文第{line}行 🔎\r\n").unwrap();
                text
            })
        };
        let text = lines(70);
        let first = lines(64);
        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(chunk_end(&units, 0, 2048, 64), first.encode_utf16().count());
        let chunks = check_chunks(&text, 2048, 64);
        assert_eq!(chunks.len(), 2);
        assert_eq!(line_breaks(&chunks[0]), 64);
        assert_eq!(line_breaks(&chunks[1]), 6);
    }
}
