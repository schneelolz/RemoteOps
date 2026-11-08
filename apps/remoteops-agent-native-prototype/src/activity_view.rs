//! 两个 Win32 产品入口共用的生命周期呈现与动画边界。
#![allow(unsafe_code)]

use remoteops_agent_desktop_host::{
    NativeLanguage, ProductSnapshot,
    lifecycle::{LifecycleKind, LifecycleTicker, execution_brightness, marquee_offset},
};
use std::{cell::Cell, time::Instant};
use windows_sys::Win32::{
    Foundation::{RECT, SIZE},
    Graphics::Gdi::{
        DT_NOPREFIX, DT_SINGLELINE, DrawTextW, GetTextExtentPoint32W, HDC, HFONT,
        IntersectClipRect, RestoreDC, SaveDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
    },
    UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW},
};

#[derive(Clone, Debug)]
pub(crate) struct ActivityView {
    ticker: LifecycleTicker,
    last_tick: Instant,
    clock: f64,
    row_since: f64,
    last_sequence: u64,
    pub(crate) paused: bool,
    motion: bool,
    redraw_pending: bool,
    overflow: Cell<bool>,
    preparation: Option<String>,
}

impl Default for ActivityView {
    fn default() -> Self {
        Self {
            ticker: LifecycleTicker::default(),
            last_tick: Instant::now(),
            clock: 0.0,
            row_since: 0.0,
            last_sequence: 0,
            paused: false,
            motion: false,
            redraw_pending: false,
            overflow: Cell::new(false),
            preparation: None,
        }
    }
}

impl ActivityView {
    pub(crate) fn tick(&mut self, snapshot: &ProductSnapshot, visible: bool) -> bool {
        let now = Instant::now();
        let delta = now.duration_since(self.last_tick).as_secs_f64().min(0.2);
        self.last_tick = now;
        self.tick_with_motion(snapshot, visible, animations_enabled(), delta)
    }

    fn tick_with_motion(
        &mut self,
        snapshot: &ProductSnapshot,
        visible: bool,
        motion: bool,
        delta: f64,
    ) -> bool {
        self.ticker.ingest(&snapshot.lifecycle_events);
        self.redraw_pending |= self.motion != motion;
        self.motion = motion;
        if !visible {
            return false;
        }
        let redraw = std::mem::take(&mut self.redraw_pending);
        if self.paused {
            return redraw;
        }
        let preparation = snapshot
            .resource_progress
            .as_ref()
            .map(|(_, text)| text.clone());
        let progress_changed = self.preparation != preparation;
        self.preparation = preparation;
        let previous = self.ticker.rows().back().map(|event| event.sequence);
        let previous_len = self.ticker.rows().len();
        let was_moving = self.ticker.moving();
        self.clock += delta;
        self.ticker.advance(self.clock, self.motion);
        let current = self.ticker.rows().back().map(|event| event.sequence);
        if previous != current {
            self.row_since = self.clock;
        }
        let sequence = current.unwrap_or(0);
        let changed = sequence != self.last_sequence
            || previous_len != self.ticker.rows().len()
            || (was_moving && !self.ticker.moving());
        self.last_sequence = sequence;
        redraw
            || progress_changed
            || changed
            || (self.motion
                && (self.ticker.moving()
                    || self.overflow.get()
                    || (snapshot.controllers > 0 && snapshot.running_count > 0)))
    }

    pub(crate) fn toggle_pause(&mut self) -> bool {
        self.paused = !self.paused;
        self.redraw_pending = true;
        self.last_tick = Instant::now();
        self.paused
    }

    pub(crate) fn text(&self, language: NativeLanguage) -> String {
        if let Some(preparation) = &self.preparation {
            return preparation.clone();
        }
        self.ticker
            .rows()
            .iter()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|event| event.text(language))
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    pub(crate) fn brightness(&self, running: bool) -> f64 {
        if running && self.motion && !self.paused {
            execution_brightness(self.clock)
        } else {
            1.0
        }
    }

    /// 绘制区固定三行；只滚动生命周期短文案，绝不直接绘制远端输出。
    pub(crate) fn draw(
        &self,
        hdc: HDC,
        rect: RECT,
        font: HFONT,
        dpi: u32,
        language: NativeLanguage,
        dark: bool,
    ) {
        let line = i32::try_from((22 * dpi) / 96).unwrap_or(22).max(1);
        let saved = unsafe { SaveDC(hdc) };
        if saved == 0 {
            return;
        }
        unsafe {
            IntersectClipRect(hdc, rect.left, rect.top, rect.right, rect.bottom);
            SelectObject(hdc, font);
            SetBkMode(hdc, TRANSPARENT.cast_signed());
        }
        let rows = self.ticker.rows();
        let progress = if self.motion && !self.paused {
            self.ticker.progress(self.clock)
        } else {
            1.0
        };

        let mut overflow = false;
        for (index, event) in rows.iter().enumerate() {
            let label = if index + 1 == rows.len() {
                self.preparation
                    .clone()
                    .unwrap_or_else(|| event.text(language))
            } else {
                event.text(language)
            };
            let text: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
            let mut size = SIZE::default();
            unsafe {
                GetTextExtentPoint32W(
                    hdc,
                    text.as_ptr(),
                    i32::try_from(text.len().saturating_sub(1)).unwrap_or(i32::MAX),
                    &raw mut size,
                );
            }
            let excess = (size.cx - (rect.right - rect.left)).max(0);
            overflow |= excess > 0;
            let offset = if self.motion && !self.paused {
                marquee_offset(
                    self.clock - self.row_since,
                    f64::from(excess) * 96.0 / f64::from(dpi),
                ) * f64::from(dpi)
                    / 96.0
            } else {
                0.0
            };
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let mut row = RECT {
                left: rect.left - offset as i32,
                top: rect.top
                    + ((index as f64
                        + if rows.len() > 3 {
                            -progress
                        } else if index + 1 == rows.len() {
                            1.0 - progress
                        } else {
                            0.0
                        })
                        * f64::from(line)) as i32,
                right: rect.right + excess,
                bottom: rect.bottom + line,
            };
            row.bottom = row.top + line;
            unsafe {
                SetTextColor(hdc, event_color(event.kind, dark));
                DrawTextW(
                    hdc,
                    text.as_ptr(),
                    -1,
                    &raw mut row,
                    DT_SINGLELINE | DT_NOPREFIX,
                );
            }
        }
        self.overflow.set(overflow);
        unsafe {
            RestoreDC(hdc, saved);
        }
    }
}

pub(crate) fn animations_enabled() -> bool {
    let mut enabled = 0_i32;
    unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0) != 0
            && enabled != 0
    }
}

pub(crate) const fn event_color(kind: LifecycleKind, dark: bool) -> u32 {
    match (kind, dark) {
        (LifecycleKind::Connected | LifecycleKind::Succeeded, false) => rgb(18, 116, 54),
        (LifecycleKind::Connected | LifecycleKind::Succeeded, true) => rgb(90, 214, 140),
        (LifecycleKind::Disconnected | LifecycleKind::Failed, false) => rgb(176, 34, 52),
        (LifecycleKind::Disconnected | LifecycleKind::Failed, true) => rgb(255, 130, 143),
        (LifecycleKind::Running, false) => rgb(22, 88, 183),
        (LifecycleKind::Running, true) => rgb(112, 176, 255),
    }
}

const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

pub(crate) fn blend(color: u32, background: u32, brightness: f64) -> u32 {
    let mut mixed = 0;
    for shift in [0, 8, 16] {
        let foreground = f64::from((color >> shift) & 255);
        let background = f64::from((background >> shift) & 255);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let channel =
            (background + (foreground - background) * brightness.clamp(0.0, 1.0)).round() as u32;
        mixed |= channel << shift;
    }
    mixed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_changes_repaint_without_events_and_pause_resets_pulse_immediately() {
        let snapshot = ProductSnapshot::default();
        let mut view = ActivityView::default();
        assert!(view.tick_with_motion(&snapshot, true, true, 0.0));
        view.clock = 0.7;
        assert!(view.brightness(true) < 0.6);
        assert!(view.tick_with_motion(&snapshot, true, false, 0.0));
        assert!((view.brightness(true) - 1.0).abs() < f64::EPSILON);
        assert!(!view.tick_with_motion(&snapshot, true, false, 0.0));
        assert!(view.tick_with_motion(&snapshot, true, true, 0.0));
        assert!(view.toggle_pause());
        assert!((view.brightness(true) - 1.0).abs() < f64::EPSILON);
        let clock = view.clock;
        assert!(view.tick_with_motion(&snapshot, true, true, 0.2));
        assert!((view.clock - clock).abs() < f64::EPSILON);
        assert!(!view.tick_with_motion(&snapshot, true, true, 0.2));
    }

    #[test]
    fn hidden_motion_change_keeps_one_redraw_pending_until_visible_even_while_paused() {
        let mut view = ActivityView::default();
        let snapshot = ProductSnapshot::default();
        assert!(view.toggle_pause());
        assert!(!view.tick_with_motion(&snapshot, false, true, 0.2));
        assert!(!view.tick_with_motion(&snapshot, false, false, 0.2));
        assert!(view.tick_with_motion(&snapshot, true, false, 0.0));
        assert!(!view.tick_with_motion(&snapshot, true, false, 0.0));
    }
}
