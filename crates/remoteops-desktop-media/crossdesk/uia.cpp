// 复用所固定上游版本的 Windows UIA 实现。由 Agent 已审批的图形操作调用。
#include "interactive_desktop.h"
#include "../apps/desktop/src/platform/windows/desktop_automation.cpp"
#include <limits>

namespace {
using Microsoft::WRL::ComPtr;
bool current(HWND window) {
    return window && window == GetForegroundWindow() && crossdesk::IsCurrentSessionUserDesktopActive();
}

// 对照 RemoteOps 刚按窗口指纹、控件指纹找到的 UIA RuntimeId。
bool matches(IUIAutomationElement* element, const int* numbers, int count) {
    SAFEARRAY* values = nullptr;
    if (FAILED(element->GetRuntimeId(&values)) || !values) return false;
    LONG start=0, end=-1;
    bool equal=SafeArrayGetDim(values)==1 && SUCCEEDED(SafeArrayGetLBound(values,1,&start)) &&
        SUCCEEDED(SafeArrayGetUBound(values,1,&end)) && end>=start && end-start+1==count;
    for (int i=0;equal && i<count;++i) {
        LONG at=start+i, value=0;
        equal=SUCCEEDED(SafeArrayGetElement(values,&at,&value)) && value==numbers[i];
    }
    SafeArrayDestroy(values);
    return equal;
}
}

extern "C" __declspec(dllexport) int ro_cd_uia_tree(HWND window, char* output, int capacity) noexcept {
    try {
        if (!current(window) || !output || capacity<2 || capacity>65536) return -1;
        auto snapshot=crossdesk::WindowTree(window,128,8);
        auto elements=nlohmann::json::array();
        for (auto& item:snapshot.items) {
            // 与现有观察策略一致，只交付结构。编辑框当前值不会随快照外发。
            item.data.erase("value");
            elements.push_back(std::move(item.data));
        }
        auto text=nlohmann::json{{"elements",std::move(elements)},{"truncated",snapshot.truncated}}.dump();
        if (!current(window) || text.size()>=static_cast<size_t>(capacity)) return -1;
        std::memcpy(output,text.data(),text.size());
        return static_cast<int>(text.size());
    } catch (...) { return -1; }
}

extern "C" __declspec(dllexport) int ro_cd_uia_apply(HWND window, const int* runtime, int count,
        int action, const char* text, int size) noexcept {
    try {
        if (!current(window) || !runtime || count<1 || count>128 || (action!=1 && action!=2) ||
            size<0 || size>16384 || (size && !text)) return 0;
        auto automation=crossdesk::Automation();
        ComPtr<IUIAutomationElement> root;
        ComPtr<IUIAutomationTreeWalker> walker;
        if (FAILED(automation->ElementFromHandle(window,&root)) || !root ||
            FAILED(automation->get_RawViewWalker(&walker)) || !walker) return 0;
        std::deque<ComPtr<IUIAutomationElement>> queue{root};
        auto due=std::chrono::steady_clock::now()+std::chrono::milliseconds(1200);
        for (int visited=0; !queue.empty() && visited<512 && std::chrono::steady_clock::now()<due; ++visited) {
            auto element=std::move(queue.front()); queue.pop_front();
            if (matches(element.Get(),runtime,count)) {
                BOOL secure=TRUE,enabled=FALSE;
                if (FAILED(element->get_CurrentIsPassword(&secure)) || secure ||
                    FAILED(element->get_CurrentIsEnabled(&enabled)) || !enabled) return 0;
                if (action==1) {
                    ComPtr<IUIAutomationInvokePattern> invoke;
                    if (FAILED(element->GetCurrentPatternAs(UIA_InvokePatternId,IID_PPV_ARGS(&invoke))) || !invoke) return 0;
                } else {
                    BOOL read_only=TRUE;
                    ComPtr<IUIAutomationValuePattern> value;
                    if (FAILED(element->GetCurrentPatternAs(UIA_ValuePatternId,IID_PPV_ARGS(&value))) || !value ||
                        FAILED(value->get_CurrentIsReadOnly(&read_only)) || read_only) return 0;
                }
                if (!current(window)) return 0;
                return crossdesk::Perform(element,action==1?"press":"set_value",
                    size?std::string(text,size):std::string()).value("accepted",false) ? 1 : 0;
            }
            ComPtr<IUIAutomationElement> child;
            walker->GetFirstChildElement(element.Get(),&child);
            while (child && visited+queue.size()<512 && std::chrono::steady_clock::now()<due) {
                queue.push_back(child);
                ComPtr<IUIAutomationElement> next;
                walker->GetNextSiblingElement(child.Get(),&next);child=std::move(next);
            }
        }
    } catch (...) {}
    return 0;
}
