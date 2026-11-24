// Windows x64，C++17。请以 UTF-8 保存，并使用 MSVC /utf-8 编译。
// 同一个查看器的所有调用均由其专用线程执行。查看器不使用 COM，支持 STA 和 MTA。

#ifndef WINVER
#define WINVER 0x0A00
#endif
#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0A00
#endif
#ifndef UNICODE
#define UNICODE
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#define WIN32_LEAN_AND_MEAN

#include <windows.h>
#include <windowsx.h>

#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <cwchar>
#include <new>

#pragma comment(lib, "user32.lib")
#pragma comment(lib, "gdi32.lib")
#pragma comment(lib, "kernel32.lib")

namespace {

constexpr DWORD kWindowStyle = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
constexpr int kFirstComboId = 100;
constexpr int kTakeoverId = 110;
constexpr int kDisconnectId = 111;

enum EventKind : int {
    PointerMove = 0,
    PointerButton = 1,
    PointerWheel = 2,
    VirtualKey = 3,
    ReleaseHeld = 4,
    ControlRequest = 5,
    Settings = 6
};

struct Event {
    int kind;
    int a = 0;
    int b = 0;
    int c = 0;
};

struct EventQueue {
    static constexpr std::size_t capacity = 128;

    std::array<Event, capacity> events{};
    std::size_t head = 0;
    std::size_t size = 0;

    Event& at(std::size_t index) noexcept {
        return events[(head + index) % capacity];
    }

    bool endsWith(int kind) noexcept {
        return size != 0 && at(size - 1).kind == kind;
    }

    void push(Event event) noexcept {
        at(size) = event;
        ++size;
    }

    Event pop() noexcept {
        const Event event = events[head];
        head = (head + 1) % capacity;
        --size;
        return event;
    }

    template <typename Predicate>
    void retain(Predicate keep) noexcept {
        std::size_t written = 0;
        for (std::size_t read = 0; read < size; ++read) {
            const Event event = at(read);
            if (keep(event)) {
                at(written++) = event;
            }
        }
        size = written;
    }
};

struct Choice {
    const wchar_t* label;
    int value;
};

constexpr Choice kQuality[] = {
    {L"自动", 0}, {L"低（480p）", 1}, {L"中（720p）", 2}, {L"高（1080p）", 3}
};
constexpr Choice kFrameRate[] = {
    {L"8 帧/秒", 8}, {L"15 帧/秒", 15}, {L"30 帧/秒", 30}
};
constexpr Choice kBitrate[] = {
    {L"1 Mb/s", 1000000}, {L"2 Mb/s", 2000000},
    {L"4 Mb/s", 4000000}, {L"8 Mb/s", 8000000}
};

struct Combo {
    HWND label = nullptr;
    HWND window = nullptr;
    HWND list = nullptr;
};

struct View;

struct WindowBinding {
    View* view;
    bool viewport;
};

struct View {
    View* next = nullptr;
    WindowBinding mainBinding{this, false};
    WindowBinding imageBinding{this, true};

    HINSTANCE instance = nullptr;
    ATOM windowClass = 0;
    HWND window = nullptr;
    HWND viewport = nullptr;
    HWND status = nullptr;
    HWND takeover = nullptr;
    HWND disconnect = nullptr;
    std::array<Combo, 3> combos{};
    HFONT font = nullptr;
    UINT dpi = USER_DEFAULT_SCREEN_DPI;

    unsigned char* pixels = nullptr;
    std::size_t pixelCapacity = 0;
    int imageWidth = 0;
    int imageHeight = 0;

    EventQueue queue;
    std::array<bool, 256> heldKeys{};
    unsigned heldButtons = 0;
    bool closed = false;
    bool windowFocused = false;
    bool controlGranted = false;
    bool returningControl = false;
};

// 线程内登记表用于验证句柄，也让同一消息泵正确处理线程上的多个查看器。
thread_local View* activeViews = nullptr;
const int moduleAnchor = 0;

class DpiScope {
public:
    DpiScope() noexcept
        : previous_(SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)) {}

    ~DpiScope() noexcept {
        if (previous_) {
            SetThreadDpiAwarenessContext(previous_);
        }
    }

    bool valid() const noexcept { return previous_ != nullptr; }

    DpiScope(const DpiScope&) = delete;
    DpiScope& operator=(const DpiScope&) = delete;

private:
    DPI_AWARENESS_CONTEXT previous_;
};

View* findView(const void* handle) noexcept {
    for (View* view = activeViews; view; view = view->next) {
        if (view == handle) {
            return view;
        }
    }
    return nullptr;
}

bool belongsToView(const View& view, HWND window) noexcept {
    if (!view.window || !window) {
        return false;
    }
    if (window == view.window || IsChild(view.window, window)) {
        return true;
    }
    for (const Combo& combo : view.combos) {
        if (combo.list && (window == combo.list || IsChild(combo.list, window))) {
            return true;
        }
    }
    return false;
}

bool controlEnabled(const View& view) noexcept {
    return !view.closed && view.controlGranted && !view.returningControl;
}

bool acceptsInput(const View& view) noexcept {
    return controlEnabled(view) && belongsToView(view, GetActiveWindow());
}

void releaseInputs(View& view, bool discardPendingInput) noexcept {
    if (discardPendingInput || view.queue.size == EventQueue::capacity) {
        view.queue.retain([](Event event) { return event.kind >= ControlRequest; });
    }
    if (!view.queue.endsWith(ReleaseHeld)) {
        view.queue.push({ReleaseHeld});
    }

    // ReleaseCapture 会同步通知 WM_CAPTURECHANGED，先清除按住状态。
    view.heldKeys.fill(false);
    view.heldButtons = 0;
    if (view.viewport && GetCapture() == view.viewport) {
        ReleaseCapture();
    }
}

void setWindowFocused(View& view, bool focused) noexcept {
    const bool lost = view.windowFocused && !focused;
    view.windowFocused = focused;
    if (lost && controlEnabled(view)) {
        releaseInputs(view, false);
    }
}

bool reserveInput(View& view, std::size_t count) noexcept {
    if (count <= EventQueue::capacity - view.queue.size) {
        return true;
    }
    releaseInputs(view, true);
    return false;
}

void queueSettingOrRequest(View& view, Event event) noexcept {
    // 宿主尚未读取同类操作时，只保留用户最新的选择。
    view.queue.retain([event](Event pending) { return pending.kind != event.kind; });
    if (view.queue.size == EventQueue::capacity) {
        releaseInputs(view, true);
    }
    view.queue.push(event);
}

int px(const View& view, int dip) noexcept {
    return MulDiv(dip, static_cast<int>(view.dpi), USER_DEFAULT_SCREEN_DPI);
}

RECT imageRect(const View& view) noexcept {
    RECT client{};
    if (view.imageWidth <= 0 || view.imageHeight <= 0 ||
        !GetClientRect(view.viewport, &client)) {
        return {};
    }

    const int availableWidth = client.right - client.left;
    const int availableHeight = client.bottom - client.top;
    if (availableWidth <= 0 || availableHeight <= 0) {
        return {};
    }

    const std::int64_t imageWidth = view.imageWidth;
    const std::int64_t imageHeight = view.imageHeight;
    int width = availableWidth;
    int height = availableHeight;
    if (availableWidth * imageHeight <= availableHeight * imageWidth) {
        height = std::max(1, static_cast<int>(availableWidth * imageHeight / imageWidth));
    } else {
        width = std::max(1, static_cast<int>(availableHeight * imageWidth / imageHeight));
    }
    const int x = client.left + (availableWidth - width) / 2;
    const int y = client.top + (availableHeight - height) / 2;
    return {x, y, x + width, y + height};
}

int normalize(LONG coordinate, LONG first, LONG end) noexcept {
    const std::int64_t last = end - first - 1;
    if (last <= 0) {
        return 0;
    }
    return static_cast<int>(((coordinate - first) * std::int64_t{65535} + last / 2) / last);
}

bool queuePosition(View& view, const RECT& image, POINT point,
                   std::size_t followingEvents) noexcept {
    const bool coalesce = view.queue.endsWith(PointerMove);
    if (!reserveInput(view, followingEvents + (coalesce ? 0 : 1))) {
        return false;
    }

    const Event position{PointerMove, normalize(point.x, image.left, image.right),
                         normalize(point.y, image.top, image.bottom)};
    if (coalesce) {
        view.queue.at(view.queue.size - 1) = position;
    } else {
        view.queue.push(position);
    }
    return true;
}

void handleMouseButton(View& view, int button, bool down, LPARAM position) noexcept {
    const POINT point{GET_X_LPARAM(position), GET_Y_LPARAM(position)};
    const RECT image = imageRect(view);
    const bool overImage = PtInRect(&image, point) != FALSE;
    const unsigned bit = 1u << button;

    if (down) {
        if (!overImage) {
            return;
        }
        SetFocus(view.viewport);
        if (!acceptsInput(view) || GetFocus() != view.viewport ||
            (view.heldButtons & bit) != 0) {
            return;
        }
        if (view.heldButtons == 0) {
            SetCapture(view.viewport);
            if (GetCapture() != view.viewport) {
                return;
            }
        }
        if (queuePosition(view, image, point, 1)) {
            view.queue.push({PointerButton, button, 1});
            view.heldButtons |= bit;
        }
        return;
    }

    if (!acceptsInput(view) || (view.heldButtons & bit) == 0) {
        return;
    }

    // 拖出图像后仍结清已转发的按下，远端坐标保留在最后一个有效像素。
    if (overImage ? !queuePosition(view, image, point, 1) : !reserveInput(view, 1)) {
        return;
    }
    view.queue.push({PointerButton, button, 0});
    view.heldButtons &= ~bit;
    if (view.heldButtons == 0 && GetCapture() == view.viewport) {
        ReleaseCapture();
    }
}

void handleWheel(View& view, WPARAM wparam, POINT screenPoint) noexcept {
    if (!acceptsInput(view) || !ScreenToClient(view.viewport, &screenPoint)) {
        return;
    }

    const RECT image = imageRect(view);
    if (PtInRect(&image, screenPoint) && queuePosition(view, image, screenPoint, 1)) {
        view.queue.push({PointerWheel, GET_WHEEL_DELTA_WPARAM(wparam)});
    }
}

void handleKey(View& view, WPARAM key, LPARAM flags, bool down) noexcept {
    if (!acceptsInput(view) || key >= view.heldKeys.size()) {
        return;
    }

    bool& held = view.heldKeys[static_cast<std::size_t>(key)];
    const bool repeat = (static_cast<ULONG_PTR>(flags) & 0x40000000u) != 0;
    if ((!down && !held) || (down && repeat && !held)) {
        return;
    }
    if (reserveInput(view, 1)) {
        view.queue.push({VirtualKey, static_cast<int>(key), down ? 1 : 0});
        held = down;
    }
}

void updateTakeoverButton(const View& view) noexcept {
    const wchar_t* text = view.returningControl ? L"正在退出控制"
                         : view.controlGranted ? L"返回仅查看" : L"申请接管";
    SetWindowTextW(view.takeover, text);
    EnableWindow(view.takeover, !view.returningControl);
}

void updateFont(View& view) noexcept {
    NONCLIENTMETRICSW metrics{};
    metrics.cbSize = sizeof(metrics);
    HFONT replacement = nullptr;
    if (SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, sizeof(metrics),
                                   &metrics, 0, view.dpi)) {
        replacement = CreateFontIndirectW(&metrics.lfMessageFont);
    }

    const WPARAM font = reinterpret_cast<WPARAM>(
        replacement ? replacement : GetStockObject(DEFAULT_GUI_FONT));
    const auto set = [font](HWND window) noexcept {
        if (window) {
            SendMessageW(window, WM_SETFONT, font, TRUE);
        }
    };

    set(view.status);
    set(view.takeover);
    set(view.disconnect);
    for (const Combo& combo : view.combos) {
        set(combo.label);
        set(combo.window);
    }
    if (view.font) {
        DeleteObject(view.font);
    }
    view.font = replacement;
    InvalidateRect(view.viewport, nullptr, FALSE);
}

void layout(View& view) noexcept {
    if (!view.status || !view.disconnect) {
        return;
    }

    RECT client{};
    GetClientRect(view.window, &client);
    const int width = client.right;
    const int height = client.bottom;
    const int margin = px(view, 12);
    const int gap = px(view, 8);
    const int buttonHeight = px(view, 28);
    const int takeoverWidth = px(view, 112);
    const int disconnectWidth = px(view, 84);
    const int actionWidth = takeoverWidth + gap + disconnectWidth;
    const int labelWidth = px(view, 38);
    const int comboWidths[] = {px(view, 112), px(view, 82), px(view, 104)};

    int x = margin;
    for (std::size_t i = 0; i < view.combos.size(); ++i) {
        MoveWindow(view.combos[i].label, x, gap, labelWidth, buttonHeight, TRUE);
        x += labelWidth;
        MoveWindow(view.combos[i].window, x, gap, comboWidths[i], px(view, 168), TRUE);
        x += comboWidths[i] + gap;
    }

    const bool wrapped = x + actionWidth + margin > width;
    const int actionX = std::max(margin, width - margin - actionWidth);
    const int actionY = wrapped ? gap + buttonHeight + gap : gap;
    MoveWindow(view.takeover, actionX, actionY, takeoverWidth, buttonHeight, TRUE);
    MoveWindow(view.disconnect, actionX + takeoverWidth + gap, actionY,
               disconnectWidth, buttonHeight, TRUE);

    const int viewportTop = actionY + buttonHeight + gap;
    const int statusTop = std::max(viewportTop, height - px(view, 30));
    MoveWindow(view.viewport, 0, viewportTop, width, statusTop - viewportTop, TRUE);
    MoveWindow(view.status, margin, statusTop, std::max(0, width - 2 * margin),
               std::max(0, height - statusTop), TRUE);
    InvalidateRect(view.window, nullptr, FALSE);
}

void paintViewport(const View& view) noexcept {
    PAINTSTRUCT paint{};
    HDC dc = BeginPaint(view.viewport, &paint);
    if (dc) {
        RECT client{};
        GetClientRect(view.viewport, &client);
        const RECT image = imageRect(view);
        const HBRUSH black = reinterpret_cast<HBRUSH>(GetStockObject(BLACK_BRUSH));

        if (!view.pixels || IsRectEmpty(&image)) {
            FillRect(dc, &client, black);
            RECT textArea = client;
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, RGB(210, 210, 210));
            const HGDIOBJ font = view.font ? view.font : GetStockObject(DEFAULT_GUI_FONT);
            const HGDIOBJ oldFont = SelectObject(dc, font);
            DrawTextW(dc, L"等待画面…", -1, &textArea,
                      DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
            if (oldFont) {
                SelectObject(dc, oldFont);
            }
        } else {
            const RECT padding[] = {
                {client.left, client.top, client.right, image.top},
                {client.left, image.bottom, client.right, client.bottom},
                {client.left, image.top, image.left, image.bottom},
                {image.right, image.top, client.right, image.bottom}
            };
            for (const RECT& area : padding) {
                if (!IsRectEmpty(&area)) {
                    FillRect(dc, &area, black);
                }
            }

            BITMAPINFO bitmap{};
            bitmap.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
            bitmap.bmiHeader.biWidth = view.imageWidth;
            bitmap.bmiHeader.biHeight = -view.imageHeight;
            bitmap.bmiHeader.biPlanes = 1;
            bitmap.bmiHeader.biBitCount = 32;
            bitmap.bmiHeader.biCompression = BI_RGB;

            SetStretchBltMode(dc, HALFTONE);
            SetBrushOrgEx(dc, 0, 0, nullptr);
            const int lines = StretchDIBits(dc, image.left, image.top,
                image.right - image.left, image.bottom - image.top,
                0, 0, view.imageWidth, view.imageHeight, view.pixels, &bitmap,
                DIB_RGB_COLORS, SRCCOPY);
            if (lines == 0 || lines == static_cast<int>(GDI_ERROR)) {
                FillRect(dc, &image, black);
            }
        }
    }
    EndPaint(view.viewport, &paint);
}

template <std::size_t N>
int selectedValue(const Combo& combo, const Choice (&choices)[N],
                  std::size_t fallback) noexcept {
    const LRESULT index = SendMessageW(combo.window, CB_GETCURSEL, 0, 0);
    return index >= 0 && static_cast<std::size_t>(index) < N
        ? choices[static_cast<std::size_t>(index)].value : choices[fallback].value;
}

bool handleCommand(View& view, WPARAM wparam, LPARAM lparam) noexcept {
    const int id = LOWORD(wparam);
    const int notification = HIWORD(wparam);
    const HWND sender = reinterpret_cast<HWND>(lparam);

    if (notification == CBN_SELCHANGE && id >= kFirstComboId &&
        id < kFirstComboId + static_cast<int>(view.combos.size()) &&
        sender == view.combos[static_cast<std::size_t>(id - kFirstComboId)].window) {
        queueSettingOrRequest(view, {Settings,
            selectedValue(view.combos[0], kQuality, 0),
            selectedValue(view.combos[1], kFrameRate, 1),
            selectedValue(view.combos[2], kBitrate, 1)});
        return true;
    }

    if (notification == BN_CLICKED && id == kDisconnectId && sender == view.disconnect) {
        DestroyWindow(view.window);
        return true;
    }
    if (notification == BN_CLICKED && id == kTakeoverId && sender == view.takeover) {
        if (controlEnabled(view)) {
            view.returningControl = true;
            releaseInputs(view, true);
            queueSettingOrRequest(view, {ControlRequest, 0});
            updateTakeoverButton(view);
        } else if (!view.controlGranted) {
            queueSettingOrRequest(view, {ControlRequest, 1});
        }
        return true;
    }
    return false;
}

LRESULT viewportMessage(View& view, HWND window, UINT message,
                        WPARAM wparam, LPARAM lparam) noexcept {
    switch (message) {
    case WM_ERASEBKGND:
        return 1;
    case WM_PAINT:
        paintViewport(view);
        return 0;
    case WM_SIZE:
        InvalidateRect(window, nullptr, FALSE);
        return 0;
    case WM_SETFOCUS:
        setWindowFocused(view, belongsToView(view, GetActiveWindow()));
        return 0;
    case WM_KILLFOCUS:
        // 工具栏和下拉列表同属查看器，内部切换焦点时保留已转发的按键。
        if (wparam && !belongsToView(view, reinterpret_cast<HWND>(wparam))) {
            setWindowFocused(view, false);
        }
        return 0;
    case WM_MOUSEMOVE: {
        const POINT point{GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)};
        const RECT image = imageRect(view);
        if (acceptsInput(view) && PtInRect(&image, point)) {
            queuePosition(view, image, point, 0);
        }
        return 0;
    }
    case WM_LBUTTONDOWN:
    case WM_LBUTTONUP:
        handleMouseButton(view, 0, message == WM_LBUTTONDOWN, lparam);
        return 0;
    case WM_RBUTTONDOWN:
    case WM_RBUTTONUP:
        handleMouseButton(view, 1, message == WM_RBUTTONDOWN, lparam);
        return 0;
    case WM_MBUTTONDOWN:
    case WM_MBUTTONUP:
        handleMouseButton(view, 2, message == WM_MBUTTONDOWN, lparam);
        return 0;
    case WM_MOUSEWHEEL:
        handleWheel(view, wparam, {GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)});
        return 0;
    case WM_CAPTURECHANGED:
        if (view.heldButtons && reinterpret_cast<HWND>(lparam) != window) {
            releaseInputs(view, false);
        }
        return 0;
    case WM_CANCELMODE:
        if (view.heldButtons) {
            releaseInputs(view, false);
        }
        break;
    case WM_KEYDOWN:
    case WM_SYSKEYDOWN:
    case WM_KEYUP:
    case WM_SYSKEYUP:
        if (controlEnabled(view)) {
            handleKey(view, wparam, lparam, message == WM_KEYDOWN || message == WM_SYSKEYDOWN);
            return 0;
        }
        break;
    case WM_CHAR:
    case WM_DEADCHAR:
        return 0;
    case WM_SYSCHAR:
    case WM_SYSDEADCHAR:
        if (controlEnabled(view)) {
            return 0;
        }
        break;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

LRESULT mainMessage(View& view, HWND window, UINT message,
                    WPARAM wparam, LPARAM lparam) noexcept {
    switch (message) {
    case WM_ERASEBKGND:
        return 1;
    case WM_PAINT: {
        PAINTSTRUCT paint{};
        HDC dc = BeginPaint(window, &paint);
        if (dc) {
            FillRect(dc, &paint.rcPaint, GetSysColorBrush(COLOR_BTNFACE));
        }
        EndPaint(window, &paint);
        return 0;
    }
    case WM_SIZE:
        if (wparam == SIZE_MINIMIZED) {
            setWindowFocused(view, false);
        } else {
            layout(view);
        }
        return 0;
    case WM_GETMINMAXINFO: {
        DefWindowProcW(window, message, wparam, lparam);
        RECT minimum{0, 0, px(view, 470), px(view, 250)};
        AdjustWindowRectExForDpi(&minimum, kWindowStyle, FALSE, 0, view.dpi);
        auto* info = reinterpret_cast<MINMAXINFO*>(lparam);
        info->ptMinTrackSize.x = minimum.right - minimum.left;
        info->ptMinTrackSize.y = minimum.bottom - minimum.top;
        return 0;
    }
    case WM_DPICHANGED: {
        view.dpi = LOWORD(wparam);
        const auto* suggested = reinterpret_cast<const RECT*>(lparam);
        updateFont(view);
        SetWindowPos(window, nullptr, suggested->left, suggested->top,
                     suggested->right - suggested->left, suggested->bottom - suggested->top,
                     SWP_NOZORDER | SWP_NOACTIVATE);
        layout(view);
        return 0;
    }
    case WM_ACTIVATE:
        if (LOWORD(wparam) == WA_INACTIVE) {
            if (!belongsToView(view, reinterpret_cast<HWND>(lparam))) {
                setWindowFocused(view, false);
            }
        } else if (!HIWORD(wparam)) {
            setWindowFocused(view, true);
        }
        break;
    case WM_CANCELMODE:
        if (view.heldButtons) {
            releaseInputs(view, false);
        }
        break;
    case WM_COMMAND:
        if (!view.closed && handleCommand(view, wparam, lparam)) {
            return 0;
        }
        break;
    case WM_CLOSE:
        DestroyWindow(window);
        return 0;
    case WM_DESTROY:
        // 关闭本窗口只结束查看器；宿主收到 -1 后结束该窗口的远程输入。
        view.closed = true;
        view.windowFocused = false;
        view.queue.size = 0;
        view.heldKeys.fill(false);
        view.heldButtons = 0;
        if (view.viewport && GetCapture() == view.viewport) {
            ReleaseCapture();
        }
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

LRESULT CALLBACK windowProcedure(HWND window, UINT message,
                                 WPARAM wparam, LPARAM lparam) noexcept {
    auto* binding = reinterpret_cast<WindowBinding*>(GetWindowLongPtrW(window, GWLP_USERDATA));
    if (message == WM_NCCREATE) {
        const auto* creation = reinterpret_cast<const CREATESTRUCTW*>(lparam);
        binding = static_cast<WindowBinding*>(creation->lpCreateParams);
        if (!binding) {
            return FALSE;
        }

        SetLastError(ERROR_SUCCESS);
        if (!SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(binding)) &&
            GetLastError() != ERROR_SUCCESS) {
            return FALSE;
        }
        if (binding->viewport) {
            binding->view->viewport = window;
        } else {
            binding->view->window = window;
            const UINT dpi = GetDpiForWindow(window);
            if (dpi) {
                binding->view->dpi = dpi;
            }
        }
    }

    if (!binding) {
        return DefWindowProcW(window, message, wparam, lparam);
    }

    View& view = *binding->view;
    if (message == WM_NCDESTROY) {
        SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        if (binding->viewport) {
            view.viewport = nullptr;
        } else {
            view.window = nullptr;
            view.closed = true;
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }
    return binding->viewport ? viewportMessage(view, window, message, wparam, lparam)
                             : mainMessage(view, window, message, wparam, lparam);
}

HWND createChild(const View& view, const wchar_t* windowClass, const wchar_t* text,
                 DWORD style, int id = 0) noexcept {
    return CreateWindowExW(0, windowClass, text, WS_CHILD | WS_VISIBLE | style,
                          0, 0, 1, 1, view.window,
                          reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)),
                          view.instance, nullptr);
}

template <std::size_t N>
bool createCombo(View& view, std::size_t index, const wchar_t* caption,
                 const Choice (&choices)[N], std::size_t initial) noexcept {
    Combo& combo = view.combos[index];
    combo.label = createChild(view, L"STATIC", caption, SS_CENTERIMAGE | SS_NOPREFIX);
    combo.window = createChild(view, L"COMBOBOX", L"",
        WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST, kFirstComboId + static_cast<int>(index));
    if (!combo.label || !combo.window) {
        return false;
    }
    for (const Choice& choice : choices) {
        if (SendMessageW(combo.window, CB_ADDSTRING, 0,
                         reinterpret_cast<LPARAM>(choice.label)) < 0) {
            return false;
        }
    }
    if (SendMessageW(combo.window, CB_SETCURSEL, initial, 0) == CB_ERR) {
        return false;
    }
    COMBOBOXINFO info{};
    info.cbSize = sizeof(info);
    if (GetComboBoxInfo(combo.window, &info)) {
        combo.list = info.hwndList;
    }
    return true;
}

void sizeInitialWindow(const View& view) noexcept {
    RECT target{0, 0, px(view, 960), px(view, 640)};
    AdjustWindowRectExForDpi(&target, kWindowStyle, FALSE, 0, view.dpi);
    int width = target.right - target.left;
    int height = target.bottom - target.top;
    RECT current{};
    GetWindowRect(view.window, &current);
    int x = current.left;
    int y = current.top;

    MONITORINFO info{};
    info.cbSize = sizeof(info);
    if (GetMonitorInfoW(MonitorFromWindow(view.window, MONITOR_DEFAULTTONEAREST), &info)) {
        width = std::min(width, static_cast<int>(info.rcWork.right - info.rcWork.left));
        height = std::min(height, static_cast<int>(info.rcWork.bottom - info.rcWork.top));
        x = std::clamp(x, static_cast<int>(info.rcWork.left),
                       static_cast<int>(info.rcWork.right) - width);
        y = std::clamp(y, static_cast<int>(info.rcWork.top),
                       static_cast<int>(info.rcWork.bottom) - height);
    }
    SetWindowPos(view.window, nullptr, x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE);
}

bool createWindows(View& view, const wchar_t* title) noexcept {
    if (!GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
                            GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                            reinterpret_cast<LPCWSTR>(&moduleAnchor), &view.instance)) {
        return false;
    }

    wchar_t className[80]{};
    if (swprintf_s(className, sizeof(className) / sizeof(className[0]),
                   L"RemoteOps.NativeViewer.%p", static_cast<void*>(&view)) < 0) {
        return false;
    }
    WNDCLASSEXW windowClass{};
    windowClass.cbSize = sizeof(windowClass);
    windowClass.lpfnWndProc = windowProcedure;
    windowClass.hInstance = view.instance;
    windowClass.hIcon = LoadIconW(nullptr, IDI_APPLICATION);
    windowClass.hCursor = LoadCursorW(nullptr, IDC_ARROW);
    windowClass.lpszClassName = className;
    view.windowClass = RegisterClassExW(&windowClass);
    if (!view.windowClass) {
        return false;
    }

    const LPCWSTR classAtom = MAKEINTRESOURCEW(view.windowClass);
    if (!CreateWindowExW(0, classAtom, title && *title ? title : L"RemoteOps 远程协助",
                         kWindowStyle, CW_USEDEFAULT, CW_USEDEFAULT, 960, 640,
                         nullptr, nullptr, view.instance, &view.mainBinding)) {
        return false;
    }
    if (!CreateWindowExW(0, classAtom, L"远程画面", WS_VISIBLE | WS_CHILD | WS_TABSTOP,
                         0, 0, 1, 1, view.window, nullptr, view.instance, &view.imageBinding)) {
        return false;
    }
    if (!createCombo(view, 0, L"画质", kQuality, 0) ||
        !createCombo(view, 1, L"帧率", kFrameRate, 1) ||
        !createCombo(view, 2, L"带宽", kBitrate, 1)) {
        return false;
    }
    view.takeover = createChild(view, L"BUTTON", L"申请接管",
                                WS_TABSTOP | BS_PUSHBUTTON, kTakeoverId);
    view.disconnect = createChild(view, L"BUTTON", L"断开连接",
                                  WS_TABSTOP | BS_PUSHBUTTON, kDisconnectId);
    view.status = createChild(view, L"STATIC", L"仅查看",
                              SS_CENTERIMAGE | SS_ENDELLIPSIS | SS_NOPREFIX);
    if (!view.takeover || !view.disconnect || !view.status) {
        return false;
    }

    updateFont(view);
    sizeInitialWindow(view);
    layout(view);
    return true;
}

void deleteView(View* view) noexcept {
    if (view->window) {
        DestroyWindow(view->window);
    }
    if (view->font) {
        DeleteObject(view->font);
    }
    if (view->pixels) {
        HeapFree(GetProcessHeap(), 0, view->pixels);
    }
    if (view->windowClass) {
        UnregisterClassW(MAKEINTRESOURCEW(view->windowClass), view->instance);
    }
    delete view;
}

void synchronizeFocus() noexcept {
    const HWND active = GetActiveWindow();
    const HWND focus = GetFocus();
    for (View* view = activeViews; view; view = view->next) {
        setWindowFocused(*view, belongsToView(*view, active) && belongsToView(*view, focus));
    }
}

bool routeMessage(MSG& message) noexcept {
    if (!message.hwnd) {
        return false;
    }
    View* view = activeViews;
    while (view && !belongsToView(*view, message.hwnd)) {
        view = view->next;
    }
    if (!view || view->closed) {
        return false;
    }

    if (message.message == WM_MOUSEWHEEL && message.hwnd != view->viewport &&
        acceptsInput(*view)) {
        const POINT screen{GET_X_LPARAM(message.lParam), GET_Y_LPARAM(message.lParam)};
        if (WindowFromPoint(screen) == view->viewport) {
            handleWheel(*view, message.wParam, screen);
            return true;
        }
    }

    if (message.message < WM_KEYFIRST || message.message > WM_KEYLAST) {
        return false;
    }
    if (message.hwnd == view->viewport && controlEnabled(*view)) {
        // 接管时由画面接收快捷键，避免对话框导航先消费 Tab、方向键或 Alt 组合键。
        DispatchMessageW(&message);
        return true;
    }
    if (message.message == WM_KEYUP || message.message == WM_SYSKEYUP) {
        // 焦点移到工具栏后仍结清画面曾转发的按键，工具栏继续正常处理该消息。
        handleKey(*view, message.wParam, message.lParam, false);
    }
    return IsDialogMessageW(view->window, &message) != FALSE;
}

void pumpMessages() noexcept {
    MSG message{};
    synchronizeFocus();
    while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
        if (message.message == WM_QUIT) {
            for (View* view = activeViews; view; view = view->next) {
                if (view->window) {
                    DestroyWindow(view->window);
                }
            }
        } else if (!routeMessage(message)) {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        synchronizeFocus();
    }
}

} // namespace

// 创建独立可见的查看窗口，失败时返回空句柄。
extern "C" void* ro_view_new(const wchar_t* title) noexcept {
    const DpiScope dpi;
    if (!dpi.valid()) {
        return nullptr;
    }

    auto* view = new (std::nothrow) View{};
    if (!view) {
        return nullptr;
    }
    if (!createWindows(*view, title)) {
        deleteView(view);
        return nullptr;
    }

    view->next = activeViews;
    activeViews = view;
    ShowWindow(view->window, SW_SHOWNORMAL);
    if (view->window && !IsWindowVisible(view->window)) {
        ShowWindow(view->window, SW_SHOW);
    }
    if (view->viewport && GetActiveWindow() == view->window) {
        SetFocus(view->viewport);
    }
    return view;
}

// 排空线程消息，再取出一个事件。输出指针可以为空。
extern "C" int ro_view_poll(void* handle, int* kind, int* a,
                                                 int* b, int* c) noexcept {
    const View* initial = findView(handle);
    if (!initial || initial->closed) {
        return -1;
    }

    const DpiScope dpi;
    pumpMessages();
    View* view = findView(handle);
    if (!view || view->closed) {
        return -1;
    }
    if (view->queue.size == 0) {
        return 0;
    }

    const Event event = view->queue.pop();
    if (kind) *kind = event.kind;
    if (a) *a = event.a;
    if (b) *b = event.b;
    if (c) *c = event.c;
    return 1;
}

// 复制调用方提供的自顶向下 BGRA 帧，每行紧密排列。
extern "C" void ro_view_present(void* handle, const unsigned char* bgra,
                                                     int width, int height) noexcept {
    View* view = findView(handle);
    if (!view || view->closed || !bgra || width <= 0 || height <= 0) {
        return;
    }

    const auto columns = static_cast<std::size_t>(width);
    const auto rows = static_cast<std::size_t>(height);
    if (columns > static_cast<std::size_t>(MAXDWORD) / 4 / rows) {
        return;
    }
    const std::size_t bytes = columns * rows * 4;

    if (bytes > view->pixelCapacity) {
        HANDLE heap = GetProcessHeap();
        void* replacement = view->pixels ? HeapReAlloc(heap, 0, view->pixels, bytes)
                                         : HeapAlloc(heap, 0, bytes);
        if (!replacement) {
            return;
        }
        view->pixels = static_cast<unsigned char*>(replacement);
        view->pixelCapacity = bytes;
    }

    std::memcpy(view->pixels, bgra, bytes);
    view->imageWidth = width;
    view->imageHeight = height;
    InvalidateRect(view->viewport, nullptr, FALSE);
}

// 由宿主同步状态与已确认的控制权限。界面更新不会生成设置或接管请求。
extern "C" void ro_view_status(void* handle, const wchar_t* label,
                                                    int control) noexcept {
    View* view = findView(handle);
    if (!view || view->closed) {
        return;
    }

    const DpiScope dpi;
    const bool wasEnabled = controlEnabled(*view);
    const bool wasGranted = view->controlGranted;
    const bool wasReturning = view->returningControl;
    view->controlGranted = control != 0;
    if (!view->controlGranted) {
        view->returningControl = false;
    }
    if (wasEnabled && !controlEnabled(*view)) {
        releaseInputs(*view, true);
    }

    if (wasGranted != view->controlGranted || wasReturning != view->returningControl) {
        updateTakeoverButton(*view);
    }
    const wchar_t* fallback = view->returningControl ? L"正在退出控制"
                            : view->controlGranted ? L"已获准控制" : L"仅查看";
    SetWindowTextW(view->status, label && *label ? label : fallback);
}

// 销毁窗口并释放帧、字体和窗口类。窗口已经关闭时仍须调用。
extern "C" void ro_view_free(void* handle) noexcept {
    View** link = &activeViews;
    while (*link && *link != handle) {
        link = &(*link)->next;
    }
    if (!*link) {
        return;
    }

    const DpiScope dpi;
    View* view = *link;
    *link = view->next;
    deleteView(view);
}