//! 有效人工桌面租约使用的 Windows 系统剪贴板。OLE 由独立 STA 线程驱动。
use std::{ffi::c_void, ptr::NonNull};
unsafe extern "C" {
    fn ro_clip_new() -> *mut c_void;
    fn ro_clip_free(raw: *mut c_void);
    fn ro_clip_take(raw: *mut c_void, buf: *mut u8, cap: i32) -> i32;
    fn ro_clip_push(raw: *mut c_void, buf: *const u8, n: i32) -> i32;
}
/// 一个可撤销的双向剪贴板连接。
pub struct Clipboard {
    handle: NonNull<c_void>,
    buffer: Vec<u8>,
}
// 原生入口用互斥锁跨线程排队，所有 OLE 对象留在原生 STA 线程。
unsafe impl Send for Clipboard {}
impl Clipboard {
    /// 建立本次输入租约专用的剪贴板会话，只观察建立后发生的复制。
    pub fn open() -> Option<Self> {
        NonNull::new(unsafe { ro_clip_new() }).map(|handle| Self {
            handle,
            buffer: vec![0; 256 * 1024],
        })
    }
    /// 获取下一个本地事件；错误表示工作线程已经停止。
    pub fn next_packet(&mut self) -> Result<Option<&[u8]>, &'static str> {
        let n = unsafe {
            ro_clip_take(
                self.handle.as_ptr(),
                self.buffer.as_mut_ptr(),
                self.buffer.len() as i32,
            )
        };
        if n < 0 {
            return Err("本机剪贴板不可用");
        }
        Ok((n > 0).then(|| &self.buffer[..n as usize]))
    }
    /// 接收已通过身份、租约和摘要校验的对端事件。
    pub fn receive(&mut self, packet: &[u8]) -> bool {
        packet.len() <= self.buffer.len()
            && unsafe {
                ro_clip_push(self.handle.as_ptr(), packet.as_ptr(), packet.len() as i32) == 0
            }
    }
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe { ro_clip_free(self.handle.as_ptr()) }
    }
}
