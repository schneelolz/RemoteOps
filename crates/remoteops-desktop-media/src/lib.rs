//! Windows 桌面媒体对象只由创建它的工作线程驱动。
#![cfg(windows)]
pub mod clipboard;
pub mod minirtc;
use std::{ffi::c_void, marker::PhantomData, ptr::NonNull, rc::Rc};

unsafe extern "C" {
    fn ro_codec_new(encode: i32, w: i32, h: i32, fps: i32, rate: i32) -> *mut c_void;
    fn ro_codec_free(p: *mut c_void);
    fn ro_codec_startup_error() -> i32;
    fn ro_codec_hardware(p: *mut c_void) -> i32;
    fn ro_encode(p: *mut c_void, img: *const u8, stride: i32, out: *mut u8, cap: i32) -> i32;
    fn ro_decode(
        p: *mut c_void,
        data: *const u8,
        n: i32,
        out: *mut u8,
        cap: i32,
        w: *mut i32,
        h: *mut i32,
    ) -> i32;
    fn ro_host_new() -> *mut c_void;
    fn ro_host_new_paired() -> *mut c_void;
    fn ro_host_select_crossdesk(p: *mut c_void, path: *const u16) -> i32;
    fn ro_host_free(p: *mut c_void);
    fn ro_host_poll(p: *mut c_void) -> i32;
    fn ro_host_stop_reason(p: *mut c_void) -> i32;
    fn ro_host_is_rdp(p: *mut c_void) -> i32;
    fn ro_host_request_input(p: *mut c_void, enabled: i32) -> i32;
    fn ro_host_frame(
        p: *mut c_void,
        img: *mut u8,
        cap: i32,
        mw: i32,
        mh: i32,
        w: *mut i32,
        h: *mut i32,
        pw: *mut i32,
        ph: *mut i32,
    ) -> i32;
    fn ro_host_input(p: *mut c_void, kind: i32, a: i32, b: i32) -> i32;
    fn ro_view_new(title: *const u16) -> *mut c_void;
    fn ro_view_poll(p: *mut c_void, kind: *mut i32, a: *mut i32, b: *mut i32, c: *mut i32) -> i32;
    fn ro_view_present(p: *mut c_void, img: *const u8, w: i32, h: i32);
    fn ro_view_status(p: *mut c_void, label: *const u16, control: i32);
    fn ro_view_free(p: *mut c_void);
}

struct Handle {
    p: NonNull<c_void>,
    free: unsafe extern "C" fn(*mut c_void),
    _thread: PhantomData<Rc<()>>,
}
impl Handle {
    fn new(p: *mut c_void, free: unsafe extern "C" fn(*mut c_void)) -> Result<Self, &'static str> {
        Ok(Self {
            p: NonNull::new(p).ok_or("Windows 桌面组件初始化失败")?,
            free,
            _thread: PhantomData,
        })
    }
    fn ptr(&self) -> *mut c_void {
        self.p.as_ptr()
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { (self.free)(self.ptr()) }
    }
}
const MAX_WIDTH: i32 = 1920;
const MAX_HEIGHT: i32 = 1080;
fn image_bytes(w: i32, h: i32) -> Option<usize> {
    if !(1..=MAX_WIDTH).contains(&w) || !(1..=MAX_HEIGHT).contains(&h) {
        return None;
    }
    Some(w as usize * h as usize * 4)
}
/// 专用线程上的 H.264 编码器。
pub struct Encoder {
    handle: Handle,
    width: i32,
    height: i32,
}
impl Encoder {
    /// 当前实际编码器是否来自硬件转换器枚举。
    pub fn is_hardware(&self) -> bool {
        unsafe { ro_codec_hardware(self.handle.ptr()) != 0 }
    }

    /// 为传输画面建立低延迟编码器。
    pub fn new(w: i32, h: i32, fps: i32, bitrate: i32) -> Result<Self, &'static str> {
        image_bytes(w, h).ok_or("编码尺寸无效")?;
        let handle = Handle::new(
            unsafe { ro_codec_new(1, w, h, fps, bitrate) },
            ro_codec_free,
        )?;
        Ok(Self {
            handle,
            width: w,
            height: h,
        })
    }
    /// 编码紧密排列的 BGRA 图像，0 表示编码器暂未产生数据。
    pub fn encode(&mut self, bgra: &[u8], out: &mut [u8]) -> Result<usize, &'static str> {
        if image_bytes(self.width, self.height) != Some(bgra.len()) {
            return Err("BGRA 输入长度错误");
        }
        let n = unsafe {
            ro_encode(
                self.handle.ptr(),
                bgra.as_ptr(),
                self.width * 4,
                out.as_mut_ptr(),
                i32::try_from(out.len()).map_err(|_| "编码缓冲过大")?,
            )
        };
        usize::try_from(n)
            .ok()
            .filter(|&n| n <= out.len())
            .ok_or("H.264 编码失败")
    }
}
/// 专用线程上的 H.264 解码器。
pub struct Decoder(Handle);
impl Decoder {
    /// 建立最高 1080p 的解码器。
    pub fn new() -> Result<Self, String> {
        Ok(Self(
            Handle::new(
                unsafe { ro_codec_new(0, MAX_WIDTH, MAX_HEIGHT, 30, 6_000_000) },
                ro_codec_free,
            )
            .map_err(|_| {
                format!("H.264 解码器初始化失败（HRESULT 0x{:08X}）", unsafe {
                    ro_codec_startup_error()
                })
            })?,
        ))
    }
    /// 返回完成解码的 BGRA 尺寸，或等待更多输入。
    pub fn decode(
        &mut self,
        data: &[u8],
        out: &mut [u8],
    ) -> Result<Option<(i32, i32)>, &'static str> {
        let (mut w, mut h) = (0, 0);
        let n = unsafe {
            ro_decode(
                self.0.ptr(),
                data.as_ptr(),
                i32::try_from(data.len()).map_err(|_| "码流过大")?,
                out.as_mut_ptr(),
                i32::try_from(out.len()).map_err(|_| "画布过大")?,
                &mut w,
                &mut h,
            )
        };
        if n == 0 {
            return Ok(None);
        }
        if n < 0 || image_bytes(w, h) != Some(n as usize) || n as usize > out.len() {
            return Err("视频解码失败");
        }
        Ok(Some((w, h)))
    }
}
/// 现场交互桌面与用户可停止的共享窗口。
pub struct Host(Handle);
impl Host {
    fn from_native(p: *mut c_void) -> Result<Self, &'static str> {
        Handle::new(p, ro_host_free)
            .map(Self)
            .map_err(|_| Self::reason(std::ptr::null_mut()))
    }

    fn reason(p: *mut c_void) -> &'static str {
        // 空指针读取当前线程刚才创建采集组件时保留的首个错误。
        match unsafe { ro_host_stop_reason(p) } {
            1 => "现场人员已停止共享",
            3 => "Windows 会话已锁定、切换或注销",
            4 => "无法设置采集线程的桌面 DPI 环境",
            5 => "Agent 会话与当前活动控制台会话不一致",
            6 => "Agent 所在窗口站不是可见的 WinSta0",
            7 => "采集线程不在 Default 桌面或无法读取桌面名称",
            8 => "无法读取当前 Windows 会话的扩展状态",
            9 => "Windows 会话未激活或没有登录用户",
            10 => "Windows 会话状态未报告为已解锁",
            11 => "无法确认 Agent 与本会话桌面外壳属于同一用户",
            12 => "当前输入桌面不可访问或不是 Default 桌面",
            13 => "无法读取有效的主显示器和工作区布局",
            14 => "现场共享窗口不可见或已最小化",
            15 => "共享期间收到显示器变更或检测到主屏布局改变",
            16 => "无法按当前工作区和 DPI 定位共享窗口",
            17 => "现场停止按钮不可用或超出窗口范围",
            18 => "采集线程收到 Windows 退出消息 WM_QUIT",
            19 => "现场共享窗口收到 Windows 销毁消息 WM_NCDESTROY",
            20 => "无法更新现场共享窗口的状态文字",
            21 => "无法读取 Agent 的 Windows 会话或进程令牌",
            22 => "无法创建现场共享窗口或控件",
            23 => "无法为现场共享窗口注册 Windows 会话通知",
            24 => "采集线程的 Windows COM 多线程单元初始化失败",
            25 => "无法分配桌面采集组件",
            26 => "采集组件被非创建线程访问",
            27 => "现场停止按钮被其他窗口遮挡",
            28 => "无法确认现场共享窗口可见、置顶且位于主屏",
            29 => "本机已释放桌面采集组件",
            30 => "无法读取 Agent 当前 Windows 会话的连接协议",
            31 => "Agent 所在会话既非活动控制台，也未报告 RDP 协议",
            32 => "Agent 所在的 RDP 连接已断开，桌面共享已停止",
            33 => "Windows 服务会话没有可共享的交互桌面",
            _ => "桌面采集组件未提供停止原因",
        }
    }

    /// 在当前交互 Session 建立现场确认窗口。
    pub fn open() -> Result<Self, &'static str> {
        Self::from_native(unsafe { ro_host_new() })
    }
    /// 已配对的人工桌面从观察开始，键鼠模式由有效会话授权决定。
    pub fn open_paired() -> Result<Self, &'static str> {
        let mut host = Self::from_native(unsafe { ro_host_new_paired() })?;
        if std::env::var_os("REMOTEOPS_CROSSDESK_CAPTURE").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            host.select_crossdesk_capture()?;
        }
        Ok(host)
    }
    /// 在现场会话内校验并启用随包交付的 CrossDesk 采集组件。
    pub fn select_crossdesk_capture(&mut self) -> Result<(), &'static str> {
        let expected = option_env!("REMOTEOPS_CROSSDESK_CAPTURE_SHA256")
            .ok_or("当前构建未包含 CrossDesk 采集组件")?;
        use sha2::{Digest, Sha256};
        use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt};
        let path = std::env::current_exe()
            .map_err(|_| "无法定位 CrossDesk 采集组件")?
            .with_file_name("remoteops-crossdesk-native.dll");
        let mut image = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .map_err(|_| "定制包缺少 CrossDesk 采集组件")?;
        let mut hash = Sha256::new();
        std::io::copy(&mut image, &mut hash).map_err(|_| "无法读取 CrossDesk 采集组件")?;
        if !expected.eq_ignore_ascii_case(&format!("{:x}", hash.finalize())) {
            return Err("CrossDesk 采集组件与定制包的构建摘要不匹配");
        }
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain([0])
            .collect::<Vec<_>>();
        if unsafe { ro_host_select_crossdesk(self.0.ptr(), wide.as_ptr()) } == 0 {
            return Err("CrossDesk 无法在当前会话初始化采集");
        }
        Ok(())
    }
    /// 返回本次共享是否绑定到 Agent 自身的 RDP 会话。
    pub fn is_rdp(&self) -> bool {
        unsafe { ro_host_is_rdp(self.0.ptr()) != 0 }
    }
    /// 泵取现场窗口并读取观察、控制或停止状态。
    pub fn poll(&mut self) -> i32 {
        unsafe { ro_host_poll(self.0.ptr()) }
    }
    /// 返回本地共享已停止的原因，供现场日志显示。
    pub fn stop_reason(&self) -> &'static str {
        Self::reason(self.0.ptr())
    }
    /// 请求现场批准输入，或立即释放输入。
    pub fn request_control(&mut self, enabled: bool) -> bool {
        unsafe { ro_host_request_input(self.0.ptr(), i32::from(enabled)) >= 0 }
    }
    /// 在本地授权范围内采集主屏；返回编码尺寸和主屏物理尺寸。
    pub fn capture(
        &mut self,
        img: &mut [u8],
        mw: i32,
        mh: i32,
    ) -> Result<Option<(i32, i32, i32, i32)>, &'static str> {
        let (mut w, mut h, mut pw, mut ph) = (0, 0, 0, 0);
        let n = unsafe {
            ro_host_frame(
                self.0.ptr(),
                img.as_mut_ptr(),
                i32::try_from(img.len()).map_err(|_| "采集缓冲过大")?,
                mw,
                mh,
                &mut w,
                &mut h,
                &mut pw,
                &mut ph,
            )
        };
        if n == 0 {
            return Ok(None);
        }
        if n < 0 || image_bytes(w, h).is_none_or(|n| n > img.len()) || pw <= 0 || ph <= 0 {
            return Err("主屏采集失效");
        }
        Ok(Some((w, h, pw, ph)))
    }
    /// 由受信任的人工输入租约调用；原生层逐次复核现场状态。
    pub fn input(&mut self, kind: i32, a: i32, b: i32) -> bool {
        unsafe { ro_host_input(self.0.ptr(), kind, a, b) >= 0 }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
/// 独立的控制端本机窗口。
pub struct Viewer(Handle);
impl Viewer {
    /// 先在新线程初始化 MTA，再创建可能隐式初始化 COM 的 Win32 控件。
    pub fn with_decoder(title: &str) -> Result<(Self, Decoder), String> {
        let decoder = Decoder::new()?;
        let viewer = Self::open(title).map_err(|_| "无法创建 Windows 桌面观看窗口".to_owned())?;
        Ok((viewer, decoder))
    }
    /// 创建绑定目标标题的原生窗口。
    pub fn open(title: &str) -> Result<Self, &'static str> {
        Ok(Self(Handle::new(
            unsafe { ro_view_new(wide(title).as_ptr()) },
            ro_view_free,
        )?))
    }
    /// 返回窗口操作；窗口关闭时返回错误。
    pub fn poll(&mut self) -> Result<Option<[i32; 4]>, &'static str> {
        let (mut k, mut a, mut b, mut c) = (0, 0, 0, 0);
        match unsafe { ro_view_poll(self.0.ptr(), &mut k, &mut a, &mut b, &mut c) } {
            0 => Ok(None),
            1 => Ok(Some([k, a, b, c])),
            _ => Err("桌面窗口已关闭"),
        }
    }
    /// 刷新解码画面。
    pub fn present(&mut self, img: &[u8], w: i32, h: i32) {
        if image_bytes(w, h).is_some_and(|n| n <= img.len()) {
            unsafe { ro_view_present(self.0.ptr(), img.as_ptr(), w, h) }
        }
    }
    /// 只在远端确认后启用键鼠区域。
    pub fn status(&mut self, label: &str, control: bool) {
        unsafe { ro_view_status(self.0.ptr(), wide(label).as_ptr(), i32::from(control)) }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_keeps_creation_failure_on_the_calling_thread() {
        #[link(name = "ole32")]
        unsafe extern "system" {
            fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
            fn CoUninitialize();
        }
        std::thread::spawn(|| {
            // 独立 STA 线程使采集端的 MTA 初始化确定失败。
            assert!(unsafe { CoInitializeEx(std::ptr::null_mut(), 2) } >= 0);
            let failure = Host::open_paired().err();
            unsafe { CoUninitialize() };
            assert_eq!(failure, Some("采集线程的 Windows COM 多线程单元初始化失败"));
        })
        .join()
        .unwrap();
    }

    #[test]
    #[ignore = "需要已解锁的 Windows 交互桌面"]
    fn quit_message_preserves_the_first_stop_reason() {
        unsafe extern "system" {
            fn PostQuitMessage(code: i32);
        }
        std::thread::spawn(|| {
            let mut host = Host::open_paired().expect("打开观察");
            assert_eq!(host.poll(), 2);
            unsafe { PostQuitMessage(0) };
            for _ in 0..2 {
                assert_eq!(host.poll(), 0);
                assert_eq!(host.stop_reason(), "采集线程收到 Windows 退出消息 WM_QUIT");
            }
        })
        .join()
        .unwrap();
    }

    #[test]
    #[ignore = "需要已解锁的 Windows 交互桌面"]
    fn paired_host_starts_observing_and_releases_on_mode_change() {
        let mut host = Host::open_paired().expect("创建配对桌面");
        assert_eq!(host.poll(), 2);
        assert!(!host.input(3, 0x87, 0));
        for _ in 0..2 {
            assert!(host.request_control(true));
            assert_eq!(host.poll(), 3);
            assert!(host.request_control(false));
            assert_eq!(host.poll(), 2);
            assert!(!host.input(3, 0x87, 0));
        }
        drop(host);
        assert_eq!(Host::open_paired().expect("重开配对桌面").poll(), 2);
        assert_eq!(Host::open().expect("普通现场确认桌面").poll(), 1);
    }

    #[test]
    fn viewer_and_decoder_start_on_same_worker() {
        Viewer::with_decoder("RemoteOps 本机窗口测试").expect("初始化观看组件");
    }

    #[test]
    fn one_decoder_survives_picture_mode_changes() {
        let mut decoder = Decoder::new().expect("创建解码器");
        let mut packet = vec![0; 2 * 1024 * 1024];
        let mut pixels = vec![0; 1920 * 1080 * 4];
        for (width, height, fps, bitrate) in [
            (1920, 1080, 30, 6_000_000),
            (854, 480, 8, 600_000),
            (1280, 720, 15, 2_000_000),
            (1920, 1080, 15, 6_000_000),
        ] {
            let mut encoder = Encoder::new(width, height, fps, bitrate).expect("切换编码档位");
            let input = vec![128; width as usize * height as usize * 4];
            let mut received = false;
            for _ in 0..50 {
                let n = encoder.encode(&input, &mut packet).expect("生成测试帧");
                if n > 0
                    && decoder
                        .decode(&packet[..n], &mut pixels)
                        .expect("切换画质后解码")
                        == Some((width, height))
                {
                    received = true;
                    break;
                }
            }
            assert!(received, "新档位未显示 {width}×{height}");
        }
    }

    #[test]
    fn changing_synthetic_bgra_survives_h264_round_trip() {
        let (w, h) = (320, 180);
        let mut encoder = Encoder::new(w, h, 15, 600_000).expect("创建 H.264 编码器");
        let mut decoder = Decoder::new().expect("创建 H.264 解码器");
        let mut input = vec![0; w as usize * h as usize * 4];
        let mut packet = vec![0; 2 * 1024 * 1024];
        let mut pixels = vec![0; 1920 * 1080 * 4];
        let mut decoded = 0;
        let mut samples = Vec::new();
        for index in 0..65 {
            let level = if index % 20 < 10 { 40 } else { 190 };
            for p in input.as_chunks_mut::<4>().0 {
                p.copy_from_slice(&[level, level, level, 255]);
            }
            let n = encoder.encode(&input, &mut packet).expect("合成帧编码");
            if n > 0
                && let Some(size) = decoder
                    .decode(&packet[..n], &mut pixels)
                    .expect("解码自有访问单元")
            {
                assert_eq!(size, (w, h));
                decoded += 1;
                samples.push(pixels[0]);
            }
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
        assert!(decoded >= 30, "只解出 {decoded} 帧");
        assert!(
            samples.iter().any(|&n| n < 80) && samples.iter().any(|&n| n > 130),
            "亮度变化未到达解码端"
        );
    }
}
