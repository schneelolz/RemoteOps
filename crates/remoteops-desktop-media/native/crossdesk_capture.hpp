#pragma once

#include <windows.h>

// 由 Rust 校验构建摘要并锁住映像后载入。每条桌面流单独持有采集线程。
class CrossDeskCapture {
public:
    CrossDeskCapture() = default;
    CrossDeskCapture(const CrossDeskCapture&) = delete;
    CrossDeskCapture& operator=(const CrossDeskCapture&) = delete;
    ~CrossDeskCapture() {
        if (source_) close_(source_);
        if (module_) FreeLibrary(module_);
    }
    bool Open(const wchar_t* path) noexcept {
        if (module_ || !path) return false;
        module_ = LoadLibraryExW(path, nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32);
        if (!module_) return false;
        const auto open = reinterpret_cast<OpenFn>(GetProcAddress(module_, "ro_cd_capture_open"));
        frame_ = reinterpret_cast<ReadFn>(GetProcAddress(module_, "ro_cd_capture_frame"));
        close_ = reinterpret_cast<CloseFn>(GetProcAddress(module_, "ro_cd_capture_close"));
        send_ = reinterpret_cast<SendFn>(GetProcAddress(module_, "ro_cd_send_input"));
        if (!open || !frame_ || !close_ || !send_) return false;
        source_ = open();
        return source_ != nullptr;
    }
    bool Send(const INPUT& input) noexcept { return send_(source_, &input) == 1; }
    bool Active() const noexcept { return source_ != nullptr; }
    int Read(BYTE* out, int cap, int width, int height, int targetW, int targetH) noexcept {
        return frame_(source_,out,cap,width,height,targetW,targetH);
    }
private:
    using SendFn = int (*)(void*, const INPUT*);
    SendFn send_ = nullptr;
    using OpenFn = void* (*)();
    using ReadFn = int (*)(void*, unsigned char*, int, int, int, int, int);
    using CloseFn = void (*)(void*);
    HMODULE module_ = nullptr;
    void* source_ = nullptr;
    ReadFn frame_ = nullptr;
    CloseFn close_ = nullptr;
};
