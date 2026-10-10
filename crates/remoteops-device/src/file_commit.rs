//! Coordinates cancellation with the irreversible publication of a file.

use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

use crate::DeviceError;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    #[default]
    Preparing,
    Committing,
    Finished,
    Cancelled,
}

#[derive(Debug, Default)]
struct State {
    phase: Phase,
    active: bool,
}

#[derive(Debug, Default)]
struct Inner {
    state: Mutex<State>,
    idle: Notify,
}

/// A single file write's cancellation/commit boundary.
///
/// Cancellation can win while preparing bytes or verifying a hash. Once publication
/// starts, callers must await the real result instead of reporting cancellation.
#[derive(Clone, Debug, Default)]
pub struct FileCommitControl(Arc<Inner>);

impl FileCommitControl {
    /// Requests cancellation. False means publication has started or already finished.
    #[must_use]
    pub fn try_cancel(&self) -> bool {
        let Ok(mut state) = self.0.state.lock() else {
            return false;
        };
        match state.phase {
            Phase::Preparing | Phase::Cancelled => {
                state.phase = Phase::Cancelled;
                true
            }
            Phase::Committing | Phase::Finished => false,
        }
    }

    /// Waits until the registered blocking worker has finished cleaning up.
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.0.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.0.state.lock().is_ok_and(|state| !state.active) {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn worker(&self) -> Result<WorkerGuard, DeviceError> {
        let mut state = self.lock()?;
        if state.phase != Phase::Preparing || state.active {
            return Err(cancelled());
        }
        state.active = true;
        Ok(WorkerGuard(self.clone()))
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0
            .state
            .lock()
            .is_ok_and(|state| state.phase == Phase::Cancelled)
    }

    pub(crate) fn checkpoint(&self) -> Result<(), DeviceError> {
        if self.lock()?.phase == Phase::Preparing {
            Ok(())
        } else {
            Err(cancelled())
        }
    }

    pub(crate) fn begin_commit(&self) -> Result<(), DeviceError> {
        let mut state = self.lock()?;
        if state.phase != Phase::Preparing {
            return Err(cancelled());
        }
        state.phase = Phase::Committing;
        Ok(())
    }

    pub(crate) fn cancel_on_drop(&self) -> CancelOnDrop {
        CancelOnDrop(self.clone())
    }

    #[cfg(test)]
    pub(crate) fn worker_active(&self) -> bool {
        self.0.state.lock().unwrap().active
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, DeviceError> {
        self.0
            .state
            .lock()
            .map_err(|_| DeviceError::Operation("文件提交状态锁已损坏".to_owned()))
    }
}

fn cancelled() -> DeviceError {
    DeviceError::Operation("文件操作已取消或已结束".to_owned())
}

pub(crate) struct WorkerGuard(FileCommitControl);

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.0.state.lock() {
            state.active = false;
            if state.phase != Phase::Cancelled {
                state.phase = Phase::Finished;
            }
        }
        self.0.0.idle.notify_waiters();
    }
}

pub(crate) struct CancelOnDrop(FileCommitControl);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let _ = self.0.try_cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_wins_before_publication_and_waits_for_worker() {
        let control = FileCommitControl::default();
        let worker = control.worker().unwrap();
        assert!(control.try_cancel());
        assert!(control.begin_commit().is_err());
        assert!(control.checkpoint().is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), control.wait_idle())
                .await
                .is_err()
        );
        drop(worker);
        control.wait_idle().await;
    }

    #[test]
    fn publication_winner_cannot_be_reported_as_cancelled() {
        let control = FileCommitControl::default();
        let worker = control.worker().unwrap();
        control.begin_commit().unwrap();
        assert!(!control.try_cancel());
        drop(worker);
        assert!(!control.try_cancel());
    }

    #[test]
    fn dropped_async_owner_prevents_queued_worker_publication() {
        let control = FileCommitControl::default();
        let _worker = control.worker().unwrap();
        drop(control.cancel_on_drop());
        assert!(control.begin_commit().is_err());
    }
}
