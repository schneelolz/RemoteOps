use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// 工具准备阶段的实际进度，不提供虚构的总体百分比。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparationProgress {
    /// 固定工具标识。
    pub tool_id: String,
    /// 当前准备阶段。
    pub stage: String,
    /// 实际接收字节数。
    pub completed: u64,
    /// 声明的总字节数。
    pub total: Option<u64>,
    /// 当前下载平均速度。
    pub bytes_per_second: u64,
}

/// 每个启动请求独立的有界刷新回调。
#[derive(Clone)]
pub struct ProgressReporter {
    tool_id: String,
    sink: Arc<dyn Fn(PreparationProgress) + Send + Sync>,
    state: Arc<Mutex<(String, Instant, Instant)>>,
}

impl ProgressReporter {
    /// 创建请求独立的进度回调；回调不得阻塞下载。
    pub fn new(
        tool_id: String,
        sink: impl Fn(PreparationProgress) + Send + Sync + 'static,
    ) -> Self {
        let now = Instant::now();
        Self {
            tool_id,
            sink: Arc::new(sink),
            state: Arc::new(Mutex::new((String::new(), now, now))),
        }
    }

    /// 发布实际进度；同阶段最多每半秒刷新，阶段切换立即发布。
    pub fn report(&self, stage: &str, completed: u64, total: Option<u64>) {
        let now = Instant::now();
        let mut throttle = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if throttle.0 == stage
            && now.duration_since(throttle.1).as_millis() < 500
            && total != Some(completed)
        {
            return;
        }
        if throttle.0 != stage {
            throttle.2 = now;
        }
        let nanos = now.duration_since(throttle.2).as_nanos();
        let bytes_per_second = if stage == "download" && nanos > 0 {
            u64::try_from(u128::from(completed) * 1_000_000_000 / nanos).unwrap_or(u64::MAX)
        } else {
            0
        };
        throttle.0 = stage.into();
        throttle.1 = now;
        drop(throttle);
        (self.sink)(PreparationProgress {
            tool_id: self.tool_id.clone(),
            stage: stage.into(),
            completed,
            total,
            bytes_per_second,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn throttles_download_and_immediately_publishes_transitions_and_final_bytes() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let progress = ProgressReporter::new("browser".into(), move |event| {
            sink.lock().unwrap().push(event);
        });
        progress.report("download", 0, Some(100));
        progress.report("download", 1, Some(100));
        progress.report("download", 100, Some(100));
        progress.report("verify", 0, None);
        progress.report("extract", 0, None);
        progress.report("ready", 0, None);
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 5);
        assert_eq!(events[1].completed, 100);
        assert_eq!(events.last().unwrap().stage, "ready");
    }
}
