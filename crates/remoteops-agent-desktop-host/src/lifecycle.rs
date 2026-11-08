//! 主窗口只消费结构化生命周期；不从终端输出猜测状态。

use crate::{NativeLanguage, ObservationCategory};
use std::collections::VecDeque;

/// 生命周期的语义颜色与文案来源。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleKind {
    /// 控制端接入。
    Connected,
    /// 控制端退出或连接中断。
    Disconnected,
    /// 真实任务开始执行。
    Running,
    /// 任务成功结束。
    Succeeded,
    /// 任务失败、取消或中断。
    Failed,
}

/// 不含命令、凭据和终端正文的简短生命周期记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleEvent {
    /// 本进程内单调序号，用于一次轮询接收多条事件。
    pub sequence: u64,
    /// 已确认的事件类型。
    pub kind: LifecycleKind,
    /// 已确认的操作分类；连接事件为空。
    pub category: Option<ObservationCategory>,
}

impl LifecycleEvent {
    /// 按界面当前语言生成简短文案，不携带远端输出。
    #[must_use]
    pub fn text(&self, language: NativeLanguage) -> String {
        let english = language == NativeLanguage::English;
        let text = match (self.kind, english) {
            (LifecycleKind::Connected, false) => "AI工程师已连接",
            (LifecycleKind::Connected, true) => "AI engineer connected",
            (LifecycleKind::Disconnected, false) => "AI工程师已退出",
            (LifecycleKind::Disconnected, true) => "AI engineer disconnected",
            (LifecycleKind::Running, false) => "正在执行操作",
            (LifecycleKind::Running, true) => "Executing operation",
            (LifecycleKind::Succeeded, false) => "操作执行成功",
            (LifecycleKind::Succeeded, true) => "Operation succeeded",
            (LifecycleKind::Failed, false) => "操作失败或已中断",
            (LifecycleKind::Failed, true) => "Operation failed or interrupted",
        };
        let category = match self.category {
            Some(ObservationCategory::Cmd) => "CMD",
            Some(ObservationCategory::PowerShell) => "PowerShell",
            Some(ObservationCategory::Shell) => "Shell",
            Some(ObservationCategory::Ssh) => "SSH",
            Some(ObservationCategory::Serial) if english => "Serial",
            Some(ObservationCategory::Serial) => "串口",
            Some(ObservationCategory::Other) | None => "",
        };
        if category.is_empty() {
            text.into()
        } else {
            format!("{category} · {text}")
        }
    }
}

/// 三行摘要的有界队列；暂停只影响显示，后台继续收集。
#[derive(Clone, Debug, Default)]
pub struct LifecycleTicker {
    sequence: u64,
    pending: VecDeque<LifecycleEvent>,
    shown: VecDeque<LifecycleEvent>,
    transition: Option<f64>,
}

impl LifecycleTicker {
    /// 接收自上次轮询以来的所有新事件。
    pub fn ingest<'a>(&mut self, events: impl IntoIterator<Item = &'a LifecycleEvent>) {
        for event in events {
            if event.sequence <= self.sequence {
                continue;
            }
            self.sequence = event.sequence;
            if self.pending.len() == 64 {
                self.pending.pop_front();
            }
            self.pending.push_back(event.clone());
        }
    }

    /// 推进显示时钟；调用者在暂停、隐藏或最小化时停止推进。
    pub fn advance(&mut self, now: f64, animate: bool) {
        if !animate {
            self.shown.extend(self.pending.drain(..));
            while self.shown.len() > 3 {
                self.shown.pop_front();
            }
            self.transition = None;
            return;
        }
        if self.transition.is_some_and(|start| now - start >= 0.32) {
            while self.shown.len() > 3 {
                self.shown.pop_front();
            }
            self.transition = None;
        }
        if self.transition.is_none()
            && let Some(event) = self.pending.pop_front()
        {
            self.shown.push_back(event);
            self.transition = Some(now);
        }
    }

    /// 返回最多四行；第四行只在滚入期间使用，并由绘制区裁剪。
    #[must_use]
    pub fn rows(&self) -> &VecDeque<LifecycleEvent> {
        &self.shown
    }

    /// 返回滚入的缓出进度。
    #[must_use]
    pub fn progress(&self, now: f64) -> f64 {
        self.transition.map_or(1.0, |start| {
            let t = ((now - start) / 0.32).clamp(0.0, 1.0);
            1.0 - (1.0 - t).powi(3)
        })
    }

    /// 是否有待呈现事件或尚未结束的滚动。
    #[must_use]
    pub fn moving(&self) -> bool {
        self.transition.is_some() || !self.pending.is_empty()
    }
}

/// 执行提示在约 1.4 秒内柔和变化，始终保持可见。
#[must_use]
pub fn execution_brightness(seconds: f64) -> f64 {
    0.76 + 0.24 * (seconds * std::f64::consts::TAU / 1.4).cos()
}

/// 长行开始与末尾各停顿两秒，再按每秒 22 个逻辑像素平移。
#[must_use]
pub fn marquee_offset(seconds: f64, overflow: f64) -> f64 {
    if overflow <= 0.0 {
        return 0.0;
    }
    let travel = overflow / 22.0;
    let phase = seconds.rem_euclid(4.0 + travel);
    ((phase - 2.0).max(0.0) * 22.0).min(overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64) -> LifecycleEvent {
        LifecycleEvent {
            sequence,
            kind: LifecycleKind::Running,
            category: Some(ObservationCategory::Ssh),
        }
    }

    #[test]
    fn receives_multiple_events_once_and_keeps_latest_three_without_motion() {
        let events: Vec<_> = (1..=7).map(event).collect();
        let mut ticker = LifecycleTicker::default();
        ticker.ingest(&events);
        ticker.advance(0.0, false);
        assert_eq!(
            ticker
                .rows()
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![5, 6, 7]
        );
        ticker.ingest(&events);
        ticker.advance(10.0, true);
        assert!(!ticker.moving());
        assert_eq!(ticker.rows().len(), 3);
    }

    #[test]
    fn paused_ingestion_is_bounded_and_animation_retains_history() {
        let mut ticker = LifecycleTicker::default();
        for sequence in 1..=1000 {
            ticker.ingest([&event(sequence)]);
        }
        assert_eq!(ticker.pending.len(), 64);
        assert!(ticker.rows().is_empty());
        ticker.advance(1.0, true);
        assert_eq!(ticker.rows()[0].sequence, 937);
        assert!(ticker.progress(1.0).abs() < f64::EPSILON);
        ticker.advance(1.4, true);
        assert_eq!(ticker.rows().len(), 2);
        ticker.advance(1.8, false);
        assert_eq!(ticker.rows().back().unwrap().sequence, 1000);
        ticker.advance(100.0, false);
        assert_eq!(ticker.rows().back().unwrap().sequence, 1000);
    }

    #[test]
    fn pulse_never_extinguishes_and_long_text_pauses_at_both_ends() {
        assert!((execution_brightness(0.0) - execution_brightness(1.4)).abs() < 0.001);
        assert!((execution_brightness(0.7) - 0.52).abs() < 0.001);
        assert!((marquee_offset(1.0, 44.0) - 0.0).abs() < f64::EPSILON);
        assert!((marquee_offset(3.0, 44.0) - 22.0).abs() < f64::EPSILON);
        assert!((marquee_offset(5.0, 44.0) - 44.0).abs() < f64::EPSILON);
        assert!((marquee_offset(6.0, 44.0) - 0.0).abs() < f64::EPSILON);
    }
}
