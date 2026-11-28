// 仅测试程序替换 WTS 的会话数据。窗口、桌面归属、采集和模式切换均使用真实 Windows 组件。
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <wtsapi32.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cwchar>
#include <memory>
#include <vector>

namespace fixture {
DWORD session;
DWORD console;
USHORT protocol = WTS_PROTOCOL_TYPE_RDP;
LONGLONG connectedAt = 1000;
bool locked = false;
bool disconnected = false;
bool protocolUnavailable = false;

HWND keyTarget = nullptr;
bool keyTest = false;
bool localKeyDown = false;
bool rejectKeyUp = false;
std::vector<INPUT> sentKeys;
HWND Foreground() { return keyTest ? keyTarget : GetForegroundWindow(); }
SHORT KeyState(int vk) { return keyTest ? (localKeyDown ? SHORT(0x8000) : 0) : GetAsyncKeyState(vk); }
UINT Send(UINT count, LPINPUT inputs, int) {
    if (!keyTest || count != 1 || inputs->type != INPUT_KEYBOARD) return 0;
    if (rejectKeyUp && (inputs->ki.dwFlags & KEYEVENTF_KEYUP)) return 0;
    sentKeys.push_back(*inputs);
    return 1;
}

DWORD ConsoleId() { return console; }

BOOL Query(HANDLE server, DWORD id, WTS_INFO_CLASS kind, LPWSTR* data, DWORD* bytes) {
    if (id != session || (kind == WTSClientProtocolType && protocolUnavailable)) return FALSE;
    if (!WTSQuerySessionInformationW(server, id, kind, data, bytes)) return FALSE;
    if (kind == WTSClientProtocolType && *bytes >= sizeof(protocol)) {
        std::memcpy(*data, &protocol, sizeof(protocol));
    } else if (kind == WTSSessionInfoEx && *bytes >= sizeof(WTSINFOEXW)) {
        auto& logon = reinterpret_cast<WTSINFOEXW*>(*data)->Data.WTSInfoExLevel1;
        if (locked) logon.SessionFlags = WTS_SESSIONSTATE_LOCK;
        if (disconnected) logon.SessionState = WTSDisconnected;
        logon.ConnectTime.QuadPart = connectedAt;
    }
    return TRUE;
}
}  // namespace fixture

#define WTSGetActiveConsoleSessionId fixture::ConsoleId
#define WTSQuerySessionInformationW fixture::Query
#define GetForegroundWindow fixture::Foreground
#define GetAsyncKeyState fixture::KeyState
#define SendInput fixture::Send
#include "../native/host.cpp"
#undef SendInput
#undef GetAsyncKeyState
#undef GetForegroundWindow
#undef WTSQuerySessionInformationW
#undef WTSGetActiveConsoleSessionId

namespace {
void Check(bool ok, const char* description) {
    if (!ok) {
        std::fprintf(stderr, "FAILED: %s\n", description);
        std::exit(1);
    }
}

using TestHost = std::unique_ptr<void, decltype(&ro_host_free)>;

TestHost Open(bool rdp) {
    TestHost host(ro_host_new_paired(), ro_host_free);
    if (!host || ro_host_poll(host.get()) != 2) {
        std::fprintf(stderr, "Windows host stop reason: %d\n", ro_host_stop_reason(host.get()));
    }
    Check(host && ro_host_poll(host.get()) == 2, "start in observe mode");
    Check((ro_host_is_rdp(host.get()) != 0) == rdp, "correct logged desktop type");
    return host;
}

void ExpectStopped(void* host, StopReason reason) {
    Check(ro_host_poll(host) == 0, "poll reports stopped");
    Check(ro_host_stop_reason(host) == static_cast<int>(reason), "first stop reason");
    Check(ro_host_request_input(host, 1) == -1, "stopped sharing rejects control");
}

void ExpectRefused(StopReason reason) {
    TestHost host(ro_host_new_paired(), ro_host_free);
    Check(!host && ro_host_stop_reason(nullptr) == static_cast<int>(reason), "refuse new sharing");
}

BOOL CALLBACK FindPanel(HWND window, LPARAM output) {
    wchar_t name[80]{};
    if (GetClassNameW(window, name, 80) && std::wcsncmp(name, L"RoHostConsent_", 14) == 0) {
        *reinterpret_cast<HWND*>(output) = window;
        return FALSE;
    }
    return TRUE;
}

void Notify(WPARAM event, DWORD id) {
    HWND panel = nullptr;
    EnumThreadWindows(GetCurrentThreadId(), FindPanel, reinterpret_cast<LPARAM>(&panel));
    Check(panel != nullptr, "find current test thread's sharing panel");
    SendMessageW(panel, WM_WTSSESSION_CHANGE, event, id);
}

void ObserveAndControl(void* host) {
    std::vector<BYTE> pixels(640 * 360 * 4);
    int w = 0, h = 0, pw = 0, ph = 0;
    const auto end = GetTickCount64() + 5000;
    int frame;
    do {
        frame = ro_host_frame(host, pixels.data(), static_cast<int>(pixels.size()), 640, 360, &w, &h, &pw, &ph);
        if (frame == 0) Sleep(10);
    } while (frame == 0 && GetTickCount64() < end);
    Check(frame == 1 && w > 0 && h > 0 && pw > 0 && ph > 0, "receive first desktop frame");
    Check(ro_host_request_input(host, 1) == 0 && ro_host_poll(host) == 3, "switch to control");
    fixture::keyTest = true;
    fixture::sentKeys.clear();
    fixture::keyTarget = CreateWindowExW(0, L"STATIC", L"Key fixture", WS_POPUP, 50, 50, 120, 40,
                                         nullptr, nullptr, GetModuleHandleW(nullptr), nullptr);
    Check(fixture::keyTarget != nullptr, "create primary keyboard target");
    for (int vk : {int('A'), int(VK_SPACE), int(VK_RETURN), int(VK_LCONTROL)}) {
        Check(ro_host_input(host, 3, vk, 1) == 0 && ro_host_input(host, 3, vk, 0) == 0 &&
              ro_host_poll(host) == 3, "typing retains control");
        Check(fixture::sentKeys.size() >= 2 && fixture::sentKeys.back().ki.wVk == vk &&
              (fixture::sentKeys.back().ki.dwFlags & KEYEVENTF_KEYUP), "key and release delivered");
    }
    fixture::localKeyDown = true;
    Check(ro_host_input(host, 3, 'A', 1) == 0 && ro_host_input(host, 3, 'A', 0) == 0, "local key retained");
    fixture::localKeyDown = false;
    Check(ro_host_input(host, 3, VK_SHIFT, 1) == 0, "hold a modifier during takeover");
    fixture::rejectKeyUp = true;
    Check(ro_host_request_input(host, 0) < 0 && ro_host_request_input(host, 1) < 0,
          "failed physical key release blocks acknowledgement and takeover");
    fixture::rejectKeyUp = false;
    Check(ro_host_request_input(host, 0) == 0 && ro_host_request_input(host, 1) == 0,
          "resume only after key-up succeeds");
    DestroyWindow(fixture::keyTarget);
    fixture::keyTarget = nullptr;
    Check(ro_host_input(host, 3, 'B', 1) == 0 && ro_host_poll(host) == 3 &&
          fixture::sentKeys.size() == 10, "transient focus gap drops key without revoking control");
    fixture::keyTest = false;
    Check(ro_host_request_input(host, 0) == 0 && ro_host_poll(host) == 2, "return to observe");
}
}  // namespace

int main() {
    using namespace fixture;
    Check(ProcessIdToSessionId(GetCurrentProcessId(), &session) && session != 0, "interactive test session");
    const DWORD other = session + 1;
    console = session;
    {
        auto host = Open(false);
        ObserveAndControl(host.get());
        console = other;
        ExpectStopped(host.get(), StopReason::ConsoleSession);
    }
    {
        auto host = Open(true);
        ObserveAndControl(host.get());
        Notify(WTS_REMOTE_DISCONNECT, other);
        Check(ro_host_poll(host.get()) == 2, "ignore another user's RDP disconnect");
        Notify(WTS_REMOTE_DISCONNECT, session);
        ExpectStopped(host.get(), StopReason::RdpDisconnect);
    }
    disconnected = true;
    ExpectRefused(StopReason::InactiveSession);
    disconnected = false;
    {
        auto host = Open(true);
        ObserveAndControl(host.get());
        ++connectedAt;
        ExpectStopped(host.get(), StopReason::SessionChange);
    }
    {
        auto host = Open(true);
        console = session;
        ExpectStopped(host.get(), StopReason::SessionChange);
        console = other;
    }
    {
        auto host = Open(true);
        locked = true;
        ExpectStopped(host.get(), StopReason::LockedSession);
    }
    ExpectRefused(StopReason::LockedSession);
    locked = false;
    protocolUnavailable = true;
    ExpectRefused(StopReason::ProtocolQuery);
    protocolUnavailable = false;
    protocol = 1;
    ExpectRefused(StopReason::UnsupportedProtocol);
    protocol = WTS_PROTOCOL_TYPE_CONSOLE;
    ExpectRefused(StopReason::UnsupportedProtocol);
    TestHost unpaired(ro_host_new(), ro_host_free);
    Check(!unpaired && ro_host_stop_reason(nullptr) == static_cast<int>(StopReason::ConsoleSession),
          "standalone local-consent host remains console-only");
    std::puts("Console and simulated RDP session checks passed, including capture, control, disconnect and reopen.");
}
