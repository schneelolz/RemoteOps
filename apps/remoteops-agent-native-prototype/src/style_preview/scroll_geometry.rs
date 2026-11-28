//! 只读文本区的纯几何计算，不参与文本布局或替代原生滚动消息。

/// 输入尺寸均为物理像素；内容尺寸由实际字体测量得到。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ViewMetrics {
    /// 外部容器宽度。
    pub width: i32,
    /// 外部容器高度。
    pub height: i32,
    /// 最宽内容行的像素宽度。
    pub content_width: i32,
    /// 全部内容的像素高度。
    pub content_height: i32,
    /// 四边使用相同的内部留白。
    pub padding: i32,
    /// 垂直滚动条宽度。
    pub bar_width: i32,
    /// 水平滚动条高度。
    pub bar_height: i32,
}

/// 内框及文本可视区域，不包含窗口的屏幕坐标。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ViewLayout {
    /// 去除左右留白后的宽度。
    pub inner_width: i32,
    /// 去除上下留白后的高度。
    pub inner_height: i32,
    /// 再去除垂直滚动条后的文本可视宽度。
    pub text_width: i32,
    /// 再去除水平滚动条后的文本可视高度。
    pub text_height: i32,
    /// 内容是否需要横向滚动。
    pub horizontal: bool,
    /// 内容是否需要纵向滚动。
    pub vertical: bool,
}

/// 单调加入必需的滚动条；一条占用空间后可能触发另一条。
pub(super) fn layout(metrics: ViewMetrics) -> ViewLayout {
    let inset = metrics.padding.max(0).saturating_mul(2);
    let inner_width = metrics.width.saturating_sub(inset).max(0);
    let inner_height = metrics.height.saturating_sub(inset).max(0);
    let vertical_bar_width = metrics.bar_width.max(0);
    let horizontal_bar_height = metrics.bar_height.max(0);
    let content_width = metrics.content_width.max(0);
    let content_height = metrics.content_height.max(0);
    let (mut horizontal, mut vertical) = (false, false);
    // 两个开关只会从关变开，最多两次变化和一次稳定检查。
    for _ in 0..3 {
        let text_width = inner_width
            .saturating_sub(if vertical { vertical_bar_width } else { 0 })
            .max(0);
        let text_height = inner_height
            .saturating_sub(if horizontal { horizontal_bar_height } else { 0 })
            .max(0);
        let next_horizontal = horizontal || content_width > text_width;
        let next_vertical = vertical || content_height > text_height;
        if (horizontal, vertical) == (next_horizontal, next_vertical) {
            break;
        }
        horizontal = next_horizontal;
        vertical = next_vertical;
    }
    ViewLayout {
        inner_width,
        inner_height,
        text_width: inner_width
            .saturating_sub(if vertical { vertical_bar_width } else { 0 })
            .max(0),
        text_height: inner_height
            .saturating_sub(if horizontal { horizontal_bar_height } else { 0 })
            .max(0),
        horizontal,
        vertical,
    }
}

#[cfg(test)]
mod tests {
    use super::{ViewMetrics, layout};

    fn metrics(content_width: i32, content_height: i32) -> ViewMetrics {
        ViewMetrics {
            width: 200,
            height: 100,
            content_width,
            content_height,
            padding: 8,
            bar_width: 16,
            bar_height: 16,
        }
    }

    #[test]
    fn empty_and_exactly_fitting_content_need_no_bars() {
        for (width, height) in [(0, 0), (184, 84)] {
            let result = layout(metrics(width, height));
            assert!(!result.horizontal && !result.vertical);
            assert_eq!((result.text_width, result.text_height), (184, 84));
        }
    }

    #[test]
    fn wide_line_only_reserves_horizontal_bar() {
        let result = layout(metrics(200, 40));
        assert!(result.horizontal && !result.vertical);
        assert_eq!((result.text_width, result.text_height), (184, 68));
    }

    #[test]
    fn many_lines_only_reserve_vertical_bar() {
        let result = layout(metrics(100, 200));
        assert!(!result.horizontal && result.vertical);
        assert_eq!((result.text_width, result.text_height), (168, 84));
    }

    #[test]
    fn either_bar_can_trigger_the_other_without_wrapping_text() {
        for (content_width, content_height) in [(101, 100), (100, 101)] {
            let result = layout(ViewMetrics {
                width: 116,
                height: 116,
                content_width,
                content_height,
                ..metrics(0, 0)
            });
            assert!(result.horizontal && result.vertical);
            assert_eq!((result.text_width, result.text_height), (84, 84));
        }
    }

    #[test]
    fn independent_bar_dimensions_can_trigger_each_other() {
        for (content_width, content_height) in [(101, 80), (95, 101)] {
            let result = layout(ViewMetrics {
                width: 116,
                height: 116,
                content_width,
                content_height,
                bar_width: 10,
                bar_height: 30,
                ..metrics(0, 0)
            });
            assert!(result.horizontal && result.vertical);
            assert_eq!((result.text_width, result.text_height), (90, 70));
        }
    }

    #[test]
    fn tiny_and_invalid_sizes_never_create_negative_viewports() {
        let result = layout(ViewMetrics {
            width: 5,
            height: 7,
            ..metrics(1, 1)
        });
        assert!(result.horizontal && result.vertical);
        assert_eq!((result.text_width, result.text_height), (0, 0));
        let invalid = layout(ViewMetrics {
            width: -1,
            height: -1,
            padding: i32::MAX,
            ..metrics(-1, -1)
        });
        assert!(!invalid.horizontal && !invalid.vertical);
        assert_eq!((invalid.inner_width, invalid.inner_height), (0, 0));
    }
}
