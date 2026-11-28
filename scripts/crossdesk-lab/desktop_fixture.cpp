// 用独立窗口验证 CrossDesk Windows UIA、键盘与真实像素。只向本进程的窗口发送测试动作。
#include <windows.h>
#include <UIAutomation.h>
#include <wrl/client.h>
#include "desktop_automation.h"
#include "interactive_desktop.h"
#include "windows_input_injector.h"
#include <atomic>
#include <chrono>
#include <future>
#include <iostream>
#include <stdexcept>
#include <thread>
#include <vector>

extern "C" __declspec(dllimport) int ro_cd_uia_tree(HWND,char*,int);
extern "C" __declspec(dllimport) int ro_cd_uia_apply(HWND,const int*,int,int,const char*,int);
extern "C" __declspec(dllimport) int ro_cd_pointer(HWND,int,int,int,int,int);
extern "C" __declspec(dllimport) int ro_cd_snapshot(unsigned char*,int,int,int);
extern "C" __declspec(dllimport) void* ro_cd_capture_open();
extern "C" __declspec(dllimport) int ro_cd_capture_frame(void*, unsigned char*, int, int, int, int, int);
extern "C" __declspec(dllimport) void ro_cd_capture_close(void*);

namespace {
using namespace std::chrono_literals;
using json = nlohmann::json;
std::atomic<int> phase{0};
constexpr COLORREF colors[] = { RGB(36,52,192), RGB(188,45,60), RGB(26,170,81) };
void check(bool ok, const char* reason) { if (!ok) throw std::runtime_error(reason); }

LRESULT CALLBACK handle(HWND window, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
    case WM_APP+1: SetFocus(window); return 0;
    case WM_COMMAND:
        if (LOWORD(wp) == 100 && HIWORD(wp) == BN_CLICKED) {
            phase = 1; InvalidateRect(window, nullptr, TRUE); return 0;
        }
        break;
    case WM_KEYDOWN:
        if (wp == VK_F24) { phase = 2; InvalidateRect(window, nullptr, TRUE); return 0; }
        break;
    case WM_PAINT: {
        PAINTSTRUCT ps{};
        HDC dc = BeginPaint(window, &ps);
        HBRUSH brush = CreateSolidBrush(colors[phase.load()]);
        FillRect(dc, &ps.rcPaint, brush);
        DeleteObject(brush);
        EndPaint(window, &ps);
        return 0;
    }
    case WM_DESTROY: PostQuitMessage(0); return 0;
    }
    return DefWindowProcW(window,msg,wp,lp);
}

class TestWindow {
public:
    TestWindow() {
        auto result = ready_.get_future();
        thread_ = std::thread([this] {
            WNDCLASSW wc{};
            wc.lpfnWndProc = handle; wc.hInstance = GetModuleHandleW(nullptr);
            wc.lpszClassName = L"RemoteOpsCrossDeskTestWindow";
            HWND w = nullptr;
            if (RegisterClassW(&wc)) {
                w = CreateWindowExW(WS_EX_TOPMOST, wc.lpszClassName, L"RemoteOps · 图形验收",
                    WS_OVERLAPPEDWINDOW, 100, 100, 420, 215, nullptr, nullptr, wc.hInstance, nullptr);
            }
            if (w) {
                CreateWindowW(L"BUTTON",L"Apply fixture change",WS_CHILD|WS_VISIBLE|BS_PUSHBUTTON,
                    20,26,165,43,w,reinterpret_cast<HMENU>(100),wc.hInstance,nullptr);
                ShowWindow(w,SW_SHOW); SetForegroundWindow(w); SetFocus(w); UpdateWindow(w);
            }
            ready_.set_value(w);
            if (w) {
                MSG m{};
                while (GetMessageW(&m,nullptr,0,0)>0) { TranslateMessage(&m); DispatchMessageW(&m); }
            }
        });
        hwnd = result.get();
    }
    ~TestWindow() { if (hwnd) PostMessageW(hwnd, WM_CLOSE, 0, 0); thread_.join(); }
    HWND hwnd = nullptr;
private:
    std::promise<HWND> ready_;
    std::thread thread_;
};

void focus_test_window(HWND w) {
    POINT p{240,105}, previous{};
    check(ClientToScreen(w,&p) && WindowFromPoint(p)==w && GetCursorPos(&previous) &&
          !(GetAsyncKeyState(VK_LBUTTON)&0x8000), "test click target unavailable");
    const int x=GetSystemMetrics(SM_XVIRTUALSCREEN), y=GetSystemMetrics(SM_YVIRTUALSCREEN);
    const int width=GetSystemMetrics(SM_CXVIRTUALSCREEN), height=GetSystemMetrics(SM_CYVIRTUALSCREEN);
    check(width>1 && height>1, "test display unavailable");
    INPUT move{};
    move.type=INPUT_MOUSE; move.mi.dwFlags=MOUSEEVENTF_MOVE|MOUSEEVENTF_ABSOLUTE|MOUSEEVENTF_VIRTUALDESK;
    move.mi.dx=MulDiv(p.x-x,65535,width-1); move.mi.dy=MulDiv(p.y-y,65535,height-1);
    check(crossdesk::SendInputOnUserDesktop(move)==1,"CrossDesk mouse move failed");
    POINT hit{};
    check(GetCursorPos(&hit) && WindowFromPoint(hit)==w, "pointer left fixture window");
    INPUT click[2]{};
    click[0].type=click[1].type=INPUT_MOUSE;
    click[0].mi.dwFlags=MOUSEEVENTF_LEFTDOWN; click[1].mi.dwFlags=MOUSEEVENTF_LEFTUP;
    const auto count=crossdesk::SendInputOnUserDesktop(click,2);
    if (count==1) crossdesk::SendInputOnUserDesktop(click[1]);
    check(count==2,"CrossDesk test click failed");
    const auto until = std::chrono::steady_clock::now()+2s;
    while (GetForegroundWindow()!=w && std::chrono::steady_clock::now()<until) Sleep(10);
    POINT current{};
    if (GetCursorPos(&current) && current.x==hit.x && current.y==hit.y)
        SetCursorPos(previous.x,previous.y);
    check(GetForegroundWindow()==w,"test window is not foreground");
    SendMessageW(w,WM_APP+1,0,0);
}

std::vector<int> button_identity(HWND hwnd) {
    using Microsoft::WRL::ComPtr;
    ComPtr<IUIAutomation> automation;
    check(SUCCEEDED(CoCreateInstance(__uuidof(CUIAutomation),nullptr,CLSCTX_INPROC_SERVER,IID_PPV_ARGS(&automation))),"UIA client unavailable");
    ComPtr<IUIAutomationElement> button;
    check(SUCCEEDED(automation->ElementFromHandle(GetDlgItem(hwnd,100),&button)) && button,"test button unavailable");
    SAFEARRAY* id=nullptr;
    check(SUCCEEDED(button->GetRuntimeId(&id)) && id,"UIA identity unavailable");
    LONG first=0,last=-1;
    check(SUCCEEDED(SafeArrayGetLBound(id,1,&first)) && SUCCEEDED(SafeArrayGetUBound(id,1,&last)) && last>=first && last-first<128,"invalid runtime id");
    std::vector<int> numbers;
    for(LONG i=first;i<=last;++i) {
        LONG number; check(SUCCEEDED(SafeArrayGetElement(id,&i,&number)),"invalid UIA element");
        numbers.push_back(number);
    }
    SafeArrayDestroy(id);
    return numbers;
}

struct Capture {
    void* handle = ro_cd_capture_open();
    ~Capture() { if (handle) ro_cd_capture_close(handle); }
    void expect(HWND w, int stage) {
        const int native_w = GetSystemMetrics(SM_CXSCREEN), native_h = GetSystemMetrics(SM_CYSCREEN);
        POINT p{240,105};
        check(ClientToScreen(w,&p) && p.x >= 0 && p.y >= 0 && p.x < native_w && p.y < native_h &&
              WindowFromPoint(p)==w, "test pixel covered");
        const int width = std::min(native_w, 1280) & ~1;
        const int height = int(double(width) * native_h / native_w) & ~1;
        check(width && height > 0 && height <= 1080, "unsupported primary geometry");
        std::vector<unsigned char> pixels(width*height*4);
        const int i = ((p.y*height/native_h)*width+p.x*width/native_w)*4;
        const auto due = std::chrono::steady_clock::now()+4s;
        while (std::chrono::steady_clock::now()<due) {
            const auto result=ro_cd_capture_frame(handle,pixels.data(),int(pixels.size()),native_w,native_h,width,height);
            if (result == 1 && phase==stage && abs(int(pixels[i])-GetBValue(colors[stage]))<20 &&
                abs(int(pixels[i+1])-GetGValue(colors[stage]))<20 && abs(int(pixels[i+2])-GetRValue(colors[stage]))<20) return;
            check(result >= 0,"capture failure");
            Sleep(12);
        }
        throw std::runtime_error("expected desktop pixels were not observed");
    }
};
}

int main() {
    try {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        std::string reason;
        if (!crossdesk::IsCurrentSessionUserDesktopActive(&reason))
            throw std::runtime_error("interactive desktop unavailable: " + reason);
        TestWindow w;
        check(w.hwnd != nullptr,"could not create test window");
        std::cerr << "Windows fixture: window ready\n";
        crossdesk::ScopedUserDesktopInputThread input;
        focus_test_window(w.hwnd);
        std::cerr << "Windows fixture: mouse target confirmed\n";
        Capture capture;
        check(capture.handle != nullptr,"CrossDesk capture unavailable");
        capture.expect(w.hwnd,0);
        std::cerr << "Windows fixture: first pixels received\n";
        crossdesk::DesktopAutomation automation;
        auto tree = automation.Execute("fixture", {{"operation","tree"},{"pid", GetCurrentProcessId()}});
        json press;
        for (const auto& item : tree.at("elements")) {
            if (item.value("name","")=="Apply fixture change") {
                press={{"operation","press"},{"snapshot_id",tree.at("snapshot_id")},{"element_id",item.at("element_id")}};
                break;
            }
        }
        check(!press.empty(),"UIA did not return fixture button");
        std::cerr << "Windows fixture: UIA button found\n";
        bool denied = false;
        try { automation.Execute("other-viewer",press); } catch (const std::invalid_argument&) {denied=true;}
        check(denied && phase==0,"UIA ownership was not enforced");
        check(automation.Execute("fixture",press).at("accepted")==true,"UIA press failed");
        capture.expect(w.hwnd,1);
        std::cerr << "Windows fixture: UIA change visible\n";
        denied=false;
        try { automation.Execute("fixture",press); } catch (const std::invalid_argument&) {denied=true;}
        check(denied,"used UIA reference remained valid");
        check(GetForegroundWindow()==w.hwnd,"test window lost foreground");
        SendMessageW(w.hwnd,WM_APP+1,0,0);
        INPUT keys[2]{};
        keys[0].type=INPUT_KEYBOARD; keys[0].ki.wVk=VK_F24;
        keys[1]=keys[0]; keys[1].ki.dwFlags=KEYEVENTF_KEYUP;
        check(crossdesk::SendInputOnUserDesktop(keys,2)==2,"CrossDesk keyboard was not delivered");
        capture.expect(w.hwnd,2);
        std::vector<char> observed(65536);
        check(ro_cd_uia_tree(w.hwnd,observed.data(),int(observed.size()))>0,"native Agent UIA observation");
        const auto ids=button_identity(w.hwnd);
        check(ro_cd_uia_apply(w.hwnd,ids.data(),int(ids.size()),1,nullptr,0)==1,"native Agent UIA invoke");
        capture.expect(w.hwnd,1);
        POINT spot{240,105};ClientToScreen(w.hwnd,&spot);
        check(ro_cd_pointer(w.hwnd,spot.x,spot.y,0,0,1)==1,"native scoped pointer");
        int rw=GetSystemMetrics(SM_CXSCREEN),rh=GetSystemMetrics(SM_CYSCREEN);
        std::vector<unsigned char> pixels(rw*rh*4);
        check(ro_cd_snapshot(pixels.data(),int(pixels.size()),rw,rh)==1,"native Agent screenshot");
        std::cout << "Windows UIA, cross-owner rejection, stale reference, keyboard and three captured pixel states PASS\n";
        return 0;
    } catch(const std::exception& e) { std::cerr<<e.what()<<'\n'; return 1; }
}
