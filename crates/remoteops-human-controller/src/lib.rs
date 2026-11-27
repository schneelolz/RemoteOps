//! 人工控制端本地 ABI。调用者持有句柄至停止，单次调用期间缓冲区必须有效。
mod desktop;
mod session;
mod tools;
use std::{collections::VecDeque, ffi::{CStr,CString,c_char}, sync::{Arc,Mutex}, thread};
use remoteops_domain::{RequestId, SessionId};
use serde_json::Value;
use tokio::sync::{mpsc,oneshot,watch};

type ReleaseSignal = watch::Sender<Option<(SessionId,u64)>>;

type Events = std::sync::mpsc::SyncSender<Value>;
type Video = Arc<Mutex<VideoBuffer>>;
#[derive(Default)]
struct VideoBuffer {
    stream: Option<RequestId>,
    view: u64,
    received: u32,
    capture: Option<(u8, std::time::Instant)>,
    packets: VecDeque<Vec<u8>>,
}
impl VideoBuffer {
    fn reset(&mut self, stream: Option<RequestId>, view:u64) {
        self.view = view;
        self.stream = stream;
        self.packets.clear();
        self.received = 0;
        self.capture = None;
    }
}
struct Client {
    commands: mpsc::Sender<Value>,
    release: ReleaseSignal,
    events: std::sync::mpsc::Receiver<Value>,
    video: Video,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

fn message(kind:&str, text:&str) -> Value { serde_json::json!({"kind":kind,"text":text}) }

/// 构造本地人工连接；配置只在当前进程内使用。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ro_start_config(config:*const c_char) -> *mut std::ffi::c_void {
    let config = if config.is_null() { Value::Null } else {
        let bytes = unsafe { CStr::from_ptr(config) }.to_bytes();
        let Ok(value) = serde_json::from_slice(bytes) else { return std::ptr::null_mut() };
        value
    };
    let (tx,rx) = mpsc::channel(256);
    let (out,events) = std::sync::mpsc::sync_channel(512);
    let (stop,shutdown) = oneshot::channel();
    let video = Video::default();
    let buffers = video.clone();
    let (release, _) = watch::channel(None);
    let overflow = release.clone();
    let worker = thread::Builder::new().name("human-session".into()).spawn(move || {
        let result = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()
            .map_err(anyhow::Error::from).and_then(|rt| rt.block_on(session::run(config,rx,out.clone(),buffers,shutdown,overflow)));
        if let Err(error) = result {
            let _ = out.try_send(serde_json::json!({"kind":"connection","online":false,"retry":true,"text":error.to_string()}));
        }
    });
    let Ok(worker) = worker else { return std::ptr::null_mut() };
    Box::into_raw(Box::new(Client { commands:tx, release, events, video, stop:Some(stop), worker:Some(worker) })).cast()
}

/// 采用本机人工身份环境变量启动。
#[unsafe(no_mangle)]
pub extern "C" fn ro_start() -> *mut std::ffi::c_void { unsafe { ro_start_config(std::ptr::null()) } }

/// 停止并等待会话资源释放，调用者须序列化同一 handle 上的调用。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ro_stop(raw:*mut std::ffi::c_void) {
    if raw.is_null() { return }
    let mut handle = unsafe { Box::from_raw(raw.cast::<Client>()) };
    if let Some(stop) = handle.stop.take() { let _=stop.send(()); }
    if let Some(worker) = handle.worker.take() { let _=worker.join(); }
}

// 为关闭和断开保留队列空间。丢失任一输入时通过单独的信号交还整组键鼠。
fn post(commands:&mpsc::Sender<Value>, release:&ReleaseSignal, value:Value) -> bool {
    if commands.is_closed() { return false; }
    if matches!(value["op"].as_str(), Some("input" | "control" | "release")) && commands.capacity() <= 4 {
        let Ok(session) = serde_json::from_value(value["session"].clone()) else {return false;};
        let Some(view) = value["view"].as_u64() else {return false;};
        release.send_replace(Some((session,view)));
        return true;
    }
    commands.try_send(value).is_ok()
}

/// 排队一次来自界面的显式操作。输入拥塞时请求释放键鼠。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ro_post(raw:*mut std::ffi::c_void,json:*const c_char) -> i32 {
    if raw.is_null() || json.is_null(){ return -1 }
    let handle = unsafe { &*raw.cast::<Client>() };
    let data = unsafe { CStr::from_ptr(json) }.to_bytes();
    if data.len()>64*1024 { return -1 }
    let Ok(value) = serde_json::from_slice(data) else { return -1 };
    if post(&handle.commands, &handle.release, value) { 0 } else { -1 }
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn input_pressure_releases_only_its_view_and_reserves_space_for_close() {
        let (queue,mut rx)=mpsc::channel(5);
        let (release,mut notice)=watch::channel(None);
        let session=SessionId::new();
        let cmd=json!({"op":"input","session":session,"view":17,"epoch":2,"event":"Release"});
        assert!(post(&queue,&release,cmd.clone()));
        assert!(post(&queue,&release,cmd.clone()));
        assert!(notice.has_changed().unwrap());
        assert_eq!(*notice.borrow_and_update(),Some((session,17)));
        let close=json!({"op":"end","session":session,"view":17});
        assert!(post(&queue,&release,close.clone()));
        assert_eq!(rx.try_recv().unwrap(),cmd);
        assert_eq!(rx.try_recv().unwrap(),close);
        assert!(rx.try_recv().is_err());
    }
}

/// 返回单个状态通知，用 ro_free_string 释放。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ro_event(raw:*mut std::ffi::c_void) -> *mut c_char {
    if raw.is_null() { return std::ptr::null_mut() }
    let handle=unsafe { &*raw.cast::<Client>() };
    handle.events.try_recv().ok().and_then(|v| CString::new(v.to_string()).ok())
        .map_or(std::ptr::null_mut(), CString::into_raw)
}
/// 释放通知的 UTF-8 缓冲区。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ro_free_string(p:*mut c_char) {
    if !p.is_null() { drop(unsafe { CString::from_raw(p) }); }
}
/// 输出一个完整 H.264 访问单元；缓冲不足时保留原帧。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ro_frame(raw:*mut std::ffi::c_void,dst:*mut u8,cap:i32) -> i32 {
    unsafe { ro_frame_for(raw,0,dst,cap) }
}

/// 只读当前观看窗口创建的流，切换设备时两个解码线程不会读到彼此的帧。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ro_frame_for(raw:*mut std::ffi::c_void, view:u64, dst:*mut u8,cap:i32) -> i32 {
    if raw.is_null() || dst.is_null() || cap<0 { return -1 }
    let handle = unsafe { &*raw.cast::<Client>() };
    let Ok(mut video)=handle.video.lock() else { return -1 };
    if video.view != view { return 0 }
    let Some(packet)=video.packets.front() else { return 0 };
    if packet.len()>cap as usize { return -1 }
    let size=packet.len();
    unsafe { std::ptr::copy_nonoverlapping(packet.as_ptr(), dst,size) };
    video.packets.pop_front();
    size as i32
}
