use std::{path::PathBuf, sync::Mutex, thread::JoinHandle, time::Duration};

use crate::{ResourceError, Result};

/// 只收集本运行时创建的临时目录清理；等待期间不持有任务锁。
#[derive(Default)]
pub(crate) struct CleanupWorkers {
    jobs: Mutex<Vec<JoinHandle<bool>>>,
}

impl CleanupWorkers {
    pub(crate) fn schedule(&self, path: PathBuf) {
        let task = std::thread::spawn(move || {
            for _ in 0..1200 {
                if std::fs::remove_dir_all(&path).is_ok() || !path.exists() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        });
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(task);
    }

    pub(crate) async fn finish(&self) -> Result<()> {
        let mut complete = true;
        loop {
            let jobs = std::mem::take(
                &mut *self
                    .jobs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            if jobs.is_empty() {
                break;
            }
            complete &= tokio::task::spawn_blocking(move || join(jobs))
                .await
                .unwrap_or(false);
        }
        if complete { Ok(()) } else { Err(pending()) }
    }
}

impl Drop for CleanupWorkers {
    fn drop(&mut self) {
        let jobs = std::mem::take(
            self.jobs
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let _ = join(jobs);
    }
}

fn join(jobs: Vec<JoinHandle<bool>>) -> bool {
    // 必须等待每个任务；首个失败后也不能跳过其余线程。
    let mut complete = true;
    for job in jobs {
        complete &= job.join().unwrap_or(false);
    }
    complete
}

pub(crate) fn pending() -> ResourceError {
    ResourceError {
        code: "cleanup_pending".into(),
        message: "资源进程已请求终止，但临时状态清理尚未完成；请核实本机占用".into(),
        retry_safe: false,
        diagnostics: None,
    }
}
