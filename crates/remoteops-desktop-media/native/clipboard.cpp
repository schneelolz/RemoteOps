#include "clipboard_internal.hpp"
#include <shellapi.h>

namespace roclip {
struct FileHandle {
    HANDLE value=INVALID_HANDLE_VALUE;
    explicit FileHandle(HANDLE h):value(h){}
    ~FileHandle(){if(value!=INVALID_HANDLE_VALUE)CloseHandle(value);}
    FileHandle(FileHandle&& old) noexcept:value(old.value){old.value=INVALID_HANDLE_VALUE;}
    FileHandle(const FileHandle&)=delete;
};
struct ClipboardLock {
    bool held;
    explicit ClipboardLock(HWND owner):held(OpenClipboard(owner)!=FALSE){}
    ~ClipboardLock(){if(held)CloseClipboard();}
};
class ClipboardWorker {
    std::shared_ptr<Channel> ch;
    HWND owner;
    DWORD seen;
    uint64_t selection=0;
    std::vector<FileHandle> handles;
    IDataObject* object=nullptr;
    void clearObject() {
        if(object){if(OleIsCurrentClipboard(object)==S_OK)OleSetClipboard(nullptr);object->Release();object=nullptr;}
        ch->remoteOffer=0;ch->changed.notify_all();
    }
    // 只为复制操作真正选中的本地普通文件保留只读句柄；对端只能按索引和区间请求。
    Bytes selectionPacket(HDROP drop) {
        UINT count=DragQueryFileW(drop,0xffffffff,nullptr,0);
        if(count==0 || count>128)return {Clear};
        std::vector<FileHandle> next;
        Bytes packet{Files};put(packet,++selection);put<uint32_t>(packet,count);
        for(UINT i=0;i<count;++i){
            UINT n=DragQueryFileW(drop,i,nullptr,0);
            if(n<3 || n>32767)return {Clear};
            std::wstring path(n+1,0);
            if(DragQueryFileW(drop,i,path.data(),n+1)!=n)return {Clear};
            path.resize(n);
            if(path.size()<3 || path[1]!=L':' || path[2]!=L'\\')return {Clear};
            auto root=path.substr(0,3);
            auto drive=GetDriveTypeW(root.c_str());
            if(drive!=DRIVE_FIXED && drive!=DRIVE_REMOVABLE && drive!=DRIVE_RAMDISK)return {Clear};
            auto name=path.substr(path.find_last_of(L'\\')+1);
            if(!safeName(name))return {Clear};
            FileHandle h(CreateFileW(path.c_str(),GENERIC_READ,FILE_SHARE_READ,nullptr,OPEN_EXISTING,
                                      FILE_FLAG_OPEN_REPARSE_POINT|FILE_FLAG_RANDOM_ACCESS,nullptr));
            BY_HANDLE_FILE_INFORMATION info{};
            if(h.value==INVALID_HANDLE_VALUE || GetFileType(h.value)!=FILE_TYPE_DISK ||
               !GetFileInformationByHandle(h.value,&info) ||
               (info.dwFileAttributes&(FILE_ATTRIBUTE_DIRECTORY|FILE_ATTRIBUTE_REPARSE_POINT)))return {Clear};
            put<uint64_t>(packet,(uint64_t(info.nFileSizeHigh)<<32)|info.nFileSizeLow);
            putString(packet,utf8(name));next.push_back(std::move(h));
        }
        handles=std::move(next);return packet;
    }
    void sample() {
        auto seq=GetClipboardSequenceNumber();if(seq==seen || !ch->live)return;
        if(object && OleIsCurrentClipboard(object)==S_OK){seen=seq;return;}
        ClipboardLock lock(owner);if(!lock.held)return;
        auto current=GetClipboardSequenceNumber();
        Bytes packet{Clear};handles.clear();
        if(IsClipboardFormatAvailable(CF_HDROP)){
            auto drop=static_cast<HDROP>(GetClipboardData(CF_HDROP));
            if(drop)packet=selectionPacket(drop);
        }else if(IsClipboardFormatAvailable(CF_UNICODETEXT)){
            auto h=GetClipboardData(CF_UNICODETEXT);
            size_t bytes=h?GlobalSize(h):0;
            if(bytes>=sizeof(wchar_t) && bytes<=MaxPacket*2) {
                auto mem=static_cast<const wchar_t*>(GlobalLock(h));
                if(mem){
                    std::wstring w;
                    try{w.assign(mem,wcsnlen_s(mem,bytes/sizeof(wchar_t)));}catch(...){GlobalUnlock(h);throw;}
                    GlobalUnlock(h);
                    auto s=utf8(w);
                    if(s.size()<=MaxPacket-5){packet={Text};putString(packet,s);}
                }
            }
        }
        ch->remoteOffer=0;ch->changed.notify_all();
        if(ch->send(std::move(packet)))seen=current;
    }
    void serve(Reader& r) {
        auto offer=r.number<uint64_t>();auto index=r.number<uint32_t>();
        auto at=r.number<uint64_t>();auto req=r.number<uint64_t>();auto length=r.number<uint32_t>();
        if(r.at!=r.b.size() || length>BlockSize || !length)return;
        Bytes reply{Block};put(reply,req);reply.push_back(0);
        if(offer==selection && index<handles.size() && GetClipboardSequenceNumber()==seen) {
            LARGE_INTEGER offset{};offset.QuadPart=static_cast<LONGLONG>(at);
            if(offset.QuadPart>=0 && SetFilePointerEx(handles[index].value,offset,nullptr,FILE_BEGIN)){
                reply.resize(10+length);DWORD read=0;
                if(ReadFile(handles[index].value,reply.data()+10,length,&read,nullptr) && read==length)reply[9]=1;
                else reply.resize(10);
            }
        }
        ch->send(std::move(reply));
    }
    bool apply(const Bytes& b) {
        Reader r{b};
        if(b[0]==Pull){serve(r);return true;}
        if(b[0]==Clear){clearObject();return true;}
        if(b[0]==Text){
            auto w=wide(r.string());if(r.at!=b.size() || w.find(L'\0')!=w.npos)return true;
            ClipboardLock lock(owner);if(!lock.held)return false;
            const auto bytes=(w.size()+1)*sizeof(wchar_t);
            HGLOBAL h=GlobalAlloc(GMEM_MOVEABLE,bytes);if(!h)return false;
            void* p=GlobalLock(h);if(!p){GlobalFree(h);return false;}
            memcpy(p,w.c_str(),bytes);GlobalUnlock(h);
            if(!ch->live || !EmptyClipboard() || !SetClipboardData(CF_UNICODETEXT,h)){GlobalFree(h);return false;}
            ch->remoteOffer=0;ch->changed.notify_all();handles.clear();seen=GetClipboardSequenceNumber();
            return true;
        }
        if(b[0]!=Files)return true;
        const auto id=r.number<uint64_t>();const auto n=r.number<uint32_t>();
        if(!id || !n || n>128)return true;
        std::vector<Entry> entries;
        for(uint32_t i=0;i<n;++i){
            auto size=r.number<uint64_t>();auto name=wide(r.string());
            if(!safeName(name) || size>INT64_MAX)return true;
            entries.push_back({std::move(name),size});
        }
        if(r.at!=b.size())return true;
        auto* next=makeFiles(ch,id,std::move(entries));if(!next)return false;
        auto previous=ch->remoteOffer.exchange(id);ch->changed.notify_all();
        if(!ch->live || FAILED(OleSetClipboard(next))){ch->remoteOffer=previous;next->Release();return false;}
        if(object)object->Release();object=next;
        handles.clear();seen=GetClipboardSequenceNumber();return true;
    }
public:
    ClipboardWorker(std::shared_ptr<Channel> state,HWND window):ch(std::move(state)),owner(window),seen(GetClipboardSequenceNumber()){}
    ~ClipboardWorker(){clearObject();}
    void run() {
        Bytes pending;unsigned failures=0;
        while(ch->live){
            MSG msg{};
            for(int n=0;n<32 && PeekMessageW(&msg,nullptr,0,0,PM_REMOVE);++n){TranslateMessage(&msg);DispatchMessageW(&msg);}
            if(!ch->live)break;
            if(pending.empty()){
                std::lock_guard lock(ch->mutex);
                if(!ch->incoming.empty()){pending=std::move(ch->incoming.front());ch->incoming.pop_front();failures=0;}
            }
            try {
                if(!pending.empty() && (apply(pending) || ++failures>=12))pending.clear();
                if(ch->live)sample();
            }catch(const std::invalid_argument&){pending.clear();seen=GetClipboardSequenceNumber();handles.clear();}
            MsgWaitForMultipleObjectsEx(1,&ch->wake,pending.empty()?18:12,QS_ALLINPUT,MWMO_INPUTAVAILABLE);
        }
    }
};
using Shared=std::shared_ptr<Channel>;
}
using namespace roclip;
extern "C" void* ro_clip_new() noexcept {
    try{
        auto ch=std::make_shared<Channel>();if(!ch->wake)return nullptr;
        auto result=std::make_unique<Shared>(ch);
        std::thread([ch]{
            if(FAILED(OleInitialize(nullptr))){ch->cancel();return;}
            auto hwnd=CreateWindowExW(0,L"STATIC",L"",0,0,0,0,0,HWND_MESSAGE,nullptr,GetModuleHandleW(nullptr),nullptr);
            if(hwnd){
                try{ClipboardWorker worker(ch,hwnd);worker.run();}catch(...){}
                DestroyWindow(hwnd);
            }
            ch->cancel();OleUninitialize();
        }).detach();
        return result.release();
    }catch(...){return nullptr;}
}
extern "C" void ro_clip_free(void* raw) noexcept {
    if(raw){auto ch=static_cast<Shared*>(raw);(*ch)->cancel();delete ch;}
}
extern "C" int ro_clip_take(void* raw,unsigned char* out,int cap) noexcept {
    if(!raw || !out || cap<=0)return -1;
    auto& ch=**static_cast<Shared*>(raw);
    std::lock_guard lock(ch.mutex);
    if(!ch.live)return -1;
    if(ch.outgoing.empty())return 0;
    auto& b=ch.outgoing.front();
    if(b.size()>size_t(cap))return -1;
    int n=static_cast<int>(b.size());memcpy(out,b.data(),b.size());ch.outgoing.pop_front();return n;
}
extern "C" int ro_clip_push(void* raw,const unsigned char* data,int n) noexcept {
    if(!raw || !data || n<1 || n>int(MaxPacket))return -1;
    try{return (**static_cast<Shared*>(raw)).receive(Bytes(data,data+n))?0:-1;}catch(...){return -1;}
}
