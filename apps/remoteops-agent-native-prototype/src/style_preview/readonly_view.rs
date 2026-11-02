//! 原生 EDIT 的共享外观：保留标准滚动、输入与无障碍消息，只适配框体和非客户区绘制。
use super::line_measure_cache::LineMeasureCache;
use super::scroll_geometry::{ViewMetrics, layout};
use super::{Panel, fill, round, value, wide};
use std::{
    cell::{Cell, RefCell},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

const SUBCLASS: usize = 0x524F_5053;
const TRACK_TIMER: usize = 0x524F_0101;
const DECOR_CLASS: &str = "RemoteOpsStyleScrollDecoration";
struct Decoration {
    view: Cell<*const View>,
    edit: HWND,
    vertical: bool,
    hwnd: Cell<HWND>,
}
impl Decoration {
    fn new(edit: HWND, vertical: bool) -> Self {
        Self {
            view: Cell::new(null()),
            edit,
            vertical,
            hwnd: Cell::new(null_mut()),
        }
    }
}

pub(super) fn register(instance: HINSTANCE) -> Result<(), String> {
    unsafe {
        let name = wide(DECOR_CLASS);
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(decoration_proc),
            hInstance: instance,
            lpszClassName: name.as_ptr(),
            ..Default::default()
        };
        if RegisterClassExW(&raw const class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err("无法注册滚动条装饰窗口。".into());
        }
    }
    Ok(())
}
struct View {
    roles: RefCell<Vec<remoteops_agent_desktop_host::ObservationRole>>,
    panel: *const Panel,
    frame: Cell<RECT>,
    busy: Cell<bool>,
    content_width: Cell<i32>,
    content_height: Cell<i32>,
    decorations: [Decoration; 2],
    decorating: Cell<bool>,
    // 字体改变时清空，普通追加只测量新完整行。
    line_widths: RefCell<LineMeasureCache>,
    // 文本/选区/滚动同步消息共用一次外观更新，不改变窗口可见状态。
    batch_depth: Cell<usize>,
    measure_pending: Cell<bool>,
    layout_pending: Cell<bool>,
}
impl View {
    fn panel(&self) -> &Panel {
        unsafe { &*self.panel }
    }
}

/// 合并已挂接正文的字体、文本和几何更新，不改变窗口可见性或焦点。
pub(super) fn begin_update(hwnd: HWND) -> bool {
    let Some(ptr) = state(hwnd) else {
        return false;
    };
    unsafe {
        (*ptr)
            .batch_depth
            .set((*ptr).batch_depth.get().saturating_add(1));
    }
    true
}

/// 结束一层更新；最外层才测量、布局和刷新外观。
pub(super) fn end_update(hwnd: HWND) -> bool {
    let Some(ptr) = state(hwnd) else {
        return false;
    };
    if unsafe { (*ptr).batch_depth.get() } == 0 {
        return false;
    }
    unsafe {
        flush_output(hwnd, ptr);
    }
    state(hwnd) == Some(ptr)
}

pub(super) fn attach(panel: &Panel, hwnd: HWND, bounds: [i32; 4]) {
    let [x, y, w, h] = bounds;
    let frame = RECT {
        left: panel.px(x),
        top: panel.px(y),
        right: panel.px(x.saturating_add(w)),
        bottom: panel.px(y.saturating_add(h)),
    };
    let content = inset_geometry(
        [panel.px(x), panel.px(y), panel.px(w), panel.px(h)],
        panel.px(8),
    );
    if let Some(ptr) = state(hwnd) {
        // 同一个原生EDIT只挂接一次，保留测宽缓存、选区和装饰窗生命周期。
        if unsafe { (*ptr).panel } != std::ptr::from_ref(panel) {
            return;
        }
        if begin_update(hwnd) {
            unsafe {
                if apply_bounds(hwnd, ptr, frame, content) == Some(true) {
                    update_layout(hwnd, &*ptr);
                }
            }
            end_update(hwnd);
        }
        return;
    }
    let view = Box::new(View {
        roles: RefCell::default(),
        panel: std::ptr::from_ref(panel),
        frame: Cell::new(frame),
        busy: Cell::new(false),
        content_width: Cell::new(0),
        content_height: Cell::new(0),
        decorations: [Decoration::new(hwnd, true), Decoration::new(hwnd, false)],
        decorating: Cell::new(false),
        line_widths: RefCell::default(),
        batch_depth: Cell::new(1),
        measure_pending: Cell::new(false),
        layout_pending: Cell::new(false),
    });
    unsafe {
        let ptr = Box::into_raw(view);
        if SetWindowSubclass(hwnd, Some(subclass), SUBCLASS, ptr as usize) == 0 {
            drop(Box::from_raw(ptr));
            return;
        }
        for decoration in &(*ptr).decorations {
            decoration.view.set(ptr);
            let overlay = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_NOPARENTNOTIFY,
                wide(DECOR_CLASS).as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                panel.hwnd.get(),
                null_mut(),
                panel.app().instance,
                std::ptr::from_ref(decoration).cast(),
            );
            if overlay.is_null() || SetLayeredWindowAttributes(overlay, 0, 255, LWA_ALPHA) == 0 {
                super::record_native_failure(super::NativeFailure::ScrollDecorationCreation);
            }
        }
        if apply_bounds(hwnd, ptr, frame, content).is_some() {
            measure(hwnd, &*ptr);
            update_layout(hwnd, &*ptr);
            flush_output(hwnd, ptr);
        }
        InvalidateRect(panel.hwnd.get(), null(), 0);
    }
}

fn inset_geometry([x, y, width, height]: [i32; 4], padding: i32) -> [i32; 4] {
    let padding = padding.max(0);
    [
        x.saturating_add(padding),
        y.saturating_add(padding),
        width.saturating_sub(padding.saturating_mul(2)).max(1),
        height.saturating_sub(padding.saturating_mul(2)).max(1),
    ]
}
fn same_frame(left: RECT, right: RECT) -> bool {
    (left.left, left.top, left.right, left.bottom)
        == (right.left, right.top, right.right, right.bottom)
}
unsafe fn apply_bounds(
    hwnd: HWND,
    expected: *const View,
    frame: RECT,
    content: [i32; 4],
) -> Option<bool> {
    unsafe {
        let parent = (*expected).panel().hwnd.get();
        let changed_frame = !same_frame((*expected).frame.replace(frame), frame);
        let mut current = RECT::default();
        let mut point = POINT::default();
        let valid = GetWindowRect(hwnd, &raw mut current) != 0;
        point.x = current.left;
        point.y = current.top;
        let valid = valid && ScreenToClient(parent, &raw mut point) != 0;
        let actual = [
            point.x,
            point.y,
            current.right - current.left,
            current.bottom - current.top,
        ];
        let changed_position = !valid || actual != content;
        if changed_position {
            SetWindowPos(
                hwnd,
                null_mut(),
                content[0],
                content[1],
                content[2],
                content[3],
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if state(hwnd) != Some(expected) {
                return None;
            }
        }
        if changed_frame {
            InvalidateRect(parent, null(), 0);
        }
        (state(hwnd) == Some(expected)).then_some(changed_frame || changed_position)
    }
}

fn state(hwnd: HWND) -> Option<*const View> {
    let mut data = 0;
    (unsafe { GetWindowSubclass(hwnd, Some(subclass), SUBCLASS, &raw mut data) } != 0)
        .then_some(data as *const View)
}
/// 文本控件重建前的阅读状态；不持有旧 HWND 或正文副本。
#[derive(Clone, Copy, Debug)]
pub(super) struct ViewState {
    selection: (u32, u32),
    first_line: i32,
    horizontal: i32,
    at_bottom: bool,
}
impl ViewState {
    /// 选区存在时，外观重建应保留对应正文，避免旧下标指向新日志。
    pub(super) fn has_selection(&self) -> bool {
        self.selection.0 != self.selection.1
    }
}

/// 读取原生选区及视口，供主题或布局重建后恢复。
pub(super) fn capture_view(hwnd: HWND) -> Option<ViewState> {
    let expected = state(hwnd)?;
    unsafe {
        let (mut start, mut end) = (0u32, 0u32);
        SendMessageW(
            hwnd,
            EM_GETSEL,
            &raw mut start as usize,
            &raw mut end as isize,
        );
        let first_line = i32::try_from(SendMessageW(hwnd, EM_GETFIRSTVISIBLELINE, 0, 0))
            .unwrap_or(0)
            .max(0);
        let vertical_visible = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_VSCROLL != 0;
        let vertical = if vertical_visible {
            read_scroll(hwnd, SB_VERT)
        } else {
            None
        };
        let horizontal = read_scroll(hwnd, SB_HORZ).map_or(0, |info| info.nPos.max(0));
        let at_bottom = view_at_bottom(vertical_visible, vertical.as_ref());
        (state(hwnd) == Some(expected)).then_some(ViewState {
            selection: (start, end),
            first_line,
            horizontal,
            at_bottom,
        })
    }
}

/// 恢复重建前的阅读位置；只有原先追尾且无选区时才继续跟随底部。
pub(super) fn restore_view(hwnd: HWND, saved: &ViewState, follow_latest: bool) -> bool {
    let Some(expected) = state(hwnd) else {
        return false;
    };
    let text: Vec<_> = value(hwnd).encode_utf16().collect();
    if state(hwnd) != Some(expected) {
        return false;
    }
    let selection = clamp_selection(&text, saved.selection);
    unsafe {
        let visible = IsWindowVisible(hwnd) != 0;
        (*expected)
            .batch_depth
            .set((*expected).batch_depth.get().saturating_add(1));
        // 超长横向位置恢复包含一次单位探测，整个恢复期间不显示中间滚动状态。
        if visible {
            SendMessageW(hwnd, WM_SETREDRAW, 0, 0);
        }
        if state(hwnd) != Some(expected) {
            return false;
        }
        SendMessageW(
            hwnd,
            EM_SETSEL,
            usize::try_from(selection.0).unwrap_or(usize::MAX),
            isize::try_from(selection.1).unwrap_or(isize::MAX),
        );
        if state(hwnd) != Some(expected) {
            return false;
        }
        if should_follow_latest(saved, follow_latest) {
            SendMessageW(hwnd, WM_VSCROLL, SB_BOTTOM as usize, 0);
        } else {
            let count =
                i32::try_from(SendMessageW(hwnd, EM_GETLINECOUNT, 0, 0)).unwrap_or(i32::MAX);
            let first =
                i32::try_from(SendMessageW(hwnd, EM_GETFIRSTVISIBLELINE, 0, 0)).unwrap_or(0);
            let target = clamp_first_line(saved.first_line, count);
            SendMessageW(
                hwnd,
                EM_LINESCROLL,
                0,
                isize::try_from(target.saturating_sub(first)).unwrap_or(0),
            );
        }
        if state(hwnd) != Some(expected) {
            return false;
        }
        restore_horizontal(hwnd, expected, saved.horizontal);
        if state(hwnd) != Some(expected) {
            return false;
        }
        if visible {
            SendMessageW(hwnd, WM_SETREDRAW, 1, 0);
        }
        if state(hwnd) != Some(expected) {
            return false;
        }
        flush_output(hwnd, expected);
    }
    state(hwnd) == Some(expected)
}

fn read_scroll(hwnd: HWND, bar: SCROLLBAR_CONSTANTS) -> Option<SCROLLINFO> {
    let mut info = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        ..Default::default()
    };
    (unsafe { GetScrollInfo(hwnd, bar, &raw mut info) } != 0).then_some(info)
}

fn scroll_limit(minimum: i32, maximum: i32, page: u32) -> i32 {
    let limit = i64::from(maximum) - i64::from(page.saturating_sub(1));
    i32::try_from(limit.max(i64::from(minimum))).unwrap_or(minimum)
}

fn scroll_at_bottom(position: i32, minimum: i32, maximum: i32, page: u32) -> bool {
    position >= scroll_limit(minimum, maximum, page)
}

fn view_at_bottom(vertical_visible: bool, info: Option<&SCROLLINFO>) -> bool {
    // 无滚动条时正文已全部可见；忽略系统可能返回的默认0..100范围。
    !vertical_visible
        || info.is_some_and(|info| scroll_at_bottom(info.nPos, info.nMin, info.nMax, info.nPage))
}

fn should_follow_latest(saved: &ViewState, enabled: bool) -> bool {
    enabled && saved.at_bottom && saved.selection.0 == saved.selection.1
}

fn clamp_first_line(first_line: i32, line_count: i32) -> i32 {
    first_line.clamp(0, line_count.saturating_sub(1).max(0))
}

fn clamp_selection(text: &[u16], selection: (u32, u32)) -> (u32, u32) {
    let limit = u32::try_from(text.len()).unwrap_or(u32::MAX);
    let (mut start, mut end) = (
        selection.0.min(selection.1).min(limit),
        selection.0.max(selection.1).min(limit),
    );
    let split_pair = |index: u32| {
        let index = usize::try_from(index).unwrap_or(usize::MAX);
        index > 0
            && index < text.len()
            && (0xD800..=0xDBFF).contains(&text[index - 1])
            && (0xDC00..=0xDFFF).contains(&text[index])
    };
    if start == end {
        if split_pair(start) {
            start -= 1;
        }
        return (start, start);
    }
    if split_pair(start) {
        start -= 1;
    }
    if split_pair(end) {
        end += 1;
    }
    (start, end)
}

fn horizontal_parts(position: i32, unit: i32) -> Option<(u16, usize)> {
    if position < 0 || unit <= 0 || unit > i32::from(u16::MAX) {
        return None;
    }
    Some((
        u16::try_from(position % unit).ok()?,
        usize::try_from(position / unit).ok()?,
    ))
}

unsafe fn restore_horizontal(hwnd: HWND, expected: *const View, position: i32) {
    unsafe {
        let Some(info) = read_scroll(hwnd, SB_HORZ) else {
            return;
        };
        let target = position.clamp(
            info.nMin.max(0),
            scroll_limit(info.nMin, info.nMax, info.nPage).max(0),
        );
        if let Ok(short) = u16::try_from(target) {
            SendMessageW(
                hwnd,
                WM_HSCROLL,
                (usize::from(short) << 16) | SB_THUMBPOSITION as usize,
                0,
            );
            return;
        }
        // WM_HSCROLL 的位置只有16位；EM_LINESCROLL参数是字符数，不能直接传滚动条位置。
        // 从最左端测一次原生单位，再用短余数和整字符数恢复，避免超长行位置截断。
        SendMessageW(hwnd, WM_HSCROLL, SB_LEFT as usize, 0);
        if state(hwnd) != Some(expected) {
            return;
        }
        SendMessageW(hwnd, EM_LINESCROLL, 1, 0);
        if state(hwnd) != Some(expected) {
            return;
        }
        let unit = read_scroll(hwnd, SB_HORZ).map_or(0, |info| info.nPos);
        let Some((remainder, columns)) = horizontal_parts(target, unit) else {
            // 无法识别原生单位时安全回到最左，不保留探测造成的偏移。
            SendMessageW(hwnd, WM_HSCROLL, SB_LEFT as usize, 0);
            return;
        };
        SendMessageW(
            hwnd,
            WM_HSCROLL,
            (usize::from(remainder) << 16) | SB_THUMBPOSITION as usize,
            0,
        );
        if state(hwnd) == Some(expected) {
            SendMessageW(hwnd, EM_LINESCROLL, columns, 0);
        }
    }
}

/// 合并文本、选区和滚动的外观更新；未挂接时返回 false，供调用方保留标准 EDIT 路径。
pub(super) fn replace_output(hwnd: HWND, text: &str, force: bool) -> bool {
    let Some(expected) = state(hwnd) else {
        return false;
    };
    let changed = value(hwnd) != text;
    if state(hwnd) != Some(expected) || (!changed && !force) {
        return true;
    }
    unsafe {
        (*expected)
            .batch_depth
            .set((*expected).batch_depth.get().saturating_add(1));
        if changed {
            SetWindowTextW(hwnd, wide(text).as_ptr());
            if state(hwnd) != Some(expected) {
                return true;
            }
        }
        let end = GetWindowTextLengthW(hwnd).max(0) as usize;
        if state(hwnd) != Some(expected) {
            return true;
        }
        SendMessageW(hwnd, EM_SETSEL, end, end.cast_signed());
        if state(hwnd) != Some(expected) {
            return true;
        }
        SendMessageW(hwnd, EM_SCROLLCARET, 0, 0);
        if state(hwnd) != Some(expected) {
            return true;
        }
        if force {
            SendMessageW(hwnd, WM_VSCROLL, SB_BOTTOM as usize, 0);
            if state(hwnd) != Some(expected) {
                return true;
            }
        }
        flush_output(hwnd, expected);
    }
    true
}
unsafe fn flush_output(hwnd: HWND, expected: *const View) {
    unsafe {
        let depth = (*expected).batch_depth.get();
        if depth > 1 {
            (*expected).batch_depth.set(depth - 1);
            return;
        }
        let measure = (*expected).measure_pending.replace(false);
        let layout = (*expected).layout_pending.replace(false);
        // 保持批量标志到原生布局结束，抑制布局触发的重入绘制。
        if measure {
            measure_now(hwnd, &*expected);
            if state(hwnd) != Some(expected) {
                return;
            }
        }
        if measure || layout {
            update_layout_now(hwnd, &*expected);
            if state(hwnd) != Some(expected) {
                return;
            }
        }
        (*expected).measure_pending.set(false);
        (*expected).layout_pending.set(false);
        (*expected).batch_depth.set(0);
        InvalidateRect(hwnd, null(), 0);
        paint_window(hwnd, &*expected);
    }
}
pub(super) fn contains(hwnd: HWND) -> bool {
    state(hwnd).is_some()
}
pub(super) fn paint_frames(panel: &Panel, dc: HDC) {
    let handles: Vec<_> = panel.controls.borrow().values().copied().collect();
    let colors = panel.app().colors();
    for hwnd in handles {
        if let Some(ptr) = state(hwnd) {
            let frame = unsafe { (*ptr).frame.get() };
            round(dc, frame, colors.content, colors.border, panel.px(8));
        }
    }
}
fn measure(hwnd: HWND, view: &View) {
    if view.batch_depth.get() != 0 {
        view.measure_pending.set(true);
        return;
    }
    measure_now(hwnd, view);
}
fn measure_now(hwnd: HWND, view: &View) {
    unsafe {
        let text = value(hwnd);
        let dc = GetDC(hwnd);
        let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
        let old = SelectObject(dc, font);
        let mut metrics = TEXTMETRICW::default();
        GetTextMetricsW(dc, &raw mut metrics);
        let tab = metrics.tmAveCharWidth.max(1).saturating_mul(8);
        // 不跨 GDI/原生调用持有 RefCell 可变借用。
        let mut cache = std::mem::take(&mut *view.line_widths.borrow_mut());
        let maximum = cache.maximum(&text, |line| {
            let mut width = 0i32;
            for (index, part) in line.split('\t').enumerate() {
                if index != 0 {
                    width = width
                        .saturating_div(tab)
                        .saturating_add(1)
                        .saturating_mul(tab);
                }
                let encoded = wide(part);
                let mut extent = SIZE::default();
                GetTextExtentPoint32W(
                    dc,
                    encoded.as_ptr(),
                    i32::try_from(encoded.len() - 1).unwrap_or(i32::MAX),
                    &raw mut extent,
                );
                width = width.saturating_add(extent.cx);
            }
            width
        });
        view.line_widths.replace(cache);
        SelectObject(dc, old);
        ReleaseDC(hwnd, dc);
        let margins = SendMessageW(hwnd, EM_GETMARGINS, 0, 0) as u32;
        let inset = i32::try_from((margins & 0xffff) + (margins >> 16)).unwrap_or(0);
        view.content_width.set(if text.is_empty() {
            0
        } else {
            maximum.saturating_add(inset).saturating_add(1)
        });
        let lines = i32::try_from(SendMessageW(hwnd, EM_GETLINECOUNT, 0, 0)).unwrap_or(i32::MAX);
        view.content_height.set(if text.is_empty() {
            0
        } else {
            lines.saturating_mul(metrics.tmHeight.max(1))
        });
    }
}
fn update_layout(hwnd: HWND, view: &View) {
    if view.batch_depth.get() != 0 {
        view.layout_pending.set(true);
        return;
    }
    let expected = std::ptr::from_ref(view);
    let changed = update_layout_now(hwnd, view);
    if state(hwnd) != Some(expected) {
        return;
    }
    // 默认过程可能同步销毁控件；原生调用后只使用核验过的实例。
    let view = unsafe { &*expected };
    if changed {
        paint_window(hwnd, view);
    } else {
        // 尺寸/位置可变而滚动条显隐不变，只同步装饰窗几何。
        sync_decorations(hwnd, view);
    }
}
fn update_layout_now(hwnd: HWND, view: &View) -> bool {
    if view.busy.replace(true) {
        return false;
    }
    let expected = std::ptr::from_ref(view);
    unsafe {
        let mut bounds = RECT::default();
        GetWindowRect(hwnd, &raw mut bounds);
        let dpi = view.panel().dpi.get();
        let result = layout(ViewMetrics {
            width: bounds.right - bounds.left,
            height: bounds.bottom - bounds.top,
            content_width: view.content_width.get(),
            content_height: view.content_height.get(),
            padding: 0,
            bar_width: GetSystemMetricsForDpi(SM_CXVSCROLL, dpi),
            bar_height: GetSystemMetricsForDpi(SM_CYHSCROLL, dpi),
        });
        let mut changed = false;
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        if (style & WS_VSCROLL != 0) != result.vertical {
            changed = true;
            ShowScrollBar(hwnd, SB_VERT, i32::from(result.vertical));
            if state(hwnd) != Some(expected) {
                return false;
            }
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        if (style & WS_HSCROLL != 0) != result.horizontal {
            changed = true;
            ShowScrollBar(hwnd, SB_HORZ, i32::from(result.horizontal));
            if state(hwnd) != Some(expected) {
                return false;
            }
        }
        view.busy.set(false);
        changed
    }
}
fn scrollbar(dc: HDC, hwnd: HWND, view: &View, vertical: bool, origin: &RECT) -> Option<RECT> {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        if style & if vertical { WS_VSCROLL } else { WS_HSCROLL } == 0 {
            return None;
        }
        let mut info = SCROLLBARINFO {
            cbSize: std::mem::size_of::<SCROLLBARINFO>() as u32,
            ..Default::default()
        };
        if GetScrollBarInfo(
            hwnd,
            if vertical {
                OBJID_VSCROLL
            } else {
                OBJID_HSCROLL
            },
            &raw mut info,
        ) == 0
            || info.rgstate[0] & 0x8000 != 0
        {
            return None;
        }
        let rect = RECT {
            left: info.rcScrollBar.left - origin.left,
            top: info.rcScrollBar.top - origin.top,
            right: info.rcScrollBar.right - origin.left,
            bottom: info.rcScrollBar.bottom - origin.top,
        };
        let panel = view.panel();
        let colors = panel.app().colors();
        fill(dc, &rect, colors.content);
        let mut cursor = POINT::default();
        GetCursorPos(&raw mut cursor);
        let hot = PtInRect(&raw const info.rcScrollBar, cursor) != 0 || GetCapture() == hwnd;
        let thumb_color = if hot {
            colors.scroll_hot
        } else {
            colors.scroll_thumb
        };
        if info.xyThumbBottom > info.xyThumbTop {
            let side = panel.px(6).max(2);
            let thumb = if vertical {
                let center = i32::midpoint(rect.left, rect.right);
                RECT {
                    left: center - side / 2,
                    top: rect.top + info.xyThumbTop + 1,
                    right: center + (side + 1) / 2,
                    bottom: rect.top + info.xyThumbBottom - 1,
                }
            } else {
                let center = i32::midpoint(rect.top, rect.bottom);
                RECT {
                    left: rect.left + info.xyThumbTop + 1,
                    top: center - side / 2,
                    right: rect.left + info.xyThumbBottom - 1,
                    bottom: center + (side + 1) / 2,
                }
            };
            round(dc, thumb, thumb_color, thumb_color, side);
        }
        // 两端箭头沿用系统真实命中区，用低对比小折线提示，不隐藏仍可操作的区域。
        let pen = CreatePen(PS_SOLID, panel.px(1).max(1), colors.scroll_thumb);
        let previous = SelectObject(dc, pen);
        let delta = panel.px(2).max(1);
        for last in [false, true] {
            let offset = if last {
                if vertical { rect.bottom } else { rect.right }
            } else {
                if vertical { rect.top } else { rect.left }
            };
            let center = offset
                + if last {
                    -info.dxyLineButton / 2
                } else {
                    info.dxyLineButton / 2
                };
            let direction = if last { 1 } else { -1 };
            if vertical {
                let x = i32::midpoint(rect.left, rect.right);
                MoveToEx(dc, x - delta, center - direction * delta / 2, null_mut());
                LineTo(dc, x, center + direction * delta / 2);
                LineTo(dc, x + delta, center - direction * delta / 2);
            } else {
                let y = i32::midpoint(rect.top, rect.bottom);
                MoveToEx(dc, center - direction * delta / 2, y - delta, null_mut());
                LineTo(dc, center + direction * delta / 2, y);
                LineTo(dc, center - direction * delta / 2, y + delta);
            }
        }
        SelectObject(dc, previous);
        DeleteObject(pen);
        Some(rect)
    }
}
fn paint(dc: HDC, hwnd: HWND, view: &View) {
    unsafe {
        let saved = SaveDC(dc);
        let mut window = RECT::default();
        GetWindowRect(hwnd, &raw mut window);
        let vertical = scrollbar(dc, hwnd, view, true, &window);
        let horizontal = scrollbar(dc, hwnd, view, false, &window);
        if let (Some(v), Some(h)) = (vertical, horizontal) {
            fill(
                dc,
                &RECT {
                    left: v.left,
                    top: h.top,
                    right: v.right,
                    bottom: h.bottom,
                },
                view.panel().app().colors().content,
            );
        }
        RestoreDC(dc, saved);
    }
}
pub(super) fn roles(hwnd: HWND) -> Vec<remoteops_agent_desktop_host::ObservationRole> {
    state(hwnd).map_or_else(Vec::new, |ptr| unsafe { (*ptr).roles.borrow().clone() })
}

pub(super) fn set_roles(hwnd: HWND, roles: Vec<remoteops_agent_desktop_host::ObservationRole>) {
    if let Some(ptr) = state(hwnd) {
        unsafe {
            let mut current = (*ptr).roles.borrow_mut();
            if *current == roles {
                return;
            }
            *current = roles;
            drop(current);
            InvalidateRect(hwnd, null(), 0);
        }
    }
}

fn font_changed(previous: isize, requested: WPARAM) -> bool {
    previous != requested.cast_signed()
}

fn role_color(role: remoteops_agent_desktop_host::ObservationRole, dark: bool) -> u32 {
    use remoteops_agent_desktop_host::ObservationRole;
    let rgb = match (role, dark) {
        (ObservationRole::Command, false) => 0x1D_4E_D8,
        (ObservationRole::Command, true) => 0x7D_D3_FC,
        (ObservationRole::Success, false) => 0x16_65_34,
        (ObservationRole::Success, true) => 0x86_EF_AC,
        (ObservationRole::Error, false) => 0xB9_1C_1C,
        (ObservationRole::Error, true) => 0xFD_A4_AF,
        (ObservationRole::Info, false) => 0x47_55_69,
        (ObservationRole::Info, true) => 0x94_A3_B8,
        (ObservationRole::Output, false) => 0x17_20_33,
        (ObservationRole::Output, true) => 0xE2_E8_F0,
    };
    ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF)
}

// 原生EDIT先完成文本和选择区绘制；仅给可见的未选中字形换色。
fn paint_terminal(hwnd: HWND, view: &View) {
    if view.batch_depth.get() != 0 {
        return;
    }
    unsafe {
        let dc = GetDC(hwnd);
        if !dc.is_null() {
            paint_terminal_to(dc, hwnd, view);
            ReleaseDC(hwnd, dc);
        }
    }
}

// dc原点为EDIT客户区；只在目标DC绘制，打印路径不接触屏幕DC。
fn paint_terminal_to(dc: HDC, hwnd: HWND, view: &View) {
    if dc.is_null() || view.batch_depth.get() != 0 || view.panel().kind != super::Kind::Observer {
        return;
    }
    unsafe {
        let mut scroll = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_POS,
            ..Default::default()
        };
        let position = (GetScrollInfo(hwnd, SB_HORZ, &raw mut scroll) != 0).then_some(scroll.nPos);
        // 别的超长行也会撑大横向范围；远滚时短行的packed16坐标同样可能wrap。
        if !color_horizontal_supported(view.content_width.get(), position) {
            return;
        }
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
        SelectObject(dc, font);
        let mut rect = RECT::default();
        SendMessageW(hwnd, EM_GETRECT, 0, &raw mut rect as isize);
        IntersectClipRect(dc, rect.left, rect.top, rect.right, rect.bottom);
        let mut metrics = TEXTMETRICW::default();
        GetTextMetricsW(dc, &raw mut metrics);
        let height = metrics.tmHeight.max(1);
        let first = SendMessageW(hwnd, EM_GETFIRSTVISIBLELINE, 0, 0).max(0) as usize;
        let count = ((rect.bottom - rect.top).max(0) / height + 2) as usize;
        let (mut selected_start, mut selected_end) = (0u32, 0u32);
        SendMessageW(
            hwnd,
            EM_GETSEL,
            &raw mut selected_start as usize,
            &raw mut selected_end as isize,
        );
        let colors = view.panel().app().colors();
        SetBkColor(dc, colors.content);
        let roles = view.roles.borrow();
        for (line, role) in roles.iter().enumerate().skip(first).take(count) {
            let index = SendMessageW(hwnd, EM_LINEINDEX, line, 0);
            if index < 0 {
                break;
            }
            let length = SendMessageW(hwnd, EM_LINELENGTH, index as usize, 0).max(0) as usize;
            // ExtTextOutW一次最多8192字；超长正文交还native完整绘制，不叠一截颜色。
            if length > 8192 {
                continue;
            }
            let mut text = vec![0u16; length.saturating_add(1)];
            text[0] = u16::try_from(length.min(65_534)).unwrap_or(65_534);
            let copied =
                SendMessageW(hwnd, EM_GETLINE, line, text.as_mut_ptr() as isize).max(0) as usize;
            text.truncate(copied);
            let index = index as usize;
            let selection = (selected_start as usize, selected_end as usize);
            if !can_color_native_line(&text, index, selection) {
                continue;
            }
            let mut line_extent = SIZE::default();
            if GetTextExtentPoint32W(
                dc,
                text.as_ptr(),
                i32::try_from(text.len()).unwrap_or(i32::MAX),
                &raw mut line_extent,
            ) == 0
                || !color_width_supported(line_extent.cx)
            {
                continue;
            }
            for (start, end) in unselected_segments(index, copied, selection) {
                if start == end {
                    continue;
                }
                let position = SendMessageW(hwnd, EM_POSFROMCHAR, index + start, 0);
                if position == -1 {
                    continue;
                }
                let location = position as u32;
                let x = i32::from((location as u16).cast_signed());
                let y = i32::from(((location >> 16) as u16).cast_signed());
                if y + height < rect.top || y >= rect.bottom {
                    continue;
                }
                let slice = &text[start..end];
                let mut extent = SIZE::default();
                GetTextExtentPoint32W(
                    dc,
                    slice.as_ptr(),
                    i32::try_from(slice.len()).unwrap_or(i32::MAX),
                    &raw mut extent,
                );
                let area = RECT {
                    left: x,
                    top: y,
                    right: x.saturating_add(extent.cx),
                    bottom: y + height,
                };
                SetTextColor(dc, role_color(*role, colors.bg == super::palette(true).bg));
                ExtTextOutW(
                    dc,
                    x,
                    y,
                    ETO_OPAQUE,
                    &raw const area,
                    slice.as_ptr(),
                    slice.len() as u32,
                    null(),
                );
            }
        }
        RestoreDC(dc, saved);
    }
}

fn print_client_requested(message: u32, flags: i32, visible: bool) -> bool {
    (message == WM_PRINTCLIENT || flags & PRF_CLIENT != 0)
        && (flags & PRF_CHECKVISIBLE == 0 || visible)
}

// WM_PRINT使用窗口原点，WM_PRINTCLIENT已是客户区原点；均保留调用者映射和裁剪。
fn print_terminal(dc: HDC, hwnd: HWND, view: &View, message: u32) {
    if dc.is_null() || view.batch_depth.get() != 0 {
        return;
    }
    if message == WM_PRINTCLIENT {
        paint_terminal_to(dc, hwnd, view);
        return;
    }
    unsafe {
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        let mut window = RECT::default();
        let mut client = POINT::default();
        if GetWindowRect(hwnd, &raw mut window) != 0 && ClientToScreen(hwnd, &raw mut client) != 0 {
            OffsetViewportOrgEx(
                dc,
                client.x - window.left,
                client.y - window.top,
                null_mut(),
            );
            paint_terminal_to(dc, hwnd, view);
        }
        RestoreDC(dc, saved);
    }
}

fn color_horizontal_supported(content_width: i32, position: Option<i32>) -> bool {
    content_width <= i32::from(i16::MAX) || position == Some(0)
}

fn color_width_supported(width: i32) -> bool {
    (0..=i32::from(i16::MAX)).contains(&width)
}

// 对制表符和落在surrogate中间的选区保留原生排版，避免破坏真实文字或选择。
fn can_color_native_line(text: &[u16], index: usize, selection: (usize, usize)) -> bool {
    if text.len() > 8192
        || text.contains(&9)
        || std::char::decode_utf16(text.iter().copied()).any(|ch| ch.is_err())
    {
        return false;
    }
    for boundary in [selection.0, selection.1] {
        let offset = boundary.saturating_sub(index);
        if boundary >= index
            && offset > 0
            && offset < text.len()
            && (0xDC00..=0xDFFF).contains(&text[offset])
            && (0xD800..=0xDBFF).contains(&text[offset - 1])
        {
            return false;
        }
    }
    true
}

fn unselected_segments(
    index: usize,
    length: usize,
    selection: (usize, usize),
) -> [(usize, usize); 2] {
    if selection.0 == selection.1 {
        return [(0, length), (0, 0)];
    }
    [
        (0, selection.0.saturating_sub(index).min(length)),
        (selection.1.saturating_sub(index).min(length), length),
    ]
}

fn paint_window(hwnd: HWND, view: &View) {
    if view.batch_depth.get() != 0 {
        return;
    }
    unsafe {
        let dc = GetWindowDC(hwnd);
        if !dc.is_null() {
            paint(dc, hwnd, view);
            ReleaseDC(hwnd, dc);
        }
        sync_decorations(hwnd, view);
    }
}

fn sync_decorations(edit: HWND, view: &View) {
    if view.decorating.replace(true) {
        return;
    }
    unsafe {
        let mut parent_origin = POINT::default();
        ClientToScreen(view.panel().hwnd.get(), &raw mut parent_origin);
        let mut edit_rect = RECT::default();
        GetWindowRect(edit, &raw mut edit_rect);
        let style = GetWindowLongPtrW(edit, GWL_STYLE) as u32;
        for decoration in &view.decorations {
            let overlay = decoration.hwnd.get();
            if overlay.is_null() {
                continue;
            }
            let mut info = SCROLLBARINFO {
                cbSize: std::mem::size_of::<SCROLLBARINFO>() as u32,
                ..Default::default()
            };
            let visible = IsWindowVisible(edit) != 0
                && style
                    & if decoration.vertical {
                        WS_VSCROLL
                    } else {
                        WS_HSCROLL
                    }
                    != 0
                && GetScrollBarInfo(
                    edit,
                    if decoration.vertical {
                        OBJID_VSCROLL
                    } else {
                        OBJID_HSCROLL
                    },
                    &raw mut info,
                ) != 0
                && info.rgstate[0] & 0x8000 == 0;
            if !visible {
                ShowWindow(overlay, SW_HIDE);
                continue;
            }
            let mut bounds = info.rcScrollBar;
            // 纵条顺带覆盖双条交角，避免系统在该小方块重新画浅底。
            if decoration.vertical {
                bounds.bottom = edit_rect.bottom;
            }
            let mut current = RECT::default();
            GetWindowRect(overlay, &raw mut current);
            if (current.left, current.top, current.right, current.bottom)
                != (bounds.left, bounds.top, bounds.right, bounds.bottom)
                || IsWindowVisible(overlay) == 0
            {
                SetWindowPos(
                    overlay,
                    HWND_TOP,
                    bounds.left - parent_origin.x,
                    bounds.top - parent_origin.y,
                    bounds.right - bounds.left,
                    bounds.bottom - bounds.top,
                    SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
                );
            }
            // 交给正常消息循环合并绘制；不要每条输出强制同步画两次装饰条。
            InvalidateRect(overlay, null(), 0);
        }
    }
    view.decorating.set(false);
}
fn paint_decoration(hwnd: HWND, dc: HDC, decoration: &Decoration) {
    unsafe {
        let view = &*decoration.view.get();
        let mut client = RECT::default();
        GetClientRect(hwnd, &raw mut client);
        fill(dc, &client, view.panel().app().colors().content);
        let mut origin = RECT::default();
        GetWindowRect(hwnd, &raw mut origin);
        scrollbar(dc, decoration.edit, view, decoration.vertical, &origin);
    }
}
unsafe extern "system" fn decoration_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let paint_started = matches!(msg, WM_PAINT | WM_NCPAINT | WM_PRINT | WM_PRINTCLIENT)
        .then(std::time::Instant::now);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        if msg == WM_NCCREATE {
            let create = &*(l as *const CREATESTRUCTW);
            let ptr = create.lpCreateParams as *const Decoration;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
            (*ptr).hwnd.set(hwnd);
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Decoration;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        match msg {
            WM_NCHITTEST => HTTRANSPARENT as isize,
            WM_MOUSEACTIVATE => MA_NOACTIVATE.cast_signed() as isize,
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &raw mut paint);
                paint_decoration(hwnd, dc, &*ptr);
                EndPaint(hwnd, &raw const paint);
                0
            }
            WM_PRINT => {
                let flags = l as i32;
                if flags & PRF_CLIENT != 0
                    && (flags & PRF_CHECKVISIBLE == 0 || IsWindowVisible(hwnd) != 0)
                {
                    paint_decoration(hwnd, w as HDC, &*ptr);
                }
                0
            }
            WM_PRINTCLIENT => {
                if l as i32 & PRF_CHECKVISIBLE == 0 || IsWindowVisible(hwnd) != 0 {
                    paint_decoration(hwnd, w as HDC, &*ptr);
                }
                0
            }
            WM_NCDESTROY => {
                (*ptr).hwnd.set(null_mut());
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, msg, w, l)
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }));
    if let Some(started) = paint_started {
        super::render_plan::record_paint(started.elapsed().as_micros());
    }
    if let Ok(result) = outcome {
        result
    } else {
        super::record_native_failure(super::NativeFailure::ScrollDecorationCallback);
        0
    }
}
unsafe extern "system" fn track_tick(hwnd: HWND, _msg: u32, _id: usize, _time: u32) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        if GetCapture() != hwnd || IsWindowVisible(hwnd) == 0 {
            KillTimer(hwnd, TRACK_TIMER);
            return;
        }
        if let Some(ptr) = state(hwnd) {
            sync_decorations(hwnd, &*ptr);
        } else {
            KillTimer(hwnd, TRACK_TIMER);
        }
    }))
    .is_err()
    {
        super::record_native_failure(super::NativeFailure::ScrollTrackingCallback);
    }
}
pub(super) fn cancel_interaction(panel: &Panel) {
    let handles: Vec<_> = panel.controls.borrow().values().copied().collect();
    unsafe {
        for hwnd in handles {
            if contains(hwnd) {
                KillTimer(hwnd, TRACK_TIMER);
                if GetCapture() == hwnd {
                    SendMessageW(hwnd, WM_CANCELMODE, 0, 0);
                    if GetCapture() == hwnd {
                        ReleaseCapture();
                    }
                }
            }
        }
    }
}
unsafe extern "system" fn subclass(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    // 包含原生EDIT绘制及终端着色，异步WM_PAINT同样计入验收，不能只看重建耗时。
    let paint_started = matches!(msg, WM_PAINT | WM_NCPAINT | WM_PRINT | WM_PRINTCLIENT)
        .then(std::time::Instant::now);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        subclass_inner(hwnd, msg, w, l, data)
    }));
    if let Some(started) = paint_started {
        super::render_plan::record_paint(started.elapsed().as_micros());
    }
    if let Ok(result) = outcome {
        result
    } else {
        super::record_native_failure(super::NativeFailure::TextCallback);
        0
    }
}
unsafe fn subclass_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM, data: usize) -> LRESULT {
    unsafe {
        match msg {
            WM_NCPAINT => {
                paint_window(hwnd, &*(data as *const View));
                0
            }
            WM_PRINT | WM_PRINTCLIENT => {
                let result = DefSubclassProc(hwnd, msg, w, l);
                let flags = l as i32;
                if msg == WM_PRINT
                    && flags & PRF_NONCLIENT != 0
                    && (flags & PRF_CHECKVISIBLE == 0 || IsWindowVisible(hwnd) != 0)
                    && let Some(ptr) = state(hwnd).filter(|ptr| *ptr as usize == data)
                {
                    paint(w as HDC, hwnd, &*ptr);
                }
                if print_client_requested(msg, flags, IsWindowVisible(hwnd) != 0)
                    && let Some(ptr) = state(hwnd).filter(|ptr| *ptr as usize == data)
                {
                    print_terminal(w as HDC, hwnd, &*ptr, msg);
                }
                result
            }
            WM_NCDESTROY => {
                KillTimer(hwnd, TRACK_TIMER);
                RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS);
                for decoration in &(*(data as *const View)).decorations {
                    let overlay = decoration.hwnd.get();
                    if !overlay.is_null() {
                        DestroyWindow(overlay);
                    }
                }
                let result = DefSubclassProc(hwnd, msg, w, l);
                drop(Box::from_raw(data as *mut View));
                result
            }
            WM_SETTEXT | WM_SETFONT => {
                let previous_text = (msg == WM_SETTEXT).then(|| value(hwnd));
                let previous_font = if msg == WM_SETFONT {
                    SendMessageW(hwnd, WM_GETFONT, 0, 0)
                } else {
                    0
                };
                // 原生过程仍接收全部消息和通知，只省略相同材料的重复测量。
                let result = DefSubclassProc(hwnd, msg, w, l);
                if let Some(ptr) = state(hwnd).filter(|ptr| *ptr as usize == data) {
                    let changed = if msg == WM_SETFONT {
                        let changed = font_changed(previous_font, w);
                        if changed {
                            (*ptr).line_widths.borrow_mut().invalidate();
                        }
                        changed
                    } else {
                        previous_text
                            .as_ref()
                            .is_some_and(|before| *before != value(hwnd))
                    };
                    if changed && state(hwnd) == Some(ptr) {
                        measure(hwnd, &*ptr);
                        if let Some(ptr) = state(hwnd).filter(|ptr| *ptr as usize == data) {
                            update_layout(hwnd, &*ptr);
                        }
                    }
                }
                result
            }
            WM_SIZE => {
                let result = DefSubclassProc(hwnd, msg, w, l);
                if let Some(ptr) = state(hwnd).filter(|ptr| *ptr as usize == data) {
                    update_layout(hwnd, &*ptr);
                }
                result
            }
            WM_NCLBUTTONDOWN => {
                SetTimer(hwnd, TRACK_TIMER, 16, Some(track_tick));
                let result = DefSubclassProc(hwnd, msg, w, l);
                KillTimer(hwnd, TRACK_TIMER);
                if let Some(ptr) = state(hwnd).filter(|ptr| *ptr as usize == data) {
                    paint_window(hwnd, &*ptr);
                }
                result
            }
            WM_SHOWWINDOW => {
                if w == 0 {
                    KillTimer(hwnd, TRACK_TIMER);
                }
                let result = DefSubclassProc(hwnd, msg, w, l);
                if let Some(ptr) = state(hwnd).filter(|ptr| *ptr as usize == data) {
                    if w == 0 {
                        for decoration in &(*ptr).decorations {
                            let overlay = decoration.hwnd.get();
                            if !overlay.is_null() {
                                ShowWindow(overlay, SW_HIDE);
                            }
                        }
                    } else {
                        sync_decorations(hwnd, &*ptr);
                    }
                }
                result
            }
            WM_CANCELMODE | WM_CAPTURECHANGED => {
                KillTimer(hwnd, TRACK_TIMER);
                let result = DefSubclassProc(hwnd, msg, w, l);
                if let Some(ptr) = state(hwnd).filter(|ptr| *ptr as usize == data) {
                    paint_window(hwnd, &*ptr);
                }
                result
            }
            WM_NCMOUSEMOVE => {
                let mut tracking = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE | TME_NONCLIENT,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&raw mut tracking);
                let result = DefSubclassProc(hwnd, msg, w, l);
                if let Some(ptr) = state(hwnd).filter(|ptr| *ptr as usize == data) {
                    paint_window(hwnd, &*ptr);
                }
                result
            }
            WM_PAINT | WM_HSCROLL | WM_VSCROLL | WM_MOUSEWHEEL | WM_MOUSEHWHEEL | WM_KEYDOWN
            | WM_KEYUP | WM_NCMOUSELEAVE | WM_NCLBUTTONUP | EM_SCROLLCARET | EM_LINESCROLL
            | EM_SETSEL => {
                let result = DefSubclassProc(hwnd, msg, w, l);
                if let Some(ptr) = state(hwnd).filter(|ptr| *ptr as usize == data) {
                    paint_window(hwnd, &*ptr);
                    paint_terminal(hwnd, &*ptr);
                }
                result
            }
            _ => DefSubclassProc(hwnd, msg, w, l),
        }
    }
}

#[cfg(test)]
mod terminal_tests {
    use super::*;
    use remoteops_agent_desktop_host::ObservationRole;

    #[test]
    fn rebuild_follows_only_an_unselected_bottom_view() {
        let mut saved = ViewState {
            selection: (24, 24),
            first_line: 90,
            horizontal: 28,
            at_bottom: true,
        };
        assert!(should_follow_latest(&saved, true));
        assert!(!should_follow_latest(&saved, false));
        saved.at_bottom = false;
        assert!(!should_follow_latest(&saved, true));
        saved.at_bottom = true;
        saved.selection = (20, 24);
        assert!(!should_follow_latest(&saved, true));
    }

    #[test]
    fn view_selection_distinguishes_caret_from_a_selected_range() {
        let mut saved = ViewState {
            selection: (24, 24),
            first_line: 0,
            horizontal: 0,
            at_bottom: false,
        };
        assert!(!saved.has_selection());
        saved.selection = (20, 24);
        assert!(saved.has_selection());
        saved.selection = (24, 20);
        assert!(saved.has_selection());
    }

    #[test]
    fn bottom_detection_accounts_for_visible_page_and_extreme_ranges() {
        assert!(!scroll_at_bottom(89, 0, 99, 10));
        assert!(scroll_at_bottom(90, 0, 99, 10));
        assert!(scroll_at_bottom(99, 0, 99, 10));
        assert!(scroll_at_bottom(0, 0, 99, 100));
        assert!(scroll_at_bottom(99, 0, 99, 0));
        assert_eq!(scroll_limit(0, i32::MAX, u32::MAX), 0);
        assert_eq!(scroll_limit(5, 5, 100), 5);
    }

    #[test]
    fn absent_vertical_bar_is_bottom_even_with_a_default_scroll_range() {
        let default_range = SCROLLINFO {
            nMax: 100,
            ..Default::default()
        };
        assert!(view_at_bottom(false, Some(&default_range)));
        assert!(view_at_bottom(false, None));
        assert!(!view_at_bottom(true, Some(&default_range)));
        assert!(!view_at_bottom(true, None));
    }

    #[test]
    fn shortened_output_clamps_viewport_without_moving_valid_history() {
        assert_eq!(clamp_first_line(12, 100), 12);
        assert_eq!(clamp_first_line(90, 8), 7);
        assert_eq!(clamp_first_line(90, 0), 0);
        assert_eq!(clamp_first_line(-1, 100), 0);
    }

    #[test]
    fn rebuilt_selection_clamps_and_keeps_surrogate_pairs_intact() {
        let text: Vec<_> = "A中😀B".encode_utf16().collect();
        assert_eq!(clamp_selection(&text, (1, 4)), (1, 4));
        assert_eq!(clamp_selection(&text, (3, 3)), (2, 2));
        assert_eq!(clamp_selection(&text, (3, 4)), (2, 4));
        assert_eq!(clamp_selection(&text, (2, 3)), (2, 4));
        assert_eq!(clamp_selection(&text, (100, 100)), (5, 5));
        assert_eq!(clamp_selection(&text, (8, 1)), (1, 5));
        assert_eq!(clamp_selection(&[], (2, 4)), (0, 0));
    }

    #[test]
    fn wide_horizontal_offsets_are_split_without_sixteen_bit_truncation() {
        assert_eq!(horizontal_parts(70_001, 7), Some((1, 10_000)));
        assert_eq!(horizontal_parts(i32::MAX, 1), Some((0, 2_147_483_647)));
        for position in [0, 65_535, 65_536, 100_000, i32::MAX] {
            let (remainder, columns) = horizontal_parts(position, 11).unwrap();
            assert_eq!(
                columns * 11 + usize::from(remainder),
                usize::try_from(position).unwrap()
            );
        }
        assert_eq!(horizontal_parts(70_000, 0), None);
        assert_eq!(horizontal_parts(70_000, 65_536), None);
        assert_eq!(horizontal_parts(-1, 7), None);
    }

    #[test]
    fn readonly_inset_geometry_preserves_position_and_clamps_small_clients() {
        assert_eq!(inset_geometry([16, 102, 700, 280], 8), [24, 110, 684, 264]);
        assert_eq!(inset_geometry([-30, -10, 4, 0], 12), [-18, 2, 1, 1]);
        assert_eq!(inset_geometry([10, 20, 40, 50], -3), [10, 20, 40, 50]);
        assert_eq!(inset_geometry([i32::MAX, 0, 2, 2], 8), [i32::MAX, 8, 1, 1]);
    }

    #[test]
    fn frame_identity_detects_both_move_and_resize() {
        let original = RECT {
            left: 16,
            top: 100,
            right: 700,
            bottom: 400,
        };
        assert!(same_frame(original, original));
        assert!(!same_frame(
            original,
            RECT {
                left: 17,
                ..original
            }
        ));
        assert!(!same_frame(
            original,
            RECT {
                bottom: 401,
                ..original
            }
        ));
    }

    #[test]
    fn identical_font_retains_unicode_measurements_but_font_change_invalidates() {
        let mut cache = LineMeasureCache::default();
        let content = "中文😀\tresult\nsecond";
        cache.maximum(content, |_| 20);
        if font_changed(7, 7) {
            cache.invalidate();
        }
        assert_eq!(
            cache.maximum(content, |_| panic!("same font was measured again")),
            20
        );
        if font_changed(7, 9) {
            cache.invalidate();
        }
        let mut count = 0;
        assert_eq!(
            cache.maximum(content, |_| {
                count += 1;
                30
            }),
            30
        );
        assert_eq!(count, 2);
    }

    #[test]
    fn print_client_flags_respect_visibility_and_message_origin() {
        assert!(print_client_requested(WM_PRINTCLIENT, 0, false));
        assert!(!print_client_requested(WM_PRINT, PRF_NONCLIENT, true));
        assert!(print_client_requested(WM_PRINT, PRF_CLIENT, false));
        assert!(!print_client_requested(
            WM_PRINT,
            PRF_CLIENT | PRF_CHECKVISIBLE,
            false
        ));
        assert!(print_client_requested(
            WM_PRINTCLIENT,
            PRF_CLIENT | PRF_CHECKVISIBLE,
            true
        ));
    }

    #[test]
    fn long_other_lines_cannot_wrap_short_text_into_the_viewport() {
        assert!(color_horizontal_supported(32767, Some(500)));
        assert!(color_horizontal_supported(32768, Some(0)));
        assert!(!color_horizontal_supported(32768, Some(1)));
        assert!(!color_horizontal_supported(100_000, Some(65_536)));
        assert!(!color_horizontal_supported(100_000, None));
    }

    #[test]
    fn packed_coordinates_fall_back_before_pixel_width_wraps() {
        assert!(color_width_supported(0));
        assert!(color_width_supported(32767));
        assert!(!color_width_supported(32768));
        assert!(!color_width_supported(-1));
    }

    #[test]
    fn unsafe_color_ranges_keep_the_entire_native_line() {
        assert!(can_color_native_line(&vec![65; 8192], 0, (0, 0)));
        assert!(!can_color_native_line(&vec![65; 8193], 0, (0, 0)));
        let text: Vec<_> = "A😀中文".encode_utf16().collect();
        assert!(can_color_native_line(&text, 10, (10, 11)));
        assert!(!can_color_native_line(&text, 10, (12, 12)));
        assert!(!can_color_native_line(&[65, 9, 66], 0, (0, 0)));
        assert!(!can_color_native_line(&[0xD800], 0, (0, 0)));
        // helper只作绘制资格判断，未改写或裁剪原始UTF16正文。
        assert_eq!(String::from_utf16(&text).unwrap(), "A😀中文");
    }

    #[test]
    fn selected_native_text_is_never_overpainted() {
        assert_eq!(unselected_segments(10, 8, (12, 15)), [(0, 2), (5, 8)]);
        assert_eq!(unselected_segments(10, 8, (0, 30)), [(0, 0), (8, 8)]);
        assert_eq!(unselected_segments(10, 8, (0, 4)), [(0, 0), (0, 8)]);
        assert_eq!(unselected_segments(10, 8, (20, 30)), [(0, 8), (8, 8)]);
        assert_eq!(unselected_segments(10, 8, (14, 14)), [(0, 8), (0, 0)]);
    }

    #[test]
    fn structured_roles_have_distinct_theme_colors() {
        for dark in [false, true] {
            let error = role_color(ObservationRole::Error, dark);
            let success = role_color(ObservationRole::Success, dark);
            let output = role_color(ObservationRole::Output, dark);
            assert_ne!(error, success);
            assert_ne!(error, output);
            assert_ne!(success, output);
        }
    }
}
