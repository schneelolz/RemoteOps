// 此适配器与固定版本的 CrossDesk ScreenCapturerGdi 一起构建。
#include "screen_capturer_gdi.h"
#include "interactive_desktop.h"
// 固定版本的输入线程也与每条流同生共灭。其析构会等待已接收的事件结束。
#include "windows_input_injector.cpp"
#include "libyuv.h"
#include <spdlog/sinks/null_sink.h>
#include <chrono>
#include <cstring>
#include <memory>
#include <mutex>
#include <vector>

// 桌面故障由 Agent 当前流的日志出口上报，组件无需在应用工作目录建立日志。
namespace crossdesk {
std::shared_ptr<spdlog::logger> get_logger() {
    static const auto sink = std::make_shared<spdlog::sinks::null_sink_mt>();
    static const auto logger = std::make_shared<spdlog::logger>("remoteops-capture", sink);
    return logger;
}
}

namespace {
using Clock = std::chrono::steady_clock;
class Capture {
public:
    bool start() {
        if (!crossdesk::IsCurrentSessionUserDesktopActive()) return false;
        if (source_.Init(30, [this](unsigned char* data, int size, int w, int h, const char*, const MiniRtcNativeVideoFrame*) {
            if (!data || w < 2 || h < 2 || w > 8192 || h > 8192 || w*h > 16777216 ||
                (w & 1) || (h & 1) || size != w*h*3/2 ||
                !crossdesk::IsCurrentSessionUserDesktopActive()) return;
            try {
                std::lock_guard lock(mutex_);
                frame_.assign(data, data + size);
                width_ = w;
                height_ = h;
                captured_ = Clock::now();
            } catch (...) {}
        }) != 0) return false;
        return source_.Start(true) == 0;
    }

    ~Capture() { source_.Stop(); }

    bool send(const INPUT& event) {
        INPUT copy = event;
        return input_.Send(&copy, 1).sent == 1;
    }

    int read(unsigned char* bgra, int capacity, int native_w, int native_h, int w, int h) {
        if (!bgra || w < 2 || h < 2 || (w & 1) || (h & 1) || w > 8192 || h > 8192 || w*h > 16777216 ||
            ((w > 1920 || h > 1080) && (w != native_w || h != native_h)) || capacity < w*h*4) return -1;
        std::lock_guard lock(mutex_);
        if (frame_.empty() || width_ != (native_w & ~1) || height_ != (native_h & ~1) ||
            Clock::now() - captured_ > std::chrono::milliseconds(500)) return 0;
        const auto* y = frame_.data();
        const auto* uv = y + width_*height_;
        if (w != width_ || h != height_) {
            scratch_.resize(w*h*3/2);
            if (libyuv::NV12Scale(y, width_, uv, width_, width_, height_, scratch_.data(), w,
                                 scratch_.data()+w*h, w, w, h, libyuv::kFilterBilinear) != 0) return -1;
            y = scratch_.data();
            uv = y + w*h;
        }
        const int result = libyuv::NV12ToARGB(y, w, uv, w, bgra, w*4, w, h);
        frame_.clear();
        return result == 0 ? 1 : -1;
    }

private:
    std::mutex mutex_;
    std::vector<unsigned char> frame_, scratch_;
    Clock::time_point captured_{};
    int width_ = 0, height_ = 0;
    crossdesk::UserDesktopInputWorker input_;
    crossdesk::ScreenCapturerGdi source_;
};
}

extern "C" __declspec(dllexport) void* ro_cd_capture_open() noexcept {
    try {
        auto handle = std::make_unique<Capture>();
        return handle->start() ? handle.release() : nullptr;
    } catch (...) { return nullptr; }
}

extern "C" __declspec(dllexport) int ro_cd_capture_frame(void* p, unsigned char* out, int cap,
    int native_w, int native_h, int w, int h) noexcept {
    try { return p ? static_cast<Capture*>(p)->read(out,cap,native_w,native_h,w,h) : -1; }
    catch (...) { return -1; }
}

namespace {
bool current_target(HWND target) {
    return target && GetForegroundWindow()==target && crossdesk::IsCurrentSessionUserDesktopActive();
}
bool target_at(HWND target, POINT where) {
    const HWND hit=WindowFromPoint(where);
    return hit && (GetAncestor(hit,GA_ROOT)==target || GetAncestor(hit,GA_ROOTOWNER)==target);
}
bool move_to(crossdesk::UserDesktopInputWorker& worker, HWND target, POINT p) {
    if (!current_target(target) || !target_at(target,p)) return false;
    const LONG w=GetSystemMetrics(SM_CXVIRTUALSCREEN), h=GetSystemMetrics(SM_CYVIRTUALSCREEN);
    if(w<2 || h<2) return false;
    INPUT ev{}; ev.type=INPUT_MOUSE;
    ev.mi.dwFlags=MOUSEEVENTF_MOVE|MOUSEEVENTF_ABSOLUTE|MOUSEEVENTF_VIRTUALDESK;
    ev.mi.dx=MulDiv(p.x-GetSystemMetrics(SM_XVIRTUALSCREEN),65535,w-1);
    ev.mi.dy=MulDiv(p.y-GetSystemMetrics(SM_YVIRTUALSCREEN),65535,h-1);
    return worker.Send(&ev,1).sent==1;
}
}
extern "C" __declspec(dllexport) int ro_cd_pointer(HWND target,int x,int y,int ex,int ey,int op) noexcept {
    try {
        // 操作码由已获批的坐标请求映射，非当前目标、锁屏、跨窗口拖拽在现场拒绝。
        if(op<0 || op>7 || !current_target(target)) return 0;
        crossdesk::UserDesktopInputWorker worker;
        if(!move_to(worker,target,{x,y}))return 0;
        if(op==0)return 1;
        if(op==7 && !target_at(target,{ex,ey}))return 0;
        const int button=op==3 ? VK_RBUTTON : op==4 ? VK_MBUTTON : VK_LBUTTON;
        if(GetAsyncKeyState(button)&0x8000)return 0;
        const DWORD down=op==3?MOUSEEVENTF_RIGHTDOWN:op==4?MOUSEEVENTF_MIDDLEDOWN:MOUSEEVENTF_LEFTDOWN;
        const DWORD up=op==3?MOUSEEVENTF_RIGHTUP:op==4?MOUSEEVENTF_MIDDLEUP:MOUSEEVENTF_LEFTUP;
        for(int n=0;n<(op==2?2:1);++n) {
            POINT actual{};
            if(!current_target(target) || !GetCursorPos(&actual) || !target_at(target,actual))return 0;
            INPUT ev{}; ev.type=INPUT_MOUSE;
            if(op==5 || op==6) {
                ev.mi.dwFlags=MOUSEEVENTF_WHEEL;ev.mi.mouseData=op==5?WHEEL_DELTA:-WHEEL_DELTA;
                return worker.Send(&ev,1).sent==1;
            }
            ev.mi.dwFlags=down;
            if(worker.Send(&ev,1).sent!=1)return 0;
            bool ok=true;
            if(op==7) {
                for(int i=1;i<=8 && ok;++i) {
                    ok=move_to(worker,target,{x+MulDiv(ex-x,i,8),y+MulDiv(ey-y,i,8)});
                    Sleep(15);
                }
            }
            ev.mi.dwFlags=up;
            if(worker.Send(&ev,1).sent!=1 || !ok)return 0;
            if(op==2 && n==0)Sleep(80);
        }
        return 1;
    } catch (...) {return 0;}
}

extern "C" __declspec(dllexport) int ro_cd_snapshot(unsigned char* out, int cap, int w, int h) noexcept {
    try {
        if (w<2 || h<2 || w>8192 || h>8192 || w*h>16777216 || cap!=w*h*4) return 0;
        Capture capture;
        if (!capture.start()) return 0;
        const auto due = Clock::now()+std::chrono::seconds(2);
        do {
            const int got=capture.read(out,cap,w,h,w,h);
            if (got==1) return crossdesk::IsCurrentSessionUserDesktopActive() ? 1 : 0;
            if (got < 0) break;
            Sleep(10);
        } while (Clock::now()<due);
    } catch (...) {}
    return 0;
}

extern "C" __declspec(dllexport) int ro_cd_send_input(void* p, const INPUT* event) noexcept {
    try { return p && event && static_cast<Capture*>(p)->send(*event) ? 1 : 0; }
    catch (...) { return 0; }
}

extern "C" __declspec(dllexport) void ro_cd_capture_close(void* p) noexcept {
    delete static_cast<Capture*>(p);
}
