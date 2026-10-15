//! 限制失效画面表面的恢复频率，避免底层立即重绘形成忙循环。

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use eframe::{egui_wgpu::SurfaceErrorAction, wgpu::CurrentSurfaceTexture};

use crate::StartupDiagnostics;

/// 与主界面的定时唤醒一致，跳过立即重试后仍能自动恢复。
pub(super) const REPAINT_INTERVAL: Duration = Duration::from_secs(1);
const REPORT_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Default)]
struct SurfaceRecovery {
    last_retry: Option<Instant>,
    last_report: Option<Instant>,
    suppressed: u64,
}

impl SurfaceRecovery {
    fn allow_retry(&mut self, now: Instant) -> bool {
        if self
            .last_retry
            .is_some_and(|last_retry| now.duration_since(last_retry) < REPAINT_INTERVAL)
        {
            self.suppressed = self.suppressed.saturating_add(1);
            return false;
        }
        self.last_retry = Some(now);
        true
    }

    fn report(&mut self, now: Instant) -> Option<u64> {
        if self
            .last_report
            .is_some_and(|last_report| now.duration_since(last_report) < REPORT_INTERVAL)
        {
            return None;
        }
        self.last_report = Some(now);
        Some(std::mem::take(&mut self.suppressed))
    }
}

/// 保留框架恢复动作，只抑制冷却期内的重复恢复，不阻塞 UI 或 Agent 线程。
pub(super) fn configure_surface_recovery(
    options: &mut eframe::NativeOptions,
    diagnostics: StartupDiagnostics,
) {
    let on_surface_status = options.wgpu_options.on_surface_status.clone();
    let recovery = Mutex::new(SurfaceRecovery::default());
    options.wgpu_options.on_surface_status = Arc::new(move |status| {
        if matches!(
            status,
            CurrentSurfaceTexture::Lost | CurrentSurfaceTexture::Outdated
        ) {
            let now = Instant::now();
            let mut recovery = recovery
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !recovery.allow_retry(now) {
                // App::ui 的定时唤醒负责下次尝试；不能在这里立即请求重绘。
                return SurfaceErrorAction::SkipFrame;
            }
            if let Some(suppressed) = recovery.report(now) {
                diagnostics.record_detail(
                    "wgpu_surface_recovery",
                    format!("status={status:?} suppressed_since_last_report={suppressed}"),
                );
            }
        }
        on_surface_status(status)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_failures_are_bounded_and_do_not_postpone_recovery() {
        let mut recovery = SurfaceRecovery::default();
        let now = Instant::now();
        assert!(recovery.allow_retry(now));
        for milliseconds in 0..1_000 {
            assert!(!recovery.allow_retry(now + Duration::from_millis(milliseconds)));
        }
        assert!(recovery.allow_retry(now + REPAINT_INTERVAL));
        assert!(!recovery.allow_retry(now + REPAINT_INTERVAL));
        assert!(recovery.allow_retry(now + REPAINT_INTERVAL * 2));
    }

    #[test]
    fn reports_are_bounded_and_accumulate_suppressed_attempts() {
        let mut recovery = SurfaceRecovery::default();
        let now = Instant::now();
        assert!(recovery.allow_retry(now));
        assert_eq!(recovery.report(now), Some(0));
        for seconds in 1..30 {
            let now = now + Duration::from_secs(seconds);
            assert!(recovery.allow_retry(now));
            assert!(!recovery.allow_retry(now));
            assert_eq!(recovery.report(now), None);
        }
        assert_eq!(recovery.report(now + REPORT_INTERVAL), Some(29));
        assert_eq!(recovery.report(now + REPORT_INTERVAL * 2), Some(0));
    }

    #[test]
    fn callback_preserves_recovery_actions_and_shares_the_retry_budget() {
        for (status, expected) in [
            (
                CurrentSurfaceTexture::Lost,
                SurfaceErrorAction::RecreateSurface,
            ),
            (
                CurrentSurfaceTexture::Outdated,
                SurfaceErrorAction::Reconfigure,
            ),
        ] {
            let mut options = eframe::NativeOptions::default();
            configure_surface_recovery(&mut options, StartupDiagnostics { path: None });
            let callback = options.wgpu_options.on_surface_status;
            assert!(
                std::mem::discriminant(&callback(&status)) == std::mem::discriminant(&expected)
            );
            assert!(matches!(
                callback(&CurrentSurfaceTexture::Lost),
                SurfaceErrorAction::SkipFrame
            ));
            assert!(matches!(
                callback(&CurrentSurfaceTexture::Outdated),
                SurfaceErrorAction::SkipFrame
            ));
            assert!(matches!(
                callback(&CurrentSurfaceTexture::Occluded),
                SurfaceErrorAction::SkipFrame
            ));
        }
    }
}
