#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0A00
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#define WIN32_LEAN_AND_MEAN

#include <windows.h>
#include <windowsx.h>
#include <commctrl.h>
#include <d3d11.h>
#include <dwmapi.h>
#include <dxgi1_2.h>
#include <wtsapi32.h>
#include <wrl/client.h>

#include <algorithm>
#include <array>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <memory>
#include <new>
#include <vector>

#pragma comment(lib, "advapi32.lib")
#pragma comment(lib, "comctl32.lib")
#pragma comment(lib, "d3d11.lib")
#pragma comment(lib, "dwmapi.lib")
#pragma comment(lib, "dxgi.lib")
#pragma comment(lib, "gdi32.lib")
#pragma comment(lib, "ole32.lib")
#pragma comment(lib, "user32.lib")
#pragma comment(lib, "wtsapi32.lib")

namespace {

using Microsoft::WRL::ComPtr;

constexpr ULONG_PTR kInputTag = 0x524F5049u;
constexpr ULONGLONG kDuplicationRetryMs = 1000;

class DpiScope {
public:
    DpiScope() noexcept
        : previous_(SetThreadDpiAwarenessContext(
              DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)) {}

    ~DpiScope() {
        if (previous_) {
            SetThreadDpiAwarenessContext(previous_);
        }
    }

    explicit operator bool() const noexcept { return previous_ != nullptr; }

private:
    DPI_AWARENESS_CONTEXT previous_;
};

struct KernelHandle {
    HANDLE value = nullptr;

    ~KernelHandle() {
        if (value) {
            CloseHandle(value);
        }
    }
};

struct DesktopHandle {
    HDESK value;

    ~DesktopHandle() {
        if (value) {
            CloseDesktop(value);
        }
    }
};

struct WtsBuffer {
    LPWSTR value = nullptr;

    ~WtsBuffer() {
        if (value) {
            WTSFreeMemory(value);
        }
    }
};

bool ObjectNamed(HANDLE object, const wchar_t* expected) noexcept {
    wchar_t name[64]{};
    DWORD needed = 0;
    return object &&
           GetUserObjectInformationW(object, UOI_NAME, name, sizeof(name), &needed) &&
           lstrcmpiW(name, expected) == 0;
}

struct TokenUserBuffer {
    alignas(TOKEN_USER) BYTE data[sizeof(TOKEN_USER) + SECURITY_MAX_SID_SIZE]{};

    PSID Read(HANDLE token) noexcept {
        DWORD needed = 0;
        if (!GetTokenInformation(token, TokenUser, data, sizeof(data), &needed)) {
            return nullptr;
        }
        PSID sid = reinterpret_cast<const TOKEN_USER*>(data)->User.Sid;
        return IsValidSid(sid) ? sid : nullptr;
    }
};

bool SameShellUser(DWORD session, HANDLE ownToken) noexcept {
    const HWND shell = GetShellWindow();
    DWORD shellPid = 0;
    DWORD shellSession = 0;
    if (!shell || !GetWindowThreadProcessId(shell, &shellPid) ||
        !ProcessIdToSessionId(shellPid, &shellSession) || shellSession != session) {
        return false;
    }

    KernelHandle process{OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, shellPid)};
    KernelHandle token;
    if (!process.value || !OpenProcessToken(process.value, TOKEN_QUERY, &token.value)) {
        return false;
    }

    TokenUserBuffer ownUser;
    TokenUserBuffer shellUser;
    PSID ownSid = ownUser.Read(ownToken);
    PSID shellSid = shellUser.Read(token.value);
    return ownSid && shellSid && EqualSid(ownSid, shellSid);
}

struct Layout {
    HMONITOR monitor = nullptr;
    RECT primary{};
    RECT work{};
    RECT virtualScreen{};
    wchar_t device[CCHDEVICENAME]{};
    DWORD orientation = 0;

    int Width() const noexcept { return primary.right - primary.left; }
    int Height() const noexcept { return primary.bottom - primary.top; }
};

struct MonitorBounds {
    RECT bounds{};
    bool found = false;
};

BOOL CALLBACK CollectMonitorBounds(HMONITOR, HDC, LPRECT rect, LPARAM parameter) {
    auto& result = *reinterpret_cast<MonitorBounds*>(parameter);
    if (!result.found) {
        result.bounds = *rect;
        result.found = true;
    } else {
        UnionRect(&result.bounds, &result.bounds, rect);
    }
    return TRUE;
}

bool ValidBounds(const RECT& rect) noexcept {
    const auto width = std::int64_t{rect.right} - rect.left;
    const auto height = std::int64_t{rect.bottom} - rect.top;
    return width >= 2 && height >= 2 && width <= INT_MAX && height <= INT_MAX;
}

bool Contains(const RECT& outer, const RECT& inner) noexcept {
    return inner.left >= outer.left && inner.top >= outer.top &&
           inner.right <= outer.right && inner.bottom <= outer.bottom;
}

bool ReadLayout(Layout& layout) noexcept {
    layout.monitor = MonitorFromPoint(POINT{0, 0}, MONITOR_DEFAULTTOPRIMARY);
    MONITORINFOEXW info{};
    info.cbSize = sizeof(info);
    MonitorBounds bounds;
    if (!layout.monitor ||
        !GetMonitorInfoW(layout.monitor, reinterpret_cast<MONITORINFO*>(&info)) ||
        !(info.dwFlags & MONITORINFOF_PRIMARY) ||
        !EnumDisplayMonitors(nullptr, nullptr, CollectMonitorBounds,
                             reinterpret_cast<LPARAM>(&bounds)) ||
        !bounds.found || !ValidBounds(bounds.bounds) || !ValidBounds(info.rcMonitor) ||
        !ValidBounds(info.rcWork) || !Contains(bounds.bounds, info.rcMonitor) ||
        !Contains(info.rcMonitor, info.rcWork)) {
        return false;
    }

    layout.primary = info.rcMonitor;
    layout.work = info.rcWork;
    layout.virtualScreen = bounds.bounds;
    std::memcpy(layout.device, info.szDevice, sizeof(layout.device));

    DEVMODEW mode{};
    mode.dmSize = sizeof(mode);
    if (EnumDisplaySettingsW(layout.device, ENUM_CURRENT_SETTINGS, &mode) &&
        (mode.dmFields & DM_DISPLAYORIENTATION)) {
        layout.orientation = mode.dmDisplayOrientation;
    }
    return true;
}

bool SameDisplay(const Layout& a, const Layout& b) noexcept {
    return a.monitor == b.monitor && EqualRect(&a.primary, &b.primary) &&
           EqualRect(&a.virtualScreen, &b.virtualScreen) &&
           lstrcmpiW(a.device, b.device) == 0 && a.orientation == b.orientation;
}

bool ImageSize(int width, int height, std::size_t& bytes) noexcept {
    const auto size = std::uint64_t{4} * static_cast<unsigned>(width) *
                      static_cast<unsigned>(height);
    if (width <= 0 || height <= 0 || size > (std::numeric_limits<std::size_t>::max)()) {
        return false;
    }
    bytes = static_cast<std::size_t>(size);
    return true;
}

struct GdiCapture {
    HDC screen = nullptr;
    HDC memory = nullptr;
    HBITMAP bitmap = nullptr;
    HGDIOBJ previous = nullptr;

    ~GdiCapture() {
        if (memory) {
            if (previous && previous != HGDI_ERROR) {
                SelectObject(memory, previous);
            }
            DeleteDC(memory);
        }
        if (bitmap) {
            DeleteObject(bitmap);
        }
        if (screen) {
            ReleaseDC(nullptr, screen);
        }
    }
};

struct DuplicationFrame {
    IDXGIOutputDuplication* duplication;

    ~DuplicationFrame() { duplication->ReleaseFrame(); }
};

struct MappedTexture {
    ID3D11DeviceContext* context;
    ID3D11Texture2D* texture;

    ~MappedTexture() { context->Unmap(texture, 0); }
};

class Capture {
public:
    void Reset() noexcept {
        ResetDuplication();
        pixels_.clear();
        retryAt_ = 0;
    }

    bool Read(const Layout& layout) noexcept {
        if (!duplication_ && GetTickCount64() >= retryAt_ && !InitDuplication(layout)) {
            PauseDuplication();
        }

        if (duplication_) {
            switch (ReadDuplication(layout)) {
            case ReadResult::Fresh:
                return true;
            case ReadResult::Idle:
                if (!pixels_.empty()) {
                    return true;
                }
                break;
            case ReadResult::Failed:
                pixels_.clear();
                PauseDuplication();
                break;
            }
        }
        return ReadGdi(layout);
    }

    const BYTE* Pixels() const noexcept { return pixels_.data(); }

private:
    enum class ReadResult { Fresh, Idle, Failed };

    void ResetDuplication() noexcept {
        staging_.Reset();
        duplication_.Reset();
        context_.Reset();
        device_.Reset();
    }

    void PauseDuplication() noexcept {
        ResetDuplication();
        retryAt_ = GetTickCount64() + kDuplicationRetryMs;
    }

    bool InitDuplication(const Layout& layout) noexcept {
        // 旋转输出交给 GDI，统一使用屏幕上实际显示的方向。
        if (layout.orientation != DMDO_DEFAULT) {
            return false;
        }

        ComPtr<IDXGIFactory1> factory;
        if (FAILED(CreateDXGIFactory1(IID_PPV_ARGS(factory.GetAddressOf())))) {
            return false;
        }

        for (UINT adapterIndex = 0;; ++adapterIndex) {
            ComPtr<IDXGIAdapter1> adapter;
            if (FAILED(factory->EnumAdapters1(adapterIndex, adapter.GetAddressOf()))) {
                return false;
            }

            for (UINT outputIndex = 0;; ++outputIndex) {
                ComPtr<IDXGIOutput> output;
                if (FAILED(adapter->EnumOutputs(outputIndex, output.GetAddressOf()))) {
                    break;
                }

                DXGI_OUTPUT_DESC outputInfo{};
                if (FAILED(output->GetDesc(&outputInfo)) || !outputInfo.AttachedToDesktop ||
                    outputInfo.Monitor != layout.monitor ||
                    !EqualRect(&outputInfo.DesktopCoordinates, &layout.primary)) {
                    continue;
                }
                if (outputInfo.Rotation != DXGI_MODE_ROTATION_IDENTITY) {
                    return false;
                }

                ComPtr<IDXGIOutput1> output1;
                if (FAILED(output.As(&output1)) ||
                    FAILED(D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                                             D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0,
                                             D3D11_SDK_VERSION, device_.GetAddressOf(),
                                             nullptr, context_.GetAddressOf())) ||
                    FAILED(output1->DuplicateOutput(device_.Get(), duplication_.GetAddressOf()))) {
                    return false;
                }

                DXGI_OUTDUPL_DESC info{};
                duplication_->GetDesc(&info);
                if (info.Rotation != DXGI_MODE_ROTATION_IDENTITY ||
                    info.ModeDesc.Width != static_cast<UINT>(layout.Width()) ||
                    info.ModeDesc.Height != static_cast<UINT>(layout.Height()) ||
                    info.ModeDesc.Format != DXGI_FORMAT_B8G8R8A8_UNORM) {
                    return false;
                }

                D3D11_TEXTURE2D_DESC staging{};
                staging.Width = info.ModeDesc.Width;
                staging.Height = info.ModeDesc.Height;
                staging.MipLevels = 1;
                staging.ArraySize = 1;
                staging.Format = info.ModeDesc.Format;
                staging.SampleDesc.Count = 1;
                staging.Usage = D3D11_USAGE_STAGING;
                staging.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
                return SUCCEEDED(device_->CreateTexture2D(&staging, nullptr,
                                                          staging_.GetAddressOf()));
            }
        }
    }

    ReadResult ReadDuplication(const Layout& layout) noexcept {
        DXGI_OUTDUPL_FRAME_INFO info{};
        ComPtr<IDXGIResource> resource;
        const HRESULT result = duplication_->AcquireNextFrame(0, &info, resource.GetAddressOf());
        if (result == DXGI_ERROR_WAIT_TIMEOUT) {
            return ReadResult::Idle;
        }
        if (FAILED(result)) {
            return ReadResult::Failed;
        }
        const DuplicationFrame frame{duplication_.Get()};

        ComPtr<ID3D11Texture2D> texture;
        if (FAILED(resource.As(&texture))) {
            return ReadResult::Failed;
        }

        D3D11_TEXTURE2D_DESC desc{};
        texture->GetDesc(&desc);
        if (desc.Width != static_cast<UINT>(layout.Width()) ||
            desc.Height != static_cast<UINT>(layout.Height()) ||
            desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM ||
            desc.MipLevels != 1 || desc.ArraySize != 1 || desc.SampleDesc.Count != 1) {
            return ReadResult::Failed;
        }

        context_->CopyResource(staging_.Get(), texture.Get());
        D3D11_MAPPED_SUBRESOURCE mapped{};
        if (FAILED(context_->Map(staging_.Get(), 0, D3D11_MAP_READ, 0, &mapped))) {
            return ReadResult::Failed;
        }
        const MappedTexture mapping{context_.Get(), staging_.Get()};
        return CopyPixels(static_cast<const BYTE*>(mapped.pData), mapped.RowPitch, layout)
                   ? ReadResult::Fresh
                   : ReadResult::Failed;
    }

    bool ReadGdi(const Layout& layout) noexcept {
        std::size_t bytes = 0;
        if (!ImageSize(layout.Width(), layout.Height(), bytes) ||
            bytes > MAXDWORD) {
            return false;
        }

        GdiCapture gdi;
        gdi.screen = GetDC(nullptr);
        if (!gdi.screen) {
            return false;
        }
        gdi.memory = CreateCompatibleDC(gdi.screen);
        if (!gdi.memory) {
            return false;
        }

        BITMAPINFO info{};
        info.bmiHeader.biSize = sizeof(info.bmiHeader);
        info.bmiHeader.biWidth = layout.Width();
        info.bmiHeader.biHeight = -layout.Height();
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        void* bits = nullptr;
        gdi.bitmap = CreateDIBSection(gdi.screen, &info, DIB_RGB_COLORS, &bits, nullptr, 0);
        if (!gdi.bitmap || !bits) {
            return false;
        }
        gdi.previous = SelectObject(gdi.memory, gdi.bitmap);
        if (!gdi.previous || gdi.previous == HGDI_ERROR ||
            !BitBlt(gdi.memory, 0, 0, layout.Width(), layout.Height(),
                    gdi.screen, layout.primary.left, layout.primary.top,
                    SRCCOPY | CAPTUREBLT) ||
            !GdiFlush()) {
            return false;
        }
        return CopyPixels(static_cast<const BYTE*>(bits), bytes / layout.Height(), layout);
    }

    bool CopyPixels(const BYTE* source, std::size_t pitch, const Layout& layout) noexcept {
        std::size_t bytes = 0;
        if (!source || !ImageSize(layout.Width(), layout.Height(), bytes)) {
            return false;
        }
        const auto height = static_cast<std::size_t>(layout.Height());
        const auto rowBytes = bytes / height;
        if (pitch < rowBytes || pitch > (std::numeric_limits<std::size_t>::max)() / height) {
            return false;
        }
        try {
            pixels_.resize(bytes);
        } catch (...) {
            return false;
        }

        for (std::size_t row = 0; row < height; ++row) {
            std::memcpy(pixels_.data() + row * rowBytes, source + row * pitch, rowBytes);
        }
        return true;
    }

    ComPtr<ID3D11Device> device_;
    ComPtr<ID3D11DeviceContext> context_;
    ComPtr<IDXGIOutputDuplication> duplication_;
    ComPtr<ID3D11Texture2D> staging_;
    std::vector<BYTE> pixels_;
    ULONGLONG retryAt_ = 0;
};

int RoundedEven(std::int64_t numerator, int denominator) noexcept {
    return static_cast<int>(2 * ((numerator + denominator) / (2LL * denominator)));
}

void Fit(int sourceW, int sourceH, int capW, int capH, int& width, int& height) noexcept {
    const int limitW = std::min(sourceW, capW) & ~1;
    const int limitH = std::min(sourceH, capH) & ~1;
    if (std::int64_t{limitW} * sourceH <= std::int64_t{limitH} * sourceW) {
        width = limitW;
        height = std::clamp(RoundedEven(std::int64_t{width} * sourceH, sourceW), 2, limitH);
    } else {
        height = limitH;
        width = std::clamp(RoundedEven(std::int64_t{height} * sourceW, sourceH), 2, limitW);
    }
}

void ScaleBgra(const BYTE* source, int sourceW, int sourceH,
               BYTE* output, int width, int height) noexcept {
    const auto sourceStride = static_cast<std::size_t>(sourceW) * 4;
    if (sourceW == width && sourceH == height) {
        std::memcpy(output, source, sourceStride * sourceH);
        return;
    }
    // 按像素中心双线性重采样，避免细字笔画在缩小时整列丢失。
    const auto xStep = (static_cast<std::int64_t>(sourceW) << 16) / width;
    const auto yStep = (static_cast<std::int64_t>(sourceH) << 16) / height;
    for (int y = 0; y < height; ++y) {
        const auto sy = y * yStep + (yStep - 65536) / 2;
        const int row = static_cast<int>(sy >> 16);
        const int fy = (sy >> 8) & 255;
        const BYTE* top = source + row * sourceStride;
        const BYTE* bottom = source + std::min(row + 1, sourceH - 1) * sourceStride;
        for (int x = 0; x < width; ++x, output += 4) {
            const auto sx = x * xStep + (xStep - 65536) / 2;
            const int left = static_cast<int>(sx >> 16) * 4;
            const int right = std::min(left + 4, (sourceW - 1) * 4);
            const int fx = (sx >> 8) & 255;
            for (int c = 0; c < 3; ++c) {
                const int a = top[left+c]*(256-fx) + top[right+c]*fx;
                const int b = bottom[left+c]*(256-fx) + bottom[right+c]*fx;
                output[c] = static_cast<BYTE>((a*(256-fy) + b*fy + 32768) >> 16);
            }
            output[3] = 255;
        }
    }
}

INPUT MouseEvent(DWORD flags, DWORD data = 0) noexcept {
    INPUT input{};
    input.type = INPUT_MOUSE;
    input.mi.dwFlags = flags;
    input.mi.mouseData = data;
    input.mi.dwExtraInfo = kInputTag;
    return input;
}

INPUT KeyEvent(WORD vk) noexcept {
    INPUT input{};
    input.type = INPUT_KEYBOARD;
    input.ki.wVk = vk;
    const UINT scan = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC_EX);
    input.ki.wScan = static_cast<WORD>(scan & 0xff);
    if ((scan & 0xff00) == 0xe000) {
        input.ki.dwFlags = KEYEVENTF_EXTENDEDKEY;
    }
    input.ki.dwExtraInfo = kInputTag;
    return input;
}

bool SendOne(INPUT& input) noexcept {
    return SendInput(1, &input, sizeof(input)) == 1;
}

// 取虚拟桌面坐标中落在目标物理像素内的归一化值。
bool NormalizePixel(LONG pixel, LONG begin, LONG end, LONG& result) noexcept {
    const auto offset = std::int64_t{pixel} - begin;
    const auto span = std::int64_t{end} - begin;
    if (offset < 0 || offset >= span) {
        return false;
    }
    const auto low = (offset * 65536 + span - 1) / span;
    const auto high = ((offset + 1) * 65536 + span - 1) / span - 1;
    if (low > high) {
        return false;
    }
    result = static_cast<LONG>((low + high) / 2);
    return true;
}

enum class Action : UINT_PTR { None, View, Control, Stop };

bool LocalMouseMessage() noexcept {
    INPUT_MESSAGE_SOURCE source{};
    return GetCurrentInputMessageSource(&source) &&
           source.deviceType == IMDT_MOUSE && source.originId == IMO_HARDWARE &&
           static_cast<ULONG_PTR>(GetMessageExtraInfo()) != kInputTag;
}

class Host {
public:
    explicit Host(bool pairedOperator) noexcept : pairedOperator_(pairedOperator) {}
    const DWORD owner = GetCurrentThreadId();
    bool comInitialized = false;

    ~Host() {
        Stop();
        FinishStop();
        if (panel_) {
            if (registeredForSession_) {
                WTSUnRegisterSessionNotification(panel_);
            }
            DestroyWindow(panel_);
        }
        if (font_) {
            DeleteObject(font_);
        }
        if (windowClass_) {
            UnregisterClassW(MAKEINTATOM(windowClass_), module_);
        }
        if (comInitialized) {
            CoUninitialize();
        }
    }

    bool Start() noexcept {
        if (!ProcessIdToSessionId(GetCurrentProcessId(), &session_) ||
            !OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &ownToken_.value)) {
            return false;
        }
        if (!Interactive() || !ReadLayout(layout_)) {
            Stop();
            return true;
        }

        if (!GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
                                    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                                reinterpret_cast<LPCWSTR>(&kInputTag), &module_)) {
            return false;
        }

        wchar_t name[64]{};
        if (swprintf_s(name, _countof(name), L"RoHostConsent_%p", static_cast<void*>(this)) < 0) {
            return false;
        }
        WNDCLASSEXW wc{};
        wc.cbSize = sizeof(wc);
        wc.hInstance = module_;
        wc.lpszClassName = name;
        wc.lpfnWndProc = PanelProc;
        wc.hCursor = LoadCursorW(nullptr, IDC_ARROW);
        wc.hbrBackground = GetSysColorBrush(COLOR_WINDOW);
        windowClass_ = RegisterClassExW(&wc);
        if (!windowClass_) {
            return false;
        }

        panel_ = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                                MAKEINTATOM(windowClass_),
                                L"\u8FDC\u7A0B\u534F\u52A9",
                                WS_POPUP | WS_BORDER,
                                layout_.work.left, layout_.work.top, 1, 1,
                                nullptr, nullptr, module_, this);
        if (!panel_) {
            return false;
        }

        label_ = CreateWindowExW(0, L"STATIC", L"", WS_CHILD | WS_VISIBLE | SS_LEFT,
                                0, 0, 0, 0, panel_, nullptr, module_, nullptr);
        viewButton_ = CreateButton(L"\u5141\u8BB8\u89C2\u770B", Action::View);
        controlButton_ = CreateButton(L"\u5141\u8BB8\u63A7\u5236", Action::Control);
        stopButton_ = CreateButton(L"\u505C\u6B62\u5171\u4EAB", Action::Stop);
        if (!label_ || !viewButton_ || !controlButton_ || !stopButton_ ||
            !PositionPanel() || !WTSRegisterSessionNotification(panel_, NOTIFY_FOR_THIS_SESSION)) {
            return false;
        }
        registeredForSession_ = true;
        viewing_ = pairedOperator_;
        // 旧版系统会把排除标记解释为整块黑色，那里保留可见的小提示。
        if (pairedOperator_) {
            using VersionFn = LONG (WINAPI*)(OSVERSIONINFOW*);
            const auto version = reinterpret_cast<VersionFn>(GetProcAddress(
                GetModuleHandleW(L"ntdll.dll"), "RtlGetVersion"));
            OSVERSIONINFOW os{};
            os.dwOSVersionInfoSize = sizeof(os);
            if (version && version(&os) == 0 && os.dwBuildNumber >= 19041)
                SetWindowDisplayAffinity(panel_, WDA_EXCLUDEFROMCAPTURE);
        }

        ShowWindow(stopButton_, SW_SHOWNA);
        UpdatePanel();
        ShowWindow(panel_, SW_SHOWNOACTIVATE);
        return Service();
    }

    bool Service() noexcept {
        Pump();
        if (stopped_ || !CheckEnvironment()) {
            Stop(2);
            FinishStop();
            return false;
        }

        if (!controlling_ && AnyHeld()) {
            ReleaseHeld();
        }
        const Action click = pendingClick_;
        pendingClick_ = Action::None;
        if (click == Action::View && !viewing_) {
            viewing_ = true;
            UpdatePanel();
        } else if (click == Action::Control && viewing_ && inputRequested_ &&
                   !controlling_ && !AnyHeld()) {
            controlling_ = true;
            UpdatePanel();
        }
        return !stopped_;
    }

    int State() const noexcept {
        return controlling_ ? 3 : viewing_ ? 2 : 1;
    }

    int RequestInput(bool enabled) noexcept {
        if (!enabled) {
            const bool changed = inputRequested_ || controlling_;
            inputRequested_ = false;
            controlling_ = false;
            if (pendingClick_ == Action::Control) {
                pendingClick_ = Action::None;
            }
            if (pressedButton_ == controlButton_) {
                CancelPress();
            }
            if (changed && !stopped_) {
                UpdatePanel();
            }
            Service();
            return 0;
        }

        if (!Service()) {
            return -1;
        }
        if (pairedOperator_) {
            if (!viewing_ || AnyHeld()) return -1;
            controlling_ = true;
            UpdatePanel();
            return stopped_ ? -1 : 0;
        }
        if (!inputRequested_) {
            inputRequested_ = true;
            UpdatePanel();
        }
        return stopped_ ? -1 : 0;
    }

    int Frame(BYTE* output, int capacity, int capW, int capH,
              int* width, int* height, int* physicalW, int* physicalH) noexcept {
        if (!Service() || !viewing_) {
            return 0;
        }

        const int sourceW = layout_.Width();
        const int sourceH = layout_.Height();
        const auto generation = generation_;
        int outW = 0;
        int outH = 0;
        Fit(sourceW, sourceH, capW, capH, outW, outH);

        std::size_t bytes = 0;
        if (!ImageSize(outW, outH, bytes) || bytes > static_cast<std::size_t>(capacity)) {
            return -1;
        }

        if (!capture_.Read(layout_)) {
            return Service() && viewing_ && generation == generation_ ? -1 : 0;
        }
        if (!Service() || !viewing_ || generation != generation_) {
            return 0;
        }

        ScaleBgra(capture_.Pixels(), sourceW, sourceH, output, outW, outH);
        if (!Service() || !viewing_ || generation != generation_) {
            std::memset(output, 0, bytes);
            return 0;
        }

        *width = outW;
        *height = outH;
        *physicalW = sourceW;
        *physicalH = sourceH;
        return 1;
    }

    int Input(int kind, int a, int b) noexcept {
        const bool active = Service();
        if (kind == 4) {
            return ReleaseHeld() ? 0 : -1;
        }
        if (!active || !controlling_) {
            return -1;
        }

        switch (kind) {
        case 0:
            return Move(a, b);
        case 1:
            return MouseButton(a, b);
        case 2:
            if (pairedOperator_ && (pointerBlocked_ || LocalPointerOnPanel())) return 0;
            if (PointerAllowed(true) && ForegroundAllowed()) {
                INPUT input = MouseEvent(MOUSEEVENTF_WHEEL, static_cast<DWORD>(a));
                return SendOne(input) ? 0 : -1;
            }
            return -1;
        case 3:
            return Keyboard(a, b);
        default:
            return -1;
        }
    }

    int StopReason() const noexcept { return stopReason_; }

    void Stop(int reason = 0) noexcept {
        if (!stopped_) {
            stopReason_ = reason;
            stopped_ = true;
            viewing_ = false;
            controlling_ = false;
            inputRequested_ = false;
            pendingClick_ = Action::None;
            ++generation_;
        }
    }

    int DpiFailure() noexcept {
        Stop(4);
        FinishStop();
        return 0;
    }

private:
    bool Interactive() const noexcept {
        if (!ownToken_.value || session_ == 0 || session_ != WTSGetActiveConsoleSessionId()) {
            return false;
        }

        const HWINSTA station = GetProcessWindowStation();
        USEROBJECTFLAGS flags{};
        DWORD needed = 0;
        if (!ObjectNamed(station, L"WinSta0") ||
            !GetUserObjectInformationW(station, UOI_FLAGS, &flags, sizeof(flags), &needed) ||
            !(flags.dwFlags & WSF_VISIBLE) ||
            !ObjectNamed(GetThreadDesktop(owner), L"Default")) {
            return false;
        }

        WtsBuffer buffer;
        DWORD bytes = 0;
        if (!WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, session_, WTSSessionInfoEx,
                                         &buffer.value, &bytes) ||
            !buffer.value || bytes < sizeof(WTSINFOEXW)) {
            return false;
        }
        const auto* info = reinterpret_cast<const WTSINFOEXW*>(buffer.value);
        if (info->Level != 1) {
            return false;
        }
        const auto& logon = info->Data.WTSInfoExLevel1;
        if (logon.SessionId != session_ || logon.SessionState != WTSActive ||
            logon.SessionFlags != WTS_SESSIONSTATE_UNLOCK || !logon.UserName[0] ||
            !SameShellUser(session_, ownToken_.value)) {
            return false;
        }

        const DesktopHandle input{OpenInputDesktop(0, FALSE, DESKTOP_READOBJECTS)};
        return ObjectNamed(input.value, L"Default") && session_ == WTSGetActiveConsoleSessionId();
    }

    bool CheckEnvironment() noexcept {
        Layout fresh;
        if (!Interactive() || !panel_ || !IsWindowVisible(panel_) || IsIconic(panel_) ||
            !ReadLayout(fresh)) {
            return false;
        }

        const bool moved = !EqualRect(&layout_.work, &fresh.work);
        const bool changed = !SameDisplay(layout_, fresh);
        layout_ = fresh;
        if (displayDirty_ || changed) {
            // 显示器切换和物理布局变更需要重新授权，即使新屏像素相同。
            if (viewing_) return false;
            displayDirty_ = false;
            capture_.Reset();
            ++generation_;
        }
        if (moved || changed || dpiDirty_) {
            if (!PositionPanel()) {
                return false;
            }
        }

        DWORD cloaked = 0;
        RECT panelRect{};
        RECT stopRect{};
        if (stopped_ || FAILED(DwmGetWindowAttribute(panel_, DWMWA_CLOAKED,
                                                      &cloaked, sizeof(cloaked))) ||
            cloaked || !(GetWindowLongPtrW(panel_, GWL_EXSTYLE) & WS_EX_TOPMOST) ||
            !GetWindowRect(panel_, &panelRect) || !Contains(layout_.primary, panelRect) ||
            !IsWindowVisible(stopButton_) || !IsWindowEnabled(stopButton_) ||
            !GetWindowRect(stopButton_, &stopRect) || !Contains(panelRect, stopRect) ||
            !ValidBounds(stopRect)) {
            return false;
        }

        const POINT center{stopRect.left + (stopRect.right - stopRect.left) / 2,
                           stopRect.top + (stopRect.bottom - stopRect.top) / 2};
        return WindowFromPoint(center) == stopButton_;
    }

    void Pump() noexcept {
        MSG message{};
        for (unsigned count = 0; count < 128 && !stopped_; ++count) {
            if (!PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
                break;
            }
            if (message.message == WM_QUIT) {
                Stop();
                PostQuitMessage(static_cast<int>(message.wParam));
                break;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    void FinishStop() noexcept {
        if (!stopApplied_) {
            stopApplied_ = true;
            CancelPress();
            if (panel_ && IsWindowVisible(panel_)) {
                ShowWindow(panel_, SW_HIDE);
            }
            capture_.Reset();
        }
        if (AnyHeld()) {
            ReleaseHeld();
        }
    }

    HWND CreateButton(const wchar_t* text, Action action) noexcept {
        const auto id = static_cast<UINT_PTR>(action);
        const HWND button = CreateWindowExW(0, L"BUTTON", text, WS_CHILD | BS_PUSHBUTTON,
                                            0, 0, 0, 0, panel_,
                                            reinterpret_cast<HMENU>(id), module_, nullptr);
        if (!button || (action != Action::Stop && !SetWindowSubclass(button, ButtonProc, id,
                                          reinterpret_cast<DWORD_PTR>(this)))) {
            return nullptr;
        }
        return button;
    }

    bool PositionPanel() noexcept {
        const UINT dpi = GetDpiForWindow(panel_);
        if (!dpi) {
            return false;
        }
        dpiDirty_ = false;
        const int margin = MulDiv(pairedOperator_ ? 6 : 12, dpi, 96);
        const int width = std::min<LONG>(MulDiv(pairedOperator_ ? 210 : 390, dpi, 96), layout_.work.right - layout_.work.left);
        const int height = MulDiv(pairedOperator_ ? 36 : 105, dpi, 96);
        if (width < MulDiv(pairedOperator_ ? 180 : 255, dpi, 96) ||
            height > layout_.work.bottom - layout_.work.top) {
            return false;
        }

        const int x = std::max(layout_.work.left, layout_.work.right - width - margin);
        const int y = std::min(layout_.work.top + margin, layout_.work.bottom - height);
        if (!SetWindowPos(panel_, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE)) {
            return false;
        }
        RECT client{};
        if (!GetClientRect(panel_, &client)) {
            return false;
        }

        const int buttonW = MulDiv(pairedOperator_ ? 64 : 104, dpi, 96);
        const int buttonH = MulDiv(pairedOperator_ ? 24 : 30, dpi, 96);
        const int buttonY = client.bottom - margin - buttonH;
        const int stopX = client.right - margin - buttonW;
        const int allowX = stopX - margin - buttonW;
        if (pairedOperator_) {
            if (!MoveWindow(label_, margin, (client.bottom - MulDiv(18,dpi,96))/2,
                            stopX-2*margin, MulDiv(18,dpi,96), TRUE) ||
                !MoveWindow(stopButton_, stopX, (client.bottom-buttonH)/2, buttonW, buttonH, TRUE))
                return false;
        } else if (allowX < margin || buttonY <= margin ||
            !MoveWindow(label_, margin, margin, client.right - 2 * margin,
                        buttonY - margin, TRUE) ||
            !MoveWindow(viewButton_, allowX, buttonY, buttonW, buttonH, TRUE) ||
            !MoveWindow(controlButton_, allowX, buttonY, buttonW, buttonH, TRUE) ||
            !MoveWindow(stopButton_, stopX, buttonY, buttonW, buttonH, TRUE)) {
            return false;
        }

        if (dpi != fontDpi_) {
            NONCLIENTMETRICSW metrics{};
            metrics.cbSize = sizeof(metrics);
            HFONT font = nullptr;
            if (SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, sizeof(metrics),
                                           &metrics, 0, dpi)) {
                font = CreateFontIndirectW(&metrics.lfMessageFont);
            }
            const HFONT displayFont = font ? font :
                static_cast<HFONT>(GetStockObject(DEFAULT_GUI_FONT));
            for (HWND control : {label_, viewButton_, controlButton_, stopButton_}) {
                SendMessageW(control, WM_SETFONT, reinterpret_cast<WPARAM>(displayFont), TRUE);
            }
            if (font_) {
                DeleteObject(font_);
            }
            font_ = font;
            fontDpi_ = dpi;
        }
        return true;
    }

    void UpdatePanel() noexcept {
        if (stopped_) {
            return;
        }

        const wchar_t* text;
        if (pairedOperator_) {
            text = controlling_ ? L"正在共享 · 人工控制中" : L"正在共享 · 仅观察";
        } else if (!viewing_) {
            text = L"\u8FDC\u7A0B\u534F\u52A9\u8BF7\u6C42\u89C2\u770B\u4E3B\u5C4F\u5E55\u3002";
        } else if (controlling_) {
            text = L"\u6B63\u5728\u5171\u4EAB\u4E3B\u5C4F\u5E55\uff0c\u5DF2\u5141\u8BB8\u9F20\u6807\u548C\u952E\u76D8\u63A7\u5236\u3002";
        } else if (inputRequested_) {
            text = L"\u6B63\u5728\u5171\u4EAB\u4E3B\u5C4F\u5E55\uff0c\u5BF9\u65B9\u8BF7\u6C42\u9F20\u6807\u548C\u952E\u76D8\u63A7\u5236\u3002";
        } else {
            text = L"\u6B63\u5728\u5171\u4EAB\u4E3B\u5C4F\u5E55\uff0c\u4EC5\u5141\u8BB8\u89C2\u770B\u3002";
        }

        if (!SetWindowTextW(label_, text)) {
            Stop();
            return;
        }
        ShowWindow(viewButton_, !viewing_ ? SW_SHOWNA : SW_HIDE);
        ShowWindow(controlButton_, viewing_ && inputRequested_ && !controlling_ ? SW_SHOWNA : SW_HIDE);
    }

    bool ActionAllowed(Action action) const noexcept {
        if (stopped_) {
            return false;
        }
        switch (action) {
        case Action::View:
            return !viewing_ && !controlling_;
        case Action::Control:
            return viewing_ && inputRequested_ && !controlling_ && !AnyHeld();
        case Action::Stop:
            return true;
        default:
            return false;
        }
    }

    static bool ButtonHit(HWND button, LPARAM position) noexcept {
        POINT p{GET_X_LPARAM(position), GET_Y_LPARAM(position)};
        RECT rect{};
        return IsWindowVisible(button) && GetClientRect(button, &rect) &&
               PtInRect(&rect, p) && ClientToScreen(button, &p) && WindowFromPoint(p) == button;
    }

    void CancelPress() noexcept {
        const HWND button = pressedButton_;
        pressedButton_ = nullptr;
        if (button) {
            SendMessageW(button, BM_SETSTATE, FALSE, 0);
            if (GetCapture() == button) {
                ReleaseCapture();
            }
        }
    }

    static LRESULT CALLBACK ButtonProc(HWND window, UINT message, WPARAM wparam,
                                       LPARAM lparam, UINT_PTR id, DWORD_PTR reference) noexcept {
        auto& host = *reinterpret_cast<Host*>(reference);
        const auto action = static_cast<Action>(id);
        switch (message) {
        case WM_MOUSEACTIVATE:
            return MA_NOACTIVATE;
        case WM_LBUTTONDOWN:
            if (LocalMouseMessage() && host.ActionAllowed(action) && ButtonHit(window, lparam)) {
                host.CancelPress();
                SetCapture(window);
                host.pressedButton_ = window;
                SendMessageW(window, BM_SETSTATE, TRUE, 0);
            }
            return 0;
        case WM_LBUTTONUP:
            if (LocalMouseMessage() && host.pressedButton_ == window) {
                const bool clicked = host.ActionAllowed(action) && ButtonHit(window, lparam);
                host.CancelPress();
                if (clicked) {
                    if (action == Action::Stop) {
                        host.Stop(1);
                    } else {
                        host.pendingClick_ = action;
                    }
                }
            }
            return 0;
        case WM_LBUTTONDBLCLK:
        case BM_CLICK:
            return 0;
        case WM_CAPTURECHANGED:
        case WM_CANCELMODE:
            if (host.pressedButton_ == window) {
                host.CancelPress();
            }
            break;
        case WM_NCDESTROY:
            if (host.pressedButton_ == window) {
                host.pressedButton_ = nullptr;
            }
            RemoveWindowSubclass(window, ButtonProc, id);
            break;
        }
        return DefSubclassProc(window, message, wparam, lparam);
    }

    static LRESULT CALLBACK PanelProc(HWND window, UINT message, WPARAM wparam,
                                      LPARAM lparam) noexcept {
        if (message == WM_NCCREATE) {
            const auto* create = reinterpret_cast<const CREATESTRUCTW*>(lparam);
            SetLastError(ERROR_SUCCESS);
            if (!SetWindowLongPtrW(window, GWLP_USERDATA,
                                   reinterpret_cast<LONG_PTR>(create->lpCreateParams)) &&
                GetLastError() != ERROR_SUCCESS) {
                return FALSE;
            }
        }
        auto* host = reinterpret_cast<Host*>(GetWindowLongPtrW(window, GWLP_USERDATA));
        if (host) {
            switch (message) {
            case WM_MOUSEACTIVATE:
                return MA_NOACTIVATE;
            case WM_NCHITTEST:
                if (host->pairedOperator_ && host->stopButton_) {
                    RECT stopArea{};
                    POINT p{GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)};
                    if (GetWindowRect(host->stopButton_, &stopArea) && !PtInRect(&stopArea,p))
                        return HTCAPTION;
                }
                break;
            case WM_MOVING:
                if (host->pairedOperator_ && lparam) {
                    auto& r = *reinterpret_cast<RECT*>(lparam);
                    const auto& work = host->layout_.work;
                    const LONG w = r.right-r.left, h = r.bottom-r.top;
                    r.left = std::clamp(r.left,work.left,std::max(work.left,work.right-w));
                    r.top = std::clamp(r.top,work.top,std::max(work.top,work.bottom-h));
                    r.right=r.left+w; r.bottom=r.top+h;
                    return TRUE;
                }
                break;
            case WM_COMMAND:
                // 停止支持标准按钮及辅助功能；授权按钮仍单独核对输入来源。
                if (HIWORD(wparam) == BN_CLICKED && host->stopButton_ &&
                    reinterpret_cast<HWND>(lparam) == host->stopButton_) host->Stop(1);
                return 0;
            case WM_CLOSE:
                host->Stop(1);
                return 0;
            case WM_DISPLAYCHANGE:
                host->displayDirty_ = true;
                ++host->generation_;
                return 0;
            case WM_DPICHANGED:
                host->dpiDirty_ = true;
                return 0;
            case WM_WTSSESSION_CHANGE:
                if (static_cast<DWORD>(lparam) == host->session_ &&
                    (wparam == WTS_SESSION_LOCK || wparam == WTS_CONSOLE_DISCONNECT ||
                     wparam == WTS_REMOTE_CONNECT || wparam == WTS_REMOTE_DISCONNECT ||
                     wparam == WTS_SESSION_LOGOFF)) {
                    host->Stop(3);
                }
                return 0;
            case WM_NCDESTROY:
                host->Stop();
                host->panel_ = nullptr;
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                break;
            }
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }

    bool IsPrimaryWindow(HWND window) const noexcept {
        return window && GetAncestor(window, GA_ROOT) != panel_ &&
               MonitorFromWindow(window, MONITOR_DEFAULTTONULL) == layout_.monitor;
    }

    bool ForegroundAllowed() const noexcept {
        const HWND foreground = GetForegroundWindow();
        GUITHREADINFO info{};
        info.cbSize = sizeof(info);
        const DWORD thread = foreground ? GetWindowThreadProcessId(foreground, nullptr) : 0;
        return IsPrimaryWindow(foreground) && thread && GetGUIThreadInfo(thread, &info) &&
               (!info.hwndFocus || IsPrimaryWindow(info.hwndFocus));
    }

    bool LocalPointerOnPanel() const noexcept {
        RECT panel{};
        POINT cursor{};
        return pressedButton_ || (GetWindowRect(panel_, &panel) && GetCursorPos(&cursor) &&
                                  PtInRect(&panel, cursor));
    }

    bool PointerAllowed(bool requirePrimary) const noexcept {
        RECT panelRect{};
        POINT cursor{};
        if (pressedButton_ || !GetWindowRect(panel_, &panelRect) ||
            !GetCursorPos(&cursor) || PtInRect(&panelRect, cursor) ||
            (requirePrimary && !PtInRect(&layout_.primary, cursor))) {
            return false;
        }

        GUITHREADINFO info{};
        info.cbSize = sizeof(info);
        return GetGUIThreadInfo(0, &info) &&
               (!info.hwndCapture || IsPrimaryWindow(info.hwndCapture));
    }

    int Move(int a, int b) noexcept {
        if (a < 0 || a > 65535 || b < 0 || b > 65535) {
            return -1;
        }

        const POINT target{
            layout_.primary.left + static_cast<LONG>((std::int64_t{a} * (layout_.Width() - 1) + 32767) / 65535),
            layout_.primary.top + static_cast<LONG>((std::int64_t{b} * (layout_.Height() - 1) + 32767) / 65535)
        };
        RECT panelRect{};
        if (!GetWindowRect(panel_, &panelRect)) return -1;
        // 留给现场人员的停止按钮不接受远端输入，忽略移动后也不能点击旧坐标。
        pointerBlocked_ = pairedOperator_ && (PtInRect(&panelRect, target) || LocalPointerOnPanel());
        if (pointerBlocked_) return 0;
        if (PtInRect(&panelRect, target) ||
            !PointerAllowed(std::any_of(mouseHeld_.begin(), mouseHeld_.end(),
                                        [](bool held) { return held; }))) {
            return -1;
        }

        INPUT input = MouseEvent(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK);
        const RECT& screen = layout_.virtualScreen;
        if (!NormalizePixel(target.x, screen.left, screen.right, input.mi.dx) ||
            !NormalizePixel(target.y, screen.top, screen.bottom, input.mi.dy)) {
            return -1;
        }
        return SendOne(input) ? 0 : -1;
    }

    int MouseButton(int button, int down) noexcept {
        if (button < 0 || button >= static_cast<int>(mouseHeld_.size()) ||
            (down != 0 && down != 1)) {
            return -1;
        }
        if (down && pairedOperator_ && (pointerBlocked_ || LocalPointerOnPanel())) return 0;
        auto& held = mouseHeld_[button];
        if (held == (down != 0)) {
            return 0;
        }
        if (down && (!PointerAllowed(true) || (GetAsyncKeyState(mouseVks_[button]) & 0x8000))) {
            return -1;
        }

        INPUT input = MouseEvent(down ? mouseDown_[button] : mouseUp_[button]);
        if (!SendOne(input)) {
            return -1;
        }
        held = down != 0;
        return 0;
    }

    int Keyboard(int vk, int down) noexcept {
        if (vk <= VK_XBUTTON2 || vk >= 255 || vk == VK_PACKET || vk == VK_PROCESSKEY ||
            (down != 0 && down != 1)) {
            return -1;
        }
        switch (vk) {
        case VK_SHIFT:   vk = VK_LSHIFT; break;
        case VK_CONTROL: vk = VK_LCONTROL; break;
        case VK_MENU:    vk = VK_LMENU; break;
        }

        auto& release = keyUp_[vk];
        if (!down) {
            if (release.type != INPUT_KEYBOARD) {
                return 0;
            }
            if (!SendOne(release)) {
                return -1;
            }
            release = {};
            return 0;
        }

        if (pressedButton_ || !ForegroundAllowed() ||
            (release.type != INPUT_KEYBOARD && (GetAsyncKeyState(vk) & 0x8000))) {
            return -1;
        }
        INPUT input = release.type == INPUT_KEYBOARD ? release : KeyEvent(static_cast<WORD>(vk));
        input.ki.dwFlags &= ~KEYEVENTF_KEYUP;
        if (!SendOne(input)) {
            return -1;
        }
        input.ki.dwFlags |= KEYEVENTF_KEYUP;
        release = input;
        return 0;
    }

    bool AnyHeld() const noexcept {
        return std::any_of(mouseHeld_.begin(), mouseHeld_.end(), [](bool held) { return held; }) ||
               std::any_of(keyUp_.begin(), keyUp_.end(),
                           [](const INPUT& input) { return input.type == INPUT_KEYBOARD; });
    }

    bool ReleaseHeld() noexcept {
        if (!AnyHeld()) {
            return true;
        }
        // 安全桌面期间保留待释放状态，回到原交互桌面后再尝试。
        if (!Interactive()) {
            return false;
        }
        for (std::size_t i = 0; i < mouseHeld_.size(); ++i) {
            if (mouseHeld_[i]) {
                INPUT input = MouseEvent(mouseUp_[i]);
                if (SendOne(input)) {
                    mouseHeld_[i] = false;
                }
            }
        }
        for (INPUT& input : keyUp_) {
            if (input.type == INPUT_KEYBOARD && SendOne(input)) {
                input = {};
            }
        }
        return !AnyHeld();
    }

    static constexpr std::array<int, 3> mouseVks_{VK_LBUTTON, VK_RBUTTON, VK_MBUTTON};
    static constexpr std::array<DWORD, 3> mouseDown_{
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_MIDDLEDOWN
    };
    static constexpr std::array<DWORD, 3> mouseUp_{
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_MIDDLEUP
    };

    DWORD session_ = 0;
    KernelHandle ownToken_;
    HINSTANCE module_ = nullptr;
    ATOM windowClass_ = 0;
    HWND panel_ = nullptr;
    HWND label_ = nullptr;
    HWND viewButton_ = nullptr;
    HWND controlButton_ = nullptr;
    HWND stopButton_ = nullptr;
    HWND pressedButton_ = nullptr;
    HFONT font_ = nullptr;
    UINT fontDpi_ = 0;
    Layout layout_;
    Capture capture_;
    std::array<INPUT, 256> keyUp_{};
    std::array<bool, 3> mouseHeld_{};
    std::uint64_t generation_ = 0;
    Action pendingClick_ = Action::None;
    const bool pairedOperator_;
    bool pointerBlocked_ = false;
    bool registeredForSession_ = false;
    bool stopped_ = false;
    bool stopApplied_ = false;
    int stopReason_ = 0;
    bool viewing_ = false;
    bool inputRequested_ = false;
    bool controlling_ = false;
    bool displayDirty_ = false;
    bool dpiDirty_ = false;
};

Host* OnOwnerThread(void* context) noexcept {
    auto* host = static_cast<Host*>(context);
    return host && host->owner == GetCurrentThreadId() ? host : nullptr;
}

}  // namespace

/// 在调用方的专用 MTA 线程上创建本地许可面板。
static void* createHost(bool pairedOperator) noexcept {
    const DpiScope dpi;
    if (!dpi) {
        return nullptr;
    }

    std::unique_ptr<Host> host(new (std::nothrow) Host(pairedOperator));
    if (!host || FAILED(CoInitializeEx(nullptr, COINIT_MULTITHREADED))) {
        return nullptr;
    }
    host->comInitialized = true;
    return host->Start() ? host.release() : nullptr;
}

extern "C" void* ro_host_new() noexcept { return createHost(false); }
extern "C" void* ro_host_new_paired() noexcept { return createHost(true); }

/// 在创建线程上停止共享并释放资源。
extern "C" void ro_host_free(void* context) noexcept {
    const DpiScope dpi;
    delete OnOwnerThread(context);
}

/// 返回首次停止桌面的本地原因。
extern "C" int ro_host_stop_reason(void* context) noexcept {
    const Host* host = OnOwnerThread(context);
    return host ? host->StopReason() : 0;
}

/// 轮询许可状态。已有观看许可时，等待控制授权仍返回 2。
extern "C" int ro_host_poll(void* context) noexcept {
    Host* host = OnOwnerThread(context);
    if (!host) {
        return 0;
    }
    const DpiScope dpi;
    if (!dpi) {
        return host->DpiFailure();
    }
    return host->Service() ? host->State() : 0;
}

/// 请求本地控制授权，或立即撤回控制并尝试释放按键。返回 0 表示受理。
extern "C" int ro_host_request_input(void* context, int enabled) noexcept {
    Host* host = OnOwnerThread(context);
    if (!host) {
        return -1;
    }
    const DpiScope dpi;
    if (!dpi) {
        host->DpiFailure();
        return enabled ? -1 : 0;
    }
    return host->RequestInput(enabled != 0);
}

/// 成功返回 1，无可交付帧返回 0，参数或采集错误返回 -1。
extern "C" int ro_host_frame(
    void* context, unsigned char* bgra, int capacity, int capwidth, int capheight,
    int* width, int* height, int* physical_w, int* physical_h) noexcept {
    if (width) *width = 0;
    if (height) *height = 0;
    if (physical_w) *physical_w = 0;
    if (physical_h) *physical_h = 0;

    Host* host = OnOwnerThread(context);
    if (!host) {
        return -1;
    }
    const DpiScope dpi;
    if (!dpi) {
        return host->DpiFailure();
    }
    if (!bgra || capacity < 0 || capwidth < 2 || capheight < 2 ||
        !width || !height || !physical_w || !physical_h) {
        host->Service();
        return -1;
    }
    return host->Frame(bgra, capacity, capwidth, capheight, width, height, physical_w, physical_h);
}

/// 发送获准的输入，或释放本实例持有的按键和按钮。成功返回 0。
extern "C" int ro_host_input(void* context, int kind, int a, int b) noexcept {
    Host* host = OnOwnerThread(context);
    if (!host) {
        return -1;
    }
    const DpiScope dpi;
    if (!dpi) {
        host->DpiFailure();
        return -1;
    }
    return host->Input(kind, a, b);
}