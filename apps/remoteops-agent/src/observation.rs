//! 有界的本地表现层订阅；不影响远程线协议。
use crate::AgentEvent;
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, mpsc::TryRecvError},
    time::Duration,
};
use tokio::sync::watch;

struct State {
    events: VecDeque<AgentEvent>,
    closed: bool,
    dropped: u64,
    senders: usize,
    shutdown: Option<watch::Receiver<bool>>,
}
struct Shared {
    state: Mutex<State>,
    available: Condvar,
    capacity: usize,
}

/// 有界表现层事件发布端；输出满时淘汰日志，关键事件保留次序。
pub struct BoundedAgentEventSender {
    shared: Arc<Shared>,
    // 宿主可按事件唤醒界面；回调只在队列锁释放后调用。
    wakeup: Option<Arc<dyn Fn() + Send + Sync>>,
}
/// 供单个表现层排空的有界事件订阅。
pub struct BoundedAgentEventReceiver {
    shared: Arc<Shared>,
}

/// 创建有界的本地观察订阅，容量至少为一，不创建后台线程。
#[must_use]
pub fn bounded_agent_events(
    capacity: usize,
) -> (BoundedAgentEventSender, BoundedAgentEventReceiver) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            events: VecDeque::new(),
            closed: false,
            dropped: 0,
            senders: 1,
            shutdown: None,
        }),
        available: Condvar::new(),
        capacity: capacity.clamp(1, 8192),
    });
    (
        BoundedAgentEventSender {
            shared: shared.clone(),
            wakeup: None,
        },
        BoundedAgentEventReceiver { shared },
    )
}
fn is_log(event: &AgentEvent) -> bool {
    match event {
        AgentEvent::OperationLog(_)
        | AgentEvent::OperationPresentation { .. }
        | AgentEvent::OperationOutputUpdate { .. } => true,
        AgentEvent::ScopedOperation { event, .. } => is_log(event),
        _ => false,
    }
}
// 相同稳定记录只保留最新累计值；诊断日志与观察显示分别更新，连接代次不能混合。
fn update_key(event: &AgentEvent) -> Option<(Option<u64>, bool, u64)> {
    match event {
        AgentEvent::OperationLog(log) => log.repetition.map(|value| (None, false, value.record_id)),
        AgentEvent::OperationOutputUpdate { repetition, .. } => {
            Some((None, true, repetition.record_id))
        }
        AgentEvent::ScopedOperation { epoch, event } => {
            update_key(event).map(|(_, kind, id)| (Some(*epoch), kind, id))
        }
        _ => None,
    }
}

impl Clone for BoundedAgentEventSender {
    fn clone(&self) -> Self {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .senders += 1;
        Self {
            shared: self.shared.clone(),
            wakeup: self.wakeup.clone(),
        }
    }
}
impl Drop for BoundedAgentEventSender {
    fn drop(&mut self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .senders -= 1;
        self.shared.available.notify_all();
    }
}
impl BoundedAgentEventSender {
    pub(crate) fn attach_shutdown(&self, shutdown: watch::Receiver<bool>) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutdown = Some(shutdown);
    }
    /// 为此发布端及其后续克隆绑定界面唤醒回调，不另建无界事件转发通道。
    #[must_use]
    pub fn with_wakeup(mut self, wakeup: impl Fn() + Send + Sync + 'static) -> Self {
        self.wakeup = Some(Arc::new(wakeup));
        self
    }

    /// 发布本地事件；关键状态会背压，宿主退出前应关闭订阅解除等待。
    pub fn send(&self, event: AgentEvent) {
        self.enqueue(event);
        if let Some(wakeup) = &self.wakeup {
            wakeup();
        }
    }

    fn enqueue(&self, event: AgentEvent) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if state.closed
                || state
                    .shutdown
                    .as_ref()
                    .is_some_and(|shutdown| *shutdown.borrow())
            {
                return;
            }
            if let Some(key) = update_key(&event)
                && let Some(existing) = state
                    .events
                    .iter_mut()
                    .find(|queued| update_key(queued) == Some(key))
            {
                *existing = event;
                return;
            }
            if state.events.len() < self.shared.capacity {
                state.events.push_back(event);
                return;
            }
            if let Some(index) = state.events.iter().position(is_log) {
                state.events.remove(index);
                state.dropped = state.dropped.saturating_add(1);
                continue;
            }
            if is_log(&event) {
                state.dropped = state.dropped.saturating_add(1);
                return;
            }
            // 只对关键事件背压；关闭订阅或请求停止都能解除等待。
            state = self
                .shared
                .available
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
    /// 停止观察并解除关键事件背压；宿主应在等待 Agent 停止前调用。
    pub fn close(&self) {
        close(&self.shared);
    }
}
impl BoundedAgentEventReceiver {
    /// 非阻塞地按原始次序读取下一条本地事件。
    ///
    /// # Errors
    /// 暂无事件时返回 Empty，订阅关闭且队列排空时返回 Disconnected。
    pub fn try_recv(&self) -> Result<AgentEvent, TryRecvError> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(event) = state.events.pop_front() {
            self.shared.available.notify_all();
            return Ok(event);
        }
        if state.closed || state.senders == 0 {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }
    /// 输出队列满时已淘汰的日志条数，关键状态不计入此数。
    #[must_use]
    pub fn dropped_log_lines(&self) -> u64 {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .dropped
    }
    /// 停止观察并解除发送等待，不影响 Agent 的远程操作。
    pub fn close(&self) {
        close(&self.shared);
    }
}
impl Drop for BoundedAgentEventReceiver {
    fn drop(&mut self) {
        self.close();
    }
}
fn close(shared: &Shared) {
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.closed = true;
    state.events.clear();
    shared.available.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentLogLevel, AgentOperationLog};
    fn line(index: usize) -> AgentEvent {
        AgentEvent::OperationLog(AgentOperationLog {
            repetition: None,
            request_id: None,
            occurred_at: chrono::Utc::now(),
            level: AgentLogLevel::Info,
            message: index.to_string(),
        })
    }
    #[test]
    fn wakeup_runs_outside_queue_lock_and_survives_sender_clone() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (sender, receiver) = bounded_agent_events(4);
        let receiver = Arc::new(receiver);
        let wakeups = Arc::new(AtomicUsize::new(0));
        let callback_receiver = receiver.clone();
        let callback_wakeups = wakeups.clone();
        let sender = sender.with_wakeup(move || {
            assert!(callback_receiver.try_recv().is_ok());
            callback_wakeups.fetch_add(1, Ordering::Relaxed);
        });
        sender.send(line(1));
        sender.clone().send(line(2));
        assert_eq!(wakeups.load(Ordering::Relaxed), 2);
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }
    #[test]
    fn queue_is_bounded_and_keeps_critical_states() {
        let (sender, receiver) = bounded_agent_events(4);
        sender.send(AgentEvent::Connecting);
        for index in 0..1000 {
            sender.send(line(index));
        }
        sender.send(AgentEvent::Stopped);
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        assert_eq!(events.len(), 4);
        assert!(matches!(events.first(), Some(AgentEvent::Connecting)));
        assert!(matches!(events.last(), Some(AgentEvent::Stopped)));
        assert_eq!(receiver.dropped_log_lines(), 998);
    }
    #[test]
    fn terminal_presentation_can_be_trimmed_without_losing_true_lifecycle() {
        use crate::{AgentOperationState, AgentPresentationRole};
        let (sender, receiver) = bounded_agent_events(2);
        let request_id = remoteops_domain::RequestId::new();
        sender.send(AgentEvent::OperationStateChanged {
            request_id,
            state: AgentOperationState::Running,
        });
        for index in 0..3 {
            sender.send(AgentEvent::OperationPresentation {
                request_id,
                role: AgentPresentationRole::Output,
                text: index.to_string(),
                terminal: false,
            });
        }
        sender.send(AgentEvent::OperationStateChanged {
            request_id,
            state: AgentOperationState::Succeeded,
        });
        assert!(matches!(
            receiver.try_recv().unwrap(),
            AgentEvent::OperationStateChanged {
                state: AgentOperationState::Running,
                ..
            }
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            AgentEvent::OperationStateChanged {
                state: AgentOperationState::Succeeded,
                ..
            }
        ));
        assert_eq!(receiver.dropped_log_lines(), 3);
    }

    #[test]
    fn dropping_receiver_releases_critical_sender() {
        let (sender, receiver) = bounded_agent_events(1);
        sender.send(AgentEvent::Connecting);
        let (done_sender, done_receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            sender.send(AgentEvent::Stopped);
            done_sender.send(()).unwrap();
        });
        drop(receiver);
        done_receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
    }
    #[test]
    fn shutdown_releases_critical_sender_without_consumer() {
        let (sender, _receiver) = bounded_agent_events(1);
        let (stop, shutdown) = watch::channel(false);
        sender.attach_shutdown(shutdown);
        sender.send(AgentEvent::Connecting);
        let (done_sender, done_receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            sender.send(AgentEvent::Stopped);
            done_sender.send(()).unwrap();
        });
        stop.send(true).unwrap();
        done_receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
    }
    #[test]
    fn publisher_close_releases_waiter_and_disconnects() {
        let (sender, receiver) = bounded_agent_events(1);
        sender.send(AgentEvent::Connecting);
        let pending = sender.clone();
        let thread = std::thread::spawn(move || pending.send(AgentEvent::Stopped));
        sender.close();
        thread.join().unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }
    #[test]
    fn scoped_events_keep_epoch_and_order() {
        let (sender, receiver) = bounded_agent_events(2);
        sender.send(AgentEvent::ConnectionEpoch(7));
        sender.send(AgentEvent::ScopedOperation {
            epoch: 7,
            event: Box::new(line(1)),
        });
        assert!(matches!(
            receiver.try_recv().unwrap(),
            AgentEvent::ConnectionEpoch(7)
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            AgentEvent::ScopedOperation { epoch: 7, .. }
        ));
    }
}
