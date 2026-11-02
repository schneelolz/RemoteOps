//! Win32 边界：窗口只在 UI 线程访问；Box 由消息循环持有，回调仅借用稳定地址。
//! 不在持有 `RefCell` 借用时调用可能同步重入的窗口 API。
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use crate::{
    Check, Options, Report,
    backend::{Backend, Scenario},
    model::{Event, Transcript, scaled},
};
pub(crate) mod clipboard;
mod log_view;
mod product_ui;
use crate::product::ProductHost;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{null, null_mut},
    sync::mpsc::{Receiver, sync_channel},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{
        CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH, DeleteObject, GetSysColorBrush, HFONT,
        MapWindowPoints,
    },
    System::{
        LibraryLoader::GetModuleHandleW,
        RemoteDesktop::{
            NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
            WTSUnRegisterSessionNotification,
        },
        Threading::{GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources},
    },
    UI::{
        Controls::{EM_GETSEL, EM_LIMITTEXT, EM_SETREADONLY, EM_SETSEL},
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::{EnableWindow, GetFocus, SetFocus},
        WindowsAndMessaging::{
            BS_DEFPUSHBUTTON, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL,
            ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, ES_WANTRETURN, GWL_STYLE, GWLP_USERDATA,
            GetClientRect, GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindowLongPtrW,
            GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IDC_ARROW, IsDialogMessageW,
            IsWindow, IsWindowVisible, KillTimer, LoadCursorW, LoadIconW, MINMAXINFO, MSG,
            MoveWindow, PostMessageW, PostQuitMessage, RegisterClassExW, SM_REMOTESESSION, SW_HIDE,
            SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
            SWP_SHOWWINDOW, SendMessageW, SetTimer, SetWindowLongPtrW, SetWindowPos,
            SetWindowTextW, ShowWindow, TranslateMessage, WM_CHAR, WM_CLOSE, WM_COMMAND,
            WM_DESTROY, WM_DPICHANGED, WM_GETMINMAXINFO, WM_NCCREATE, WM_NCDESTROY, WM_SETFONT,
            WM_SIZE, WM_TIMER, WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_BORDER, WS_CHILD, WS_HSCROLL,
            WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
        },
    },
};

const QUEUE_CAPACITY: usize = 4096;
// 模型接收仍受 4 毫秒预算限制；控件呈现另行分块，不让小显示片段压低接收吞吐。
const MAX_DRAIN_EVENTS: usize = 1024;
const HEADING: i32 = 101;
const SUBTITLE: i32 = 102;
const STATUS: i32 = 103;
const ENVIRONMENT: i32 = 104;
const QUERY: i32 = 110;
const SSH: i32 = 111;
const CONSOLE: i32 = 112;
const STRESS: i32 = 113;
const OBSERVE: i32 = 114;
const STOP: i32 = 115;
const CONNECT: i32 = 116;
const NOTE: i32 = 117;
const RECENT: i32 = 118;
const OBS_HEADER: i32 = 201;
const TERMINAL: i32 = 202;
const OBS_STATUS: i32 = 203;
const FOLLOW: i32 = 204;
const HIDE: i32 = 205;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

struct Core {
    transcript: Transcript,
    backend: Backend,
    receiver: Receiver<Event>,
    status: String,
    recent: String,
    dirty: bool,
    was_busy: bool,
}

#[derive(Default)]
struct ProductTimings {
    host: Cell<u128>,
    receive: Cell<u128>,
    observer: Cell<u128>,
    flush: Cell<u128>,
    refresh: Cell<u128>,
    page: Cell<u128>,
    fonts: Cell<u128>,
    theme: Cell<u128>,
    create_controls: Cell<u128>,
    destroy_controls: Cell<u128>,
    visibility: Cell<u128>,
    layout: Cell<u128>,
}

impl ProductTimings {
    fn reset(&self) {
        for maximum in [
            &self.host,
            &self.receive,
            &self.observer,
            &self.flush,
            &self.refresh,
            &self.page,
            &self.fonts,
            &self.theme,
            &self.create_controls,
            &self.destroy_controls,
            &self.visibility,
            &self.layout,
        ] {
            maximum.set(0);
        }
    }
}

struct PhaseTimer<'a> {
    maximum: &'a Cell<u128>,
    started: Instant,
}
impl<'a> PhaseTimer<'a> {
    fn new(maximum: &'a Cell<u128>) -> Self {
        Self {
            maximum,
            started: Instant::now(),
        }
    }
}
impl Drop for PhaseTimer<'_> {
    fn drop(&mut self) {
        self.maximum
            .set(self.maximum.get().max(self.started.elapsed().as_millis()));
    }
}

struct App {
    options: Options,
    product: RefCell<ProductHost>,
    product_ui: RefCell<product_ui::ProductUi>,
    product_timings: ProductTimings,
    startup_prewarm_ms: Cell<u128>,
    startup_ui_ms: Cell<u128>,
    startup_control_count: Cell<usize>,
    core: RefCell<Core>,
    main: Cell<HWND>,
    observer: Cell<HWND>,
    main_controls: RefCell<BTreeMap<i32, HWND>>,
    observer_controls: RefCell<BTreeMap<i32, HWND>>,
    main_fonts: Cell<[HFONT; 3]>,
    observer_fonts: Cell<[HFONT; 3]>,
    dpi: Cell<u32>,
    observer_dpi: Cell<u32>,
    follow: Cell<bool>,
    allow_observer: Cell<bool>,
    // 用户暂停或选中复制时，不要求后台完成状态等待呈现追赶。
    render_blocked: Cell<bool>,
    // 正常关闭前核对真实控件文本，防止仅有空 pending 被误判为已显示。
    rendered_text_matches_model_on_close: Cell<Option<bool>>,
    started: Instant,
    test_started: Cell<bool>,
    closing: Cell<bool>,
    callback_failed: Cell<bool>,
    ticks: Cell<u64>,
    max_tick_ms: Cell<u128>,
    peak_drain_events: Cell<usize>,
    session_notifications: Cell<u32>,
    checks: RefCell<Vec<Check>>,
    log_view: log_view::LogView,
    tick_histogram: RefCell<[u64; 1001]>,
    last_tick: Cell<Option<Instant>>,
    max_timer_gap_ms: Cell<u128>,
    stress_observer_visible: Cell<bool>,
}

impl App {
    fn diagnostic(&self) -> bool {
        self.options.demo || self.options.self_test
    }

    fn check(&self, name: &str, passed: bool, detail: impl Into<String>) {
        self.checks.borrow_mut().push(Check {
            name: name.to_owned(),
            passed,
            detail: detail.into(),
        });
    }

    fn control(&self, id: i32) -> HWND {
        if !self.diagnostic() {
            let product_control = product_ui::control(self, id);
            if !product_control.is_null() {
                return product_control;
            }
        }
        self.main_controls
            .borrow()
            .get(&id)
            .copied()
            .or_else(|| self.observer_controls.borrow().get(&id).copied())
            .unwrap_or(null_mut())
    }

    fn set_status(&self, value: impl Into<String>) {
        let value = value.into();
        self.core.borrow_mut().status.clone_from(&value);
        set_text(self.control(STATUS), &value);
    }

    fn open_observer(&self) -> Result<(), String> {
        if self.observer.get().is_null() {
            create_observer(self)?;
        }
        // 不设置焦点，不使用永久 NOACTIVATE 样式，用户仍可主动选择和复制。
        unsafe {
            SetWindowPos(
                self.observer.get(),
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        self.core.borrow_mut().dirty = true;
        Ok(())
    }

    fn begin(&self, scenario: Scenario) {
        self.allow_observer.set(true);
        let result = {
            let mut core = self.core.borrow_mut();
            let result = core.backend.start(scenario);
            if result.is_ok() {
                core.was_busy = true;
            }
            result
        };
        if let Err(error) = result {
            self.set_status(error);
        }
    }

    fn layout(&self, observer: bool, dpi: u32) {
        if !observer && !self.diagnostic() {
            if !product_ui::changing_page(self) {
                product_ui::layout(self, dpi);
            }
            return;
        }
        let parent = if observer {
            self.observer.get()
        } else {
            self.main.get()
        };
        if parent.is_null() {
            return;
        }
        let mut rect = RECT::default();
        unsafe {
            GetClientRect(parent, &raw mut rect);
        }
        let s = |v| scaled(v, dpi);
        let width = rect.right.max(s(360));
        let height = rect.bottom.max(s(280));
        let controls = if observer {
            self.observer_controls.borrow().clone()
        } else {
            self.main_controls.borrow().clone()
        };
        let place = |id: i32, x: i32, y: i32, w: i32, h: i32| {
            if let Some(handle) = controls.get(&id) {
                unsafe {
                    MoveWindow(*handle, x, y, w.max(1), h.max(1), 1);
                }
            }
        };
        if observer {
            place(OBS_HEADER, s(18), s(14), width - s(36), s(26));
            place(TERMINAL, s(18), s(50), width - s(36), height - s(142));
            place(OBS_STATUS, s(18), height - s(82), width - s(36), s(26));
            place(FOLLOW, s(18), height - s(44), s(150), s(30));
            place(HIDE, width - s(168), height - s(44), s(150), s(30));
        } else {
            place(HEADING, s(22), s(20), width - s(44), s(32));
            place(SUBTITLE, s(22), s(60), width - s(44), s(46));
            place(STATUS, s(22), s(112), width - s(44), s(26));
            let button_width = (width - s(64)) / 3;
            for (column, id) in [QUERY, SSH, CONSOLE].into_iter().enumerate() {
                place(
                    id,
                    s(22) + (button_width + s(10)) * column as i32,
                    s(152),
                    button_width,
                    s(34),
                );
            }
            for (column, id) in [STRESS, OBSERVE, STOP].into_iter().enumerate() {
                place(
                    id,
                    s(22) + (button_width + s(10)) * column as i32,
                    s(198),
                    button_width,
                    s(34),
                );
            }
            place(CONNECT, s(22), s(246), width - s(44), s(34));
            place(NOTE, s(22), s(290), width - s(44), s(40));
            place(RECENT, s(22), height - s(96), width - s(44), s(44));
            place(ENVIRONMENT, s(22), height - s(44), width - s(44), s(24));
        }
    }

    fn fonts(&self, observer: bool, dpi: u32) -> Result<(), String> {
        let _timer = (!self.diagnostic()).then(|| PhaseTimer::new(&self.product_timings.fonts));
        let make = |size, weight, name: &str| unsafe {
            CreateFontW(
                -scaled(size, dpi),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                u32::from(DEFAULT_CHARSET),
                0,
                0,
                5,
                u32::from(DEFAULT_PITCH),
                wide(name).as_ptr(),
            )
        };
        let fonts = [
            // 日志优先中文系统字体，避免每批插入触发西文字体的中文回退排版。
            make(14, 400, "Microsoft YaHei UI"),
            make(22, 600, "Microsoft YaHei UI"),
            make(
                if observer || self.diagnostic() {
                    14
                } else {
                    42
                },
                if observer || self.diagnostic() {
                    400
                } else {
                    600
                },
                "Microsoft YaHei UI",
            ),
        ];
        if fonts.iter().any(|font| font.is_null()) {
            delete_fonts(fonts);
            return Err("无法创建系统字体".to_owned());
        }
        let controls = if !observer && !self.diagnostic() {
            product_ui::font_controls(self).into_iter().collect()
        } else if observer {
            self.observer_controls.borrow().clone()
        } else {
            self.main_controls.borrow().clone()
        };
        for (id, handle) in controls {
            let font = if id == product_ui::CODE_ID && !observer && !self.diagnostic() {
                fonts[2]
            } else if id == HEADING || (!observer && product_ui::is_heading(id)) {
                fonts[1]
            } else if id == TERMINAL {
                fonts[2]
            } else {
                fonts[0]
            };
            unsafe {
                SendMessageW(handle, WM_SETFONT, font as usize, 1);
            }
        }
        let old = if observer {
            self.observer_fonts.replace(fonts)
        } else {
            self.main_fonts.replace(fonts)
        };
        delete_fonts(old);
        Ok(())
    }

    fn flush(&self) {
        let english = !self.diagnostic() && product_ui::english(self);
        let (dropped_observations, product_busy) = product_ui::observation_state(self);
        let mut start = 0u32;
        let mut end = 0u32;
        let terminal = self.control(TERMINAL);
        if !terminal.is_null() {
            unsafe {
                SendMessageW(
                    terminal,
                    EM_GETSEL,
                    (&raw mut start) as usize,
                    (&raw mut end) as isize,
                );
            }
        }
        let selected = start != end && unsafe { GetFocus() } == terminal;
        let snapshot = {
            let mut core = self.core.borrow_mut();
            let can_render = !terminal.is_null() && self.follow.get() && !selected;
            self.render_blocked.set(!can_render);
            let needs_render =
                can_render && self.log_view.cursor() != Some(core.transcript.cursor());
            if !core.dirty && !needs_render {
                return;
            }
            let text = needs_render.then(|| core.transcript.patch_since(self.log_view.cursor()));
            core.dirty = false;
            (
                text,
                core.status.clone(),
                core.recent.clone(),
                if !self.diagnostic() && english {
                    format!(
                        "{} lines retained · {} history lines trimmed · {} display records omitted",
                        core.transcript.retained_lines(),
                        core.transcript.trimmed_lines(),
                        dropped_observations
                    )
                } else if !self.diagnostic() {
                    format!(
                        "保留 {} 行 · 历史裁剪 {} 行 · 显示遗漏 {} 条",
                        core.transcript.retained_lines(),
                        core.transcript.trimmed_lines(),
                        dropped_observations
                    )
                } else if english {
                    format!(
                        "{} lines retained · {} history lines trimmed · {} queue records dropped",
                        core.transcript.retained_lines(),
                        core.transcript.trimmed_lines(),
                        core.backend.dropped_lines()
                    )
                } else {
                    format!(
                        "保留 {} 行 · 历史裁剪 {} 行 · 队列丢弃 {} 条",
                        core.transcript.retained_lines(),
                        core.transcript.trimmed_lines(),
                        core.backend.dropped_lines()
                    )
                },
                if self.diagnostic() {
                    core.backend.busy()
                } else {
                    product_busy
                },
            )
        };
        set_text(self.control(STATUS), &snapshot.1);
        set_text(self.control(RECENT), &snapshot.2);
        set_text(self.control(OBS_STATUS), &snapshot.3);
        if let Some(patch) = snapshot.0 {
            self.log_view.apply_streaming(terminal, &patch, snapshot.4);
        }
    }

    // 接收预算与原生控件呈现分离，只有模型和视图都追赶后才结束正常任务。
    fn receive_events(&self, started: Instant) -> (usize, bool) {
        let mut received = 0usize;
        let mut line_seen = false;
        {
            let mut core = self.core.borrow_mut();
            while received < MAX_DRAIN_EVENTS && started.elapsed() < Duration::from_millis(4) {
                let Ok(event) = core.receiver.try_recv() else {
                    break;
                };
                received += 1;
                match event {
                    Event::Line(text) => {
                        core.transcript.append(&text);
                        core.recent = text.chars().take(100).collect();
                        line_seen = true;
                    }
                    Event::Status(text) => core.status = text,
                    Event::Finished => "本次任务已结束".clone_into(&mut core.status),
                }
                core.dirty = true;
            }
            if core.was_busy
                && !core.backend.busy()
                && received == 0
                && (self.render_blocked.get()
                    || self.log_view.cursor() == Some(core.transcript.cursor()))
            {
                core.was_busy = false;
                "本次任务已结束，日志已接收".clone_into(&mut core.status);
                core.dirty = true;
            }
        }
        (received, line_seen)
    }

    fn product_tick(&self) {
        // 宿主返回的观察记录已经脱敏；UI 不运行第二份 Agent，也不持有认证输入。
        let observations = {
            let _timer = PhaseTimer::new(&self.product_timings.host);
            self.product.borrow_mut().tick()
        };
        self.peak_drain_events
            .set(self.peak_drain_events.get().max(observations.len()));
        let mut received = false;
        let receive_timer = PhaseTimer::new(&self.product_timings.receive);
        for observation in observations {
            if observation.new_task {
                self.allow_observer.set(true);
            }
            let mut core = self.core.borrow_mut();
            core.transcript.append(&observation.text);
            core.dirty = true;
            received = true;
        }
        drop(receive_timer);
        if received
            && self.allow_observer.get()
            && (self.observer.get().is_null()
                || unsafe { IsWindowVisible(self.observer.get()) } == 0)
        {
            let _timer = PhaseTimer::new(&self.product_timings.observer);
            let _ = self.open_observer();
        }
        {
            let _timer = PhaseTimer::new(&self.product_timings.flush);
            self.flush();
        }
        {
            let _timer = PhaseTimer::new(&self.product_timings.refresh);
            product_ui::refresh_tick(self);
        }
    }

    fn record_tick_start(&self, started: Instant) {
        if let Some(previous) = self.last_tick.replace(Some(started)) {
            self.max_timer_gap_ms.set(
                self.max_timer_gap_ms
                    .get()
                    .max(started.duration_since(previous).as_millis()),
            );
        }
    }

    fn record_tick_end(&self, started: Instant) {
        let elapsed_ms = started.elapsed().as_millis();
        self.max_tick_ms.set(self.max_tick_ms.get().max(elapsed_ms));
        self.tick_histogram.borrow_mut()[elapsed_ms.min(1000) as usize] += 1;
    }

    fn tick(&self) {
        self.ticks.set(self.ticks.get() + 1);
        if !self.diagnostic() {
            let started = Instant::now();
            self.record_tick_start(started);
            self.product_tick();
            self.record_tick_end(started);
            return;
        }
        if self.options.self_test && !self.test_started.replace(true) {
            self.basic_checks();
            let _ = self.open_observer();
            self.begin(Scenario::Stress(self.options.stress_lines));
        }
        let started = Instant::now();
        self.record_tick_start(started);
        let (received, line_seen) = self.receive_events(started);
        self.peak_drain_events
            .set(self.peak_drain_events.get().max(received));
        if line_seen
            && self.allow_observer.get()
            && !self.options.self_test
            && (self.observer.get().is_null()
                || unsafe { IsWindowVisible(self.observer.get()) } == 0)
            && let Err(error) = self.open_observer()
        {
            self.set_status(error);
        }
        self.flush();
        self.record_tick_end(started);
        if self.options.self_test && self.test_started.get() {
            if line_seen && unsafe { IsWindowVisible(self.observer.get()) } == 0 {
                self.stress_observer_visible.set(false);
            }
            let elapsed = self.started.elapsed().as_millis();
            let timed_exit = self
                .options
                .exit_after_ms
                .is_some_and(|limit| elapsed >= u128::from(limit))
                && self.core.borrow().backend.produced_lines() > 0;
            let finished = {
                let core = self.core.borrow();
                !core.backend.busy()
                    && received == 0
                    && self.log_view.cursor() == Some(core.transcript.cursor())
            };
            if timed_exit || finished || elapsed > 60_000 {
                if !timed_exit {
                    self.check(
                        "rendered_transcript_matches_model",
                        log_view::text(self.control(TERMINAL))
                            == self.core.borrow().transcript.text(),
                        "真实可见 标准多行 EDIT 与脱敏有界模型逐字一致",
                    );
                }
                self.check(
                    "stress_completed_or_cancelled",
                    timed_exit || finished,
                    if timed_exit {
                        "输出期间请求退出"
                    } else {
                        "后台完成且事件队列排空"
                    },
                );
                if !timed_exit {
                    let produced = self.core.borrow().backend.produced_lines();
                    self.check(
                        "stress_produced_requested_lines",
                        produced == self.options.stress_lines,
                        format!("实际生成 {produced} 条"),
                    );
                }
                unsafe {
                    PostMessageW(self.main.get(), WM_CLOSE, 0, 0);
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)] // 原生消息表或隔离自测保持同一生命周期，避免拆散清理顺序。
    fn basic_checks(&self) {
        let foreground = unsafe { GetForegroundWindow() };
        let opened = self.open_observer().is_ok();
        self.check(
            "native_main_and_observer",
            opened && !self.observer.get().is_null(),
            "User32 + Unicode 标准多行 EDIT，无 GPU 渲染器",
        );
        self.check(
            "observer_does_not_activate",
            unsafe { GetForegroundWindow() } == foreground,
            "自动出现前后前台窗口保持一致",
        );
        let terminal = self.control(TERMINAL);
        set_text(
            terminal,
            "中文验证：交换机、观察窗、路径含空格 🔎\r\n{\\rtf1 这是一条普通日志}",
        );
        let before = get_text(terminal);
        unsafe {
            SendMessageW(terminal, WM_CHAR, usize::from(b'X'), 0);
        }
        self.check(
            "unicode_round_trip",
            before.contains("交换机、观察窗") && before.contains('🔎') && !before.contains('�'),
            "Windows UTF-16 中文与代理对往返",
        );
        self.check(
            "readonly_keyboard_rejected",
            before == get_text(terminal)
                && unsafe { GetWindowLongPtrW(terminal, GWL_STYLE) }
                    & isize::from(ES_READONLY as i16)
                    != 0,
            "只读控件拒绝 WM_CHAR，程序仍可追加文本",
        );
        self.check(
            "log_is_plain_text",
            before.contains("{\\rtf1"),
            "输出不会作为 RTF 文档解释",
        );
        self.render_checks();
        self.app_render_checks();
        self.chunk_render_checks();
        let original_dpi = self.dpi.get();
        let original_observer_dpi = self.observer_dpi.get();
        for dpi in [96, 120, 144, 192] {
            self.dpi.set(dpi);
            self.observer_dpi.set(dpi);
            resize_client(self.main.get(), 600, 460, dpi);
            resize_client(self.observer.get(), 800, 500, dpi);
            let fonts = self.fonts(false, dpi).is_ok() && self.fonts(true, dpi).is_ok();
            self.layout(false, dpi);
            self.layout(true, dpi);
            let mut minimum = MINMAXINFO::default();
            unsafe {
                SendMessageW(
                    self.main.get(),
                    WM_GETMINMAXINFO,
                    0,
                    (&raw mut minimum) as isize,
                );
                SetWindowPos(
                    self.main.get(),
                    null_mut(),
                    0,
                    0,
                    minimum.ptMinTrackSize.x,
                    minimum.ptMinTrackSize.y,
                    SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER,
                );
            }
            let mut minimum_client = RECT::default();
            unsafe {
                GetClientRect(self.main.get(), &raw mut minimum_client);
            }
            self.check(
                &format!("minimum_client_{dpi}"),
                minimum_client.right >= scaled(600, dpi)
                    && minimum_client.bottom >= scaled(460, dpi),
                "拖到最小外框仍能容纳完整客户区",
            );
            self.check(
                &format!("dpi_layout_{dpi}"),
                fonts
                    && controls_fit(self.main.get(), &self.main_controls.borrow())
                    && controls_fit(self.observer.get(), &self.observer_controls.borrow()),
                "注入逻辑 DPI 的布局检查，不改变系统显示缩放",
            );
        }
        self.dpi.set(original_dpi);
        self.observer_dpi.set(original_observer_dpi);
        resize_client(self.main.get(), 600, 460, original_dpi);
        resize_client(self.observer.get(), 800, 500, original_observer_dpi);
        let _ = self.fonts(false, original_dpi);
        let _ = self.fonts(true, original_observer_dpi);
        self.layout(false, original_dpi);
        self.layout(true, original_observer_dpi);
        unsafe {
            SendMessageW(self.main.get(), WM_WTSSESSION_CHANGE, 4, 0);
            SendMessageW(self.main.get(), WM_WTSSESSION_CHANGE, 3, 0);
        }
        self.check(
            "session_notification_handler",
            self.session_notifications.get() == 2,
            "仅验证远程会话消息处理，未实际建立/断开 RDP",
        );
        let before_gdi = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        for _ in 0..10 {
            unsafe {
                DestroyWindow(self.observer.get());
            }
            delete_fonts(self.observer_fonts.replace([null_mut(); 3]));
            let _ = self.open_observer();
        }
        let after_gdi = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        self.check(
            "observer_recreate_gdi_bounded",
            after_gdi <= before_gdi + 4,
            format!("十轮重建 GDI 对象 {before_gdi} → {after_gdi}"),
        );
        unsafe {
            SendMessageW(self.observer.get(), WM_CLOSE, 0, 0);
        }
        self.check(
            "observer_close_only_hides",
            unsafe {
                IsWindow(self.observer.get()) != 0 && IsWindowVisible(self.observer.get()) == 0
            },
            "关闭观察窗保留主窗口和任务宿主",
        );
        self.allow_observer.set(true);
    }

    fn render_checks(&self) {
        let terminal = self.control(TERMINAL);
        let view = log_view::LogView::default();
        let mut transcript = Transcript::new(3, 1000);
        for index in 0..10 {
            transcript.append(&format!("中文 {index} 🔎\n第二段 {index}"));
            view.apply(terminal, &transcript.patch_since(view.cursor()));
            self.check(
                &format!("edit_incremental_{index}"),
                log_view::text(terminal) == transcript.text(),
                "真实 标准多行 EDIT 的 UTF-16 / CRLF 段落裁剪与模型一致",
            );
        }
        let before_pause = log_view::text(terminal);
        for _ in 0..10 {
            transcript.append("暂停后追赶 🔎");
        }
        self.check(
            "paused_view_preserved",
            log_view::text(terminal) == before_pause,
            "模型继续接收时未改动正在阅读的控件",
        );
        view.apply(terminal, &transcript.patch_since(view.cursor()));
        self.check(
            "paused_view_catches_up",
            log_view::text(terminal) == transcript.text(),
            "旧视图完全落后后恢复最近历史",
        );
        self.log_view.reset_window();
    }

    #[allow(clippy::too_many_lines)] // 两种真实控件场景共用一次隔离与恢复，便于核对状态边界。
    fn chunk_render_checks(&self) {
        let terminal = self.control(TERMINAL);
        let view = log_view::LogView::default();
        let mut transcript = Transcript::new(256, 32_768);
        for index in 0..180 {
            transcript.append(&format!("旧目标第{index}行：中文状态 🔎"));
        }
        view.apply(terminal, &transcript.patch_since(view.cursor()));
        self.check(
            "chunk_initial_target_is_partial",
            view.cursor().is_none() && view.deferred_metrics().2 > 0,
            "大目标首次仅呈现一个片段，保留待追赶文本",
        );
        transcript.append("最新目标：旧中间帧可被取代，最终记录必须完整");
        for _ in 0..100 {
            if view.cursor() == Some(transcript.cursor()) {
                break;
            }
            view.apply(terminal, &transcript.patch_since(view.cursor()));
        }
        let (coalesced, peak, pending) = view.deferred_metrics();
        self.check(
            "chunk_superseded_target_catches_up",
            coalesced > 0
                && peak <= log_view::MAX_APPEND_UNITS
                && pending == 0
                && view.cursor() == Some(transcript.cursor())
                && log_view::text(terminal) == transcript.text(),
            "取代部分旧帧后，无新日志也能分块完成最新模型，未重复或漏显最终历史",
        );

        let (saved_transcript, saved_dirty) = {
            let mut core = self.core.borrow_mut();
            (std::mem::take(&mut core.transcript), core.dirty)
        };
        let saved_follow = self.follow.replace(true);
        let saved_blocked = self.render_blocked.get();
        let previous_focus = unsafe { GetFocus() };
        self.log_view.reset_window();
        let record = "{\\rtf1 字面长日志} ".to_owned() + &"中文🔎".repeat(2_048);
        self.core.borrow_mut().transcript.append(&record);
        self.flush();
        let partial = log_view::text(terminal);
        let remaining = self.log_view.deferred_metrics().2;
        self.check(
            "chunk_long_line_is_partial",
            record.encode_utf16().count() >= 8192
                && !partial.is_empty()
                && self.log_view.cursor().is_none()
                && remaining > 0,
            "超过八千 UTF-16 单元的中文与 emoji 单行分块呈现",
        );
        self.follow.set(false);
        self.flush();
        self.check(
            "chunk_pause_preserves_partial",
            log_view::text(terminal) == partial && self.log_view.deferred_metrics().2 == remaining,
            "暂停跟随时保留已经显示的部分长行和剩余进度",
        );
        self.follow.set(true);
        unsafe {
            SetFocus(terminal);
            SendMessageW(terminal, EM_SETSEL, 0, 4);
        }
        self.flush();
        self.check(
            "chunk_selection_preserves_partial",
            unsafe { GetFocus() } == terminal
                && log_view::text(terminal) == partial
                && self.log_view.deferred_metrics().2 == remaining,
            "选中复制时不改动部分文本或推进待呈现片段",
        );
        unsafe {
            SendMessageW(terminal, EM_SETSEL, 0, 0);
            SetFocus(previous_focus);
        }
        for _ in 0..100 {
            let target = self.core.borrow().transcript.cursor();
            if self.log_view.cursor() == Some(target) {
                break;
            }
            self.flush();
        }
        let rendered = log_view::text(terminal);
        let target = self.core.borrow().transcript.cursor();
        let expected = self.core.borrow().transcript.text();
        self.check(
            "chunk_long_line_resumes_without_new_output",
            self.log_view.cursor() == Some(target)
                && self.log_view.deferred_metrics().2 == 0
                && self.log_view.deferred_metrics().1 <= log_view::MAX_APPEND_UNITS
                && rendered == expected
                && rendered.starts_with("{\\rtf1"),
            "解除暂停与选择后，无新日志也逐字追赶中文、代理对和字面 RTF 长行",
        );
        {
            let mut core = self.core.borrow_mut();
            core.transcript = saved_transcript;
            core.dirty = saved_dirty;
        }
        self.follow.set(saved_follow);
        self.render_blocked.set(saved_blocked);
        self.log_view.reset_window();
    }
    fn app_render_checks(&self) {
        let terminal = self.control(TERMINAL);
        let saved = std::mem::take(&mut self.core.borrow_mut().transcript);
        self.log_view.reset_window();
        self.core.borrow_mut().transcript.append("保留选中文字 🔎");
        self.flush();
        let initial = log_view::text(terminal);
        self.follow.set(false);
        self.core.borrow_mut().transcript.append("暂停期间继续接收");
        self.flush();
        self.check(
            "app_paused_view_preserved",
            log_view::text(terminal) == initial,
            "真实 flush 路径在暂停跟随时保留控件文本",
        );
        self.follow.set(true);
        self.flush();
        self.check(
            "app_resume_catches_up",
            log_view::text(terminal) == self.core.borrow().transcript.text(),
            "恢复跟随后追赶模型，无需等待下一条日志",
        );
        let previous_focus = unsafe { GetFocus() };
        unsafe {
            SetFocus(terminal);
            SendMessageW(terminal, EM_SETSEL, 0, 4);
        }
        let selected = log_view::text(terminal);
        self.core.borrow_mut().transcript.append("选中期间继续接收");
        self.flush();
        self.check(
            "app_selection_preserved",
            unsafe { GetFocus() } == terminal && log_view::text(terminal) == selected,
            "选中复制期间不覆盖文本或改变选择",
        );
        unsafe {
            SendMessageW(terminal, EM_SETSEL, 0, 0);
            SetFocus(previous_focus);
        }
        self.flush();
        self.check(
            "app_selection_release_catches_up",
            log_view::text(terminal) == self.core.borrow().transcript.text(),
            "解除选择后追加之前积累的记录",
        );
        unsafe {
            ShowWindow(self.observer.get(), SW_HIDE);
        }
        self.core.borrow_mut().transcript.append("隐藏期间继续接收");
        self.flush();
        self.check(
            "hidden_flush_keeps_observer_hidden",
            unsafe { IsWindowVisible(self.observer.get()) } == 0,
            "隐藏期间刷新不会被 WM_SETREDRAW 意外显示",
        );
        self.core.borrow_mut().transcript = saved;
        self.log_view.reset_window();
        let _ = self.open_observer();
    }
}

fn delete_fonts(fonts: [HFONT; 3]) {
    for font in fonts {
        if !font.is_null() {
            unsafe {
                DeleteObject(font);
            }
        }
    }
}

fn set_text(handle: HWND, text: &str) {
    if !handle.is_null() {
        unsafe {
            SetWindowTextW(handle, wide(text).as_ptr());
        }
    }
}

fn get_text(handle: HWND) -> String {
    if handle.is_null() {
        return String::new();
    }
    let length = unsafe { GetWindowTextLengthW(handle) }.max(0) as usize;
    let mut buffer = vec![0u16; length + 1];
    let copied =
        unsafe { GetWindowTextW(handle, buffer.as_mut_ptr(), buffer.len() as i32) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

fn controls_fit(parent: HWND, controls: &BTreeMap<i32, HWND>) -> bool {
    let mut client = RECT::default();
    unsafe {
        GetClientRect(parent, &raw mut client);
    }
    controls.values().all(|handle| {
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(*handle, &raw mut rect);
        }
        let mut points = [
            POINT {
                x: rect.left,
                y: rect.top,
            },
            POINT {
                x: rect.right,
                y: rect.bottom,
            },
        ];
        unsafe {
            MapWindowPoints(null_mut(), parent, points.as_mut_ptr(), 2);
        }
        points[0].x >= 0
            && points[0].y >= 0
            && points[1].x <= client.right
            && points[1].y <= client.bottom
            && rect.right > rect.left
            && rect.bottom > rect.top
    })
}

fn resize_client(window: HWND, width: i32, height: i32, dpi: u32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scaled(width, dpi),
        bottom: scaled(height, dpi),
    };
    unsafe {
        windows_sys::Win32::UI::HiDpi::AdjustWindowRectExForDpi(
            &raw mut rect,
            GetWindowLongPtrW(window, GWL_STYLE) as u32,
            0,
            0,
            dpi,
        );
        SetWindowPos(
            window,
            null_mut(),
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn child(parent: HWND, id: i32, class: &str, text: &str, style: u32) -> Result<HWND, String> {
    let handle = unsafe {
        CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            1,
            1,
            parent,
            id as usize as _,
            GetModuleHandleW(null()),
            null(),
        )
    };
    if handle.is_null() {
        Err(format!(
            "创建原生控件失败：{}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(handle)
    }
}

fn create_observer(app: &App) -> Result<(), String> {
    let window = unsafe {
        CreateWindowExW(
            0,
            wide("RemoteOpsNativeObserver").as_ptr(),
            wide("RemoteOps · 只读操作观察窗").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            800,
            500,
            app.main.get(),
            null_mut(),
            GetModuleHandleW(null()),
            std::ptr::from_ref(app).cast(),
        )
    };
    if window.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    app.observer.set(window);
    let result = (|| {
        let entries = [
            (OBS_HEADER, "STATIC", "操作实况 · 只读观看，认证信息隐藏", 0),
            (
                TERMINAL,
                "EDIT",
                "",
                (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | ES_AUTOHSCROLL | ES_WANTRETURN)
                    as u32
                    | WS_VSCROLL
                    | WS_HSCROLL
                    | WS_TABSTOP
                    | WS_BORDER,
            ),
            (OBS_STATUS, "STATIC", "等待操作", 0),
            (FOLLOW, "BUTTON", "暂停跟随", WS_TABSTOP),
            (HIDE, "BUTTON", "关闭观察窗", WS_TABSTOP),
        ];
        for (id, class, text, style) in entries {
            let handle = child(window, id, class, text, style)?;
            app.observer_controls.borrow_mut().insert(id, handle);
        }
        let terminal = app.control(TERMINAL);
        app.log_view.reset_window();
        log_view::disable_undo(terminal);
        unsafe {
            SendMessageW(terminal, EM_LIMITTEXT, 300_000, 0);
            SendMessageW(terminal, EM_SETREADONLY, 1, 0);
        }
        let dpi = app
            .options
            .preview_dpi
            .unwrap_or_else(|| unsafe { GetDpiForWindow(window) }.max(96));
        app.observer_dpi.set(dpi);
        app.fonts(true, dpi)?;
        resize_client(window, 800, 500, dpi);
        app.layout(true, dpi);
        if !app.diagnostic() {
            product_ui::observer_created(app);
        }
        Ok(())
    })();
    if result.is_err() {
        unsafe {
            DestroyWindow(window);
        }
        delete_fonts(app.observer_fonts.replace([null_mut(); 3]));
    }
    result
}

#[allow(clippy::too_many_lines)] // 原生消息表或隔离自测保持同一生命周期，避免拆散清理顺序。
fn create_main(app: &App) -> Result<(), String> {
    if !app.diagnostic() {
        return product_ui::create(app);
    }
    let instance = unsafe { GetModuleHandleW(null()) };
    let window = unsafe {
        CreateWindowExW(
            0,
            wide("RemoteOpsNativeMain").as_ptr(),
            wide("RemoteOps · Windows 原生界面小样").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            600,
            460,
            null_mut(),
            null_mut(),
            instance,
            std::ptr::from_ref(app).cast(),
        )
    };
    if window.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    app.main.set(window);
    let mode = if cfg!(feature = "agent-runtime") {
        "Agent 核心版"
    } else {
        "仅界面版"
    };
    let entries = [
        (HEADING, "STATIC", "RemoteOps 原生界面验证", 0),
        (
            SUBTITLE,
            "STATIC",
            "独立小样 · 现有版本保持可用\r\n默认离线运行，SSH / Console 样例明确标注为演示。",
            0,
        ),
        (STATUS, "STATIC", "就绪，等待选择验证场景", 0),
        (QUERY, "BUTTON", "本地只读查询", WS_TABSTOP),
        (SSH, "BUTTON", "SSH 交互演示", WS_TABSTOP),
        (CONSOLE, "BUTTON", "Console 交互演示", WS_TABSTOP),
        (STRESS, "BUTTON", "10 万行压力测试", WS_TABSTOP),
        (
            OBSERVE,
            "BUTTON",
            "打开观察窗",
            WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
        ),
        (STOP, "BUTTON", "停止当前任务", WS_TABSTOP),
        (CONNECT, "BUTTON", "连接测试 Relay", WS_TABSTOP),
        (
            NOTE,
            "STATIC",
            "未指定测试配置，远程连接按钮不可用。\r\n观察窗可选择复制文本，不能向设备输入。",
            0,
        ),
        (RECENT, "STATIC", "尚无操作记录", 0),
        (ENVIRONMENT, "STATIC", mode, 0),
    ];
    for (id, class, text, style) in entries {
        let handle = child(window, id, class, text, style)?;
        app.main_controls.borrow_mut().insert(id, handle);
    }
    if app.options.stress_lines != 100_000 {
        set_text(
            app.control(STRESS),
            &format!("{} 行压力测试", app.options.stress_lines),
        );
    }
    unsafe {
        EnableWindow(
            app.control(CONNECT),
            i32::from(app.options.agent_config.is_some() && cfg!(feature = "agent-runtime")),
        );
    }
    if app.options.agent_config.is_some() {
        set_text(
            app.control(NOTE),
            "已指定独立测试配置，点击连接后才启动 Agent。\r\n权限和认证继续由既有 Agent 核心处理。",
        );
    }
    let dpi = app
        .options
        .preview_dpi
        .unwrap_or_else(|| unsafe { GetDpiForWindow(window) }.max(96));
    app.dpi.set(dpi);
    app.fonts(false, dpi)?;
    resize_client(window, 600, 460, dpi);
    app.layout(false, dpi);
    let remote = unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0;
    set_text(
        app.control(ENVIRONMENT),
        &format!(
            "{mode} · Win32 / 系统字体 · 布局 {dpi} DPI · {}",
            if remote {
                "RDP 会话"
            } else {
                "本地/未识别会话"
            }
        ),
    );
    unsafe {
        WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION);
        SetTimer(window, 1, 16, None);
    }
    if !app.options.self_test || app.options.visible {
        unsafe {
            ShowWindow(window, SW_SHOWNOACTIVATE);
        }
    }
    Ok(())
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // 所有 FFI 回调捕获 panic；不允许 Rust 展开穿越 Windows 消息循环。
    catch_unwind(AssertUnwindSafe(|| dispatch(hwnd, message, wparam, lparam))).unwrap_or_else(
        |_| unsafe {
            let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const App;
            if !pointer.is_null() {
                (*pointer).callback_failed.set(true);
                PostMessageW((*pointer).main.get(), WM_CLOSE, 0, 0);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        },
    )
}

#[allow(clippy::too_many_lines)] // 原生消息表或隔离自测保持同一生命周期，避免拆散清理顺序。
fn dispatch(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // CREATESTRUCT 和 USERDATA 均只指向 run 持有的 Box；它在所有窗口销毁后才释放。
    unsafe {
        if message == WM_NCCREATE {
            let creation = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, creation.lpCreateParams as isize);
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const App;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        let app = &*pointer;
        let observer = hwnd != app.main.get() && !app.main.get().is_null();
        if !app.diagnostic()
            && let Some(result) = product_ui::paint(app, hwnd, message, wparam, lparam)
        {
            return result;
        }
        match message {
            windows_sys::Win32::UI::WindowsAndMessaging::WM_CTLCOLORSTATIC => {
                use windows_sys::Win32::Graphics::Gdi as gdi;
                let dc = wparam as gdi::HDC;
                gdi::SetBkMode(dc, gdi::TRANSPARENT as i32);
                gdi::SetTextColor(dc, gdi::GetSysColor(gdi::COLOR_WINDOWTEXT));
                return gdi::GetSysColorBrush(gdi::COLOR_WINDOW) as isize;
            }
            WM_COMMAND => {
                if !observer && !app.diagnostic() {
                    let id = (wparam & 0xffff) as i32;
                    let source = lparam as HWND;
                    // 只接收真实按钮点击和对话框键盘命令，忽略编辑/焦点通知。
                    if (wparam >> 16) == 0
                        && ((!source.is_null() && product_ui::control(app, id) == source)
                            || (source.is_null() && matches!(id, 1 | 2)))
                    {
                        product_ui::command(app, id);
                    }
                    return 0;
                }
                match (wparam & 0xffff) as i32 {
                    QUERY => app.begin(Scenario::LocalQuery),
                    SSH => app.begin(Scenario::SshDemo),
                    CONSOLE => app.begin(Scenario::ConsoleDemo),
                    STRESS => app.begin(Scenario::Stress(app.options.stress_lines)),
                    OBSERVE => {
                        app.allow_observer.set(true);
                        if let Err(e) = app.open_observer() {
                            app.set_status(e);
                        }
                    }
                    STOP => {
                        app.core.borrow().backend.stop();
                        app.set_status("正在停止当前任务");
                    }
                    FOLLOW => {
                        let follow = !app.follow.get();
                        app.follow.set(follow);
                        set_text(
                            app.control(FOLLOW),
                            if follow {
                                "暂停跟随"
                            } else {
                                "返回最新"
                            },
                        );
                        app.core.borrow_mut().dirty = true;
                        app.flush();
                    }
                    HIDE => {
                        SendMessageW(app.observer.get(), WM_CLOSE, 0, 0);
                    }
                    CONNECT =>
                    {
                        #[cfg(feature = "agent-runtime")]
                        if let Some(path) = &app.options.agent_config {
                            let result = {
                                let mut core = app.core.borrow_mut();
                                let result = core.backend.start_agent(path);
                                if result.is_ok() {
                                    core.was_busy = true;
                                }
                                result
                            };
                            if let Err(e) = result {
                                app.set_status(e);
                            } else {
                                app.allow_observer.set(true);
                            }
                        }
                    }
                    _ => {}
                }
                return 0;
            }
            WM_TIMER => {
                if !app.closing.get() {
                    app.tick();
                }
                return 0;
            }
            WM_SIZE => {
                app.layout(
                    observer,
                    if observer {
                        app.observer_dpi.get()
                    } else {
                        app.dpi.get()
                    },
                );
                return 0;
            }
            WM_DPICHANGED => {
                let dpi = (wparam & 0xffff) as u32;
                if observer {
                    app.observer_dpi.set(dpi);
                } else {
                    app.dpi.set(dpi);
                }
                if lparam != 0 {
                    let rect = *(lparam as *const RECT);
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                }
                let _ = app.fonts(observer, dpi);
                app.layout(observer, dpi);
                return 0;
            }
            WM_WTSSESSION_CHANGE => {
                app.session_notifications
                    .set(app.session_notifications.get() + 1);
                app.layout(false, app.dpi.get());
                return 0;
            }
            WM_GETMINMAXINFO => {
                let info = &mut *(lparam as *mut MINMAXINFO);
                let dpi = if observer {
                    app.observer_dpi.get()
                } else {
                    app.dpi.get()
                };
                let (width, height) = if !observer && !app.diagnostic() {
                    product_ui::minimum_size(app)
                } else if observer {
                    (500, 320)
                } else {
                    (600, 460)
                };
                let mut minimum = RECT {
                    left: 0,
                    top: 0,
                    right: scaled(width, dpi),
                    bottom: scaled(height, dpi),
                };
                windows_sys::Win32::UI::HiDpi::AdjustWindowRectExForDpi(
                    &raw mut minimum,
                    GetWindowLongPtrW(hwnd, GWL_STYLE) as u32,
                    0,
                    0,
                    dpi,
                );
                info.ptMinTrackSize = POINT {
                    x: minimum.right - minimum.left,
                    y: minimum.bottom - minimum.top,
                };
                if !observer && !app.diagnostic() {
                    info.ptMaxTrackSize = info.ptMinTrackSize;
                }
                return 0;
            }
            WM_CLOSE => {
                if observer {
                    app.allow_observer.set(false);
                    ShowWindow(hwnd, SW_HIDE);
                } else {
                    if !app.diagnostic()
                        && !app.callback_failed.get()
                        && !product_ui::request_close(app)
                    {
                        return 0;
                    }
                    let expected = app.core.borrow().transcript.text();
                    let actual = log_view::text(app.control(TERMINAL));
                    app.rendered_text_matches_model_on_close
                        .set(Some(actual == expected));
                    app.closing.set(true);
                    product_ui::shutdown_clipboard(app);
                    app.core.borrow().backend.stop();
                    KillTimer(hwnd, 1);
                    WTSUnRegisterSessionNotification(hwnd);
                    DestroyWindow(hwnd);
                }
                return 0;
            }
            WM_DESTROY => {
                if !observer {
                    PostQuitMessage(0);
                }
                return 0;
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if observer {
                    app.observer.set(null_mut());
                    app.observer_controls.borrow_mut().clear();
                }
                return DefWindowProcW(hwnd, message, wparam, lparam);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

fn register(instance: HINSTANCE, name: &str) -> Result<(), String> {
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        // MAKEINTRESOURCE(1) 编码资源 ID，不是可解引用的字符串指针。
        hIcon: unsafe { LoadIconW(instance, std::ptr::without_provenance(1)) },
        hIconSm: unsafe { LoadIconW(instance, std::ptr::without_provenance(1)) },
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        hbrBackground: unsafe { GetSysColorBrush(5) },
        ..Default::default()
    };
    // 类名临时 UTF-16 在 RegisterClassExW 返回前有效，系统复制类名。
    let name = wide(name);
    let class = WNDCLASSEXW {
        lpszClassName: name.as_ptr(),
        ..class
    };
    if unsafe { RegisterClassExW(&raw const class) } == 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_lines)] // 原生消息表或隔离自测保持同一生命周期，避免拆散清理顺序。
pub(super) fn run(options: Options) -> Result<Report, String> {
    let instance = unsafe { GetModuleHandleW(null()) };
    register(instance, "RemoteOpsNativeMain")?;
    register(instance, "RemoteOpsNativeObserver")?;
    let (sender, receiver) = sync_channel(QUEUE_CAPACITY);
    let auto_observer = options.self_test || options.demo;
    let product = ProductHost::new(&options);
    let app = Box::new(App {
        options,
        product: RefCell::new(product),
        product_ui: RefCell::new(product_ui::ProductUi::default()),
        product_timings: ProductTimings::default(),
        startup_prewarm_ms: Cell::new(0),
        startup_ui_ms: Cell::new(0),
        startup_control_count: Cell::new(0),
        core: RefCell::new(Core {
            transcript: Transcript::default(),
            backend: Backend::new(sender),
            receiver,
            status: "就绪".to_owned(),
            recent: String::new(),
            dirty: false,
            was_busy: false,
        }),
        main: Cell::new(null_mut()),
        observer: Cell::new(null_mut()),
        main_controls: RefCell::new(BTreeMap::new()),
        observer_controls: RefCell::new(BTreeMap::new()),
        main_fonts: Cell::new([null_mut(); 3]),
        observer_fonts: Cell::new([null_mut(); 3]),
        dpi: Cell::new(96),
        observer_dpi: Cell::new(96),
        follow: Cell::new(true),
        allow_observer: Cell::new(auto_observer),
        render_blocked: Cell::new(false),
        rendered_text_matches_model_on_close: Cell::new(None),
        started: Instant::now(),
        test_started: Cell::new(false),
        closing: Cell::new(false),
        callback_failed: Cell::new(false),
        ticks: Cell::new(0),
        max_tick_ms: Cell::new(0),
        peak_drain_events: Cell::new(0),
        session_notifications: Cell::new(0),
        checks: RefCell::new(Vec::new()),
        log_view: log_view::LogView::default(),
        tick_histogram: RefCell::new([0; 1001]),
        last_tick: Cell::new(None),
        max_timer_gap_ms: Cell::new(0),
        stress_observer_visible: Cell::new(true),
    });
    let created = create_main(&app);
    if let Err(error) = created {
        if !app.main.get().is_null() {
            unsafe {
                DestroyWindow(app.main.get());
            }
        }
        delete_fonts(app.main_fonts.replace([null_mut(); 3]));
        let _ = app.product.borrow_mut().shutdown();
        return Err(error);
    }
    let mut msg = MSG::default();
    let mut loop_error = None;
    loop {
        let result = unsafe { GetMessageW(&raw mut msg, null_mut(), 0, 0) };
        if result == 0 {
            break;
        }
        if result < 0 {
            loop_error = Some(std::io::Error::last_os_error().to_string());
            break;
        }
        unsafe {
            if IsDialogMessageW(app.main.get(), &raw const msg) == 0
                && (app.observer.get().is_null()
                    || IsDialogMessageW(app.observer.get(), &raw const msg) == 0)
            {
                TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
        }
    }
    // GetMessage 错误与正常退出共用清理路径，不能留下引用 App 的存活窗口。
    app.closing.set(true);
    unsafe {
        if IsWindow(app.main.get()) != 0 {
            DestroyWindow(app.main.get());
        }
        if IsWindow(app.observer.get()) != 0 {
            DestroyWindow(app.observer.get());
        }
    }
    let product_shutdown = app.product.borrow_mut().shutdown();
    let shutdown = app.core.borrow_mut().backend.shutdown() && product_shutdown;
    app.check(
        "bounded_backend_shutdown",
        shutdown,
        "请求停止后两秒内后台退出",
    );
    app.check(
        "no_callback_panic",
        !app.callback_failed.get(),
        "无 Rust panic 跨越窗口回调",
    );
    let core = app.core.borrow();
    let text = core.transcript.text();
    app.check(
        "bounded_transcript",
        core.transcript.retained_lines() <= 2000 && text.encode_utf16().count() <= 262_144,
        "行数及 UTF-16 单元双重限额",
    );
    app.check(
        "password_hidden",
        !text.contains("原型测试秘密"),
        "压力样例认证内容已脱敏",
    );
    app.check(
        "message_loop_responsive",
        app.max_tick_ms.get() <= 100,
        "单次 UI 事件批处理不超过 100 毫秒",
    );
    if !app.diagnostic() {
        app.check(
            "product_page_responsive",
            app.product_timings.page.get() <= 100,
            "产品页转场包含控件重建、最终布局与同步绘制，不超过100毫秒",
        );
    }
    let actual_dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() };
    let remote = unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0;
    let histogram = app.tick_histogram.borrow();
    let percentile_rank = (histogram.iter().sum::<u64>() * 95).div_ceil(100);
    let mut cumulative = 0;
    let p95_tick_ms = histogram
        .iter()
        .position(|count| {
            cumulative += count;
            cumulative >= percentile_rank
        })
        .unwrap_or(0);
    let (full_replacements, incremental_updates) = app.log_view.counters();
    let (coalesced_updates, peak_append_units, pending_units) = app.log_view.deferred_metrics();
    app.check(
        "render_chunk_budget",
        peak_append_units <= log_view::MAX_APPEND_UNITS,
        "单次追加量不超过分块预算",
    );
    let product_snapshot = app.product.borrow().snapshot();
    let mut metrics = serde_json::json!({"product_host_max_ms":app.product_timings.host.get(),"product_receive_max_ms":app.product_timings.receive.get(),"product_observer_max_ms":app.product_timings.observer.get(),"product_flush_max_ms":app.product_timings.flush.get(),"product_refresh_max_ms":app.product_timings.refresh.get(),"product_page_max_ms":app.product_timings.page.get(),"product_fonts_max_ms":app.product_timings.fonts.get(),"product_theme_max_ms":app.product_timings.theme.get(),"product_create_controls_max_ms":app.product_timings.create_controls.get(),"product_destroy_controls_max_ms":app.product_timings.destroy_controls.get(),"product_layout_max_ms":app.product_timings.layout.get(),"product_visibility_max_ms":app.product_timings.visibility.get(),"produced_lines":app.diagnostic().then(||core.backend.produced_lines()),"received_lines":core.transcript.total_lines(),"retained_lines":core.transcript.retained_lines(),"trimmed_lines":core.transcript.trimmed_lines(),"dropped_lines":app.diagnostic().then(||core.backend.dropped_lines()),"dropped_observations":product_snapshot.dropped_observations,"peak_observation_records":(!app.diagnostic()).then_some(app.peak_drain_events.get()),"elapsed_ms":app.started.elapsed().as_millis(),"ui_ticks":app.ticks.get(),"max_tick_ms":app.max_tick_ms.get(),"p95_tick_ms":p95_tick_ms,"max_timer_gap_ms":app.max_timer_gap_ms.get(),"view_caught_up":app.log_view.cursor()==Some(core.transcript.cursor()),"rendered_total_lines":app.log_view.cursor().map(|cursor|cursor.total_lines),"rendered_text_matches_model_on_close":app.rendered_text_matches_model_on_close.get(),"rendered_utf16_units":app.log_view.cursor().map_or(0,|cursor|cursor.utf16_units),"full_text_replacements":full_replacements,"incremental_text_updates":incremental_updates,"render_updates":full_replacements+incremental_updates,"observer_visible_during_stress":app.diagnostic().then_some(app.stress_observer_visible.get()),"peak_drain_events":app.peak_drain_events.get(),"render_phase_max_ms":app.log_view.phase_max_ms(),"append_max_ms":app.log_view.append_max_ms(),"coalesced_updates":coalesced_updates,"peak_append_units":peak_append_units,"pending_units":pending_units,"queue_capacity":if app.diagnostic(){QUEUE_CAPACITY}else{1024},"agent_core_included":cfg!(feature="agent-runtime"),"cancelled_during_output":app.options.exit_after_ms.is_some()});
    metrics["startup_prewarm_ms"] = serde_json::json!(app.startup_prewarm_ms.get());
    metrics["startup_ui_ms"] = serde_json::json!(app.startup_ui_ms.get());
    metrics["startup_control_count"] = serde_json::json!(app.startup_control_count.get());
    drop(core);
    delete_fonts(app.main_fonts.replace([null_mut(); 3]));
    delete_fonts(app.observer_fonts.replace([null_mut(); 3]));
    if let Some(error) = loop_error {
        return Err(error);
    }
    let dpi_layout_checks = if app.options.self_test {
        "96/120/144/192 逻辑 DPI 注入，非系统缩放切换".to_owned()
    } else if let Some(dpi) = app.options.preview_dpi {
        format!("仅 {dpi} DPI 逻辑布局预览，未修改或验收系统缩放")
    } else {
        "未运行 DPI 布局注入或系统缩放切换验收".to_owned()
    };
    let limitations = if app.diagnostic() {
        vec![
            "未替换现有 GUI；默认离线，SSH 与 Console 为明确标注的展示样例".to_owned(),
            "DPI 布局注入不等于真实多显示器切换；RDP 断开重连仍需现场验证".to_owned(),
            "Agent 核心版引用真实核心以比较体积，但本轮自测不会连接远端".to_owned(),
        ]
    } else {
        vec![
            "独立产品预览入口；旧版保留，尚未正式迁移".to_owned(),
            if app.options.product_demo {
                "离线产品界面演示；不启动 Agent 连接，演示日志不是实际命令结果".to_owned()
            } else if cfg!(feature = "agent-runtime") {
                "复用真实 Agent 有界事件流；SSH 和串口现场设备尚未联调验收".to_owned()
            } else {
                "仅界面构建，不包含真实 Agent 连接能力".to_owned()
            },
            "本轮产品入口的 RDP 断开重连和系统缩放现场验收尚待执行".to_owned(),
        ]
    };
    let checks = app.checks.replace(Vec::new());
    Ok(Report {
        schema_version: 1,
        passed: checks.iter().all(|check| check.passed),
        checks,
        metrics,
        environment: serde_json::json!({"dpi":actual_dpi,"remote_session":remote,"rdp_test_status":if remote{"当前进程报告远程会话；重连尚未验证"}else{"无真实 RDP 连接/重连证据"},"renderer":"Win32 / Unicode 标准多行 EDIT","dpi_layout_checks":dpi_layout_checks}),
        limitations,
    })
}
