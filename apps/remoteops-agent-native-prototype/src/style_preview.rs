//! 共用 Win32 窗口层：样式演示使用合成数据，默认产品入口连接共享业务宿主。
#![allow(clippy::wildcard_imports)] // Win32 FFI 常量集中在此独立边界。
#![allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{
        LibraryLoader::GetModuleHandleW,
        Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
    },
    UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

mod business;
#[cfg(feature = "agent-runtime")]
mod test_key;
use crate::win32::clipboard;
mod line_measure_cache;
mod observer_data;
mod observer_demo;
mod readonly_view;
mod render_plan;
mod theme_draw;
use observer_data::ObserverData;
use observer_demo::Category;
use render_plan::{ControlSpec, ControlWork, PageFrame};
use theme_draw::{Palette, palette, rgb};
mod scroll_geometry;

const MAIN_CLASS: &str = "RemoteOpsStylePreviewMain";
const DIALOG_CLASS: &str = "RemoteOpsStylePreviewDialog";
const OBS_CLASS: &str = "RemoteOpsStylePreviewObserver";
const MAIN_WIDTH: i32 = 480;
const MAIN_HEIGHT: i32 = 440;
const CODE_COPY_GAP: i32 = 16;
const CODE_COPY_SIZE: i32 = 32;
const MAIN_FOOTER_TOP: i32 = MAIN_HEIGHT - 48;
const MAIN_FOOTER_BUTTON_Y: i32 = MAIN_FOOTER_TOP + 8;
const TIMER: usize = 71;
const REBUILD_PAGE: u32 = WM_APP + 41;
const BUTTON_SUBCLASS: usize = 0x524F_5042;
const CHECKBOX_SUBCLASS: usize = 0x524F_5043;
#[derive(Clone, Copy)]
enum NativeFailure {
    WindowCallback,
    TextCallback,
    ScrollDecorationCallback,
    ScrollTrackingCallback,
    ScrollDecorationCreation,
}
impl NativeFailure {
    const fn check_name(self) -> &'static str {
        match self {
            Self::WindowCallback => "native_window_callback",
            Self::TextCallback => "native_text_callback",
            Self::ScrollDecorationCallback => "native_scroll_decoration_callback",
            Self::ScrollTrackingCallback => "native_scroll_tracking_callback",
            Self::ScrollDecorationCreation => "native_scroll_decoration_creation",
        }
    }
}
thread_local! {
    // 只保存首个原生故障类别，不收集窗口文字或现场命令。
    static NATIVE_FAILURE: Cell<Option<NativeFailure>> = const { Cell::new(None) };
}
fn record_native_failure(reason: NativeFailure) {
    NATIVE_FAILURE.with(|failure| {
        if failure.get().is_none() {
            failure.set(Some(reason));
        }
    });
    unsafe { PostQuitMessage(1) };
}
fn finish_report(
    mut report: crate::Report,
    loop_failed: bool,
    cleanup_complete: bool,
    failure: Option<NativeFailure>,
) -> crate::Report {
    report.checks.extend([
        crate::Check {
            name: "native_message_loop_completed".into(),
            passed: !loop_failed,
            detail: "消息循环正常结束；与刷新耗时检查分别记录".into(),
        },
        crate::Check {
            name: "background_cleanup_completed".into(),
            passed: cleanup_complete,
            detail: "后台清理完成；未通过时不冒充正常退出".into(),
        },
    ]);
    if let Some(reason) = failure {
        report.checks.push(crate::Check {
            name: reason.check_name().into(),
            passed: false,
            detail: "原生回调或资源创建失败，仅记录固定类别".into(),
        });
    }
    report.metrics["native_loop_failed"] = loop_failed.into();
    report.metrics["background_cleanup_complete"] = cleanup_complete.into();
    report.metrics["native_callback_failed"] = failure.is_some().into();
    report.passed &= !loop_failed && cleanup_complete && failure.is_none();
    report
}
// SDK STATIC_STYLES：避免为两个样式值额外启用 SystemServices。
const SS_CENTER: u32 = 1;
const SS_LEFT: u32 = 0;
const SS_RIGHT: u32 = 2;
const SS_CENTERIMAGE: u32 = 0x200;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Main,
    Details,
    Permissions,
    Grant,
    Exit,
    Setup,
    Tls,
    Observer,
    About,
    Licenses,
    #[cfg(feature = "agent-runtime")]
    TestKey,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Theme {
    System,
    Light,
    Dark,
}
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)] // 独立演示开关互不依赖，不复用产品状态机。
struct State {
    legal_notices: Option<&'static str>,
    legal_pages: Option<std::sync::Arc<Vec<usize>>>,
    legal_page: usize,
    english: bool,
    theme: Theme,
    connected: bool,
    configured: bool,
    logs: bool,
    filter: usize,
    cleared: bool,
    copied: Option<Instant>,
    paused: bool,
    observer_demo: ObserverData,
    activity_view: crate::activity_view::ActivityView,
    product: Option<remoteops_agent_desktop_host::ProductSnapshot>,
    business_error: String,
    demo_tick: Option<Instant>,
    relay: String,
    full: bool,
    default_full: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            legal_notices: None,
            legal_pages: None,
            legal_page: 0,
            english: false,
            theme: Theme::Light,
            connected: false,
            configured: true,
            logs: false,
            filter: 0,
            cleared: false,
            copied: None,
            paused: false,
            observer_demo: ObserverData::default(),
            activity_view: crate::activity_view::ActivityView::default(),
            product: None,
            business_error: String::new(),
            demo_tick: None,
            relay: "relay.example.test:7443".into(),
            full: false,
            default_full: false,
        }
    }
}
// 只读取 Windows 应用主题偏好；失败或未知值均使用浅色，不修改系统设置。
fn system_prefers_dark() -> bool {
    let mut value = 1u32;
    let mut bytes = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize").as_ptr(),
            wide("AppsUseLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&raw mut value).cast(),
            &raw mut bytes,
        )
    };
    status == ERROR_SUCCESS && bytes == 4 && value == 0
}

struct App {
    loading_started: Cell<Option<Instant>>,
    instance: HINSTANCE,
    main: Cell<HWND>,
    observer: Cell<HWND>,
    state: RefCell<State>,
    business: RefCell<Option<business::Business>>,
    #[cfg(feature = "agent-runtime")]
    test_key: Option<test_key::Dialog>,
    #[allow(clippy::vec_box)] // GWLP_USERDATA 要求 Box 地址不随 Vec 扩容移动。
    windows: RefCell<Vec<Box<Panel>>>,
    quitting: Cell<bool>,
    // 系统主题通过消息更新，不在每个控件绘制时反复读注册表。
    system_dark: Cell<bool>,
    light: HBRUSH,
    dark: HBRUSH,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct LayoutStamp {
    width: i32,
    height: i32,
    dpi: u32,
}
fn layout_changed(previous: Option<LayoutStamp>, current: LayoutStamp) -> bool {
    previous != Some(current)
}
fn panel_layout(panel: &Panel) -> LayoutStamp {
    let mut rect = RECT::default();
    unsafe {
        GetClientRect(panel.hwnd.get(), &raw mut rect);
    }
    LayoutStamp {
        width: rect.right,
        height: rect.bottom,
        dpi: panel.dpi.get(),
    }
}
fn rebuild_if_resized(panel: &Panel) {
    let current = panel_layout(panel);
    if layout_changed(panel.layout.get(), current) {
        build(panel);
    }
}
struct Panel {
    app: *const App,
    kind: Kind,
    hwnd: Cell<HWND>,
    owner: HWND,
    dpi: Cell<u32>,
    controls: RefCell<BTreeMap<i32, HWND>>,
    control_specs: RefCell<BTreeMap<i32, ControlSpec>>,
    declared_controls: RefCell<BTreeSet<i32>>,
    building: Cell<bool>,
    build_pending: Cell<bool>,
    built: Cell<bool>,
    work: Cell<ControlWork>,
    observer_mutated: Cell<bool>,
    fonts: RefCell<Vec<HFONT>>,
    // 字体随DPI改变，颜色或语言重建不需要重复创建。
    font_dpi: Cell<u32>,
    layout: Cell<Option<LayoutStamp>>,
    title_dark: Cell<Option<i32>>,
    hovered_control: Cell<HWND>,
}
impl Panel {
    fn app(&self) -> &App {
        unsafe { &*self.app }
    }
    fn px(&self, v: i32) -> i32 {
        ((i64::from(v) * i64::from(self.dpi.get()) + 48) / 96) as i32
    }
    fn control(&self, id: i32) -> HWND {
        self.controls
            .borrow()
            .get(&id)
            .copied()
            .unwrap_or(null_mut())
    }
    fn text(&self, id: i32, text: &str) {
        let h = self.control(id);
        if !h.is_null() && value(h) != text {
            let started = Instant::now();
            unsafe {
                SetWindowTextW(h, wide(text).as_ptr());
            }
            self.record_work(|work| {
                work.text_writes += 1;
                work.text_us += started.elapsed().as_micros();
            });
        }
    }
    fn record_work(&self, update: impl FnOnce(&mut ControlWork)) {
        if self.building.get() {
            let mut work = self.work.get();
            update(&mut work);
            self.work.set(work);
        }
    }
}
impl App {
    fn is_test_key(&self) -> bool {
        #[cfg(feature = "agent-runtime")]
        {
            self.test_key.is_some()
        }
        #[cfg(not(feature = "agent-runtime"))]
        {
            false
        }
    }
    fn snapshot(&self) -> State {
        self.state.borrow().clone()
    }
    fn colors(&self) -> Palette {
        let theme = self.state.borrow().theme;
        palette(theme == Theme::Dark || (theme == Theme::System && self.system_dark.get()))
    }
    fn t<'app>(&self, zh: &'app str, en: &'app str) -> &'app str {
        if self.state.borrow().english { en } else { zh }
    }
    fn panels(&self) -> Vec<*const Panel> {
        self.windows
            .borrow()
            .iter()
            .map(|panel| &raw const **panel)
            .collect()
    }
    fn refresh(&self) {
        for panel in self.panels() {
            let panel = unsafe { &*panel };
            if unsafe { IsWindow(panel.hwnd.get()) } != 0 {
                build(panel);
                unsafe {
                    InvalidateRect(panel.hwnd.get(), null(), 1);
                }
            }
        }
    }
    fn shutdown(&self) {
        if !business::prepare_shutdown(self) {
            return;
        }
        self.destroy_windows();
    }
    // 循环已终止时也必须先销毁 HWND，再释放回调所借用的 App／Panel。
    fn destroy_windows(&self) {
        if self.quitting.replace(true) {
            return;
        }
        let windows: Vec<HWND> = self
            .panels()
            .iter()
            .map(|panel| unsafe { (**panel).hwnd.get() })
            .collect();
        for h in windows.into_iter().rev() {
            if h != self.main.get() && unsafe { IsWindow(h) } != 0 {
                unsafe {
                    DestroyWindow(h);
                }
            }
        }
        unsafe {
            DestroyWindow(self.main.get());
        }
    }
    fn open(&self, kind: Kind, owner: HWND) -> HWND {
        if kind == Kind::Exit && !owner.is_null() && unsafe { IsIconic(owner) } != 0 {
            // 主窗最小化时发起退出，也应让确认框和它的主窗可见。
            unsafe { ShowWindow(owner, SW_RESTORE) };
        }
        for panel in self.panels() {
            let panel = unsafe { &*panel };
            if panel.kind == kind && unsafe { IsWindow(panel.hwnd.get()) } != 0 {
                unsafe {
                    ShowWindow(
                        panel.hwnd.get(),
                        if kind == Kind::Observer {
                            // 自动操作尊重用户的最小化选择；主动打开由入口单独激活。
                            if IsIconic(panel.hwnd.get()) != 0 {
                                SW_SHOWMINNOACTIVE
                            } else {
                                SW_SHOWNA
                            }
                        } else {
                            SW_SHOW
                        },
                    );
                }
                return panel.hwnd.get();
            }
        }
        create_panel(self, kind, owner)
    }
    fn observer(&self) {
        let h = self.open(Kind::Observer, self.main.get());
        self.observer.set(h);
        self.sync_observer_button();
        // 隐藏期间事件仍推进；重开必须立即取当前状态，不能等待下一条事件。
        if let Some(ptr) = self
            .panels()
            .into_iter()
            .find(|ptr| unsafe { (**ptr).hwnd.get() == h })
        {
            sync_output(unsafe { &*ptr }, false);
        }
    }
    fn observer_visible(&self) -> bool {
        let hwnd = self.observer.get();
        !hwnd.is_null() && unsafe { IsWindowVisible(hwnd) } != 0
    }
    fn sync_observer_button(&self) {
        let visible = self.observer_visible();
        let minimized = visible && unsafe { IsIconic(self.observer.get()) } != 0;
        let text = if minimized {
            self.t("还原实况", "Restore viewer")
        } else if visible {
            self.t("隐藏实况", "Hide viewer")
        } else {
            self.t("操作实况", "Observer")
        };
        for ptr in self.panels() {
            let panel = unsafe { &*ptr };
            if panel.kind == Kind::Main {
                panel.text(338, text);
                let button = panel.control(338);
                if !button.is_null() {
                    unsafe {
                        InvalidateRect(button, null(), 1);
                    }
                }
            }
        }
    }
    fn toggle_observer(&self) {
        let hwnd = self.observer.get();
        let minimized = !hwnd.is_null() && unsafe { IsIconic(hwnd) } != 0;
        if observer_toggle_hides(self.observer_visible(), minimized) {
            if let Some(ptr) = self
                .panels()
                .into_iter()
                .find(|ptr| unsafe { (**ptr).hwnd.get() == hwnd })
            {
                readonly_view::cancel_interaction(unsafe { &*ptr });
            }
            unsafe { ShowWindow(hwnd, SW_HIDE) };
            self.sync_observer_button();
        } else {
            self.observer();
            let hwnd = self.observer.get();
            if !hwnd.is_null() {
                // 用户主动打开可聚焦；后台自动显示仍不激活窗口。
                unsafe {
                    ShowWindow(
                        hwnd,
                        if IsIconic(hwnd) != 0 {
                            SW_RESTORE
                        } else {
                            SW_SHOW
                        },
                    );
                    SetForegroundWindow(hwnd);
                }
            }
            self.sync_observer_button();
        }
    }
    fn enqueue(&self, scene: usize) {
        {
            let mut state = self.state.borrow_mut();
            state.observer_demo.start_scene(scene);
            state.demo_tick = Some(Instant::now());
        }
        self.observer();
    }
}
fn observer_toggle_hides(visible: bool, minimized: bool) -> bool {
    visible && !minimized
}
fn window_style(kind: Kind) -> (u32, u32) {
    let style = match kind {
        Kind::Main => WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
        Kind::Observer => WS_OVERLAPPEDWINDOW,
        _ => WS_POPUP | WS_CAPTION | WS_SYSMENU,
    };
    // 独立工作窗口有任务栏入口；普通确认/设置对话框仍由调用窗口持有。
    let extended = if kind == Kind::Observer {
        WS_EX_APPWINDOW
    } else {
        0
    };
    (style | WS_CLIPCHILDREN, extended)
}
fn native_window_owner(kind: Kind, anchor: HWND) -> HWND {
    if kind == Kind::Observer {
        null_mut()
    } else {
        anchor
    }
}
fn wide(state: &str) -> Vec<u16> {
    state.encode_utf16().chain(Some(0)).collect()
}
fn size(kind: Kind) -> (i32, i32) {
    match kind {
        Kind::Main => (MAIN_WIDTH, MAIN_HEIGHT),
        Kind::Details => (460, 294),
        Kind::Permissions => (440, 356),
        Kind::Grant => (440, 252),
        Kind::Exit => (440, 292),
        Kind::Setup => (620, 382),
        Kind::Tls => (560, 336),
        Kind::Observer => (820, 490),
        Kind::About => (420, 268),
        Kind::Licenses => (680, 440),
        #[cfg(feature = "agent-runtime")]
        Kind::TestKey => (456, 184),
    }
}
fn window_title(app: &App, kind: Kind) -> String {
    if let Some(snapshot) = app.state.borrow().product.as_ref() {
        return match kind {
            Kind::Main => snapshot.title.clone(),
            Kind::Details => snapshot.t("连接详情", "Connection details").into(),
            Kind::Permissions => snapshot.t("控制权限", "Control permissions").into(),
            Kind::Grant => snapshot.t("允许完全控制", "Allow full control").into(),
            Kind::Exit => snapshot.t("退出确认", "Confirm exit").into(),
            Kind::Setup => snapshot.t("连接设置", "Connection setup").into(),
            Kind::Tls => snapshot.t("证书确认", "Certificate confirmation").into(),
            Kind::Observer => snapshot.t("操作实况", "Operation observer").into(),
            Kind::Licenses => snapshot.t("组件与许可", "Components and licenses").into(),
            #[cfg(feature = "agent-runtime")]
            Kind::TestKey => unreachable!("GUID 窗口不初始化业务宿主"),
            Kind::About => format!("{} {}", snapshot.t("关于", "About"), snapshot.product_name),
        };
    }
    match kind {
        Kind::Main => app.t("RemoteOps · 界面预览", "RemoteOps · UI preview"),
        Kind::Details => app.t("连接详情 · 预览", "Details · Preview"),
        Kind::Permissions => app.t("控制权限 · 预览", "Control permissions · Preview"),
        Kind::Grant => app.t("权限确认 · 预览", "Confirm permissions · Preview"),
        Kind::Exit => app.t("退出确认 · 预览", "Confirm exit · Preview"),
        Kind::Setup => app.t("首次设置 · 预览", "First setup · Preview"),
        Kind::Tls => app.t("证书确认 · 预览", "Certificate · Preview"),
        Kind::Observer => app.t("操作实况 · 预览", "Observer · Preview"),
        Kind::Licenses => app.t("组件与许可", "Components and licenses"),
        #[cfg(feature = "agent-runtime")]
        Kind::TestKey => return test_key::title(app),
        Kind::About => app.t("关于 RemoteOps · 预览", "About RemoteOps · Preview"),
    }
    .into()
}
// 较大的附属窗不能按小主窗居中后把标题栏放到工作区之外。
fn visible_origin(x: i32, y: i32, width: i32, height: i32, work: RECT) -> (i32, i32) {
    (
        x.clamp(work.left, work.right.saturating_sub(width).max(work.left)),
        y.clamp(work.top, work.bottom.saturating_sub(height).max(work.top)),
    )
}
// 使用显示器物理工作区；负坐标和超大窗口也保留可访问的标题栏。
fn centered_origin(width: i32, height: i32, work: RECT) -> (i32, i32) {
    visible_origin(
        work.left + (work.right - work.left - width) / 2,
        work.top + (work.bottom - work.top - height) / 2,
        width,
        height,
        work,
    )
}
fn create_panel(app: &App, kind: Kind, owner: HWND) -> HWND {
    unsafe {
        let dpi = if owner.is_null() {
            let mut cursor = POINT::default();
            GetCursorPos(&raw mut cursor);
            let (mut x, mut y) = (96, 96);
            if GetDpiForMonitor(
                MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST),
                MDT_EFFECTIVE_DPI,
                &raw mut x,
                &raw mut y,
            ) >= 0
            {
                x.max(96)
            } else {
                GetDpiForSystem().max(96)
            }
        } else {
            GetDpiForWindow(owner).max(96)
        };
        let panel = Box::new(Panel {
            app: std::ptr::from_ref(app),
            kind,
            hwnd: Cell::new(null_mut()),
            owner: native_window_owner(kind, owner),
            dpi: Cell::new(dpi),
            controls: RefCell::new(BTreeMap::new()),
            control_specs: RefCell::new(BTreeMap::new()),
            declared_controls: RefCell::new(BTreeSet::new()),
            building: Cell::new(false),
            build_pending: Cell::new(false),
            built: Cell::new(false),
            work: Cell::new(ControlWork::default()),
            observer_mutated: Cell::new(false),
            fonts: RefCell::new(vec![]),
            font_dpi: Cell::new(0),
            layout: Cell::new(None),
            title_dark: Cell::new(None),
            hovered_control: Cell::new(null_mut()),
        });
        let ptr = &raw const *panel;
        let class = match kind {
            Kind::Main => MAIN_CLASS,
            Kind::Observer => OBS_CLASS,
            _ => DIALOG_CLASS,
        };
        let title = window_title(app, kind);
        let (w, h) = size(kind);
        let (w, h) = if kind == Kind::Setup
            && app
                .state
                .borrow()
                .product
                .as_ref()
                .is_some_and(|p| !p.endpoint_editable)
        {
            (408, 210)
        } else {
            (w, h)
        };
        let h = if kind == Kind::Details && app.state.borrow().product.is_some() {
            if app
                .state
                .borrow()
                .product
                .as_ref()
                .is_some_and(|p| !p.endpoint_editable)
            {
                h - 14
            } else {
                h + 22
            }
        } else {
            h
        };
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: panel.px(w),
            bottom: panel.px(h),
        };
        let (style, extended) = window_style(kind);
        AdjustWindowRectExForDpi(&raw mut rect, style, 0, extended, dpi);
        let mut x = CW_USEDEFAULT;
        let mut y = CW_USEDEFAULT;
        if owner.is_null() {
            let mut cursor = POINT::default();
            GetCursorPos(&raw mut cursor);
            let mut monitor = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(
                MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST),
                &raw mut monitor,
            ) != 0
            {
                (x, y) = centered_origin(
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    monitor.rcWork,
                );
            }
        } else {
            let mut owner_bounds = RECT::default();
            GetWindowRect(owner, &raw mut owner_bounds);
            x = owner_bounds.left
                + (owner_bounds.right - owner_bounds.left - (rect.right - rect.left)) / 2;
            y = owner_bounds.top
                + (owner_bounds.bottom - owner_bounds.top - (rect.bottom - rect.top)) / 2;
            let mut monitor = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(
                MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST),
                &raw mut monitor,
            ) != 0
            {
                (x, y) = visible_origin(
                    x,
                    y,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    monitor.rcWork,
                );
            }
        }
        // 主窗只作为首次定位参照；观察窗不设Win32 owner，独立最小化和切换。
        let hwnd = CreateWindowExW(
            extended,
            wide(class).as_ptr(),
            wide(&title).as_ptr(),
            style,
            x,
            y,
            rect.right - rect.left,
            rect.bottom - rect.top,
            panel.owner,
            null_mut(),
            app.instance,
            ptr.cast(),
        );
        if hwnd.is_null() {
            return hwnd;
        }
        // 显式设置标题，也覆盖首次 NCCREATE 期间的系统标题初始化差异。
        SetWindowTextW(hwnd, wide(&window_title(app, kind)).as_ptr());
        app.windows.borrow_mut().push(panel);
        if kind == Kind::Main {
            app.main.set(hwnd);
        } else if kind == Kind::Observer {
            app.observer.set(hwnd);
        } else if !owner.is_null() {
            EnableWindow(owner, 0);
        }
        build(&*ptr);
        if kind == Kind::Main {
            let interval = if app.business.borrow().is_some() {
                33
            } else {
                250
            };
            SetTimer(hwnd, TIMER, interval, None);
        }
        #[cfg(feature = "agent-runtime")]
        if kind == Kind::TestKey {
            SetTimer(hwnd, TIMER, 40, None);
        }
        // 初始鼠标操作不显示传统虚线框；Tab 等键盘操作仍由系统恢复焦点提示。
        SendMessageW(
            hwnd,
            WM_CHANGEUISTATE,
            (UIS_SET as usize) | ((UISF_HIDEFOCUS as usize) << 16),
            0,
        );
        ShowWindow(
            hwnd,
            if kind == Kind::Observer {
                SW_SHOWNOACTIVATE
            } else {
                SW_SHOW
            },
        );
        if kind != Kind::Observer {
            let first = (&*ptr)
                .controls
                .borrow()
                .iter()
                .find(|(id, _)| matches!(**id, 301 | 409 | 425 | 442 | 462 | 502 | 521 | 541))
                .map(|(_, h)| *h);
            #[cfg(feature = "agent-runtime")]
            let first = if kind == Kind::TestKey {
                Some((&*ptr).control(test_key::COPY))
            } else {
                first
            };
            if let Some(h) = first {
                SetFocus(h);
            }
        }
        hwnd
    }
}
fn close_panel(panel: &Panel) {
    if business::close(panel) {
        return;
    }
    close_panel_raw(panel);
}
fn close_panel_raw(panel: &Panel) {
    unsafe {
        if panel.kind == Kind::Observer {
            readonly_view::cancel_interaction(panel);
            ShowWindow(panel.hwnd.get(), SW_HIDE);
            panel.app().sync_observer_button();
            return;
        }
        let owner = panel.owner;
        DestroyWindow(panel.hwnd.get());
        if !owner.is_null() && IsWindow(owner) != 0 && !panel.app().quitting.get() {
            EnableWindow(owner, 1);
            SetActiveWindow(owner);
        }
    }
}
fn make_fonts(panel: &Panel) -> Vec<HFONT> {
    if panel.font_dpi.get() == panel.dpi.get() {
        let fonts = panel.fonts.borrow();
        if fonts.len() == 6 && fonts.iter().all(|font| !font.is_null()) {
            return vec![];
        }
    }
    unsafe {
        let mut new = vec![];
        for (height, weight, face) in [
            (14, 400, "Microsoft YaHei UI"),
            (19, 600, "Microsoft YaHei UI"),
            (40, 700, "Consolas"),
            (12, 400, "Microsoft YaHei UI"),
            (13, 400, "Consolas"),
            (13, 600, "Microsoft YaHei UI"),
        ] {
            new.push(CreateFontW(
                -panel.px(height),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET.into(),
                0,
                0,
                CLEARTYPE_QUALITY.into(),
                DEFAULT_PITCH.into(),
                wide(face).as_ptr(),
            ));
        }
        let old = panel.fonts.replace(new);
        panel.font_dpi.set(panel.dpi.get());
        // 幸存控件切换字体、过期控件销毁后，才能释放旧字体。
        old
    }
}

unsafe extern "system" fn button_subclass(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        let panel_ptr = data as *const Panel;
        match msg {
            WM_ERASEBKGND => 1,
            WM_MOUSEMOVE => {
                if !panel_ptr.is_null() {
                    let panel = &*panel_ptr;
                    if panel.hovered_control.get() != hwnd {
                        panel.hovered_control.set(hwnd);
                        let mut track = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        TrackMouseEvent(&raw mut track);
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                DefSubclassProc(hwnd, msg, w, l)
            }
            WM_MOUSELEAVE => {
                if !panel_ptr.is_null() {
                    let panel = &*panel_ptr;
                    if panel.hovered_control.get() == hwnd {
                        panel.hovered_control.set(null_mut());
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                DefSubclassProc(hwnd, msg, w, l)
            }
            WM_SETCURSOR => {
                if IsWindowEnabled(hwnd) != 0 {
                    SetCursor(LoadCursorW(null_mut(), IDC_HAND));
                    return 1;
                }
                DefSubclassProc(hwnd, msg, w, l)
            }
            WM_NCDESTROY => {
                if !panel_ptr.is_null() {
                    let panel = &*panel_ptr;
                    if panel.hovered_control.get() == hwnd {
                        panel.hovered_control.set(null_mut());
                    }
                }
                RemoveWindowSubclass(hwnd, Some(button_subclass), BUTTON_SUBCLASS);
                DefSubclassProc(hwnd, msg, w, l)
            }
            _ => DefSubclassProc(hwnd, msg, w, l),
        }
    }
}

unsafe extern "system" fn checkbox_subclass(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        let panel_ptr = data as *const Panel;
        match msg {
            WM_ERASEBKGND => 1,
            WM_MOUSEMOVE => {
                if !panel_ptr.is_null() {
                    let panel = &*panel_ptr;
                    if panel.hovered_control.get() != hwnd {
                        panel.hovered_control.set(hwnd);
                        let mut track = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        TrackMouseEvent(&raw mut track);
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                DefSubclassProc(hwnd, msg, w, l)
            }
            WM_MOUSELEAVE => {
                if !panel_ptr.is_null() {
                    let panel = &*panel_ptr;
                    if panel.hovered_control.get() == hwnd {
                        panel.hovered_control.set(null_mut());
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                DefSubclassProc(hwnd, msg, w, l)
            }
            WM_SETCURSOR => {
                if IsWindowEnabled(hwnd) != 0 {
                    SetCursor(LoadCursorW(null_mut(), IDC_HAND));
                    return 1;
                }
                DefSubclassProc(hwnd, msg, w, l)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &raw mut ps);
                if !panel_ptr.is_null() {
                    paint_custom_checkbox(&*panel_ptr, hwnd, hdc);
                }
                EndPaint(hwnd, &raw const ps);
                0
            }
            WM_PRINTCLIENT => {
                if !panel_ptr.is_null() && w != 0 {
                    paint_custom_checkbox(&*panel_ptr, hwnd, w as HDC);
                }
                0
            }
            BM_SETCHECK | WM_SETFOCUS | WM_KILLFOCUS | WM_ENABLE | WM_LBUTTONUP | WM_KEYUP => {
                let res = DefSubclassProc(hwnd, msg, w, l);
                InvalidateRect(hwnd, null(), 0);
                res
            }
            WM_NCDESTROY => {
                if !panel_ptr.is_null() {
                    let panel = &*panel_ptr;
                    if panel.hovered_control.get() == hwnd {
                        panel.hovered_control.set(null_mut());
                    }
                }
                RemoveWindowSubclass(hwnd, Some(checkbox_subclass), CHECKBOX_SUBCLASS);
                DefSubclassProc(hwnd, msg, w, l)
            }
            _ => DefSubclassProc(hwnd, msg, w, l),
        }
    }
}

fn paint_custom_checkbox(panel: &Panel, hwnd: HWND, target_dc: HDC) {
    unsafe {
        let mut rect = RECT::default();
        GetClientRect(hwnd, &raw mut rect);
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        let Some(mut surface) = theme_draw::Surface::new(target_dc, w, h) else {
            return;
        };
        let c = panel.app().colors();
        let checked = SendMessageW(hwnd, BM_GETCHECK, 0, 0) != 0;
        let hovered = panel.hovered_control.get() == hwnd;
        let focused = GetFocus() == hwnd;
        let disabled = IsWindowEnabled(hwnd) == 0;
        // 复选框位于控制权限卡片内部，圆角外底色与卡片严格一致。
        surface.clear(c.card);
        let side = (panel.px(18) as f32)
            .min(w as f32 - 2.0)
            .min(h as f32 - 2.0)
            .max(12.0);
        let bx = (w as f32 - side) * 0.5;
        let by = (h as f32 - side) * 0.5;
        let radius = (panel.px(5) as f32).max(3.0);
        let (fill_color, border_color) = if checked {
            let fill = if disabled {
                c.border
            } else if hovered {
                c.accent_hover
            } else {
                c.accent
            };
            (fill, fill)
        } else {
            let fill = if hovered && !disabled {
                c.btn_hover
            } else {
                c.content
            };
            let border = if focused {
                c.accent
            } else if hovered && !disabled {
                c.border_hover
            } else {
                c.border
            };
            (fill, border)
        };
        surface.draw_rounded_rect(
            bx,
            by,
            side,
            side,
            radius,
            fill_color,
            border_color,
            if focused && !checked { 1.5 } else { 1.0 },
            0.0,
            None,
        );
        if checked {
            surface.draw_checkmark(bx, by, side, (side * 0.13).max(1.7), rgb(255, 255, 255));
        }
        surface.blit(target_dc, 0, 0);
    }
}
struct AddedControl {
    hwnd: HWND,
    fresh: bool,
    geometry_changed: bool,
    batching: bool,
}
#[allow(clippy::too_many_arguments)] // 静态布局声明保持坐标、样式和控件 ID 就地可读。
fn add(
    panel: &Panel,
    id: i32,
    class: &'static str,
    text: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    style: u32,
    font: usize,
) -> AddedControl {
    unsafe {
        let font = panel
            .fonts
            .borrow()
            .get(font)
            .copied()
            .unwrap_or(null_mut());
        let spec = ControlSpec {
            class,
            style,
            bounds: [panel.px(x), panel.px(y), panel.px(w), panel.px(h)],
            font: font as usize,
        };
        panel.declared_controls.borrow_mut().insert(id);
        let old = panel.control(id);
        let owned = !old.is_null()
            && IsWindow(old) != 0
            && GetParent(old) == panel.hwnd.get()
            && GetDlgCtrlID(old) == id;
        let previous = owned
            .then(|| panel.control_specs.borrow().get(&id).copied())
            .flatten();
        let current_text = if owned && class != "EDIT" {
            value(old)
        } else {
            String::new()
        };
        let plan = render_plan::plan_control(previous, spec, &current_text, text);
        let hwnd = if plan.recreate {
            // 销毁可能同步触发辅助功能回调，不跨原生调用保留映射借用。
            panel.controls.borrow_mut().remove(&id);
            panel.control_specs.borrow_mut().remove(&id);
            if owned {
                DestroyWindow(old);
                panel.record_work(|work| work.removed += 1);
            }
            let started = Instant::now();
            let hwnd = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                spec.bounds[0],
                spec.bounds[1],
                spec.bounds[2],
                spec.bounds[3],
                panel.hwnd.get(),
                id as usize as HMENU,
                panel.app().instance,
                null(),
            );
            panel.record_work(|work| {
                work.created += 1;
                work.create_us += started.elapsed().as_micros();
            });
            if !hwnd.is_null() {
                panel.controls.borrow_mut().insert(id, hwnd);
            }
            hwnd
        } else {
            panel.record_work(|work| work.reused += 1);
            old
        };
        let batching = class == "EDIT" && readonly_view::begin_update(hwnd);
        if SendMessageW(hwnd, WM_GETFONT, 0, 0) as usize != font as usize {
            let started = Instant::now();
            SendMessageW(hwnd, WM_SETFONT, font as usize, 0);
            panel.record_work(|work| {
                work.font_changes += 1;
                work.font_us += started.elapsed().as_micros();
            });
        }
        if !plan.recreate && class != "EDIT" {
            if plan.text {
                panel.text(id, text);
            }
            if plan.bounds {
                let started = Instant::now();
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    spec.bounds[0],
                    spec.bounds[1],
                    spec.bounds[2],
                    spec.bounds[3],
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                panel.record_work(|work| {
                    work.moves += 1;
                    work.move_us += started.elapsed().as_micros();
                });
            }
        }
        panel.control_specs.borrow_mut().insert(id, spec);
        AddedControl {
            hwnd,
            fresh: plan.recreate,
            geometry_changed: plan.bounds || plan.font,
            batching,
        }
    }
}
#[allow(clippy::too_many_arguments)] // 静态布局声明保持坐标、样式和控件 ID 就地可读。
fn label(
    panel: &Panel,
    id: i32,
    text: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    font: usize,
    center: bool,
) {
    let text = business::text_for(panel, id, text);
    add(
        panel,
        id,
        "STATIC",
        &text,
        x,
        y,
        w,
        h,
        if id == 306 {
            13_u32
        } else {
            (if id == 305 {
                SS_RIGHT
            } else if center {
                SS_CENTER
            } else {
                SS_LEFT
            }) | match id {
                224 | 300 | 302 | 303 | 305 | 342 | 343 | 346 | 347 => SS_CENTERIMAGE,
                344 if panel.kind == Kind::Main => SS_CENTERIMAGE,
                427 => SS_CENTERIMAGE | 0x100,
                _ => 0,
            }
        },
        font,
    );
}
// 配对码含两个分隔符。按当前等宽字体量取整行，避免 DPI 或字体回退导致复制键偏移。
fn code_row(panel: &Panel) -> (i32, i32) {
    let mut size = SIZE::default();
    let font = panel.fonts.borrow()[2];
    let measured = unsafe {
        let hwnd = panel.hwnd.get();
        let dc = GetDC(hwnd);
        let previous = SelectObject(dc, font);
        let ok = GetTextExtentPoint32W(dc, wide("000-000-000").as_ptr(), 11, &raw mut size);
        SelectObject(dc, previous);
        ReleaseDC(hwnd, dc);
        ok != 0
    };
    let dpi = i32::try_from(panel.dpi.get()).unwrap_or(96).max(1);
    let width = if measured {
        (size.cx * 96 + dpi - 1) / dpi + 2
    } else {
        250
    };
    let left = (MAIN_WIDTH - width - CODE_COPY_GAP - CODE_COPY_SIZE) / 2;
    (left, width)
}

fn button(panel: &Panel, id: i32, text: &str, x: i32, y: i32, w: i32, h: i32) {
    let text = business::text_for(panel, id, text);
    let added = add(
        panel,
        id,
        "BUTTON",
        &text,
        x,
        y,
        w,
        h,
        WS_TABSTOP | BS_OWNERDRAW as u32,
        if matches!(id, 307..=309 | 324..=329 | 338 | 341 | 345) {
            3
        } else {
            0
        },
    );
    if added.fresh && !added.hwnd.is_null() {
        unsafe {
            let _ = SetWindowSubclass(
                added.hwnd,
                Some(button_subclass),
                BUTTON_SUBCLASS,
                std::ptr::from_ref(panel) as usize,
            );
        }
    }
}
#[allow(clippy::too_many_arguments)] // 静态布局声明保持坐标、样式和控件 ID 就地可读。
fn edit(panel: &Panel, id: i32, text: &str, x: i32, y: i32, w: i32, h: i32, readonly: bool) {
    let text = business::text_for(panel, id, text);
    let style = WS_TABSTOP
        | if readonly {
            ES_MULTILINE as u32
                | ES_AUTOHSCROLL as u32
                | WS_CLIPSIBLINGS
                | ES_READONLY as u32
                | ES_AUTOVSCROLL as u32
                | WS_VSCROLL
                | WS_HSCROLL
        } else {
            ES_AUTOHSCROLL as u32
        };
    let added = add(
        panel,
        id,
        "EDIT",
        if readonly { "" } else { &text },
        x,
        y,
        w,
        h,
        style,
        if panel.kind == Kind::Observer || matches!(id, 311 | 524 | 551) {
            4
        } else {
            0
        },
    );
    let hwnd = added.hwnd;
    let started = Instant::now();
    unsafe {
        if added.fresh {
            SendMessageW(hwnd, EM_LIMITTEXT, 262_144, 0);
        }
        let text_changed = value(hwnd) != text;
        if panel.kind == Kind::Observer && id == 202 && (added.geometry_changed || text_changed) {
            panel.observer_mutated.set(true);
        }
        if readonly {
            readonly_view::attach(panel, hwnd, [x, y, w, h]);
            // 大文本在控件、字体和限制就绪后填充，不走窗口创建时的初始文本路径。
            if text_changed {
                panel.text(id, &text);
            }
            if panel.kind == Kind::Observer && id == 202 {
                readonly_view::set_roles(
                    hwnd,
                    panel.app().state.borrow().observer_demo.line_roles(),
                );
            }
        } else {
            if text_changed {
                panel.text(id, &text);
            }
            if added.geometry_changed {
                // 保留单行原生编辑能力，以实际字体高度居中；外框由父窗绘制。
                let dc = GetDC(hwnd);
                let old = SelectObject(dc, panel.fonts.borrow()[0]);
                let mut metrics = TEXTMETRICW::default();
                let measured = GetTextMetricsW(dc, &raw mut metrics);
                SelectObject(dc, old);
                ReleaseDC(hwnd, dc);
                let text_height = if measured != 0 {
                    metrics.tmHeight
                } else {
                    panel.px(14)
                };
                let height = text_height + 2;
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    panel.px(x + 10),
                    panel.px(y) + (panel.px(h) - height) / 2,
                    panel.px(w - 20),
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        if added.batching {
            readonly_view::end_update(hwnd);
        }
    }
    panel.record_work(|work| work.readonly_us += started.elapsed().as_micros());
}
// 标准 Unicode EDIT 的段落分隔符必须为 CRLF，初次与增量呈现共用此入口。
fn presentation_lines<T: AsRef<str>>(lines: &[T]) -> String {
    lines
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join("\r\n")
}

struct BuildGuard<'a>(&'a Panel);
impl Drop for BuildGuard<'_> {
    fn drop(&mut self) {
        self.0.building.set(false);
        if self.0.build_pending.replace(false) && !self.0.hwnd.get().is_null() {
            // 原生回调内的重入只安排下一轮，避免破坏本轮声明集。
            unsafe {
                PostMessageW(self.0.hwnd.get(), REBUILD_PAGE, 0, 0);
            }
        }
    }
}
fn build(panel: &Panel) {
    if panel.building.replace(true) {
        panel.build_pending.set(true);
        return;
    }
    let _guard = BuildGuard(panel);
    let started = Instant::now();
    let initial = !panel.built.get();
    panel.work.set(ControlWork::default());
    panel.declared_controls.borrow_mut().clear();
    panel.observer_mutated.set(false);
    business::preserve_setup_input(panel);
    panel.layout.set(Some(panel_layout(panel)));
    let app = panel.app();
    let title = window_title(app, panel.kind);
    if value(panel.hwnd.get()) != title {
        unsafe {
            SetWindowTextW(panel.hwnd.get(), wide(&title).as_ptr());
        }
    }
    // DWM 亮暗标题栏与边框配色同步，切换主题时立即刷新非客户区。
    let colors = app.colors();
    let dark = i32::from(colors.dark);
    if panel.title_dark.get() != Some(dark) {
        theme_draw::apply_window_chrome(panel.hwnd.get(), colors);
        panel.title_dark.set(Some(dark));
    }
    let state = app.snapshot();
    let previous_view = if panel.kind == Kind::Observer {
        readonly_view::capture_view(panel.control(202))
    } else {
        None
    };
    let selected = previous_view
        .as_ref()
        .is_some_and(readonly_view::ViewState::has_selection);
    // 主题或尺寸变化不解除暂停及复制选区的正文冻结。
    let previous_output = if panel.kind == Kind::Observer
        && (state.paused || selected)
        && !panel.control(202).is_null()
    {
        Some(value(panel.control(202)))
    } else {
        None
    };
    let previous_roles = previous_output
        .as_ref()
        .map(|_| readonly_view::roles(panel.control(202)));
    let prepare_elapsed = started.elapsed();
    let fonts_started = Instant::now();
    let retired_fonts = make_fonts(panel);
    let fonts_elapsed = fonts_started.elapsed();
    let controls_started = Instant::now();
    let t = |zh, en| if state.english { en } else { zh };
    match panel.kind {
        #[cfg(feature = "agent-runtime")]
        Kind::TestKey => test_key::build(panel),
        Kind::Main if !state.logs => {
            let (code_x, code_width) = code_row(panel);
            label(
                panel,
                343,
                t("AI 工程师", "AI engineer"),
                33,
                10,
                133,
                23,
                5,
                false,
            );
            label(
                panel,
                344,
                t("人工运维", "Operator"),
                205,
                10,
                128,
                23,
                5,
                false,
            );
            let action_offset = if state.english { 66 } else { 45 };
            label(
                panel,
                346,
                if state.connected {
                    t("已连接", "Connected")
                } else {
                    t("未连接", "Offline")
                },
                33,
                32,
                action_offset,
                22,
                3,
                false,
            );
            label(
                panel,
                347,
                t("未连接", "Offline"),
                205,
                32,
                action_offset,
                22,
                3,
                false,
            );
            button(panel, 341, t("断开", "End"), 33 + action_offset, 31, 38, 24);
            button(
                panel,
                345,
                t("断开", "End"),
                205 + action_offset,
                31,
                38,
                24,
            );
            unsafe {
                ShowWindow(panel.control(341), SW_HIDE);
                ShowWindow(panel.control(345), SW_HIDE);
            }
            button(panel, 301, t("设置", "Settings"), 376, 16, 84, 34);

            label(
                panel,
                302,
                t("临时控制码", "Pairing code"),
                38,
                86,
                140,
                22,
                0,
                false,
            );
            label(
                panel,
                305,
                t("剩余 09:46", "Expires in 09:46"),
                180,
                86,
                262,
                22,
                3,
                false,
            );
            label(
                panel,
                303,
                if state.configured {
                    "123-456-789"
                } else {
                    "--- --- ---"
                },
                code_x,
                116,
                code_width,
                48,
                2,
                true,
            );
            button(
                panel,
                304,
                if state.copied.is_some() {
                    "✓"
                } else {
                    t("复制", "Copy")
                },
                code_x + code_width + CODE_COPY_GAP,
                124,
                CODE_COPY_SIZE,
                CODE_COPY_SIZE,
            );

            label(
                panel,
                342,
                t("操作动态", "Activity"),
                20,
                198,
                110,
                26,
                5,
                false,
            );
            label(panel, 300, "", 132, 198, 328, 26, 3, false);
            label(panel, 306, "", 20, 234, 440, 130, 0, false);
            label(panel, 312, "", 20, 368, 440, 18, 3, false);
            button(
                panel,
                307,
                t("连接详情", "Details"),
                14,
                MAIN_FOOTER_BUTTON_Y,
                90,
                32,
            );
            button(
                panel,
                308,
                t("实时日志", "Logs"),
                121,
                MAIN_FOOTER_BUTTON_Y,
                90,
                32,
            );
            button(
                panel,
                338,
                if app.observer_visible() {
                    t("隐藏实况", "Hide viewer")
                } else {
                    t("操作实况", "Observer")
                },
                228,
                MAIN_FOOTER_BUTTON_Y,
                91,
                32,
            );
            button(
                panel,
                309,
                t("退出 Agent", "Exit Agent"),
                363,
                MAIN_FOOTER_BUTTON_Y,
                103,
                32,
            );
        }
        Kind::Main => {
            label(
                panel,
                340,
                t("实时日志", "Activity log"),
                20,
                18,
                280,
                28,
                1,
                false,
            );
            button(panel, 310, t("返回", "Back"), 386, 17, 78, 30);
            let filters = [
                t("全部", "All"),
                t("运行中", "Running"),
                t("成功", "Success"),
                t("错误", "Error"),
                t("信息", "Info"),
            ];
            for (i, name) in filters.iter().enumerate() {
                button(
                    panel,
                    324 + i32::try_from(i).unwrap_or(0),
                    name,
                    20 + i32::try_from(i).unwrap_or(0) * 71,
                    58,
                    66,
                    28,
                );
            }
            button(panel, 329, t("清空", "Clear"), 394, 58, 70, 28);
            let rows = [
                "12:00:01  [demo-01]  信息  系统就绪 · 等待连接",
                "12:00:04  [demo-02]  运行中  正在执行合成命令",
                "12:00:05  [demo-03]  成功  中文输出显示正常",
                "12:00:06  [demo-04]  错误  连接超时（演示）",
            ];
            let text = if state.cleared {
                String::new()
            } else if state.filter == 0 {
                presentation_lines(&rows)
            } else {
                rows[match state.filter {
                    1 => 1,
                    2 => 2,
                    3 => 3,
                    _ => 0,
                }]
                .to_owned()
            };
            edit(panel, 311, &text, 16, 98, 448, 326, true);
        }
        Kind::Details => {
            label(
                panel,
                400,
                t("连接详情", "Connection details"),
                22,
                16,
                380,
                28,
                1,
                false,
            );
            let details_offset = if state.product.as_ref().is_some_and(|p| !p.endpoint_editable) {
                38
            } else {
                0
            };
            for (id, y, name, value) in [
                (401, 66, t("Relay 地址", "Relay"), state.relay.as_str()),
                (
                    403,
                    104,
                    t("运行权限", "Privileges"),
                    t("普通用户 · 界面演示", "Standard user · demo"),
                ),
                (
                    405,
                    142,
                    t("接收目录", "Receive folder"),
                    "C:\\演示 文件\\RemoteOps",
                ),
            ] {
                if id == 401 && state.product.as_ref().is_some_and(|p| !p.endpoint_editable) {
                    continue;
                }
                let y = y - details_offset;
                label(panel, id, name, 34, y, 96, 22, 3, false);
                if id == 405 && state.product.is_some() {
                    // 完整接收路径允许原生选择和水平滚动，不将长路径截成不可取回的标签。
                    edit(panel, 406, value, 134, y - 4, 292, 28, false);
                    unsafe {
                        SendMessageW(panel.control(406), EM_SETREADONLY, 1, 0);
                    }
                } else {
                    label(panel, id + 1, value, 134, y, 292, 22, 3, false);
                }
            }
            label(
                panel,
                407,
                t("可用能力", "Capabilities"),
                34,
                180 - details_offset,
                96,
                22,
                3,
                false,
            );
            label(
                panel,
                408,
                "Shell  ·  SSH  ·  Console",
                134,
                180 - details_offset,
                292,
                22,
                3,
                false,
            );
            if state.product.is_some() {
                label(panel, 410, "", 22, 222 - details_offset, 416, 22, 3, false);
                button(
                    panel,
                    337,
                    t("操作实况", "Observer"),
                    20,
                    264 - details_offset,
                    126,
                    34,
                );
                button(
                    panel,
                    323,
                    t("打开接收目录", "Open folder"),
                    156,
                    264 - details_offset,
                    136,
                    34,
                );
            } else {
                button(
                    panel,
                    337,
                    t("打开操作观察窗", "Open observer"),
                    20,
                    244,
                    184,
                    34,
                );
            }
            button(
                panel,
                409,
                t("关闭", "Close"),
                340,
                if state.product.is_some() {
                    264 - details_offset
                } else {
                    244
                },
                100,
                34,
            );
        }
        Kind::Permissions => {
            label(
                panel,
                420,
                t("控制权限", "Control permissions"),
                24,
                18,
                392,
                28,
                1,
                false,
            );
            label(
                panel,
                421,
                t(
                    "由本机用户决定工程师可以执行的操作。",
                    "Choose the operations allowed for the engineer.",
                ),
                24,
                52,
                392,
                34,
                0,
                false,
            );
            button(
                panel,
                422,
                t("逐项确认", "Confirm each action"),
                24,
                96,
                190,
                44,
            );
            button(panel, 423, t("完全控制", "Full control"), 226, 96, 190, 44);
            unsafe {
                EnableWindow(panel.control(422), i32::from(state.connected));
                EnableWindow(panel.control(423), i32::from(state.connected));
            }
            let checkbox = add(
                panel,
                424,
                "BUTTON",
                &business::text_for(
                    panel,
                    424,
                    t(
                        "以后默认允许完全控制（演示）",
                        "Allow full control by default (demo)",
                    ),
                ),
                38,
                168,
                20,
                22,
                BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
                0,
            );
            if checkbox.fresh && !checkbox.hwnd.is_null() {
                unsafe {
                    let _ = SetWindowSubclass(
                        checkbox.hwnd,
                        Some(checkbox_subclass),
                        CHECKBOX_SUBCLASS,
                        std::ptr::from_ref(panel) as usize,
                    );
                }
            }
            label(
                panel,
                427,
                t("以后默认允许完全控制", "Allow full control by default"),
                66,
                164,
                336,
                30,
                0,
                false,
            );
            unsafe {
                SendMessageW(
                    panel.control(424),
                    BM_SETCHECK,
                    usize::from(state.default_full),
                    0,
                );
            }
            label(
                panel,
                426,
                if !state.connected {
                    t(
                        "当前没有控制端连接，控制模式暂不可切换。",
                        "No controller is connected; mode changes are disabled.",
                    )
                } else if state.full {
                    t("当前：完全控制（演示）", "Current: full control (demo)")
                } else {
                    t(
                        "当前：逐项确认（演示）",
                        "Current: confirm each action (demo)",
                    )
                },
                38,
                224,
                364,
                48,
                3,
                false,
            );
            button(panel, 425, t("完成", "Done"), 306, 306, 110, 34);
        }
        Kind::Grant => {
            label(
                panel,
                440,
                t("允许完全控制？", "Allow full control?"),
                66,
                22,
                350,
                28,
                1,
                false,
            );
            label(
                panel,
                441,
                t(
                    "工程师将可以执行命令、SSH 和串口操作。\r\n此处仅演示确认过程，不会授予实际权限。",
                    "The engineer may run commands, SSH and console operations.\r\nThis preview does not grant real permissions.",
                ),
                38,
                78,
                364,
                84,
                0,
                false,
            );
            button(panel, 442, t("取消", "Cancel"), 172, 200, 112, 34);
            button(panel, 443, t("确认允许", "Allow"), 296, 200, 120, 34);
        }
        Kind::Exit => {
            label(
                panel,
                460,
                t("确认退出 RemoteOps？", "Exit RemoteOps?"),
                66,
                22,
                350,
                28,
                1,
                false,
            );
            label(
                panel,
                461,
                t(
                    "退出后，工程师将无法继续连接此电脑。\r\n\r\n当前没有实际会话；此预览只展示确认样式。\r\n请在真实维护工作结束后再退出 Agent。",
                    "The engineer will no longer be able to connect.\r\n\r\nNo real session is active in this preview.\r\nExit the Agent after maintenance is complete.",
                ),
                38,
                78,
                364,
                114,
                0,
                false,
            );
            button(panel, 462, t("继续运行", "Keep running"), 166, 236, 124, 36);
            button(panel, 463, t("确认退出", "Exit"), 302, 236, 114, 36);
        }
        Kind::Setup if state.product.as_ref().is_some_and(|p| !p.endpoint_editable) => {
            label(panel, 500, "", 24, 18, 360, 28, 1, false);
            label(panel, 503, "", 36, 66, 336, 34, 0, false);
            label(panel, 504, "", 36, 102, 336, 36, 3, false);
            button(panel, 505, t("取消", "Cancel"), 182, 162, 86, 32);
            button(panel, 506, t("重新连接", "Reconnect"), 278, 162, 108, 32);
        }
        Kind::Setup => {
            label(
                panel,
                500,
                t("欢迎使用 RemoteOps", "Welcome to RemoteOps"),
                26,
                20,
                568,
                30,
                1,
                false,
            );
            label(
                panel,
                501,
                t("Relay 地址", "Relay address"),
                42,
                78,
                160,
                22,
                5,
                false,
            );
            edit(panel, 502, &state.relay, 42, 106, 536, 36, false);
            label(
                panel,
                503,
                t(
                    "填写组织提供的中继地址，保存后显示临时控制码。\r\n本窗口是样式演示，不会建立连接或保存配置。",
                    "Enter the relay address provided by your organization.\r\nThis preview neither connects nor saves configuration.",
                ),
                42,
                156,
                536,
                46,
                3,
                false,
            );
            label(
                panel,
                509,
                t(
                    "工具由已连接中继统一管理。",
                    "Tools are managed by the connected relay.",
                ),
                42,
                224,
                536,
                46,
                3,
                false,
            );
            label(panel, 504, "", 26, 298, 568, 22, 3, false);
            button(panel, 507, "中文", 24, 336, 72, 32);
            button(panel, 508, "English", 102, 336, 86, 32);
            button(panel, 505, t("取消", "Cancel"), 346, 336, 94, 32);
            button(
                panel,
                506,
                t("模拟连接", "Preview connection"),
                450,
                336,
                146,
                32,
            );
        }
        Kind::Tls => {
            label(
                panel,
                520,
                t("确认中继服务器证书", "Confirm relay certificate"),
                66,
                20,
                432,
                28,
                1,
                false,
            );
            label(
                panel,
                526,
                t(
                    "请通过可信渠道核对指纹后再继续。",
                    "Verify this fingerprint using a trusted channel.",
                ),
                24,
                64,
                512,
                24,
                3,
                false,
            );
            edit(
                panel,
                524,
                "Relay: relay.example.test\r\nSHA256: DEMO-ONLY-0123-4567-89AB-CDEF\r\n此指纹仅为合成样式示例，不连接真实服务器。",
                24,
                96,
                512,
                152,
                true,
            );
            button(panel, 521, t("取消连接", "Cancel"), 24, 282, 130, 36);
            button(panel, 522, t("仅信任本次", "Trust once"), 168, 282, 156, 36);
            button(
                panel,
                523,
                t("信任并保存（演示）", "Trust and save (demo)"),
                338,
                282,
                198,
                36,
            );
            button(panel, 525, "×", 510, 18, 28, 28);
        }
        Kind::Observer => {
            let mut rect = RECT::default();
            unsafe {
                GetClientRect(panel.hwnd.get(), &raw mut rect);
            }
            let w = ((rect.right * 96) / i32::try_from(panel.dpi.get()).unwrap_or(96)).max(640);
            let h = ((rect.bottom * 96) / i32::try_from(panel.dpi.get()).unwrap_or(96)).max(360);
            label(
                panel,
                201,
                t("操作实况", "Activity"),
                18,
                12,
                96,
                28,
                1,
                false,
            );
            label(panel, 224, t("只读", "Read only"), 122, 17, 56, 18, 3, true);
            if state.product.is_none() {
                button(panel, 220, t("演示并发", "Run demo"), w - 234, 11, 102, 30);
                button(
                    panel,
                    221,
                    t("全部结束", "Finish all"),
                    w - 122,
                    11,
                    104,
                    30,
                );
            }
            let extra = (w - 36 - 5 * 8 - 6 * 70 - 44) / 6;
            let mut x = 18;
            for index in 0..6 {
                let width = 70 + extra + if index == 2 { 44 } else { 0 };
                let id = 210 + index;
                button(
                    panel,
                    id,
                    &observer_filter_text(&state, id),
                    x,
                    50,
                    width,
                    32,
                );
                x += width + 8;
            }
            label(
                panel,
                222,
                &observer_status(&state),
                18,
                88,
                w - 36,
                20,
                3,
                false,
            );
            edit(
                panel,
                202,
                &observer_rebuild_text(&state, previous_output.as_deref(), selected),
                16,
                114,
                w - 32,
                h - 174,
                true,
            );
            if let Some(roles) = previous_roles {
                readonly_view::set_roles(panel.control(202), roles);
            }
            label(
                panel,
                223,
                &observer_empty_text(&state),
                32,
                128,
                w - 72,
                24,
                3,
                false,
            );
            unsafe {
                ShowWindow(
                    panel.control(223),
                    if state.observer_demo.visible_count() == 0 {
                        SW_SHOWNA
                    } else {
                        SW_HIDE
                    },
                );
            }
            label(
                panel,
                203,
                &observer_count(&state),
                18,
                h - 33,
                w - 356,
                22,
                3,
                false,
            );
            button(
                panel,
                204,
                if state.paused {
                    t("继续显示", "Resume view")
                } else {
                    t("暂停显示", "Pause view")
                },
                w - 322,
                h - 40,
                96,
                32,
            );
            button(
                panel,
                206,
                t("返回最新", "Latest"),
                w - 216,
                h - 40,
                104,
                32,
            );
            button(panel, 205, t("关闭", "Close"), w - 102, h - 40, 84, 32);
        }
        Kind::About => {
            label(panel, 540, "RemoteOps", 82, 20, 314, 28, 1, false);
            label(
                panel,
                543,
                &format!(
                    "{} v{}",
                    t("界面预览", "UI preview"),
                    env!("CARGO_PKG_VERSION")
                ),
                82,
                50,
                314,
                20,
                3,
                false,
            );
            label(
                panel,
                542,
                t(
                    "用于确认窗口样式与操作布局。\r\n本预览不建立连接，也不更改现有配置。",
                    "Preview window styles and interactions.\r\nNo connections or configuration changes are made.",
                ),
                38,
                102,
                344,
                72,
                0,
                false,
            );
            if state.legal_notices.is_some() {
                button(panel, 544, t("组件与许可", "Licenses"), 24, 218, 152, 34);
            }
            button(panel, 541, t("知道了", "OK"), 288, 218, 108, 34);
        }
        Kind::Licenses => {
            label(
                panel,
                550,
                t("组件与许可 · 只读", "Components and licenses · Read only"),
                20,
                14,
                640,
                28,
                1,
                false,
            );
            edit(
                panel,
                551,
                license_page_text(&state),
                20,
                52,
                640,
                318,
                true,
            );
            button(panel, 553, t("上一页", "Previous"), 20, 392, 98, 32);
            button(panel, 554, t("下一页", "Next"), 128, 392, 98, 32);
            let pages = state
                .legal_pages
                .as_ref()
                .map_or(1, |pages| pages.len().saturating_sub(1).max(1));
            label(
                panel,
                555,
                &format!("{} / {}", state.legal_page + 1, pages),
                244,
                398,
                240,
                22,
                3,
                false,
            );
            unsafe {
                EnableWindow(panel.control(553), i32::from(state.legal_page > 0));
                EnableWindow(panel.control(554), i32::from(state.legal_page + 1 < pages));
            }
            button(panel, 552, t("关闭", "Close"), 558, 392, 102, 32);
        }
    }
    let declare_elapsed = controls_started.elapsed();
    let prune_started = Instant::now();
    let retired_ids = render_plan::stale_ids(
        panel.controls.borrow().keys().copied(),
        &panel.declared_controls.borrow(),
    );
    for id in retired_ids {
        let hwnd = panel.controls.borrow_mut().remove(&id);
        panel.control_specs.borrow_mut().remove(&id);
        if let Some(hwnd) = hwnd {
            unsafe {
                DestroyWindow(hwnd);
            }
            panel.record_work(|work| work.removed += 1);
        }
    }
    for font in retired_fonts {
        unsafe {
            DeleteObject(font);
        }
    }
    let prune_elapsed = prune_started.elapsed();
    let decorate_started = Instant::now();
    business::decorate(panel);
    let decorate_elapsed = decorate_started.elapsed();
    let restore_started = Instant::now();
    if panel.observer_mutated.get()
        && let Some(view) = previous_view
    {
        // 重建只恢复阅读状态；选中历史记录或暂停时不跳到末尾。
        readonly_view::restore_view(panel.control(202), &view, !state.paused);
    }
    // 保留控件句柄后，主题变化只需重绘；不触发文本重设、测宽和选区重置。
    let controls: Vec<HWND> = panel.controls.borrow().values().copied().collect();
    for hwnd in controls {
        unsafe {
            InvalidateRect(hwnd, null(), 0);
        }
    }
    unsafe {
        InvalidateRect(panel.hwnd.get(), null(), 0);
    }
    let restore_elapsed = restore_started.elapsed();
    panel.built.set(true);
    business::page_rendered(
        app,
        panel.kind,
        PageFrame {
            initial,
            total_us: started.elapsed().as_micros(),
            prepare_us: prepare_elapsed.as_micros(),
            fonts_us: fonts_elapsed.as_micros(),
            declare_us: declare_elapsed.as_micros(),
            decorate_us: decorate_elapsed.as_micros(),
            restore_us: restore_elapsed.as_micros(),
            prune_us: prune_elapsed.as_micros(),
            work: panel.work.get(),
        },
    );
}
fn menu(panel: &Panel) {
    unsafe {
        let app = panel.app();
        let state = app.snapshot();
        let menu = CreatePopupMenu();
        let t = |zh, en| if state.english { en } else { zh };
        let language = CreatePopupMenu();
        for (id, name, selected) in [
            (5001, "中文", !state.english),
            (5002, "English", state.english),
        ] {
            AppendMenuW(
                language,
                MF_STRING | if selected { MF_CHECKED } else { 0 },
                id,
                wide(name).as_ptr(),
            );
        }
        AppendMenuW(
            menu,
            MF_POPUP,
            language as usize,
            wide(t("语言", "Language")).as_ptr(),
        );
        let appearance = CreatePopupMenu();
        for (id, name, theme) in [
            (5010, t("跟随系统", "System"), Theme::System),
            (5011, t("浅色", "Light"), Theme::Light),
            (5012, t("深色", "Dark"), Theme::Dark),
        ] {
            AppendMenuW(
                appearance,
                MF_STRING | if state.theme == theme { MF_CHECKED } else { 0 },
                id,
                wide(name).as_ptr(),
            );
        }
        AppendMenuW(
            menu,
            MF_POPUP,
            appearance as usize,
            wide(t("外观", "Appearance")).as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(
            menu,
            MF_STRING,
            5020,
            wide(t("控制权限", "Control permissions")).as_ptr(),
        );
        AppendMenuW(menu, MF_STRING, 5021, wide(t("版本信息", "About")).as_ptr());
        if let Some(product) = &state.product {
            if app
                .business
                .borrow()
                .as_ref()
                .is_some_and(|business| business.demo)
            {
                AppendMenuW(
                    menu,
                    MF_STRING,
                    5035,
                    wide(t("演示双连接", "Preview both connections")).as_ptr(),
                );
            }
            if product.endpoint_editable {
                AppendMenuW(
                    menu,
                    MF_STRING,
                    5022,
                    wide(t("连接设置", "Connection setup")).as_ptr(),
                );
            }
        } else {
            let scenes = CreatePopupMenu();
            for (id, text) in [
                (5030, t("等待工程师连接", "Waiting for engineer")),
                (5031, t("AI工程师已连接", "AI engineer connected")),
                (5032, t("未配置状态", "Unconfigured")),
                (5033, t("首次配置窗口", "First setup")),
                (5034, t("证书确认窗口", "Certificate confirmation")),
                (5040, t("CMD 操作演示", "CMD output")),
                (5041, t("SSH 交互演示", "SSH output")),
                (5042, t("串口操作演示", "Console output")),
                (5043, t("只读操作观察窗", "Read-only observer")),
                (5045, t("分类并发演示", "Concurrent category demo")),
                (5046, t("结束全部演示", "Finish demo operations")),
            ] {
                AppendMenuW(scenes, MF_STRING, id, wide(text).as_ptr());
            }
            AppendMenuW(
                scenes,
                MF_STRING,
                5044,
                wide(t("大量日志演示", "Large log demonstration")).as_ptr(),
            );
            AppendMenuW(
                menu,
                MF_POPUP,
                scenes as usize,
                wide(t("样式演示", "Demonstrations")).as_ptr(),
            );
        }
        let mut rect = RECT::default();
        GetWindowRect(panel.control(301), &raw mut rect);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTALIGN,
            rect.right,
            rect.bottom + 4,
            0,
            panel.hwnd.get(),
            null(),
        );
        DestroyMenu(menu);
        if command != 0 {
            command_menu(panel, command);
        }
    }
}
fn command_menu(panel: &Panel, id: i32) {
    if business::menu(panel, id) {
        return;
    }
    let app = panel.app();
    match id {
        5001 | 5002 => {
            app.state.borrow_mut().english = id == 5002;
            app.refresh();
        }
        5010..=5012 => {
            app.state.borrow_mut().theme = match id {
                5010 => Theme::System,
                5011 => Theme::Light,
                _ => Theme::Dark,
            };
            app.refresh();
        }
        5020 => {
            app.open(Kind::Permissions, panel.hwnd.get());
        }
        5021 => {
            app.open(Kind::About, panel.hwnd.get());
        }
        5030..=5032 => {
            {
                let mut state = app.state.borrow_mut();
                state.connected = id == 5031;
                state.configured = id != 5032;
            }
            app.refresh();
        }
        5033 => {
            app.open(Kind::Setup, panel.hwnd.get());
        }
        5034 => {
            app.open(Kind::Tls, panel.hwnd.get());
        }
        5044 => {
            let mut output = Vec::with_capacity(10_001);
            output.push(format!(
                "[大日志演示 · 合成数据] {}",
                "长行 ABCD ".repeat(1200)
            ));
            output.extend((1..=10_000).map(|index| {
                format!("{index:05} · 中文日志 · Shell / SSH / Console · synthetic output")
            }));
            {
                let mut state = app.state.borrow_mut();
                state.observer_demo.replace_large_demo(output);
                state.demo_tick = None;
                state.paused = true;
            }
            app.observer();
            let h = app.observer.get();
            if let Some(ptr) = app
                .panels()
                .into_iter()
                .find(|ptr| unsafe { (**ptr).hwnd.get() == h })
            {
                let panel = unsafe { &*ptr };
                sync_output(panel, true);
                panel.text(204, app.t("继续显示", "Resume view"));
            }
        }
        5040..=5042 => app.enqueue((id - 5040) as usize),
        5043 => app.observer(),
        5045 => {
            change_demo(app, true);
            app.observer();
        }
        5046 => {
            change_demo(app, false);
            for ptr in app.panels() {
                let other = unsafe { &*ptr };
                if other.kind == Kind::Observer {
                    sync_output(other, false);
                }
            }
        }
        _ => {}
    }
}
fn value(h: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(h).max(0) as usize;
        let mut b = vec![0; n + 1];
        let count = GetWindowTextW(
            h,
            b.as_mut_ptr(),
            i32::try_from(b.len()).unwrap_or(i32::MAX),
        );
        String::from_utf16_lossy(&b[..count.max(0) as usize])
    }
}
// 许可只在用户打开时索引；每页有界，不把全量文本交给EDIT做布局。
fn license_page_offsets(text: &str) -> Vec<usize> {
    let mut pages = vec![0];
    let mut units = 0;
    for (offset, ch) in text.char_indices() {
        if units + ch.len_utf16() > 16_384 {
            let split = if ch == '\n' && offset > 0 && text.as_bytes()[offset - 1] == b'\r' {
                offset - 1
            } else {
                offset
            };
            pages.push(split);
            // 若边界落在CRLF中间，把CR连同LF移到下一页。
            units = usize::from(split < offset);
        }
        units += ch.len_utf16();
    }
    pages.push(text.len());
    pages
}
fn license_page_text(state: &State) -> &str {
    let text = state.legal_notices.unwrap_or("");
    state
        .legal_pages
        .as_ref()
        .and_then(|pages| pages.get(state.legal_page..state.legal_page + 2))
        .and_then(|page| text.get(page[0]..page[1]))
        .unwrap_or("")
}
fn command(panel: &Panel, id: i32) {
    #[cfg(feature = "agent-runtime")]
    if panel.kind == Kind::TestKey {
        test_key::command(panel, id);
        return;
    }
    if business::command(panel, id) {
        return;
    }
    let app = panel.app();
    match id {
        544 => {
            {
                let mut state = app.state.borrow_mut();
                if state.legal_pages.is_none() {
                    state.legal_pages = Some(std::sync::Arc::new(license_page_offsets(
                        state.legal_notices.unwrap_or(""),
                    )));
                }
                state.legal_page = 0;
            }
            app.open(Kind::Licenses, panel.hwnd.get());
        }
        553 | 554 => {
            {
                let mut state = app.state.borrow_mut();
                let count = state
                    .legal_pages
                    .as_ref()
                    .map_or(1, |pages| pages.len().saturating_sub(1).max(1));
                state.legal_page = if id == 553 {
                    state.legal_page.saturating_sub(1)
                } else {
                    state.legal_page.saturating_add(1).min(count - 1)
                };
            }
            build(panel);
        }
        301 => menu(panel),
        304 => {
            app.state.borrow_mut().copied = Some(Instant::now());
            panel.text(304, "✓");
        }
        307 => {
            app.open(Kind::Details, panel.hwnd.get());
        }
        308 => {
            app.state.borrow_mut().logs = true;
            build(panel);
        }
        309 => {
            app.open(Kind::Exit, panel.hwnd.get());
        }
        310 => {
            app.state.borrow_mut().logs = false;
            build(panel);
        }
        324..=328 => {
            app.state.borrow_mut().filter = (id - 324) as usize;
            build(panel);
        }
        329 => {
            app.state.borrow_mut().cleared = true;
            build(panel);
        }
        337 => app.observer(),
        338 => app.toggle_observer(),
        205 | 409 | 425 | 442 | 462 | 505 | 521 | 525 | 541 | 552 => close_panel(panel),
        422 => {
            app.state.borrow_mut().full = false;
            build(panel);
        }
        423 => {
            app.open(Kind::Grant, panel.hwnd.get());
        }
        424 => {
            let checked = unsafe { SendMessageW(panel.control(424), BM_GETCHECK, 0, 0) } != 0;
            app.state.borrow_mut().default_full = checked;
        }
        443 => {
            app.state.borrow_mut().full = true;
            close_panel(panel);
            app.refresh();
        }
        463 => app.shutdown(),
        506 => {
            let relay = value(panel.control(502));
            if relay.trim().is_empty() {
                panel.text(
                    504,
                    app.t(
                        "请输入 Relay 地址（演示）。",
                        "Enter a relay address (demo).",
                    ),
                );
            } else {
                app.state.borrow_mut().relay = relay;
                close_panel(panel);
                app.open(Kind::Tls, app.main.get());
            }
        }
        507 | 508 => {
            let relay = value(panel.control(502));
            {
                let mut state = app.state.borrow_mut();
                state.english = id == 508;
                state.relay = relay;
            }
            app.refresh();
        }
        522 | 523 => {
            app.state.borrow_mut().configured = true;
            close_panel(panel);
            app.refresh();
        }
        210..=215 => {
            app.state
                .borrow_mut()
                .observer_demo
                .select(observer_category(id));
            sync_output(panel, true);
        }
        220 | 221 => {
            change_demo(app, id == 220);
            sync_output(panel, false);
        }
        204 => {
            let paused = {
                let mut state = app.state.borrow_mut();
                state.paused = !state.paused;
                state.paused
            };
            panel.text(
                204,
                if paused {
                    app.t("继续显示", "Resume view")
                } else {
                    app.t("暂停显示", "Pause view")
                },
            );
            if !paused {
                sync_output(panel, false);
            }
        }
        206 => {
            app.state.borrow_mut().paused = false;
            sync_output(panel, true);
            panel.text(204, app.t("暂停显示", "Pause view"));
        }
        _ => {}
    }
}
fn observer_category(id: i32) -> Option<Category> {
    usize::try_from(id - 211)
        .ok()
        .and_then(|index| Category::ALL.get(index).copied())
}
fn observer_filter_text(state: &State, id: i32) -> String {
    let Some(category) = observer_category(id) else {
        return if state.english { "All" } else { "全部" }.into();
    };
    let count = state.observer_demo.running_count(Some(category));
    let name = category.label(state.english);
    if count == 0 {
        name.into()
    } else {
        format!("{name} ·{count}")
    }
}
fn observer_selected_name(state: &State) -> &str {
    state
        .observer_demo
        .selected()
        .map_or(if state.english { "All" } else { "全部" }, |category| {
            category.label(state.english)
        })
}
fn observer_status(state: &State) -> String {
    let name = observer_selected_name(state);
    let running = state.observer_demo.total_running();
    let detail = state.observer_demo.status(state.english);
    if state.english {
        format!("Viewing: {name} · Running: {running} · {detail}")
    } else {
        format!("查看：{name} · 执行中：{running} · {detail}")
    }
}
fn observer_count(state: &State) -> String {
    let shown = state.observer_demo.visible_count();
    let total = state.observer_demo.total_count();
    if state.english {
        format!("{shown}/{total} lines · passwords hidden")
    } else {
        format!("{shown}/{total} 行 · 密码已隐藏")
    }
}
fn observer_empty_text(state: &State) -> String {
    let name = observer_selected_name(state);
    if state.english {
        format!("No {name} interaction records")
    } else {
        format!("暂无 {name} 交互记录")
    }
}
fn observer_text(state: &State) -> String {
    let rows = if state.observer_demo.selected().is_none() {
        state.observer_demo.all_rows()
    } else {
        state.observer_demo.rows()
    };
    presentation_lines(&rows)
}
fn observer_rebuild_text(state: &State, previous: Option<&str>, selected: bool) -> String {
    if (state.paused || selected)
        && let Some(previous) = previous
    {
        previous.into()
    } else {
        observer_text(state)
    }
}
fn change_demo(app: &App, start: bool) {
    let mut state = app.state.borrow_mut();
    if start {
        state.observer_demo.start_concurrent();
        state.demo_tick = Some(Instant::now());
    } else {
        state.observer_demo.finish_all();
        state.demo_tick = None;
    }
}
fn sync_observer_chrome(panel: &Panel) {
    let (labels, status, count, empty, show_empty) = {
        let state = panel.app().state.borrow();
        (
            (210..=215)
                .map(|id| (id, observer_filter_text(&state, id)))
                .collect::<Vec<_>>(),
            observer_status(&state),
            observer_count(&state),
            observer_empty_text(&state),
            state.observer_demo.visible_count() == 0,
        )
    };
    for (id, text) in labels {
        if value(panel.control(id)) != text {
            panel.text(id, &text);
        }
        unsafe {
            InvalidateRect(panel.control(id), null(), 0);
        }
    }
    for (id, text) in [(222, status), (203, count), (223, empty)] {
        if value(panel.control(id)) != text {
            panel.text(id, &text);
        }
    }
    unsafe {
        ShowWindow(
            panel.control(223),
            if show_empty { SW_SHOWNA } else { SW_HIDE },
        );
    }
}
fn sync_output(panel: &Panel, force: bool) {
    let h = panel.control(202);
    if h.is_null() {
        return;
    }
    sync_observer_chrome(panel);
    if panel.app().state.borrow().paused && !force {
        return;
    }
    unsafe {
        let (mut start, mut end) = (0u32, 0u32);
        SendMessageW(h, EM_GETSEL, &raw mut start as usize, &raw mut end as isize);
        if start != end && !force {
            return;
        }
        let text = observer_text(&panel.app().state.borrow());
        let changed = value(h) != text;
        if changed || force {
            readonly_view::set_roles(h, panel.app().state.borrow().observer_demo.line_roles());
        }
        if (changed || force) && readonly_view::replace_output(h, &text, force) {
            return;
        }
        if changed {
            SetWindowTextW(h, wide(&text).as_ptr());
        }
        if changed || force {
            let end = GetWindowTextLengthW(h).max(0);
            SendMessageW(h, EM_SETSEL, end as usize, end as isize);
            SendMessageW(h, EM_SCROLLCARET, 0, 0);
            if force {
                SendMessageW(h, WM_VSCROLL, SB_BOTTOM as usize, 0);
            }
        }
    }
}
fn connection_feedback(
    english: bool,
    status: &str,
    tls_waiting: bool,
    elapsed: Duration,
) -> String {
    let frame = (elapsed.as_millis() / 250) as usize % 4;
    let dots = ["", ".", "..", "..."][frame];
    let seconds = elapsed.as_secs();
    let label = if tls_waiting {
        if english {
            "Confirm certificate"
        } else {
            "等待证书确认"
        }
    } else {
        status
    };
    let hint = if seconds >= 15 {
        if tls_waiting {
            if english {
                " · Review certificate"
            } else {
                " · 请处理证书确认"
            }
        } else if english {
            " · Connection pending"
        } else {
            " · 连接尚未完成"
        }
    } else {
        ""
    };
    if english {
        format!("{label}{dots} · {seconds}s{hint}")
    } else {
        format!("{label}{dots} · 已等待 {seconds} 秒{hint}")
    }
}
fn loading_feedback(panel: &Panel) {
    let app = panel.app();
    let state = app.state.borrow();
    let Some(product) = &state.product else {
        return;
    };
    if !matches!(
        product.phase,
        crate::product::ProductPhase::Connecting | crate::product::ProductPhase::Reconnecting
    ) {
        app.loading_started.set(None);
        return;
    }
    if state.logs {
        return;
    }
    let started = app.loading_started.get().unwrap_or_else(|| {
        let now = Instant::now();
        app.loading_started.set(Some(now));
        now
    });
    let text = connection_feedback(
        state.english,
        &product.status,
        product.tls.is_some(),
        started.elapsed(),
    );
    if value(panel.control(300)) != text {
        panel.text(300, &text);
    }
}
fn activity_tick(panel: &Panel) {
    let app = panel.app();
    let visible =
        unsafe { IsWindowVisible(panel.hwnd.get()) != 0 && IsIconic(panel.hwnd.get()) == 0 }
            && !app.state.borrow().logs;
    let repaint = {
        let mut state = app.state.borrow_mut();
        let State {
            product,
            activity_view,
            ..
        } = &mut *state;
        product
            .as_ref()
            .is_some_and(|snapshot| activity_view.tick(snapshot, visible))
    };
    if repaint {
        unsafe {
            InvalidateRect(panel.control(306), null(), 0);
            let header = RECT {
                left: panel.px(16),
                top: panel.px(12),
                right: panel.px(364),
                bottom: panel.px(58),
            };
            InvalidateRect(panel.hwnd.get(), &raw const header, 0);
        }
    }
}
fn tick(panel: &Panel) {
    #[cfg(feature = "agent-runtime")]
    if panel.kind == Kind::TestKey {
        test_key::poll(panel);
        return;
    }
    let app = panel.app();
    if business::poll(app) {
        activity_tick(panel);
        loading_feedback(panel);
        return;
    }
    let (expired, changed) = {
        let mut state = app.state.borrow_mut();
        let expired = state
            .copied
            .is_some_and(|t| t.elapsed() > Duration::from_secs(3));
        if expired {
            state.copied = None;
        }
        let changed = if state
            .demo_tick
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(1))
        {
            state.demo_tick = Some(Instant::now());
            state.observer_demo.tick()
        } else {
            false
        };
        (expired, changed)
    };
    if expired {
        panel.text(304, app.t("复制", "Copy"));
    }
    if changed {
        for other in app.panels() {
            let other = unsafe { &*other };
            if other.kind == Kind::Observer && unsafe { IsWindowVisible(other.hwnd.get()) } != 0 {
                sync_output(other, false);
            }
        }
    }
}
fn fill(hdc: HDC, rect: &RECT, color: u32) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(hdc, rect, brush);
        DeleteObject(brush);
    }
}
fn button_outside_bg(panel: &Panel, id: i32, c: Palette) -> u32 {
    match (panel.kind, id) {
        (Kind::Main, 304) => c.code_surface(),
        (Kind::Main, 307..=309 | 338)
        | (Kind::Details, 337 | 323 | 409)
        | (Kind::Permissions, 425)
        | (Kind::Grant, 442 | 443)
        | (Kind::Exit, 462 | 463)
        | (Kind::Setup, 505..=508)
        | (Kind::Tls, 521..=523)
        | (Kind::Observer, 204..=206)
        | (Kind::About, 541 | 544)
        | (Kind::Licenses, 552..=554) => c.footer_bg,
        #[cfg(feature = "agent-runtime")]
        (Kind::TestKey, 102 | 106) => c.footer_bg,
        _ => c.bg,
    }
}

fn button_glyph(id: i32, copied: bool) -> Option<&'static str> {
    match id {
        301 => Some("\u{E713}"),
        304 if copied => Some("\u{E73E}"),
        304 => Some("\u{E8C8}"),
        307 => Some("\u{E8FD}"),
        308 => Some("\u{E8A5}"),
        338 => Some("\u{E756}"),
        309 => Some("\u{E7E8}"),
        310 => Some("\u{E72B}"),
        _ => None,
    }
}

fn draw_button(panel: &Panel, di: &DRAWITEMSTRUCT) {
    unsafe {
        let app = panel.app();
        let c = app.colors();
        let id = i32::try_from(di.CtlID).unwrap_or(0);
        let state = app.state.borrow();
        let disabled = di.itemState & ODS_DISABLED != 0;
        let pressed = di.itemState & ODS_SELECTED != 0;
        let hovered = !disabled && panel.hovered_control.get() == di.hwndItem;
        let focused = di.itemState & ODS_FOCUS != 0 && di.itemState & ODS_NOFOCUSRECT == 0;

        let danger_solid = id == 463;
        let flat_main = panel.kind == Kind::Main && matches!(id, 301 | 307..=309 | 338 | 341 | 345);
        let ghost_close = id == 525;
        let copy_copied = id == 304 && state.copied.is_some();
        let primary = matches!(id, 102 | 304 | 409 | 425 | 443 | 506 | 523 | 541);
        let viewer_open = id == 338 && app.observer_visible();
        let log_filter_active = (324..=328).contains(&id) && state.filter == (id - 324) as usize;
        let lang_active = (id == 507 && !state.english) || (id == 508 && state.english);
        let perm_active = if matches!(id, 422 | 423) && !disabled {
            let is_full = state.product.as_ref().map_or(state.full, |p| {
                p.control_summary.contains("完全")
                    || p.control_summary.to_ascii_lowercase().contains("full")
            });
            (id == 422 && !is_full) || (id == 423 && is_full)
        } else {
            false
        };
        let (category_selected, category_running) = if (210..=215).contains(&id) {
            let category = observer_category(id);
            (
                state.observer_demo.selected() == category,
                category
                    .is_some_and(|category| state.observer_demo.running_count(Some(category)) > 0),
            )
        } else {
            (false, false)
        };
        let unread_logs = id == 308 && state.product.as_ref().is_some_and(|p| p.unread > 0);
        let selected_chip =
            viewer_open || category_selected || log_filter_active || lang_active || perm_active;
        drop(state);

        let (bg, border, fg) = if disabled {
            (c.card_subtle, c.border_subtle, c.muted)
        } else if copy_copied {
            (
                c.success,
                c.success,
                if c.dark { c.bg } else { rgb(0, 28, 10) },
            )
        } else if danger_solid {
            let fill = if pressed {
                c.danger_solid_pressed
            } else if hovered {
                c.danger_solid_hover
            } else {
                c.danger_solid
            };
            (fill, fill, rgb(255, 255, 255))
        } else if flat_main {
            let fill = if pressed {
                c.btn_pressed
            } else if hovered {
                c.btn_hover
            } else {
                button_outside_bg(panel, id, c)
            };
            let ink = if (hovered || pressed) && matches!(id, 309 | 341 | 345) {
                c.danger
            } else if matches!(id, 341 | 345) || selected_chip {
                c.accent_text
            } else {
                c.secondary
            };
            (fill, fill, ink)
        } else if primary {
            let fill = if pressed {
                c.accent_pressed
            } else if hovered {
                c.accent_hover
            } else {
                c.accent
            };
            (fill, fill, rgb(255, 255, 255))
        } else if ghost_close {
            let fill = if pressed {
                c.btn_pressed
            } else if hovered {
                c.btn_hover
            } else {
                c.bg
            };
            let bdr = if hovered || pressed { c.border } else { c.bg };
            (fill, bdr, if hovered { c.text } else { c.secondary })
        } else if selected_chip {
            let fill = if pressed {
                c.btn_pressed
            } else {
                c.accent_soft
            };
            (fill, c.accent_border, c.accent_text)
        } else {
            let fill = if pressed {
                c.btn_pressed
            } else if hovered {
                c.btn_hover
            } else {
                c.btn_bg
            };
            let bdr = if hovered { c.border_hover } else { c.border };
            (fill, bdr, c.text)
        };

        let outside_bg = button_outside_bg(panel, id, c);
        let w = di.rcItem.right - di.rcItem.left;
        let h = di.rcItem.bottom - di.rcItem.top;
        let Some(mut surface) = theme_draw::Surface::new(di.hDC, w, h) else {
            return;
        };
        surface.clear(outside_bg);
        let radius = if matches!(id, 341 | 345 | 324..=329) {
            panel.px(7) as f32
        } else {
            panel.px(8) as f32
        };
        let border_w = if selected_chip && !disabled {
            1.35
        } else {
            1.0
        };
        let top_hl =
            if c.dark && !disabled && !primary && !danger_solid && !ghost_close && !flat_main {
                Some(c.card_highlight)
            } else {
                None
            };
        surface.draw_rounded_rect(
            0.5,
            0.5,
            (w as f32 - 1.0).max(1.0),
            (h as f32 - 1.0).max(1.0),
            radius,
            bg,
            border,
            border_w,
            0.0,
            top_hl,
        );

        let mut text_rect = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        if category_running {
            let cy = h as f32 * 0.5;
            let cx = panel.px(11) as f32;
            surface.draw_glow_dot(
                cx,
                cy,
                panel.px(3) as f32,
                panel.px(6) as f32,
                c.success,
                if c.dark { 0.4 } else { 0.25 },
            );
            text_rect.left += panel.px(14);
        }
        let text = value(di.hwndItem);
        let mem_dc = surface.hdc();
        SetBkMode(mem_dc, TRANSPARENT.cast_signed());
        SetTextColor(mem_dc, fg);
        let font_idx = if matches!(id, 307..=309 | 324..=329 | 338 | 341 | 345) {
            3
        } else if selected_chip || primary || danger_solid {
            5
        } else {
            0
        };
        let font = panel.fonts.borrow()[font_idx];
        let old = SelectObject(mem_dc, font);
        let icon_only = id == 304;
        if let Some(glyph) = button_glyph(id, copy_copied || text == "✓") {
            let glyph_px = if matches!(id, 301 | 304) {
                panel.px(15)
            } else {
                panel.px(14)
            };
            if icon_only {
                theme_draw::draw_icon_glyph(mem_dc, text_rect, glyph, glyph_px, fg);
            } else {
                let mut extent = SIZE::default();
                let text_wide = wide(&text);
                if GetTextExtentPoint32W(
                    mem_dc,
                    text_wide.as_ptr(),
                    i32::try_from(text_wide.len().saturating_sub(1)).unwrap_or(i32::MAX),
                    &raw mut extent,
                ) == 0
                {
                    extent.cx = (w - panel.px(34)).max(0);
                }
                let side = glyph_px + panel.px(2);
                let gap = panel.px(5);
                let total_w = side + gap + extent.cx;
                let left = text_rect.left
                    + ((text_rect.right - text_rect.left - total_w) / 2).max(panel.px(4));
                let icon_rect = RECT {
                    left,
                    top: text_rect.top,
                    right: left + side,
                    bottom: text_rect.bottom,
                };
                text_rect.left = icon_rect.right + gap;
                text_rect.right = (text_rect.left + extent.cx + panel.px(2)).min(w - panel.px(4));
                theme_draw::draw_icon_glyph(mem_dc, icon_rect, glyph, glyph_px, fg);
                DrawTextW(
                    mem_dc,
                    text_wide.as_ptr(),
                    -1,
                    &raw mut text_rect,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
            }
        } else if perm_active && !disabled {
            let text_wide = wide(&text);
            let mut extent = SIZE::default();
            let _ = GetTextExtentPoint32W(
                mem_dc,
                text_wide.as_ptr(),
                i32::try_from(text_wide.len().saturating_sub(1)).unwrap_or(i32::MAX),
                &raw mut extent,
            );
            let dot_d = panel.px(7);
            let gap = panel.px(7);
            let total_w = dot_d + gap + extent.cx;
            let start_x = text_rect.left
                + ((text_rect.right - text_rect.left - total_w) / 2).max(panel.px(8));
            surface.draw_circle(
                (start_x + dot_d / 2) as f32,
                h as f32 * 0.5,
                panel.px(3) as f32,
                c.accent,
                1.0,
            );
            text_rect.left = start_x + dot_d + gap;
            text_rect.right = (text_rect.left + extent.cx + panel.px(2)).min(w - panel.px(4));
            DrawTextW(
                mem_dc,
                text_wide.as_ptr(),
                -1,
                &raw mut text_rect,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        } else {
            let text_wide = wide(&text);
            DrawTextW(
                mem_dc,
                text_wide.as_ptr(),
                -1,
                &raw mut text_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
        SelectObject(mem_dc, old);

        if unread_logs {
            surface.draw_circle(
                (w - panel.px(8)) as f32,
                panel.px(7) as f32,
                panel.px(3) as f32,
                c.danger,
                1.0,
            );
        }
        if focused {
            surface.draw_focus_ring(
                2.0,
                2.0,
                (w as f32 - 4.0).max(1.0),
                (h as f32 - 4.0).max(1.0),
                (radius - 1.5).max(2.0),
                if primary || danger_solid {
                    rgb(255, 255, 255)
                } else {
                    c.accent
                },
                1.5,
            );
        }
        surface.blit(di.hDC, di.rcItem.left, di.rcItem.top);
    }
}

fn paint_footer_bar(
    surface: &mut theme_draw::Surface,
    width: i32,
    height: i32,
    top: i32,
    c: Palette,
) {
    let top = top.clamp(0, height);
    if top < height {
        surface.draw_rounded_rect(
            0.0,
            top as f32,
            width as f32,
            (height - top) as f32,
            0.0,
            c.footer_bg,
            c.footer_bg,
            0.0,
            0.0,
            None,
        );
        surface.draw_hline(0, width, top, c.border_subtle);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_header_badge(
    surface: &mut theme_draw::Surface,
    x: i32,
    y: i32,
    size: i32,
    fill: u32,
    border: u32,
    glyph: &str,
    glyph_px: i32,
    glyph_color: u32,
) {
    surface.draw_rounded_rect(
        x as f32,
        y as f32,
        size as f32,
        size as f32,
        size as f32 * 0.5,
        fill,
        border,
        1.0,
        0.0,
        None,
    );
    theme_draw::draw_icon_glyph(
        surface.hdc(),
        RECT {
            left: x,
            top: y,
            right: x + size,
            bottom: y + size,
        },
        glyph,
        glyph_px,
        glyph_color,
    );
}

fn paint(panel: &Panel) {
    let started = Instant::now();
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(panel.hwnd.get(), &raw mut ps);
        let mut rect = RECT::default();
        GetClientRect(panel.hwnd.get(), &raw mut rect);
        let c = panel.app().colors();
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        let subtle_shadow = if c.dark { 0.0 } else { 0.04 };
        let card_hl = c.dark.then_some(c.card_highlight);

        if let Some(mut surface) = theme_draw::Surface::new(hdc, w, h) {
            surface.clear(c.bg);
            match panel.kind {
                Kind::Main if !panel.app().state.borrow().logs => {
                    let state = panel.app().state.borrow();
                    let roles = state
                        .product
                        .as_ref()
                        .map_or((state.connected, false), |p| {
                            (p.ai_connected, p.human_connected)
                        });
                    drop(state);
                    for (x, connected) in [(24, roles.0), (196, roles.1)] {
                        surface.draw_circle(
                            panel.px(x) as f32,
                            panel.px(22) as f32,
                            panel.px(3) as f32,
                            if connected { c.success } else { c.muted },
                            1.0,
                        );
                    }
                    let divider = RECT {
                        left: panel.px(174),
                        top: panel.px(17),
                        right: panel.px(174) + 1,
                        bottom: panel.px(50),
                    };
                    fill(surface.hdc(), &divider, c.border_subtle);
                    surface.draw_rounded_rect(
                        panel.px(20) as f32,
                        panel.px(74) as f32,
                        panel.px(MAIN_WIDTH - 40) as f32,
                        panel.px(108) as f32,
                        panel.px(9) as f32,
                        c.code_surface(),
                        c.code_surface(),
                        0.0,
                        0.0,
                        None,
                    );
                    surface.draw_hline(panel.px(20), panel.px(460), panel.px(231), c.border_subtle);
                    paint_footer_bar(&mut surface, w, h, panel.px(MAIN_FOOTER_TOP), c);
                }
                Kind::Main => {}
                Kind::Details => {
                    let state = panel.app().state.borrow();
                    let editable = state.product.as_ref().is_none_or(|p| p.endpoint_editable);
                    let is_product = state.product.is_some();
                    drop(state);
                    let offset = if editable { 0 } else { 38 };
                    let card_h = 160 - offset;
                    surface.draw_rounded_rect(
                        panel.px(20) as f32,
                        panel.px(54) as f32,
                        panel.px(420) as f32,
                        panel.px(card_h) as f32,
                        panel.px(10) as f32,
                        c.card,
                        c.border_subtle,
                        1.0,
                        subtle_shadow,
                        card_hl,
                    );
                    let dividers: &[i32] = if editable {
                        &[95, 133, 171]
                    } else {
                        &[95, 133]
                    };
                    for &dy in dividers {
                        surface.draw_hline(
                            panel.px(34),
                            panel.px(426),
                            panel.px(dy),
                            c.border_subtle,
                        );
                    }
                    let footer_y = if is_product { 250 - offset } else { 230 };
                    paint_footer_bar(&mut surface, w, h, panel.px(footer_y), c);
                }
                Kind::Permissions => {
                    // 默认控制权限开关卡片
                    surface.draw_rounded_rect(
                        panel.px(24) as f32,
                        panel.px(156) as f32,
                        panel.px(392) as f32,
                        panel.px(46) as f32,
                        panel.px(10) as f32,
                        c.card,
                        c.border_subtle,
                        1.0,
                        subtle_shadow,
                        card_hl,
                    );
                    // 当前状态摘要卡片
                    surface.draw_rounded_rect(
                        panel.px(24) as f32,
                        panel.px(214) as f32,
                        panel.px(392) as f32,
                        panel.px(66) as f32,
                        panel.px(10) as f32,
                        c.card_subtle,
                        c.border_subtle,
                        1.0,
                        0.0,
                        None,
                    );
                    paint_footer_bar(&mut surface, w, h, panel.px(292), c);
                }
                Kind::Grant => {
                    paint_header_badge(
                        &mut surface,
                        panel.px(24),
                        panel.px(18),
                        panel.px(32),
                        c.warning_soft,
                        c.warning_border,
                        "\u{E7BA}",
                        panel.px(16),
                        c.warning,
                    );
                    surface.draw_rounded_rect(
                        panel.px(24) as f32,
                        panel.px(66) as f32,
                        panel.px(392) as f32,
                        panel.px(108) as f32,
                        panel.px(10) as f32,
                        c.warning_soft,
                        c.warning_border,
                        1.0,
                        0.0,
                        None,
                    );
                    paint_footer_bar(&mut surface, w, h, panel.px(186), c);
                }
                Kind::Exit => {
                    paint_header_badge(
                        &mut surface,
                        panel.px(24),
                        panel.px(18),
                        panel.px(32),
                        c.danger_bg,
                        c.danger_border,
                        "\u{E7E8}",
                        panel.px(15),
                        c.danger,
                    );
                    surface.draw_rounded_rect(
                        panel.px(24) as f32,
                        panel.px(66) as f32,
                        panel.px(392) as f32,
                        panel.px(138) as f32,
                        panel.px(10) as f32,
                        c.card,
                        c.border_subtle,
                        1.0,
                        subtle_shadow,
                        card_hl,
                    );
                    paint_footer_bar(&mut surface, w, h, panel.px(220), c);
                }
                Kind::Setup => {
                    let fixed = panel
                        .app()
                        .state
                        .borrow()
                        .product
                        .as_ref()
                        .is_some_and(|p| !p.endpoint_editable);
                    if fixed {
                        surface.draw_rounded_rect(
                            panel.px(24) as f32,
                            panel.px(56) as f32,
                            panel.px(360) as f32,
                            panel.px(86) as f32,
                            panel.px(10) as f32,
                            c.card,
                            c.border_subtle,
                            1.0,
                            subtle_shadow,
                            card_hl,
                        );
                        paint_footer_bar(&mut surface, w, h, panel.px(150), c);
                    } else {
                        surface.draw_rounded_rect(
                            panel.px(24) as f32,
                            panel.px(62) as f32,
                            panel.px(572) as f32,
                            panel.px(226) as f32,
                            panel.px(12) as f32,
                            c.card,
                            c.border_subtle,
                            1.0,
                            subtle_shadow,
                            card_hl,
                        );
                        if !panel.control(502).is_null() {
                            let focused = GetFocus() == panel.control(502);
                            surface.draw_rounded_rect(
                                panel.px(42) as f32,
                                panel.px(106) as f32,
                                panel.px(536) as f32,
                                panel.px(36) as f32,
                                panel.px(8) as f32,
                                c.content,
                                if focused { c.accent } else { c.border },
                                if focused { 1.5 } else { 1.0 },
                                0.0,
                                None,
                            );
                        }
                        surface.draw_hline(
                            panel.px(42),
                            panel.px(578),
                            panel.px(212),
                            c.border_subtle,
                        );
                        paint_footer_bar(&mut surface, w, h, panel.px(322), c);
                    }
                }
                Kind::Tls => {
                    paint_header_badge(
                        &mut surface,
                        panel.px(24),
                        panel.px(17),
                        panel.px(32),
                        c.accent_soft,
                        c.accent_border,
                        "\u{E83D}",
                        panel.px(16),
                        c.accent_text,
                    );
                    paint_footer_bar(&mut surface, w, h, panel.px(268), c);
                }
                Kind::Observer => {
                    // 只读徽章背景（内边距留足，避免静态文本背景覆盖圆角边框）
                    surface.draw_rounded_rect(
                        panel.px(116) as f32,
                        panel.px(15) as f32,
                        panel.px(68) as f32,
                        panel.px(22) as f32,
                        panel.px(6) as f32,
                        c.accent_soft,
                        c.accent_border,
                        1.0,
                        0.0,
                        None,
                    );
                    paint_footer_bar(&mut surface, w, h, h - panel.px(52), c);
                }
                Kind::About => {
                    surface.draw_rounded_rect(
                        panel.px(24) as f32,
                        panel.px(20) as f32,
                        panel.px(46) as f32,
                        panel.px(46) as f32,
                        panel.px(10) as f32,
                        c.card,
                        c.border_subtle,
                        1.0,
                        subtle_shadow,
                        card_hl,
                    );
                    let icon = LoadIconW(panel.app().instance, std::ptr::without_provenance(1));
                    DrawIconEx(
                        surface.hdc(),
                        panel.px(30),
                        panel.px(26),
                        icon,
                        panel.px(34),
                        panel.px(34),
                        0,
                        null_mut(),
                        DI_NORMAL,
                    );
                    surface.draw_rounded_rect(
                        panel.px(24) as f32,
                        panel.px(88) as f32,
                        panel.px(372) as f32,
                        panel.px(100) as f32,
                        panel.px(10) as f32,
                        c.card,
                        c.border_subtle,
                        1.0,
                        subtle_shadow,
                        card_hl,
                    );
                    paint_footer_bar(&mut surface, w, h, panel.px(204), c);
                }
                Kind::Licenses => {
                    paint_footer_bar(&mut surface, w, h, panel.px(380), c);
                }
                #[cfg(feature = "agent-runtime")]
                Kind::TestKey => {}
            }
            surface.blit(hdc, 0, 0);
        } else {
            fill(hdc, &rect, c.bg);
        }
        #[cfg(feature = "agent-runtime")]
        if panel.kind == Kind::TestKey {
            test_key::paint(panel, hdc, c);
        }
        readonly_view::paint_frames(panel, hdc);
        EndPaint(panel.hwnd.get(), &raw const ps);
    }
    render_plan::record_paint(started.elapsed().as_micros());
}
// 系统回调不允许 Rust panic 越过 ABI；失败通过消息循环明确回传。
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Ok(result) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        wndproc_inner(hwnd, msg, w, l)
    })) {
        result
    } else {
        record_native_failure(NativeFailure::WindowCallback);
        0
    }
}
unsafe fn wndproc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(l as *const CREATESTRUCTW);
            let ptr = cs.lpCreateParams as *const Panel;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
            (*ptr).hwnd.set(hwnd);
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Panel;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let panel = &*ptr;
        match msg {
            REBUILD_PAGE => {
                build(panel);
                0
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED
                if panel.kind == Kind::Main || panel.app().is_test_key() =>
            {
                let app = panel.app();
                let current = system_prefers_dark();
                let previous = app.system_dark.replace(current);
                if current != previous && app.state.borrow().theme == Theme::System {
                    app.refresh();
                }
                activity_tick(panel);
                DefWindowProcW(hwnd, msg, w, l)
            }
            WM_COMMAND => {
                let id = i32::from((w & 0xffff) as u16);
                let notification = (w >> 16) & 0xffff;
                if panel.kind == Kind::Permissions
                    && id == 427
                    && notification == 0
                    && l != 0
                    && panel.control(427) == l as HWND
                {
                    SendMessageW(panel.control(424), BM_CLICK, 0, 0);
                    return 0;
                }
                // 原生菜单命令和控件通知分别校验；便于可访问性自动化同路调用。
                if panel.kind == Kind::Main
                    && l == 0
                    && notification == 0
                    && matches!(id, 5001 | 5002 | 5010..=5012 | 5020..=5022 | 5030..=5035 | 5040..=5046)
                {
                    command_menu(panel, id);
                    return 0;
                }
                if id == 502 && matches!(notification as u32, EN_SETFOCUS | EN_KILLFOCUS) {
                    InvalidateRect(hwnd, null(), 0);
                }
                if notification == BN_CLICKED as usize
                    && panel.control(id) == l as HWND
                    && !panel.control(id).is_null()
                {
                    command(panel, id);
                }
                0
            }
            WM_LBUTTONDOWN if panel.kind == Kind::Setup && !panel.control(502).is_null() => {
                let x = i32::from(l as u16);
                let y = i32::from((l >> 16) as u16);
                if x >= panel.px(42) && x < panel.px(578) && y >= panel.px(106) && y < panel.px(142)
                {
                    SetFocus(panel.control(502));
                }
                0
            }
            WM_TIMER if w == TIMER => {
                tick(panel);
                0
            }
            WM_PAINT => {
                paint(panel);
                0
            }
            WM_ERASEBKGND => 1,
            WM_DRAWITEM => {
                if l != 0 {
                    let item = &*(l as *const DRAWITEMSTRUCT);
                    if item.CtlID == 306
                        && panel.kind == Kind::Main
                        && !panel.app().state.borrow().logs
                    {
                        let app = panel.app();
                        let state = app.state.borrow();
                        let c = app.colors();
                        fill(item.hDC, &item.rcItem, c.bg);
                        state.activity_view.draw(
                            item.hDC,
                            item.rcItem,
                            panel.fonts.borrow()[3],
                            panel.dpi.get(),
                            if state.english {
                                remoteops_agent_desktop_host::NativeLanguage::English
                            } else {
                                remoteops_agent_desktop_host::NativeLanguage::Chinese
                            },
                            c.dark,
                        );
                    } else {
                        draw_button(panel, item);
                    }
                }
                1
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
                let c = panel.app().colors();
                let hdc = w as HDC;
                let id = GetDlgCtrlID(l as HWND);
                let text_view = readonly_view::contains(l as HWND);
                let state = panel.app().state.borrow();
                let fixed_setup = state.product.as_ref().is_some_and(|p| !p.endpoint_editable);
                drop(state);
                let bg = if text_view || matches!(id, 101 | 223 | 502) {
                    c.content
                } else if panel.kind == Kind::Main && matches!(id, 302 | 303 | 305) {
                    c.code_surface()
                } else if id == 224 {
                    c.accent_soft
                } else if id == 441 {
                    c.warning_soft
                } else if id == 426 {
                    c.card_subtle
                } else if matches!(id, 300 | 342) {
                    c.bg
                } else if matches!(
                    id,
                    302 | 303 | 305 | 401..=408 | 424 | 427 | 461 | 501 | 503 | 509 | 542
                ) || (id == 504 && fixed_setup)
                {
                    c.card
                } else if matches!(id, 105 | 203 | 555) {
                    c.footer_bg
                } else {
                    c.bg
                };
                SetBkMode(hdc, TRANSPARENT.cast_signed());
                SetBkColor(hdc, bg);
                SetTextColor(
                    hdc,
                    if id == 224 {
                        c.accent_text
                    } else if matches!(
                        id,
                        105 | 300
                            | 346
                            | 347
                            | 302
                            | 305
                            | 312
                            | 401
                            | 403
                            | 405
                            | 407
                            | 421
                            | 426
                            | 503
                            | 509
                            | 526
                            | 203
                            | 222
                            | 223
                            | 543
                            | 555
                    ) {
                        c.secondary
                    } else if id == 306 {
                        c.accent
                    } else if matches!(id, 504 | 410) {
                        c.danger
                    } else {
                        c.text
                    },
                );
                if bg == c.bg {
                    if c.dark {
                        panel.app().dark as isize
                    } else {
                        panel.app().light as isize
                    }
                } else {
                    // 卡片/容器底色画刷使用系统 DC_BRUSH，不为重绘额外分配对象。
                    SetDCBrushColor(hdc, bg);
                    GetStockObject(DC_BRUSH) as isize
                }
            }
            WM_GETMINMAXINFO if panel.kind == Kind::Observer => {
                let mut bounds = RECT {
                    left: 0,
                    top: 0,
                    right: panel.px(640),
                    bottom: panel.px(360),
                };
                AdjustWindowRectExForDpi(
                    &raw mut bounds,
                    window_style(Kind::Observer).0,
                    0,
                    window_style(Kind::Observer).1,
                    panel.dpi.get(),
                );
                let limits = &mut *(l as *mut MINMAXINFO);
                limits.ptMinTrackSize.x = bounds.right - bounds.left;
                limits.ptMinTrackSize.y = bounds.bottom - bounds.top;
                0
            }
            WM_SIZE if panel.kind == Kind::Observer => {
                if w != SIZE_MINIMIZED as usize && !panel.controls.borrow().is_empty() {
                    rebuild_if_resized(panel);
                }
                panel.app().sync_observer_button();
                0
            }
            WM_DPICHANGED => {
                panel.dpi.set((w & 0xffff) as u32);
                let rect = *(l as *const RECT);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                if panel.kind == Kind::Observer {
                    rebuild_if_resized(panel);
                } else {
                    build(panel);
                }
                0
            }
            WM_CLOSE => {
                if panel.kind == Kind::Main {
                    panel.app().open(Kind::Exit, hwnd);
                } else {
                    close_panel(panel);
                }
                0
            }
            WM_DESTROY => {
                if panel.kind == Kind::Main || panel.app().is_test_key() {
                    KillTimer(hwnd, TIMER);
                    PostQuitMessage(0);
                }
                0
            }
            WM_NCDESTROY => {
                panel.hwnd.set(null_mut());
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let result = DefWindowProcW(hwnd, msg, w, l);
                panel.controls.borrow_mut().clear();
                let fonts = std::mem::take(&mut *panel.fonts.borrow_mut());
                for font in fonts {
                    DeleteObject(font);
                }
                result
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}
/// 启动独立样式预览；不创建真实 Agent 或读写产品配置。
pub(super) fn run() -> Result<(), String> {
    let report = run_impl(
        None,
        #[cfg(feature = "agent-runtime")]
        None,
    )?;
    if report.passed {
        Ok(())
    } else {
        Err("窗口消息处理或后台清理失败。".into())
    }
}
/// 使用已确认的窗口样式启动现有共享产品宿主。
pub(super) fn run_product(options: &crate::Options) -> Result<crate::Report, String> {
    run_impl(
        Some(business::Business::new(options)),
        #[cfg(feature = "agent-runtime")]
        None,
    )
}
/// 显示离线生成结果，复用产品窗口但不创建 Agent 业务宿主。
#[cfg(feature = "agent-runtime")]
pub(super) fn run_test_key(
    guid: String,
    profile: &remoteops_product::ProductProfile,
) -> Result<(), String> {
    if run_impl(None, Some(test_key::Dialog::new(guid, profile)))?.passed {
        Ok(())
    } else {
        Err("GUID 窗口未正常关闭".into())
    }
}

fn run_impl(
    product: Option<business::Business>,
    #[cfg(feature = "agent-runtime")] test_key: Option<test_key::Dialog>,
) -> Result<crate::Report, String> {
    NATIVE_FAILURE.with(|failure| failure.set(None));
    render_plan::reset_paint_metrics();
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(null());
        readonly_view::register(instance)?;
        for name in [MAIN_CLASS, DIALOG_CLASS, OBS_CLASS] {
            let class_name = wide(name);
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                // MAKEINTRESOURCEW(1)：复用包内 RemoteOps 图标，不依赖外部图标文件。
                hIcon: LoadIconW(instance, std::ptr::without_provenance(1)),
                hIconSm: LoadIconW(instance, std::ptr::without_provenance(1)),
                lpszClassName: class_name.as_ptr(),
                ..Default::default()
            };
            if RegisterClassExW(&raw const class) == 0 {
                return Err("无法注册样式预览窗口。".into());
            }
        }
        let initial_state = product
            .as_ref()
            .map_or_else(State::default, business::initial_state);
        #[cfg(feature = "agent-runtime")]
        let initial_state = test_key
            .as_ref()
            .map_or(initial_state, test_key::Dialog::state);
        let app = Box::new(App {
            loading_started: Cell::new(None),
            instance,
            main: Cell::new(null_mut()),
            observer: Cell::new(null_mut()),
            state: RefCell::new(initial_state),
            business: RefCell::new(product),
            #[cfg(feature = "agent-runtime")]
            test_key,
            windows: RefCell::new(vec![]),
            quitting: Cell::new(false),
            system_dark: Cell::new(system_prefers_dark()),
            light: CreateSolidBrush(palette(false).bg),
            dark: CreateSolidBrush(palette(true).bg),
        });
        #[allow(unused_mut)]
        let mut initial_window = Kind::Main;
        #[cfg(feature = "agent-runtime")]
        if app.is_test_key() {
            initial_window = Kind::TestKey;
        }
        let hwnd = create_panel(&app, initial_window, null_mut());
        if hwnd.is_null() {
            return Err("无法创建样式预览窗口。".into());
        }
        let mut msg = MSG::default();
        let failed = loop {
            let code = GetMessageW(&raw mut msg, null_mut(), 0, 0);
            if code <= 0 {
                break code < 0 || msg.wParam != 0;
            }
            let root = GetAncestor(msg.hwnd, GA_ROOT);
            let panel_ptr = app
                .panels()
                .into_iter()
                .find(|panel| (**panel).hwnd.get() == root);
            if msg.message == WM_KEYDOWN
                && msg.wParam == VK_ESCAPE as usize
                && let Some(panel) = panel_ptr
            {
                if (*panel).kind == Kind::Main {
                    if app.state.borrow().logs {
                        app.state.borrow_mut().logs = false;
                        build(&*panel);
                    } else {
                        app.open(Kind::Exit, root);
                    }
                } else {
                    close_panel(&*panel);
                }
                continue;
            }
            #[cfg(feature = "agent-runtime")]
            if let Some(panel) = panel_ptr
                && (*panel).kind == Kind::TestKey
                && test_key::handle_key(&*panel, &msg)
            {
                continue;
            }
            if !root.is_null() && IsDialogMessageW(root, &raw const msg) != 0 {
                continue;
            }
            TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        };
        let clean = business::prepare_shutdown(&app);
        app.destroy_windows();
        for ptr in app.panels() {
            let panel = &*ptr;
            for font in panel.fonts.borrow_mut().drain(..) {
                DeleteObject(font);
            }
        }
        DeleteObject(app.light);
        DeleteObject(app.dark);
        // 即使原生循环或后台退出失败，也保留报告，避免只留下退出码 1。
        let report = if app.business.borrow().is_some() {
            business::report(&app)
        } else {
            crate::Report {
                schema_version: 1,
                passed: true,
                checks: vec![],
                metrics: serde_json::json!({"style_demo":true}),
                environment: serde_json::json!({"agent_runtime_started":false}),
                limitations: vec!["仅样式预览，未验证业务".into()],
            }
        };
        Ok(finish_report(
            report,
            failed,
            clean,
            NATIVE_FAILURE.with(Cell::get),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RECT, State, centered_origin, connection_feedback, license_page_offsets,
        observer_rebuild_text, observer_status, observer_text, presentation_lines, visible_origin,
    };

    #[test]
    fn observer_is_an_independent_taskbar_window_but_dialogs_keep_their_owner() {
        use super::{
            Kind, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX, WS_THICKFRAME,
            native_window_owner, window_style,
        };
        let anchor = std::ptr::without_provenance_mut(0x1234);
        let (style, extended) = window_style(Kind::Observer);
        assert!(native_window_owner(Kind::Observer, anchor).is_null());
        assert_ne!(extended & WS_EX_APPWINDOW, 0);
        assert_eq!(extended & WS_EX_TOOLWINDOW, 0);
        assert_ne!(style & WS_THICKFRAME, 0);
        assert_ne!(style & WS_MAXIMIZEBOX, 0);
        for kind in [
            Kind::Exit,
            Kind::About,
            Kind::Licenses,
            Kind::Setup,
            Kind::Tls,
        ] {
            assert_eq!(native_window_owner(kind, anchor), anchor);
            assert_eq!(window_style(kind).1 & WS_EX_APPWINDOW, 0);
        }
    }

    #[test]
    fn observer_entry_restores_minimized_windows_and_only_hides_normal_visible_windows() {
        use super::observer_toggle_hides;
        assert!(observer_toggle_hides(true, false));
        assert!(!observer_toggle_hides(true, true));
        assert!(!observer_toggle_hides(false, false));
        assert!(!observer_toggle_hides(false, true));
    }

    fn passing_report() -> crate::Report {
        crate::Report {
            schema_version: 1,
            passed: true,
            checks: vec![],
            metrics: serde_json::json!({}),
            environment: serde_json::json!({}),
            limitations: vec![],
        }
    }
    #[test]
    fn connection_feedback_animates_and_describes_real_wait_states() {
        use std::time::Duration;
        let frames: Vec<_> = [0, 250, 500, 750]
            .into_iter()
            .map(|millis| {
                connection_feedback(false, "正在连接", false, Duration::from_millis(millis))
            })
            .collect();
        for pair in frames.windows(2) {
            assert_ne!(pair[0], pair[1]);
        }
        let tls = connection_feedback(false, "系统就绪", true, Duration::from_secs(20));
        assert!(tls.contains("等待证书确认") && tls.contains("请处理证书确认"));
        assert!(!tls.contains("系统就绪") && !tls.contains("空闲"));
        let slow = connection_feedback(false, "正在连接", false, Duration::from_secs(20));
        assert!(slow.contains("20 秒") && slow.contains("连接尚未完成"));
        assert!(!slow.contains('%'));
    }

    #[test]
    fn license_pages_are_bounded_unicode_safe_and_lossless() {
        for text in [
            String::new(),
            "中文😀\r\n".repeat(20_000),
            "a".repeat(16_384),
            format!("{}\r\n尾😀", "a".repeat(16_383)),
        ] {
            let pages = license_page_offsets(&text);
            let chunks: Vec<_> = pages.windows(2).map(|p| &text[p[0]..p[1]]).collect();
            assert_ne!(chunks, Vec::<&str>::new());
            assert_eq!(chunks.concat(), text);
            assert!(chunks.iter().all(|chunk| !chunk.ends_with('\r')));
            assert!(chunks.iter().skip(1).all(|chunk| !chunk.starts_with('\n')));
            assert!(
                chunks
                    .iter()
                    .all(|chunk| chunk.encode_utf16().count() <= 16_384)
            );
        }
    }

    #[test]
    fn startup_center_uses_work_area_and_keeps_oversized_titles_visible() {
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        assert_eq!(centered_origin(516, 414, work), (-1218, 313));
        assert_eq!(centered_origin(2400, 1400, work), (-1920, 0));
        for dpi in [96, 144, 192] {
            let width = 516 * dpi / 96;
            let height = 414 * dpi / 96;
            let (x, y) = centered_origin(width, height, work);
            assert!(x >= work.left && y >= work.top);
            assert!(x + width <= work.right && y + height <= work.bottom);
        }
    }

    #[test]
    fn failed_callback_keeps_a_serializable_report_and_specific_cause() {
        let report = super::finish_report(
            passing_report(),
            true,
            true,
            Some(super::NativeFailure::TextCallback),
        );
        assert!(!report.passed);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "native_text_callback" && !check.passed)
        );
        assert_eq!(report.metrics["native_loop_failed"], true);
        assert!(serde_json::to_value(report).is_ok());
    }
    #[test]
    fn failed_background_cleanup_is_not_reported_as_normal_exit() {
        let report = super::finish_report(passing_report(), false, false, None);
        assert!(!report.passed);
        assert_eq!(report.metrics["background_cleanup_complete"], false);
        assert_eq!(report.metrics["native_callback_failed"], false);
    }
    #[test]
    fn quality_failure_does_not_imply_a_crashed_message_loop() {
        let mut report = passing_report();
        report.passed = false;
        let report = super::finish_report(report, false, true, None);
        assert!(!report.passed);
        assert!(report.checks.iter().all(|check| check.passed));
        assert_eq!(report.metrics["native_loop_failed"], false);
    }
    #[test]
    fn observer_same_size_messages_do_not_require_rebuilding_controls() {
        let size = super::LayoutStamp {
            width: 800,
            height: 470,
            dpi: 96,
        };
        assert!(super::layout_changed(None, size));
        assert!(!super::layout_changed(Some(size), size));
        assert!(super::layout_changed(
            Some(size),
            super::LayoutStamp {
                width: 1000,
                ..size
            }
        ));
        assert!(super::layout_changed(
            Some(size),
            super::LayoutStamp { dpi: 144, ..size }
        ));
    }
    #[test]
    fn normal_shutdown_keeps_success() {
        let report = super::finish_report(passing_report(), false, true, None);
        assert!(report.passed);
        assert_eq!(report.metrics["native_callback_failed"], false);
    }

    #[test]
    fn larger_observer_keeps_title_inside_primary_work_area() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        assert_eq!(visible_origin(-124, -21, 816, 509, work), (0, 0));
    }
    #[test]
    fn work_area_with_negative_monitor_coordinates_is_preserved() {
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        assert_eq!(visible_origin(-2100, 200, 816, 509, work), (-1920, 200));
    }
    #[test]
    fn right_bottom_and_oversized_windows_keep_accessible_origin() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        assert_eq!(visible_origin(1900, 1020, 800, 500, work), (1120, 540));
        assert_eq!(visible_origin(100, 100, 2200, 2000, work), (0, 0));
    }

    #[test]
    fn paused_serial_updates_resume_with_latest_cumulative_total() {
        let mut state = State {
            observer_demo: super::ObserverData::live(),
            ..State::default()
        };
        let record = |total| remoteops_agent_desktop_host::Observation {
            repetition: Some(remoteops_agent_desktop_host::ObservationRepetition {
                record_id: 77,
                total,
            }),
            text: if total == 1 {
                "same".into()
            } else {
                format!("same × {total}")
            },
            role: remoteops_agent_desktop_host::ObservationRole::Output,
            category: remoteops_agent_desktop_host::ObservationCategory::Serial,
            request: Some("serial-fixture".into()),
            new_task: false,
        };
        state.observer_demo.ingest(vec![record(1)], &[]);
        let previous = observer_text(&state);
        state.paused = true;
        state.observer_demo.ingest(vec![record(10_000)], &[]);
        assert_eq!(
            observer_rebuild_text(&state, Some(&previous), false),
            previous
        );
        state.paused = false;
        let latest = observer_rebuild_text(&state, Some(&previous), false);
        assert!(latest.contains("same × 10000"));
        assert_eq!(state.observer_demo.total_count(), 1);
    }

    #[test]
    fn paused_rebuild_preserves_body_while_model_and_status_advance() {
        let mut state = State::default();
        let previous = observer_text(&state);
        state.paused = true;
        state.observer_demo.start_concurrent();
        assert_ne!(observer_text(&state), previous);
        assert_eq!(
            observer_rebuild_text(&state, Some(&previous), false),
            previous
        );
        assert!(observer_status(&state).contains("执行中：2"));
        state.paused = false;
        assert_eq!(
            observer_rebuild_text(&state, Some(&previous), false),
            observer_text(&state)
        );
    }

    #[test]
    fn selected_rebuild_keeps_native_body_when_model_advances_and_resumes_after_deselection() {
        let mut state = State::default();
        let previous = observer_text(&state);
        state.observer_demo.start_concurrent();
        assert!(!state.paused);
        assert_ne!(observer_text(&state), previous);
        assert_eq!(
            observer_rebuild_text(&state, Some(&previous), true),
            previous
        );
        assert_eq!(
            observer_rebuild_text(&state, Some(&previous), false),
            observer_text(&state)
        );
        assert_eq!(
            observer_rebuild_text(&state, None, true),
            observer_text(&state)
        );
    }

    #[test]
    fn observer_presentation_preserves_rows_and_windows_newlines() {
        let state = State::default();
        let rows = state.observer_demo.all_rows();
        let text = presentation_lines(&rows);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            rows.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_eq!(text.matches("\r\n").count(), rows.len() - 1);
        assert!(
            text.as_bytes()
                .windows(2)
                .filter(|pair| pair[1] == b'\n')
                .all(|pair| pair[0] == b'\r')
        );
        assert!(!text.lines().any(|line| line.starts_with("ect")));
        assert!(text.contains("[CMD]") && text.contains("[SSH]") && text.contains("串口"));
        assert!(text.contains("[已隐藏]"));
    }

    #[test]
    fn source_blank_lines_and_multiline_command_output_are_preserved() {
        let rows = ["命令开始", "", "第一行\r\n\r\n第三行", "完成"];
        assert_eq!(
            presentation_lines(&rows),
            "命令开始\r\n\r\n第一行\r\n\r\n第三行\r\n完成"
        );
    }

    #[test]
    fn log_rows_do_not_insert_blank_lines_or_change_unicode() {
        let rows = ["12:00 中文 👋", "12:01 SSH [已隐藏]", "12:02 串口"];
        let text = presentation_lines(&rows);
        assert_eq!(text.split("\r\n").collect::<Vec<_>>(), rows);
        assert_eq!(text.matches("\r\n").count(), 2);
        assert!(!text.contains("\r\n\r\n"));
        assert!(!text.lines().any(|line| line.starts_with("ect")));
        assert_eq!(presentation_lines::<&str>(&[]), "");
        assert_eq!(presentation_lines(&["独立单行"]), "独立单行");
    }
}
