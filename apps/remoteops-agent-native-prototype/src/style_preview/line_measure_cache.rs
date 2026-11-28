//! 完整行宽度缓存；字体测量仍由调用方原样执行，不按字符数猜测中文或 emoji。
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct LineMeasureCache {
    // 每轮只保留当前文本实际包含的不同完整行。
    widths: BTreeMap<String, i32>,
}
impl LineMeasureCache {
    /// 仅对新增或变化的完整行调用测量，重复行及保留行复用上次结果。
    pub(super) fn maximum(&mut self, text: &str, mut measure: impl FnMut(&str) -> i32) -> i32 {
        let mut previous = std::mem::take(&mut self.widths);
        let mut current: BTreeMap<String, i32> = BTreeMap::new();
        let mut maximum = 0;
        for line in text.split('\n').map(|line| line.trim_end_matches('\r')) {
            let width = if let Some(width) = current.get(line) {
                *width
            } else if let Some((line, width)) = previous.remove_entry(line) {
                current.insert(line, width);
                width
            } else {
                let width = measure(line);
                current.insert(line.into(), width);
                width
            };
            maximum = maximum.max(width);
        }
        self.widths = current;
        maximum
    }

    /// 字体或 DPI 改变时不得继续使用旧像素宽度。
    pub(super) fn invalidate(&mut self) {
        self.widths.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::LineMeasureCache;

    #[test]
    fn unchanged_and_duplicate_lines_do_not_measure_again() {
        let mut cache = LineMeasureCache::default();
        let mut calls = vec![];
        assert_eq!(
            cache.maximum("same\r\nsame\r\nother", |line| {
                calls.push(line.to_owned());
                42
            }),
            42
        );
        assert_eq!(calls, ["same", "other"]);
        assert_eq!(
            cache.maximum("same\r\nsame\r\nother", |_| panic!(
                "unchanged line measured"
            )),
            42
        );
        assert_eq!(cache.widths.len(), 2);
    }

    #[test]
    fn append_only_measures_new_lines_and_trimming_discards_old_widths() {
        let mut cache = LineMeasureCache::default();
        cache.maximum("old\nkept", |_| 11);
        let mut calls = vec![];
        assert_eq!(
            cache.maximum("old\nkept\nnew", |line| {
                calls.push(line.to_owned());
                90
            }),
            90
        );
        assert_eq!(calls, ["new"]);
        assert_eq!(cache.maximum("kept", |_| panic!("kept line measured")), 11);
        assert_eq!(cache.widths.len(), 1);
        assert!(!cache.widths.contains_key("old"));
        assert!(!cache.widths.contains_key("new"));
        assert_eq!(cache.maximum("old", |_| 7), 7);
    }

    #[test]
    fn font_invalidation_measures_identical_text_with_new_metrics() {
        let mut cache = LineMeasureCache::default();
        assert_eq!(cache.maximum("text", |_| 10), 10);
        cache.invalidate();
        assert_eq!(cache.maximum("text", |_| 25), 25);
    }

    #[test]
    fn unicode_emoji_tabs_and_blank_lines_are_passed_to_the_actual_measurer() {
        let mut cache = LineMeasureCache::default();
        let mut calls = vec![];
        cache.maximum("中文🔎\tresult\r\n\r\n中文🔎\tresult\r\n", |line| {
            calls.push(line.to_owned());
            17
        });
        assert_eq!(calls, ["中文🔎\tresult", ""]);
        assert_eq!(cache.widths.len(), 2);
    }

    #[test]
    fn changed_content_at_equal_length_is_measured_and_can_shrink_maximum() {
        let mut cache = LineMeasureCache::default();
        assert_eq!(cache.maximum("wide", |_| 90), 90);
        assert_eq!(cache.maximum("thin", |_| 12), 12);
        assert_eq!(cache.widths.len(), 1);
        assert!(!cache.widths.contains_key("wide"));
    }
}
