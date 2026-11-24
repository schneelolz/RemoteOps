//! 窗口回归测试专用 ABI。提供一台虚拟设备和一帧图像，记录打开、结束及退出次数。
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering::SeqCst};

static DEVICES: AtomicBool = AtomicBool::new(true);
static FRAME_VIEW: AtomicI64 = AtomicI64::new(0);
static OPEN: AtomicU32 = AtomicU32::new(0);
static END: AtomicU32 = AtomicU32::new(0);
static STOP: AtomicU32 = AtomicU32::new(0);

#[no_mangle]
pub extern "C" fn ro_start_config(_: *const c_char) -> usize { 1 }
#[no_mangle]
pub extern "C" fn ro_event(_: usize) -> *const u8 {
    if DEVICES.swap(false, SeqCst) {
        concat!(r#"{"kind":"devices","items":[{"session":"test","device_id":"unit","name":"连接 1","online":true,"desktop":true,"writable":true}]}"#, "\0").as_ptr()
    } else { std::ptr::null() }
}
#[no_mangle]
pub extern "C" fn ro_free_string(_: *mut c_void) {}
#[no_mangle]
pub unsafe extern "C" fn ro_post(_: usize, json: *const c_char) -> i32 {
    let cmd = CStr::from_ptr(json).to_bytes();
    for (name, counter) in [(b"\"op\":\"open\"".as_slice(), &OPEN), (b"\"op\":\"end\"".as_slice(), &END)] {
        if cmd.windows(name.len()).any(|part| part == name) { counter.fetch_add(1, SeqCst); }
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn ro_frame_for(_: usize, view: i64, dst: *mut u8, cap: i32) -> i32 {
    if cap > 0 && FRAME_VIEW.swap(view, SeqCst) != view { dst.write(1); 1 } else { 0 }
}
#[no_mangle]
pub extern "C" fn ro_stop(_: usize) { STOP.fetch_add(1, SeqCst); }
#[no_mangle]
pub extern "C" fn ro_codec_new(_: i32, _: i32, _: i32, _: i32, _: i32) -> usize { 1 }
#[no_mangle]
pub extern "C" fn ro_codec_free(_: usize) {}
#[no_mangle]
pub unsafe extern "C" fn ro_decode(_: usize, _: *const u8, _: i32, dst: *mut u8, cap: i32, w: *mut i32, h: *mut i32) -> i32 {
    if cap < 16 { return -1; }
    w.write(2); h.write(2); std::ptr::write_bytes(dst, 80, 16);
    16
}
#[no_mangle]
pub extern "C" fn window_counts(which: u32) -> u32 {
    match which { 0 => OPEN.load(SeqCst), 1 => END.load(SeqCst), _ => STOP.load(SeqCst) }
}
