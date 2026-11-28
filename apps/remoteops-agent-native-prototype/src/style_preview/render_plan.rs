//! 控件声明差异及单帧诊断；不包含窗口文字、路径或现场标识。
use serde::Serialize;
use std::{cell::Cell, collections::BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ControlSpec {
    pub class: &'static str,
    pub style: u32,
    pub bounds: [i32; 4],
    pub font: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // 各种原生差异可独立发生，不是互斥状态。
pub(super) struct ControlUpdate {
    pub recreate: bool,
    pub bounds: bool,
    pub font: bool,
    pub text: bool,
}

pub(super) fn plan_control(
    previous: Option<ControlSpec>,
    next: ControlSpec,
    current_text: &str,
    desired_text: &str,
) -> ControlUpdate {
    let compatible = previous.is_some_and(|old| old.class == next.class && old.style == next.style);
    ControlUpdate {
        recreate: !compatible,
        bounds: !compatible || previous.is_some_and(|old| old.bounds != next.bounds),
        font: !compatible || previous.is_some_and(|old| old.font != next.font),
        text: current_text != desired_text,
    }
}

pub(super) fn stale_ids(existing: impl Iterator<Item = i32>, desired: &BTreeSet<i32>) -> Vec<i32> {
    existing.filter(|id| !desired.contains(id)).collect()
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(super) struct ControlWork {
    pub created: u32,
    pub reused: u32,
    pub removed: u32,
    pub text_writes: u32,
    pub font_changes: u32,
    pub moves: u32,
    pub create_us: u128,
    pub text_us: u128,
    pub font_us: u128,
    pub move_us: u128,
    pub readonly_us: u128,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(super) struct PageFrame {
    pub initial: bool,
    pub total_us: u128,
    pub prepare_us: u128,
    pub fonts_us: u128,
    pub declare_us: u128,
    pub decorate_us: u128,
    pub restore_us: u128,
    pub prune_us: u128,
    pub work: ControlWork,
}

thread_local! {
    // 单独统计真实绘制，避免仅把同步工作移入WM_PAINT后就宣称整体达标。
    static PAINT: Cell<(u128, u64)> = const { Cell::new((0, 0)) };
}
pub(super) fn record_paint(elapsed_us: u128) {
    PAINT.with(|value| {
        let (maximum, count) = value.get();
        value.set((maximum.max(elapsed_us), count.saturating_add(1)));
    });
}
pub(super) fn paint_metrics() -> (u128, u64) {
    PAINT.with(Cell::get)
}
pub(super) fn reset_paint_metrics() {
    PAINT.with(|value| value.set((0, 0)));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec() -> ControlSpec {
        ControlSpec {
            class: "EDIT",
            style: 8,
            bounds: [10, 20, 300, 200],
            font: 4,
        }
    }
    #[test]
    fn stable_declaration_and_native_text_need_no_mutation() {
        assert_eq!(
            plan_control(Some(spec()), spec(), "中文😀", "中文😀"),
            ControlUpdate::default()
        );
    }
    #[test]
    fn native_text_changes_outside_build_are_not_hidden_by_a_declaration_cache() {
        let plan = plan_control(Some(spec()), spec(), "复制完成", "复制控制码");
        assert!(plan.text && !plan.recreate && !plan.bounds && !plan.font);
    }
    #[test]
    fn declaration_class_and_style_changes_replace_only_incompatible_controls() {
        let mut next = spec();
        next.class = "STATIC";
        assert!(plan_control(Some(spec()), next, "", "").recreate);
        next = spec();
        next.style ^= 2;
        assert!(plan_control(Some(spec()), next, "", "").recreate);
    }
    #[test]
    fn dpi_or_layout_updates_keep_the_window_identity() {
        let mut next = spec();
        next.bounds = [15, 30, 450, 300];
        next.font = 6;
        let plan = plan_control(Some(spec()), next, "正文", "正文");
        assert!(!plan.recreate && plan.bounds && plan.font && !plan.text);
    }
    #[test]
    fn page_switch_reclaims_only_undeclared_ids() {
        let desired = BTreeSet::from([310, 311, 340]);
        assert_eq!(
            stale_ids([300, 303, 310, 311, 338].into_iter(), &desired),
            [300, 303, 338]
        );
    }
}
