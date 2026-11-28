//! 剪贴板 Win32 调用只在专属线程执行；UI 只提交最新请求并轮询结果。
use std::{
    ptr::null_mut,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HGLOBAL, HWND},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
    },
    UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, HWND_MESSAGE, MSG, PM_REMOVE,
        PeekMessageW, TranslateMessage,
    },
};

const BUDGET: Duration = Duration::from_millis(1500);
const RETRY: Duration = Duration::from_millis(25);

struct Request {
    id: u64,
    text: String,
    deadline: Instant,
}
impl Request {
    fn current(&self, shared: &Shared) -> bool {
        !shared.stopping.load(Ordering::Acquire)
            && shared.generation.load(Ordering::Acquire) == self.id
            && Instant::now() < self.deadline
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Copied,
    Failed,
}
#[derive(Default)]
struct Shared {
    // 不在这些锁内调用 Win32；最多一个待办和一个完成结果。
    queued: Mutex<Option<Request>>,
    result: Mutex<Option<(u64, Outcome)>>,
    generation: AtomicU64,
    stopping: AtomicBool,
    unavailable: AtomicBool,
}

#[derive(Default)]
pub(crate) struct Clipboard {
    shared: Arc<Shared>,
    pending: Option<Request>,
    wake: Option<SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
    next_id: u64,
}
impl Clipboard {
    /// 同码重复点击合并，不延长原预算；新内容替换尚未执行的旧请求。
    pub(crate) fn request(&mut self, text: &str) -> Result<(), ()> {
        if self.shared.stopping.load(Ordering::Acquire)
            || self.shared.unavailable.load(Ordering::Acquire)
        {
            return Err(());
        }
        if self.worker.is_none() {
            let (wake, receiver) = mpsc::sync_channel(1);
            let shared = self.shared.clone();
            self.worker = Some(
                std::thread::Builder::new()
                    .name("remoteops-clipboard".into())
                    .spawn(move || worker(&shared, &receiver))
                    .map_err(|_| ())?,
            );
            self.wake = Some(wake);
        }
        self.enqueue(text);
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
        Ok(())
    }
    fn enqueue(&mut self, text: &str) {
        if self
            .pending
            .as_ref()
            .is_some_and(|request| request.text == text)
        {
            return;
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = self.next_id;
        let deadline = Instant::now() + BUDGET;
        self.shared.generation.store(id, Ordering::Release);
        *self
            .shared
            .queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Request {
            id,
            text: text.into(),
            deadline,
        });
        self.pending = Some(Request {
            id,
            text: text.into(),
            deadline,
        });
    }
    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }
    /// UI 自己执行截止判断，系统剪贴板调用卡住也不会让 UI 等待线程。
    pub(crate) fn poll(&mut self) -> Option<Outcome> {
        let pending = self.pending.as_ref()?;
        let result = self
            .shared
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((id, outcome)) = result
            && id == pending.id
        {
            self.pending = None;
            return Some(outcome);
        }
        if Instant::now() >= pending.deadline || self.shared.unavailable.load(Ordering::Acquire) {
            self.cancel_pending();
            return Some(Outcome::Failed);
        }
        None
    }
    pub(crate) fn cancel_unless(&mut self, text: Option<&str>) {
        if self
            .pending
            .as_ref()
            .is_some_and(|request| Some(request.text.as_str()) != text)
        {
            self.cancel_pending();
        }
    }
    fn cancel_pending(&mut self) {
        self.shared.generation.store(0, Ordering::Release);
        self.shared
            .queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.pending = None;
    }
    pub(crate) fn shutdown(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.cancel_pending();
        if let Some(wake) = self.wake.take() {
            let _ = wake.try_send(());
        }
        // UI 不 join 仍可能阻塞在系统内的线程。线程不访问 App/主窗指针，退出由共享取消和自有窗口收尾。
        // 不调用 join（即使 is_finished 为真也可能尚在系统线程清理阶段）。
        drop(self.worker.take());
    }
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Owner(HWND);
impl Owner {
    fn new() -> Option<Self> {
        let class = super::wide("STATIC");
        // 只创建本线程的 message-only 窗口，不使用主 UI HWND 或全局剪贴板所有者。
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                null_mut(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        (!hwnd.is_null()).then_some(Self(hwnd))
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}
fn pump_messages() {
    let mut message: MSG = unsafe { std::mem::zeroed() };
    // 自有窗口仅接收剪贴板生命周期消息；不持共享锁进入消息分发。
    while unsafe { PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
        unsafe {
            TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }
}
fn worker(shared: &Shared, wake: &Receiver<()>) {
    let Some(owner) = Owner::new() else {
        shared.unavailable.store(true, Ordering::Release);
        return;
    };
    while !shared.stopping.load(Ordering::Acquire) {
        pump_messages();
        let request = shared
            .queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(request) = request {
            let outcome = copy_with_retry(owner.0, &request, shared, wake);
            if shared.generation.load(Ordering::Acquire) == request.id
                && !shared.stopping.load(Ordering::Acquire)
            {
                *shared
                    .result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some((request.id, outcome));
            }
        } else {
            let _ = wake.recv_timeout(RETRY);
        }
    }
}
struct Memory(HGLOBAL);
impl Memory {
    fn new(text: &str) -> Option<Self> {
        let text = super::wide(text);
        let memory =
            Self(unsafe { GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(text.as_slice())) });
        if memory.0.is_null() {
            return None;
        }
        let target = unsafe { GlobalLock(memory.0) }.cast::<u16>();
        if target.is_null() {
            return None;
        }
        // GlobalAlloc 大小覆盖整个 NUL 结尾 UTF-16；成功后交给系统前只由此 guard 持有。
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), target, text.len());
            GlobalUnlock(memory.0);
        }
        Some(memory)
    }
    fn release(&mut self) {
        self.0 = null_mut();
    }
}
impl Drop for Memory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                GlobalFree(self.0);
            }
        }
    }
}
struct OpenGuard;
impl Drop for OpenGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}
fn copy_with_retry(
    owner: HWND,
    request: &Request,
    shared: &Shared,
    wake: &Receiver<()>,
) -> Outcome {
    let Some(mut memory) = Memory::new(&request.text) else {
        return Outcome::Failed;
    };
    while request.current(shared) {
        pump_messages();
        if unsafe { OpenClipboard(owner) } == 0 {
            let _ = wake.recv_timeout(RETRY);
            continue;
        }
        let _open = OpenGuard;
        if !request.current(shared) {
            return Outcome::Failed;
        }
        // EmptyClipboard 可同步通知旧 owner，不能取消已进入的系统调用；它始终在 worker。
        if unsafe { EmptyClipboard() } == 0 {
            return Outcome::Failed;
        }
        // 旧 owner 长时间不响应后才返回时，过期/被替换/退出的请求不得再写入旧内容。
        if !request.current(shared) {
            return Outcome::Failed;
        }
        if unsafe { SetClipboardData(13, memory.0) }.is_null() {
            return Outcome::Failed;
        }
        memory.release(); // 成功后 HGLOBAL 所有权归系统，不能 GlobalFree。
        return Outcome::Copied;
    }
    Outcome::Failed
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_requests_keep_deadline_and_latest_request_replaces_old_queue() {
        let mut clipboard = Clipboard::default();
        clipboard.enqueue("fixture-first");
        let first = clipboard.pending.as_ref().unwrap();
        let (id, deadline) = (first.id, first.deadline);
        clipboard.enqueue("fixture-first");
        assert_eq!(clipboard.pending.as_ref().unwrap().id, id);
        assert_eq!(clipboard.pending.as_ref().unwrap().deadline, deadline);
        clipboard.enqueue("fixture-second");
        assert_ne!(clipboard.pending.as_ref().unwrap().id, id);
        assert_eq!(
            clipboard
                .shared
                .queued
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .text,
            "fixture-second"
        );
    }
    #[test]
    fn expired_cancelled_and_shutdown_requests_cannot_commit_after_native_call_returns() {
        let mut clipboard = Clipboard::default();
        clipboard.enqueue("expired");
        let mut expired = clipboard.shared.queued.lock().unwrap().take().unwrap();
        assert!(expired.current(&clipboard.shared));
        expired.deadline = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap();
        assert!(!expired.current(&clipboard.shared));
        clipboard.enqueue("cancelled");
        let cancelled = clipboard.shared.queued.lock().unwrap().take().unwrap();
        assert!(cancelled.current(&clipboard.shared));
        clipboard.cancel_pending();
        assert!(!cancelled.current(&clipboard.shared));
        clipboard.enqueue("shutdown");
        let shutdown = clipboard.shared.queued.lock().unwrap().take().unwrap();
        assert!(shutdown.current(&clipboard.shared));
        clipboard.shutdown();
        assert!(!shutdown.current(&clipboard.shared));
        assert!(!clipboard.pending());
    }
    #[test]
    fn ui_timeout_does_not_wait_for_worker_and_old_completion_cannot_claim_success() {
        let mut clipboard = Clipboard::default();
        clipboard.enqueue("fixture");
        let pending = clipboard.pending.as_mut().unwrap();
        pending.deadline = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap();
        assert_eq!(clipboard.poll(), Some(Outcome::Failed));
        clipboard.enqueue("new");
        let id = clipboard.pending.as_ref().unwrap().id;
        *clipboard.shared.result.lock().unwrap() = Some((id - 1, Outcome::Copied));
        assert_eq!(clipboard.poll(), None);
        *clipboard.shared.result.lock().unwrap() = Some((id, Outcome::Copied));
        assert_eq!(clipboard.poll(), Some(Outcome::Copied));
    }
}
