//! 产品窗口只负责原生控件和交互；连接、持久化及权限决策由 `ProductHost` 处理。

#[allow(clippy::wildcard_imports)] // 同一 Win32 FFI 边界内复用宿主类型与清理辅助函数。
use super::*;
use crate::product::{
    LogLevel, NativeAppearance, NativeLanguage, ProductAction, ProductPhase, ProductSnapshot,
};
use windows_sys::Win32::{
    Graphics::Gdi::{
        CreateSolidBrush, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK,
        DrawFocusRect, DrawTextW, FillRect, HBRUSH, HDC, InvalidateRect, RDW_ALLCHILDREN,
        RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW, RedrawWindow, SelectObject,
        SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
    },
    System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
    UI::Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED},
    UI::Shell::{IsUserAnAdmin, ShellExecuteW},
    UI::WindowsAndMessaging::{
        BS_OWNERDRAW, BeginDeferWindowPos, DeferWindowPos, EndDeferWindowPos, GetClassNameW, IDYES,
        MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO, MessageBoxW, SW_SHOWNORMAL, SWP_FRAMECHANGED,
        SWP_NOREDRAW, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DRAWITEM,
        WM_ERASEBKGND, WM_GETFONT, WM_SETREDRAW, WS_CAPTION, WS_DISABLED, WS_MINIMIZEBOX,
        WS_SYSMENU,
    },
};

const TITLE: i32 = 300;
const SETTINGS: i32 = 301;
const CODE_LABEL: i32 = 302;
const CODE: i32 = 303;
const COPY: i32 = 304;
const EXPIRY: i32 = 305;
const ACTIVITY: i32 = 306;
const DETAILS: i32 = 307;
const LOGS: i32 = 308;
const EXIT: i32 = 309;
const BACK: i32 = 310;
const BODY: i32 = 311;
const ERROR: i32 = 312;
const RELAY_LABEL: i32 = 313;
const RELAY: i32 = 314;
const CONFIG_PATH: i32 = 315;
const SAVE: i32 = 316;
const LANGUAGE_ZH: i32 = 317;
const LANGUAGE_EN: i32 = 318;
const THEME_SYSTEM: i32 = 319;
const THEME_LIGHT: i32 = 320;
const THEME_DARK: i32 = 321;
const PERMISSIONS: i32 = 322;
const FOLDER: i32 = 323;
const FILTER_ALL: i32 = 324;
const FILTER_RUNNING: i32 = 325;
const FILTER_SUCCESS: i32 = 326;
const FILTER_ERROR: i32 = 327;
const FILTER_INFO: i32 = 328;
const CLEAR: i32 = 329;
const STEP: i32 = 330;
const FULL: i32 = 331;
const DEFAULT_STEP: i32 = 332;
const DEFAULT_FULL: i32 = 333;
const TRUST_CANCEL: i32 = 334;
const TRUST_ONCE: i32 = 335;
const TRUST_SAVE: i32 = 336;
const OBSERVER: i32 = 337;
pub(super) const CODE_ID: i32 = CODE;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Main,
    Setup,
    Preferences,
    Details,
    Logs,
    Permissions,
    Trust,
}

pub(super) struct ProductUi {
    page: Page,
    controls: BTreeMap<i32, HWND>,
    active: std::collections::BTreeSet<i32>,
    // 文本仅在变化时写入，避免打断日志选择、设置编辑或辅助功能读取。
    texts: BTreeMap<i32, String>,
    filter: usize,
    error: String,
    brush: HBRUSH,
    dark: bool,
    copied_until: Option<Instant>,
    clipboard: super::clipboard::Clipboard,
    language: Option<NativeLanguage>,
    draft_relay: Option<String>,
    last_refresh: Option<Instant>,
    dropped_observations: u64,
    running_count: usize,
    changing_page: bool,
}

impl Default for ProductUi {
    fn default() -> Self {
        Self {
            page: Page::default(),
            controls: BTreeMap::new(),
            active: std::collections::BTreeSet::new(),
            texts: BTreeMap::new(),
            filter: 0,
            error: String::new(),
            brush: null_mut(),
            dark: false,
            copied_until: None,
            clipboard: super::clipboard::Clipboard::default(),
            language: None,
            draft_relay: None,
            last_refresh: None,
            dropped_observations: 0,
            running_count: 0,
            changing_page: false,
        }
    }
}

impl Drop for ProductUi {
    fn drop(&mut self) {
        if !self.brush.is_null() {
            unsafe {
                DeleteObject(self.brush);
            }
        }
    }
}

struct PageRedraw<'a> {
    app: &'a App,
    visible: bool,
}
impl<'a> PageRedraw<'a> {
    fn new(app: &'a App) -> Self {
        app.product_ui.borrow_mut().changing_page = true;
        let visible = unsafe { IsWindowVisible(app.main.get()) } != 0;
        if visible {
            unsafe {
                SendMessageW(app.main.get(), WM_SETREDRAW, 0, 0);
            }
        }
        Self { app, visible }
    }
}
impl Drop for PageRedraw<'_> {
    fn drop(&mut self) {
        self.app.product_ui.borrow_mut().changing_page = false;
        if self.visible {
            unsafe {
                SendMessageW(self.app.main.get(), WM_SETREDRAW, 1, 0);
                // 最终一次真实绘制仍在转场计时内，不靠延后重绘隐藏 UI 耗时。
                RedrawWindow(
                    self.app.main.get(),
                    null(),
                    null_mut(),
                    RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_FRAME | RDW_ERASE | RDW_UPDATENOW,
                );
            }
        }
    }
}

pub(super) fn changing_page(app: &App) -> bool {
    app.product_ui.borrow().changing_page
}

pub(super) fn control(app: &App, id: i32) -> HWND {
    let ui = app.product_ui.borrow();
    if ui.active.contains(&id) {
        ui.controls.get(&id).copied().unwrap_or(null_mut())
    } else {
        null_mut()
    }
}

fn write(app: &App, id: i32, text: &str) {
    let target = {
        let mut ui = app.product_ui.borrow_mut();
        if ui.texts.get(&id).is_some_and(|old| old == text) {
            return;
        }
        ui.texts.insert(id, text.to_owned());
        ui.controls.get(&id).copied()
    };
    if let Some(target) = target {
        set_text(target, text);
    }
}

fn write_log_body(app: &App, terminal: HWND, text: &str) -> bool {
    if terminal.is_null() {
        return false;
    }
    if app
        .product_ui
        .borrow()
        .texts
        .get(&BODY)
        .is_some_and(|old| old == text)
    {
        return true;
    }
    // 缓存只承认控件成功接受的正文；暂停或写入失败不能提前清除未读。
    let rendered = unsafe { SetWindowTextW(terminal, wide(text).as_ptr()) } != 0;
    if rendered {
        app.product_ui
            .borrow_mut()
            .texts
            .insert(BODY, text.to_owned());
    }
    rendered
}

fn mark_rendered_logs(host: &mut ProductHost, unread: usize, rendered: bool) -> Result<(), String> {
    if rendered && unread > 0 {
        host.action(ProductAction::MarkLogsRead)?;
    }
    Ok(())
}

fn add(app: &App, id: i32, class: &str, text: &str, style: u32) -> Result<(), String> {
    if !(TITLE..=OBSERVER).contains(&id) {
        return Err("产品控件ID超过有界缓存范围".to_owned());
    }
    let style = control_style(class, style);
    let existing = app
        .product_ui
        .borrow()
        .controls
        .get(&id)
        .copied()
        .unwrap_or(null_mut());
    if !existing.is_null() {
        let mut name = [0u16; 32];
        let length = unsafe { GetClassNameW(existing, name.as_mut_ptr(), name.len() as i32) };
        if length > 0
            && String::from_utf16_lossy(&name[..length as usize]).eq_ignore_ascii_case(class)
        {
            // 相同 ID 和类型保留 UIA 身份；每个页面仍完整更新自己的文案。
            update_control_style(existing, style)?;
            set_text(existing, text);
            {
                let mut ui = app.product_ui.borrow_mut();
                ui.texts.insert(id, text.to_owned());
                ui.active.insert(id);
            }
            unsafe {
                ShowWindow(existing, SW_SHOWNOACTIVATE);
            }
            return Ok(());
        }
        // BODY/ACTIVITY在页面间可能改变原生类型，只替换这个ID，禁止重复句柄。
        {
            let _timer = PhaseTimer::new(&app.product_timings.destroy_controls);
            if unsafe { DestroyWindow(existing) } == 0 {
                return Err("无法替换产品控件类型".to_owned());
            }
        }
        let mut ui = app.product_ui.borrow_mut();
        ui.controls.remove(&id);
        ui.texts.remove(&id);
    }
    let handle = child(app.main.get(), id, class, text, style)?;
    let mut ui = app.product_ui.borrow_mut();
    ui.controls.insert(id, handle);
    ui.active.insert(id);
    ui.texts.insert(id, text.to_owned());
    Ok(())
}

fn button(app: &App, id: i32, text: &str) -> Result<(), String> {
    add(app, id, "BUTTON", text, WS_TABSTOP)
}

fn control_style(class: &str, style: u32) -> u32 {
    if class == "BUTTON" {
        style | BS_OWNERDRAW as u32
    } else if class == "STATIC" {
        style | 0x80
    } else {
        style
    }
}

fn update_control_style(window: HWND, style: u32) -> Result<(), String> {
    let previous = unsafe { GetWindowLongPtrW(window, GWL_STYLE) } as u32;
    let desired = WS_CHILD | style | (previous & (WS_VISIBLE | WS_DISABLED));
    if previous != desired {
        unsafe {
            if SetWindowLongPtrW(window, GWL_STYLE, desired as isize) == 0 {
                return Err("无法更新产品控件样式".to_owned());
            }
            if SetWindowPos(
                window,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            ) == 0
            {
                return Err("无法更新产品控件边框".to_owned());
            }
        }
    }
    Ok(())
}

fn label(app: &App, id: i32, text: &str) -> Result<(), String> {
    add(app, id, "STATIC", text, 0)
}

fn readonly(app: &App, id: i32, text: &str) -> Result<(), String> {
    add(
        app,
        id,
        "EDIT",
        text,
        (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | ES_WANTRETURN) as u32
            | WS_VSCROLL
            | WS_TABSTOP
            | WS_BORDER,
    )
}

fn page_size(page: Page) -> (i32, i32) {
    match page {
        Page::Main | Page::Preferences => (500, 375),
        Page::Setup => (640, 330),
        Page::Details | Page::Logs | Page::Permissions | Page::Trust => (540, 560),
    }
}

fn readonly_label(app: &App, id: i32, text: &str) -> Result<(), String> {
    add(
        app,
        id,
        "EDIT",
        text,
        (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | ES_WANTRETURN) as u32,
    )
}

fn current_page_controls(app: &App) -> (Page, BTreeMap<i32, HWND>) {
    let ui = app.product_ui.borrow();
    (
        ui.page,
        ui.controls
            .iter()
            .filter(|(id, _)| ui.active.contains(id))
            .map(|(&id, &hwnd)| (id, hwnd))
            .collect::<BTreeMap<_, _>>(),
    )
}

pub(super) fn layout(app: &App, dpi: u32) {
    let _timer = PhaseTimer::new(&app.product_timings.layout);
    if app.main.get().is_null() {
        return;
    }
    let (page, controls) = current_page_controls(app);
    let mut rect = RECT::default();
    unsafe {
        GetClientRect(app.main.get(), &raw mut rect);
    }
    let s = |value| scaled(value, dpi);
    let width = rect.right;
    let height = rect.bottom;
    let mut placements = Vec::new();
    let mut place = |id, x, y, w: i32, h: i32| {
        if let Some(hwnd) = controls.get(&id) {
            placements.push((*hwnd, x, y, w.max(1), h.max(1)));
        }
    };
    place(TITLE, s(24), s(20), width - s(96), s(34));
    place(ERROR, s(24), height - s(101), width - s(48), s(42));
    if page == Page::Main {
        place(SETTINGS, width - s(58), s(20), s(34), s(32));
        place(CODE_LABEL, s(24), s(86), width - s(48), s(23));
        place(CODE, s(50), s(116), width - s(135), s(58));
        place(COPY, width - s(79), s(126), s(36), s(34));
        place(EXPIRY, s(24), s(183), width - s(48), s(25));
        place(ACTIVITY, s(24), s(233), width - s(48), s(43));
        place(DETAILS, s(24), height - s(54), s(124), s(32));
        place(LOGS, s(159), height - s(54), s(124), s(32));
        place(EXIT, width - s(134), height - s(54), s(110), s(32));
        apply_placements(app, &placements);
        return;
    }
    place(BACK, s(24), height - s(54), s(104), s(32));
    match page {
        Page::Setup => {
            place(RELAY_LABEL, s(24), s(76), s(116), s(26));
            place(RELAY, s(142), s(72), width - s(166), s(32));
            place(CONFIG_PATH, s(24), s(120), width - s(48), s(43));
            place(LANGUAGE_ZH, s(24), s(179), s(100), s(31));
            place(LANGUAGE_EN, s(134), s(179), s(100), s(31));
            place(SAVE, width - s(182), height - s(54), s(158), s(32));
        }
        Page::Preferences => {
            place(BODY, s(24), s(67), width - s(48), s(24));
            place(LANGUAGE_ZH, s(24), s(100), s(120), s(32));
            place(LANGUAGE_EN, s(154), s(100), s(120), s(32));
            for (n, id) in [THEME_SYSTEM, THEME_LIGHT, THEME_DARK]
                .into_iter()
                .enumerate()
            {
                place(id, s(24 + 145 * n as i32), s(151), s(135), s(32));
            }
            place(PERMISSIONS, s(24), s(204), width - s(48), s(34));
            place(CONFIG_PATH, s(24), s(255), width - s(48), s(40));
        }
        Page::Logs => {
            for (n, id) in [
                FILTER_ALL,
                FILTER_RUNNING,
                FILTER_SUCCESS,
                FILTER_ERROR,
                FILTER_INFO,
            ]
            .into_iter()
            .enumerate()
            {
                place(id, s(24 + 83 * n as i32), s(67), s(76), s(30));
            }
            place(BODY, s(24), s(110), width - s(48), height - s(218));
            place(ACTIVITY, s(24), height - s(100), width - s(48), s(24));
            place(CLEAR, width - s(128), height - s(54), s(104), s(32));
        }
        Page::Permissions => {
            place(BODY, s(24), s(72), width - s(48), s(142));
            place(STEP, s(24), s(228), s(228), s(34));
            place(FULL, s(264), s(228), s(252), s(34));
            place(CONFIG_PATH, s(24), s(291), width - s(48), s(65));
            place(DEFAULT_STEP, s(24), s(367), s(228), s(34));
            place(DEFAULT_FULL, s(264), s(367), s(252), s(34));
        }
        Page::Trust => {
            place(BODY, s(24), s(72), width - s(48), height - s(200));
            place(TRUST_CANCEL, s(24), height - s(54), s(104), s(32));
            place(TRUST_ONCE, s(140), height - s(54), s(150), s(32));
            place(TRUST_SAVE, s(302), height - s(54), width - s(326), s(32));
        }
        Page::Details => {
            place(BODY, s(24), s(72), width - s(48), height - s(220));
            place(FOLDER, s(24), height - s(133), width - s(48), s(34));
            place(OBSERVER, width - s(224), height - s(54), s(200), s(32));
        }
        Page::Main => {}
    }
    apply_placements(app, &placements);
}

fn apply_placements(app: &App, placements: &[(HWND, i32, i32, i32, i32)]) {
    if app.main.get().is_null() || placements.is_empty() {
        return;
    }
    let mut batch = unsafe { BeginDeferWindowPos(placements.len() as i32) };
    if !batch.is_null() {
        for &(hwnd, x, y, w, h) in placements {
            batch = unsafe {
                DeferWindowPos(
                    batch,
                    hwnd,
                    null_mut(),
                    x,
                    y,
                    w,
                    h,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOREDRAW,
                )
            };
            if batch.is_null() {
                break;
            }
        }
    }
    let completed = !batch.is_null() && unsafe { EndDeferWindowPos(batch) } != 0;
    if !completed {
        for &(hwnd, x, y, w, h) in placements {
            unsafe {
                MoveWindow(hwnd, x, y, w, h, 0);
            }
        }
    }
    if !changing_page(app) {
        unsafe {
            RedrawWindow(
                app.main.get(),
                null(),
                null_mut(),
                RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_ERASE | RDW_UPDATENOW,
            );
        }
    }
}

pub(super) fn font_controls(app: &App) -> Vec<(i32, HWND)> {
    app.product_ui
        .borrow()
        .controls
        .iter()
        .map(|(&id, &hwnd)| (id, hwnd))
        .collect()
}

pub(super) fn is_heading(id: i32) -> bool {
    id == TITLE || id == CODE
}

pub(super) fn minimum_size(app: &App) -> (i32, i32) {
    page_size(app.product_ui.borrow().page)
}

pub(super) fn english(app: &App) -> bool {
    app.product_ui.borrow().language == Some(NativeLanguage::English)
}

pub(super) fn observation_state(app: &App) -> (u64, bool) {
    let ui = app.product_ui.borrow();
    (ui.dropped_observations, ui.running_count > 0)
}

pub(super) fn observer_created(app: &App) {
    {
        let mut ui = app.product_ui.borrow_mut();
        for id in [-1, OBS_HEADER, FOLLOW, HIDE] {
            ui.texts.remove(&id);
        }
    }
    let snapshot = app.product.borrow().snapshot();
    localize_observer(app, &snapshot);
}

fn confirm(app: &App, text: &str, title: &str) -> bool {
    unsafe {
        MessageBoxW(
            app.main.get(),
            wide(text).as_ptr(),
            wide(title).as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        ) == IDYES
    }
}

#[allow(clippy::too_many_lines)] // 原生绘制消息保持颜色、画刷和 DC 清理在同一边界。
pub(super) fn paint(
    app: &App,
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    let (dark, brush) = {
        let ui = app.product_ui.borrow();
        (ui.dark, ui.brush)
    };
    let background = if dark {
        rgb(24, 28, 35)
    } else {
        rgb(247, 248, 250)
    };
    let foreground = if dark {
        rgb(242, 244, 248)
    } else {
        rgb(24, 32, 44)
    };
    unsafe {
        match message {
            WM_ERASEBKGND if !brush.is_null() => {
                let mut rect = RECT::default();
                GetClientRect(hwnd, &raw mut rect);
                FillRect(wparam as HDC, &raw const rect, brush);
                Some(1)
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN if !brush.is_null() => {
                SetBkMode(wparam as HDC, TRANSPARENT as i32);
                SetBkColor(wparam as HDC, background);
                SetTextColor(
                    wparam as HDC,
                    if lparam as HWND == control(app, ERROR) {
                        if dark {
                            rgb(249, 107, 119)
                        } else {
                            rgb(214, 32, 58)
                        }
                    } else {
                        foreground
                    },
                );
                Some(brush as isize)
            }
            WM_DRAWITEM if lparam != 0 => {
                let item = &*(lparam as *const DRAWITEMSTRUCT);
                if item.CtlID < TITLE as u32 {
                    return None;
                }
                let selected = item.itemState & ODS_SELECTED != 0;
                let disabled = item.itemState & ODS_DISABLED != 0;
                let color = if item.CtlID == EXIT as u32 {
                    if dark {
                        rgb(46, 30, 37)
                    } else {
                        rgb(255, 237, 241)
                    }
                } else if selected || item.CtlID == COPY as u32 {
                    rgb(15, 103, 242)
                } else if dark {
                    rgb(42, 49, 61)
                } else {
                    rgb(229, 236, 247)
                };
                let fill = CreateSolidBrush(color);
                if !fill.is_null() {
                    FillRect(item.hDC, &raw const item.rcItem, fill);
                    DeleteObject(fill);
                }
                let text = wide(&if item.CtlID == SETTINGS as u32 {
                    "⚙".to_owned()
                } else {
                    log_view::text(item.hwndItem)
                });
                SetBkMode(item.hDC, TRANSPARENT as i32);
                SetTextColor(
                    item.hDC,
                    if disabled {
                        rgb(130, 137, 148)
                    } else if item.CtlID == EXIT as u32 {
                        if dark {
                            rgb(249, 107, 119)
                        } else {
                            rgb(214, 32, 58)
                        }
                    } else if selected || item.CtlID == COPY as u32 {
                        rgb(255, 255, 255)
                    } else {
                        foreground
                    },
                );
                let mut rect = item.rcItem;
                let font = SendMessageW(item.hwndItem, WM_GETFONT, 0, 0);
                let previous = SelectObject(item.hDC, font as _);
                DrawTextW(
                    item.hDC,
                    text.as_ptr(),
                    -1,
                    &raw mut rect,
                    if item.CtlID == ACTIVITY as u32 {
                        DT_CENTER | DT_WORDBREAK | DT_NOPREFIX
                    } else {
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX
                    },
                );
                SelectObject(item.hDC, previous);
                if item.itemState & ODS_FOCUS != 0 {
                    rect.left += 3;
                    rect.top += 3;
                    rect.right -= 3;
                    rect.bottom -= 3;
                    DrawFocusRect(item.hDC, &raw const rect);
                }
                Some(1)
            }
            _ => None,
        }
    }
}

const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

pub(super) fn create(app: &App) -> Result<(), String> {
    let startup = Instant::now();
    app.product
        .borrow_mut()
        .set_elevated(unsafe { IsUserAnAdmin() } != 0);
    let snapshot = app.product.borrow().snapshot();
    let window = unsafe {
        CreateWindowExW(
            0,
            wide("RemoteOpsNativeMain").as_ptr(),
            wide(&snapshot.title).as_ptr(),
            WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            500,
            375,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            std::ptr::from_ref(app).cast(),
        )
    };
    if window.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    app.main.set(window);
    app.dpi.set(
        app.options
            .preview_dpi
            .unwrap_or_else(|| unsafe { GetDpiForWindow(window) }.max(96)),
    );
    let page = if snapshot.tls.is_some() {
        Page::Trust
    } else if snapshot.setup_needed {
        Page::Setup
    } else {
        Page::Main
    };
    let prewarm = Instant::now();
    // 复用唯一页面声明预建固定控件；主窗未显示，也尚未启用定时器。
    for hidden_page in [
        Page::Main,
        Page::Setup,
        Page::Preferences,
        Page::Details,
        Page::Logs,
        Page::Permissions,
        Page::Trust,
    ] {
        build_page(app, hidden_page, &snapshot, false)?;
    }
    app.startup_prewarm_ms.set(prewarm.elapsed().as_millis());
    app.startup_control_count
        .set(app.product_ui.borrow().controls.len());
    show_page(app, page, &snapshot)?;
    unsafe {
        WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION);
        ShowWindow(window, SW_SHOWNOACTIVATE);
        RedrawWindow(
            window,
            null(),
            null_mut(),
            RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_ERASE | RDW_UPDATENOW,
        );
    }
    // 初始化及首次真实绘制单独报告；交互计时只在此后开始，不掩盖后续页转场。
    app.startup_ui_ms.set(startup.elapsed().as_millis());
    app.product_timings.reset();
    unsafe {
        SetTimer(window, 1, 16, None);
    }
    Ok(())
}

fn show_page(app: &App, page: Page, snapshot: &ProductSnapshot) -> Result<(), String> {
    build_page(app, page, snapshot, true)
}

#[allow(clippy::too_many_lines)] // 页面声明同时服务隐藏预建与交互显示，避免文案/样式分叉。
fn build_page(
    app: &App,
    page: Page,
    snapshot: &ProductSnapshot,
    present: bool,
) -> Result<(), String> {
    let _timer = PhaseTimer::new(&app.product_timings.page);
    // 原子呈现页面，避免每个控件创建/移位都触发同步绘制和中间布局。
    let _redraw = PageRedraw::new(app);
    let old_active = {
        let mut ui = app.product_ui.borrow_mut();
        ui.page = page;
        ui.texts.clear();
        ui.language = Some(snapshot.language);
        let old = ui
            .active
            .iter()
            .filter_map(|id| ui.controls.get(id).copied())
            .collect::<Vec<_>>();
        ui.active.clear();
        old
    };
    {
        let _timer = PhaseTimer::new(&app.product_timings.visibility);
        for handle in old_active {
            unsafe {
                ShowWindow(handle, SW_HIDE);
            }
        }
    }
    let controls_timer = PhaseTimer::new(&app.product_timings.create_controls);
    let t = |zh, en| snapshot.t(zh, en);
    label(app, TITLE, "")?;
    label(app, ERROR, "")?;
    if !matches!(page, Page::Main | Page::Trust) {
        button(
            app,
            BACK,
            if page == Page::Setup {
                t("退出", "Exit")
            } else {
                t("返回", "Back")
            },
        )?;
    }
    match page {
        Page::Main => {
            button(app, SETTINGS, t("设置", "Settings"))?;
            add(
                app,
                CODE_LABEL,
                "STATIC",
                t("临时控制码", "Temporary pairing code"),
                1,
            )?;
            add(app, CODE, "STATIC", "··· ··· ···", 1)?;
            button(app, COPY, t("复制", "Copy"))?;
            add(app, EXPIRY, "STATIC", "", 1)?;
            button(app, ACTIVITY, "")?;
            button(app, DETAILS, t("查看连接详情", "Connection details"))?;
            button(app, LOGS, t("实时日志", "Logs"))?;
            button(app, EXIT, t("退出 Agent", "Exit Agent"))?;
        }
        Page::Setup => {
            label(app, RELAY_LABEL, "Relay")?;
            let draft = app
                .product_ui
                .borrow()
                .draft_relay
                .clone()
                .unwrap_or_else(|| snapshot.relay.clone());
            add(
                app,
                RELAY,
                "EDIT",
                &draft,
                WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            )?;
            unsafe {
                EnableWindow(control(app, RELAY), i32::from(snapshot.endpoint_editable));
                SendMessageW(control(app, RELAY), EM_LIMITTEXT, 2048, 0);
            }
            label(
                app,
                CONFIG_PATH,
                &format!(
                    "{}\r\n{}",
                    t("配置文件", "Configuration file"),
                    snapshot.config_path
                ),
            )?;
            button(app, LANGUAGE_ZH, "简体中文")?;
            button(app, LANGUAGE_EN, "English")?;
            button(app, SAVE, t("保存并连接", "Save and connect"))?;
        }
        Page::Preferences => {
            readonly_label(app, BODY, t("语言 / 外观", "Language / Appearance"))?;
            button(app, LANGUAGE_ZH, "简体中文")?;
            button(app, LANGUAGE_EN, "English")?;
            button(app, THEME_SYSTEM, t("跟随系统", "System"))?;
            button(app, THEME_LIGHT, t("浅色", "Light"))?;
            button(app, THEME_DARK, t("深色", "Dark"))?;
            button(app, PERMISSIONS, t("控制权限", "Control permissions"))?;
            label(
                app,
                CONFIG_PATH,
                &format!("{}\r\nRemoteOps {}", snapshot.title, snapshot.version),
            )?;
        }
        Page::Details => {
            readonly(app, BODY, "")?;
            button(
                app,
                FOLDER,
                t("打开文件接收目录", "Open received files folder"),
            )?;
            button(
                app,
                OBSERVER,
                t("打开只读操作观察窗", "Open operation observer"),
            )?;
        }
        Page::Logs => {
            for (id, zh, en) in [
                (FILTER_ALL, "全部", "All"),
                (FILTER_RUNNING, "执行", "Running"),
                (FILTER_SUCCESS, "成功", "Success"),
                (FILTER_ERROR, "错误", "Error"),
                (FILTER_INFO, "信息", "Info"),
            ] {
                button(app, id, t(zh, en))?;
            }
            readonly(app, BODY, "")?;
            unsafe {
                SendMessageW(control(app, BODY), EM_LIMITTEXT, 1_000_000, 0);
            }
            label(app, ACTIVITY, "")?;
            button(app, CLEAR, t("清空显示记录", "Clear displayed logs"))?;
        }
        Page::Permissions => {
            readonly_label(app, BODY, "")?;
            button(app, STEP, t("本次：逐步确认", "Session: step by step"))?;
            button(app, FULL, t("本次：完全控制…", "Session: full control…"))?;
            label(app, CONFIG_PATH, "")?;
            button(
                app,
                DEFAULT_STEP,
                t("默认：逐步确认", "Default: step by step"),
            )?;
            button(
                app,
                DEFAULT_FULL,
                t("默认：完全控制…", "Default: full control…"),
            )?;
        }
        Page::Trust => {
            readonly(app, BODY, "")?;
            button(app, TRUST_CANCEL, t("取消连接", "Cancel"))?;
            button(app, TRUST_ONCE, t("仅信任本次", "Trust once"))?;
            button(app, TRUST_SAVE, t("信任并保存", "Trust and save"))?;
        }
    }
    drop(controls_timer);
    let dpi = app.dpi.get();
    app.fonts(false, dpi)?;
    let (width, height) = page_size(page);
    resize_client(app.main.get(), width, height, dpi);
    layout(app, dpi);
    apply_theme(app, snapshot);
    if present {
        update(app, snapshot);
    }
    Ok(())
}

fn apply_theme(app: &App, snapshot: &ProductSnapshot) {
    let _timer = PhaseTimer::new(&app.product_timings.theme);
    let dark = match snapshot.appearance {
        NativeAppearance::Dark => true,
        NativeAppearance::Light => false,
        NativeAppearance::System => system_dark(),
    };
    let needs_brush = {
        let ui = app.product_ui.borrow();
        ui.brush.is_null() || ui.dark != dark
    };
    if !needs_brush {
        return;
    }
    let brush = unsafe {
        CreateSolidBrush(if dark {
            rgb(24, 28, 35)
        } else {
            rgb(247, 248, 250)
        })
    };
    if brush.is_null() {
        return;
    }
    let old = {
        let mut ui = app.product_ui.borrow_mut();
        ui.dark = dark;
        std::mem::replace(&mut ui.brush, brush)
    };
    unsafe {
        InvalidateRect(app.main.get(), null(), 1);
        for (_, hwnd) in font_controls(app) {
            InvalidateRect(hwnd, null(), 1);
        }
        if !old.is_null() {
            DeleteObject(old);
        }
    }
}

fn system_dark() -> bool {
    let mut light = 1u32;
    let mut bytes = 4u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize").as_ptr(),
            wide("AppsUseLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&raw mut light).cast(),
            &raw mut bytes,
        )
    };
    result == 0 && light == 0
}

pub(super) fn refresh_tick(app: &App) {
    let changed = poll_clipboard(app);
    let now = Instant::now();
    let due = {
        let ui = app.product_ui.borrow();
        ui.last_refresh
            .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(100))
    };
    if changed || due {
        refresh(app);
    }
}

fn desired_page(page: Page, snapshot: &ProductSnapshot) -> Page {
    if snapshot.tls.is_some() {
        Page::Trust
    } else if page == Page::Trust {
        if snapshot.setup_needed {
            Page::Setup
        } else {
            Page::Main
        }
    } else if page == Page::Setup && !snapshot.setup_needed {
        Page::Main
    } else {
        page
    }
}

pub(super) fn refresh(app: &App) {
    app.product_ui.borrow_mut().last_refresh = Some(Instant::now());
    let snapshot = app.product.borrow().snapshot();
    let (page, language) = {
        let ui = app.product_ui.borrow();
        (ui.page, ui.language)
    };
    let next = desired_page(page, &snapshot);
    if next != page || language != Some(snapshot.language) {
        if page == Page::Setup {
            let draft = log_view::text(control(app, RELAY));
            app.product_ui.borrow_mut().draft_relay = Some(draft);
        }
        if let Err(error) = show_page(app, next, &snapshot) {
            app.product_ui.borrow_mut().error = error;
        }
    } else {
        apply_theme(app, &snapshot);
        update(app, &snapshot);
    }
}

fn enabled(app: &App, id: i32, value: bool) {
    let target = control(app, id);
    if !target.is_null() {
        unsafe {
            EnableWindow(target, i32::from(value));
        }
    }
}

fn display_code(code: &str) -> String {
    if code.len() == 9 && code.bytes().all(|value| value.is_ascii_digit()) {
        format!("{}-{}-{}", &code[..3], &code[3..6], &code[6..])
    } else {
        code.to_owned()
    }
}

#[allow(clippy::too_many_lines)] // 呈现模型到已有控件的映射，不包含后台决策。
fn update(app: &App, snapshot: &ProductSnapshot) {
    let counters_changed = {
        let mut ui = app.product_ui.borrow_mut();
        let changed = ui.dropped_observations != snapshot.dropped_observations
            || ui.running_count != snapshot.running_count;
        ui.dropped_observations = snapshot.dropped_observations;
        ui.running_count = snapshot.running_count;
        changed
    };
    if counters_changed {
        app.core.borrow_mut().dirty = true;
    }
    let page = app.product_ui.borrow().page;
    let t = |zh, en| snapshot.t(zh, en);
    let title_changed = {
        let mut ui = app.product_ui.borrow_mut();
        if ui.texts.get(&0) == Some(&snapshot.title) {
            false
        } else {
            ui.texts.insert(0, snapshot.title.clone());
            true
        }
    };
    if title_changed {
        set_text(app.main.get(), &snapshot.title);
    }
    localize_observer(app, snapshot);
    let title = match page {
        Page::Main => format!("●  {}", snapshot.status),
        Page::Setup => t("首次连接设置", "First connection setup").to_owned(),
        Page::Preferences => t("偏好设置", "Preferences").to_owned(),
        Page::Details => t("连接详情", "Connection details").to_owned(),
        Page::Logs => t("实时运行日志", "Runtime logs").to_owned(),
        Page::Permissions => t("控制权限", "Control permissions").to_owned(),
        Page::Trust => t("确认 Relay 证书", "Confirm Relay certificate").to_owned(),
    };
    write(app, TITLE, &title);
    let local_error = app.product_ui.borrow().error.clone();
    let error = if !local_error.is_empty() {
        local_error.as_str()
    } else if page == Page::Permissions {
        snapshot.control_error.as_deref().unwrap_or("")
    } else if page == Page::Setup
        || (page == Page::Main
            && matches!(
                snapshot.phase,
                ProductPhase::Failed | ProductPhase::Reconnecting
            ))
    {
        snapshot.detail.as_str()
    } else {
        ""
    };
    write(app, ERROR, error);
    match page {
        Page::Main => {
            write(
                app,
                CODE,
                &snapshot
                    .code
                    .as_deref()
                    .map_or_else(|| "··· ··· ···".to_owned(), display_code),
            );
            write(app, EXPIRY, &snapshot.countdown);
            enabled(app, COPY, snapshot.can_copy);
            let copied = app
                .product_ui
                .borrow()
                .copied_until
                .is_some_and(|end| Instant::now() < end);
            let copying = app.product_ui.borrow().clipboard.pending();
            write(
                app,
                COPY,
                if copying {
                    "…"
                } else if copied {
                    "✓"
                } else {
                    t("复制", "Copy")
                },
            );
            write(
                app,
                ACTIVITY,
                &format!(
                    "{}{}",
                    snapshot.activity,
                    if snapshot.last_result.is_empty() {
                        String::new()
                    } else {
                        format!("\r\n{}", snapshot.last_result)
                    }
                ),
            );
            write(
                app,
                LOGS,
                &format!(
                    "{}{}",
                    t("实时日志", "Logs"),
                    if snapshot.unread > 0 {
                        format!(" · {}", snapshot.unread)
                    } else {
                        String::new()
                    }
                ),
            );
            write(
                app,
                EXIT,
                if snapshot.controllers > 0 {
                    t("停止协助", "Stop assistance")
                } else {
                    t("退出 Agent", "Exit Agent")
                },
            );
        }
        Page::Details => {
            let capabilities = snapshot
                .capabilities
                .iter()
                .map(|(name, active)| {
                    format!(
                        "{}: {}",
                        name,
                        if *active {
                            t("可用", "Available")
                        } else {
                            t("禁用", "Disabled")
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("\r\n");
            write(
                app,
                BODY,
                &format!(
                    "Relay: {}\r\nTLS: {}\r\n{}: {}\r\n{}: {}\r\n{}: {}\r\n\r\n{}\r\n\r\n{}: {}\r\n\r\n{}\r\n{}",
                    snapshot.relay,
                    snapshot.server_name,
                    t("管理员权限", "Elevated"),
                    if snapshot.elevated {
                        t("是", "Yes")
                    } else {
                        t("否", "No")
                    },
                    t("连接数", "Controllers"),
                    snapshot.controllers,
                    t("运行中任务", "Running tasks"),
                    snapshot.running_count,
                    snapshot.detail,
                    t("接收目录", "Received files"),
                    snapshot.transfer_root,
                    capabilities,
                    snapshot.config_path
                ),
            );
            enabled(app, FOLDER, !snapshot.transfer_root.is_empty());
        }
        Page::Logs => {
            let filter = app.product_ui.borrow().filter;
            let logs = snapshot
                .logs
                .iter()
                .filter(|log| match filter {
                    1 => log.level == LogLevel::Running,
                    2 => log.level == LogLevel::Success,
                    3 => log.level == LogLevel::Error,
                    4 => log.level == LogLevel::Info,
                    _ => true,
                })
                .collect::<Vec<_>>();
            let text = if logs.is_empty() {
                t("暂无符合条件的记录", "No matching log records").to_owned()
            } else {
                logs.iter()
                    .map(|log| {
                        format!(
                            "{} [{}] {}{}\r\n{}",
                            log.occurred,
                            match log.level {
                                LogLevel::Running => t("执行", "Running"),
                                LogLevel::Success => t("成功", "Success"),
                                LogLevel::Error => t("错误", "Error"),
                                LogLevel::Info => t("信息", "Info"),
                            },
                            log.source,
                            if log.request.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", log.request)
                            },
                            log.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\r\n\r\n")
            };
            let terminal = control(app, BODY);
            let mut start = 0u32;
            let mut end = 0u32;
            unsafe {
                SendMessageW(
                    terminal,
                    EM_GETSEL,
                    (&raw mut start) as usize,
                    (&raw mut end) as isize,
                );
            }
            let can_render = start == end || unsafe { GetFocus() } != terminal;
            let rendered = can_render && write_log_body(app, terminal, &text);
            if rendered && snapshot.unread > 0 {
                let result =
                    mark_rendered_logs(&mut app.product.borrow_mut(), snapshot.unread, rendered);
                if let Err(error) = result {
                    app.product_ui.borrow_mut().error = error;
                }
            }
            write(
                app,
                ACTIVITY,
                &format!(
                    "{} / {} {}",
                    logs.len(),
                    snapshot.logs.len(),
                    t(
                        "条记录（清空不影响审计文件）",
                        "records (clearing preserves audit files)"
                    )
                ),
            );
        }
        Page::Permissions => {
            write(
                app,
                BODY,
                &format!(
                    "{}\r\n\r\n{}\r\n\r\n{}",
                    snapshot.control_summary,
                    snapshot.control_source,
                    if snapshot.control_pending {
                        t(
                            "正在等待控制端确认…",
                            "Waiting for controller acknowledgement…",
                        )
                    } else {
                        t(
                            "本次权限仅作用于当前 AI 会话。",
                            "Session permissions apply to the current AI connection.",
                        )
                    }
                ),
            );
            let allowed = snapshot.control_enabled && !snapshot.control_pending;
            enabled(app, STEP, allowed);
            enabled(app, FULL, allowed);
            write(
                app,
                CONFIG_PATH,
                &format!(
                    "{}: {}\r\n{}",
                    t("新会话默认权限", "Default for new sessions"),
                    if snapshot.default_full {
                        t("完全控制", "Full control")
                    } else {
                        t("逐步确认", "Step by step")
                    },
                    t(
                        "保存默认值不会更改当前会话权限。",
                        "Saving a default does not change this session."
                    )
                ),
            );
        }
        Page::Trust => {
            if let Some(trust) = &snapshot.tls {
                write(
                    app,
                    BODY,
                    &format!(
                        "Relay: {}\r\n\r\n{}\r\n{}\r\n\r\n{}\r\n\r\n{}\r\n{}",
                        trust.relay,
                        t("证书指纹 (SHA-256)", "Certificate fingerprint (SHA-256)"),
                        trust.fingerprint,
                        trust.reason,
                        t(
                            "请通过可信渠道核对指纹后再连接。",
                            "Verify this fingerprint through a trusted channel before connecting."
                        ),
                        trust.error.as_deref().unwrap_or("")
                    ),
                );
            }
        }
        _ => {}
    }
    if matches!(page, Page::Setup | Page::Preferences) {
        write(
            app,
            LANGUAGE_ZH,
            if snapshot.language == NativeLanguage::Chinese {
                "✓ 简体中文"
            } else {
                "简体中文"
            },
        );
        write(
            app,
            LANGUAGE_EN,
            if snapshot.language == NativeLanguage::English {
                "✓ English"
            } else {
                "English"
            },
        );
    }
    if page == Page::Preferences {
        for (id, appearance, zh, en) in [
            (THEME_SYSTEM, NativeAppearance::System, "跟随系统", "System"),
            (THEME_LIGHT, NativeAppearance::Light, "浅色", "Light"),
            (THEME_DARK, NativeAppearance::Dark, "深色", "Dark"),
        ] {
            write(
                app,
                id,
                &format!(
                    "{}{}",
                    if snapshot.appearance == appearance {
                        "✓ "
                    } else {
                        ""
                    },
                    t(zh, en)
                ),
            );
        }
    }
    if page == Page::Logs {
        let selected = app.product_ui.borrow().filter;
        for (index, (id, zh, en)) in [
            (FILTER_ALL, "全部", "All"),
            (FILTER_RUNNING, "执行", "Running"),
            (FILTER_SUCCESS, "成功", "Success"),
            (FILTER_ERROR, "错误", "Error"),
            (FILTER_INFO, "信息", "Info"),
        ]
        .into_iter()
        .enumerate()
        {
            write(
                app,
                id,
                &format!("{}{}", if selected == index { "✓ " } else { "" }, t(zh, en)),
            );
        }
    }
}

fn localize_observer(app: &App, snapshot: &ProductSnapshot) {
    if app.observer.get().is_null() {
        return;
    }
    let title = snapshot.t(
        "RemoteOps · 只读操作观察窗",
        "RemoteOps · Read-only operation observer",
    );
    let title_changed = {
        let mut ui = app.product_ui.borrow_mut();
        if ui.texts.get(&-1).is_some_and(|old| old == title) {
            false
        } else {
            ui.texts.insert(-1, title.to_owned());
            true
        }
    };
    if title_changed {
        set_text(app.observer.get(), title);
    }
    for (id, text) in [
        (
            OBS_HEADER,
            snapshot.t(
                "操作实况 · 只读观看，认证信息隐藏",
                "Live operations · Read only; authentication information is hidden",
            ),
        ),
        (
            FOLLOW,
            if app.follow.get() {
                snapshot.t("暂停跟随", "Pause following")
            } else {
                snapshot.t("返回最新", "Return to latest")
            },
        ),
        (HIDE, snapshot.t("关闭观察窗", "Close observer")),
    ] {
        let changed = {
            let mut ui = app.product_ui.borrow_mut();
            if ui.texts.get(&id).is_some_and(|old| old == text) {
                false
            } else {
                ui.texts.insert(id, text.to_owned());
                true
            }
        };
        if changed {
            set_text(app.control(id), text);
        }
    }
}

fn perform(app: &App, action: ProductAction) {
    let result = app.product.borrow_mut().action(action);
    app.product_ui.borrow_mut().error = result.err().unwrap_or_default();
    refresh(app);
}

pub(super) fn command(app: &App, id: i32) {
    let snapshot = app.product.borrow().snapshot();
    let page = app.product_ui.borrow().page;
    if snapshot.tls.is_some() && page != Page::Trust {
        refresh(app);
        return;
    }
    let next = match id {
        SETTINGS => Some(Page::Preferences),
        DETAILS => Some(Page::Details),
        LOGS | ACTIVITY => Some(Page::Logs),
        PERMISSIONS => Some(Page::Permissions),
        BACK | 2 if !matches!(page, Page::Main | Page::Setup | Page::Trust) => Some(Page::Main),
        _ => None,
    };
    if let Some(next) = next {
        app.product_ui.borrow_mut().error.clear();
        if let Err(error) = show_page(app, next, &snapshot) {
            app.product_ui.borrow_mut().error = error;
        }
        return;
    }
    match id {
        BACK | 2 if page == Page::Setup => close_command(app),
        EXIT => close_command(app),
        COPY if snapshot.can_copy => copy_code(app, &snapshot),
        SAVE | 1 if page == Page::Setup => {
            let relay = log_view::text(control(app, RELAY));
            perform(app, ProductAction::SaveSetup { relay });
        }
        LANGUAGE_ZH | LANGUAGE_EN => {
            if page == Page::Setup {
                let draft = log_view::text(control(app, RELAY));
                app.product_ui.borrow_mut().draft_relay = Some(draft);
            }
            perform(
                app,
                ProductAction::SetLanguage(if id == LANGUAGE_ZH {
                    NativeLanguage::Chinese
                } else {
                    NativeLanguage::English
                }),
            );
        }
        THEME_SYSTEM => perform(app, ProductAction::SetAppearance(NativeAppearance::System)),
        THEME_LIGHT => perform(app, ProductAction::SetAppearance(NativeAppearance::Light)),
        THEME_DARK => perform(app, ProductAction::SetAppearance(NativeAppearance::Dark)),
        STEP if snapshot.control_enabled && !snapshot.control_pending => {
            perform(app, ProductAction::RequestControl { full: false });
        }
        FULL if snapshot.control_enabled && !snapshot.control_pending => {
            confirm_full(app, &snapshot, false);
        }
        DEFAULT_STEP => perform(app, ProductAction::SaveDefaultControl { full: false }),
        DEFAULT_FULL => confirm_full(app, &snapshot, true),
        TRUST_CANCEL => perform(app, ProductAction::TrustCancel),
        TRUST_ONCE => perform(app, ProductAction::TrustOnce),
        TRUST_SAVE => perform(app, ProductAction::TrustSave),
        CLEAR => perform(app, ProductAction::ClearLogs),
        FILTER_ALL..=FILTER_INFO => {
            app.product_ui.borrow_mut().filter = (id - FILTER_ALL) as usize;
            refresh(app);
        }
        FOLDER => open_folder(app, &snapshot),
        OBSERVER => {
            app.allow_observer.set(true);
            if let Err(error) = app.open_observer() {
                app.product_ui.borrow_mut().error = error;
                refresh(app);
            }
        }
        _ => {}
    }
}

fn close_command(app: &App) {
    unsafe {
        SendMessageW(app.main.get(), WM_CLOSE, 0, 0);
    }
}

fn copy_code(app: &App, snapshot: &ProductSnapshot) {
    if let Some(code) = &snapshot.code {
        let mut ui = app.product_ui.borrow_mut();
        ui.copied_until = None;
        ui.error.clear();
        if ui.clipboard.request(code).is_err() {
            clipboard_error(snapshot).clone_into(&mut ui.error);
        }
        // WM_COMMAND/UIA Invoke 只提交状态；控件刷新留给下次 tick，避免同步重入。
        ui.last_refresh = None;
    }
}

fn clipboard_error(snapshot: &ProductSnapshot) -> &str {
    snapshot.t(
        "复制失败：剪贴板被占用或响应超时，请重试。",
        "Copy failed: clipboard busy or not responding; try again.",
    )
}

fn poll_clipboard(app: &App) -> bool {
    // 空闲时无需复制含有日志的产品快照；只在待办期间核对控制码和结果。
    if !app.product_ui.borrow().clipboard.pending() {
        return false;
    }
    let snapshot = app.product.borrow().snapshot();
    let mut ui = app.product_ui.borrow_mut();
    ui.clipboard.cancel_unless(if snapshot.can_copy {
        snapshot.code.as_deref()
    } else {
        None
    });
    match ui.clipboard.poll() {
        Some(super::clipboard::Outcome::Copied) => {
            ui.copied_until = Some(Instant::now() + Duration::from_secs(3));
            ui.error.clear();
            true
        }
        Some(super::clipboard::Outcome::Failed) => {
            ui.copied_until = None;
            clipboard_error(&snapshot).clone_into(&mut ui.error);
            true
        }
        None => false,
    }
}

pub(super) fn shutdown_clipboard(app: &App) {
    app.product_ui.borrow_mut().clipboard.shutdown();
}

fn confirm_full(app: &App, snapshot: &ProductSnapshot, default: bool) {
    let (message, title) = if default {
        (
            snapshot.t(
                "将新会话默认权限设为完全控制？此设置会保存到配置文件。",
                "Set full control as the default for new sessions? This preference will be saved.",
            ),
            snapshot.t("确认默认完全控制", "Confirm default full control"),
        )
    } else {
        (snapshot.t(
            "允许当前 AI 会话完全控制？后续操作将不再逐步确认。",
            "Allow full control for this AI session? Subsequent operations will not require individual confirmation."),
         snapshot.t("确认本次完全控制", "Confirm session full control"))
    };
    if confirm(app, message, title) {
        perform(
            app,
            if default {
                ProductAction::SaveDefaultControl { full: true }
            } else {
                ProductAction::RequestControl { full: true }
            },
        );
    }
}

fn open_folder(app: &App, snapshot: &ProductSnapshot) {
    if snapshot.transfer_root.is_empty() {
        return;
    }
    let result = unsafe {
        ShellExecuteW(
            app.main.get(),
            wide("open").as_ptr(),
            wide(&snapshot.transfer_root).as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if result as usize <= 32 {
        snapshot
            .t(
                "无法打开文件接收目录",
                "Unable to open received files folder",
            )
            .clone_into(&mut app.product_ui.borrow_mut().error);
        refresh(app);
    }
}

pub(super) fn request_close(app: &App) -> bool {
    let snapshot = app.product.borrow().snapshot();
    if !snapshot.setup_needed
        && !matches!(snapshot.phase, ProductPhase::Stopped | ProductPhase::Failed)
        && !confirm(
            app,
            if snapshot.controllers > 0 {
                snapshot.t("停止协助并退出？当前控制端连接和运行中的操作将中断。","Stop assistance and exit? Controller connections and running operations will be interrupted.")
            } else {
                snapshot.t(
                    "确认退出 Agent？退出后无法继续接受远程协助。",
                    "Exit Agent? Remote assistance will no longer be available.",
                )
            },
            snapshot.t("退出 RemoteOps", "Exit RemoteOps"),
        )
    {
        return false;
    }
    let result = app.product.borrow_mut().action(ProductAction::Stop);
    if let Err(error) = result {
        app.product_ui.borrow_mut().error = error;
        refresh(app);
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_is_acknowledged_only_after_render_and_idle_frames_do_not_change_revision() {
        let mut host = ProductHost::new(&crate::Options {
            product_demo: true,
            ..crate::Options::default()
        });
        let pending = host.snapshot();
        assert!(pending.unread > 0);
        mark_rendered_logs(&mut host, pending.unread, false).unwrap();
        assert_eq!(host.snapshot().unread, pending.unread);
        assert_eq!(host.snapshot().revision, pending.revision);
        mark_rendered_logs(&mut host, pending.unread, true).unwrap();
        let shown = host.snapshot();
        assert_eq!(shown.unread, 0);
        assert_ne!(shown.revision, pending.revision);
        mark_rendered_logs(&mut host, shown.unread, true).unwrap();
        assert_eq!(host.snapshot().revision, shown.revision);
    }
}
