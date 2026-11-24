// Windows 交互剪贴板验收。所有操作必须在测试进程的私有窗口站内执行。
#include "../native/clipboard_internal.hpp"
#include <shellapi.h>
#include <filesystem>
#include <fstream>
#include <cstdio>
#ifdef RO_CLIPBOARD_FIXTURE
#include "clipboard_fixture.hpp"
#endif
using namespace roclip;
extern "C" void* ro_clip_new();
extern "C" void ro_clip_free(void*);
extern "C" int ro_clip_take(void*,unsigned char*,int);
extern "C" int ro_clip_push(void*,const unsigned char*,int);
static void require(bool ok,const char* name){if(!ok)throw std::runtime_error(name);}
static void tick(){
    MSG msg{};while(PeekMessageW(&msg,nullptr,0,0,PM_REMOVE)){TranslateMessage(&msg);DispatchMessageW(&msg);}
    Sleep(5);
}
template<class F> static void until(F check,const char* name) {
    auto start=GetTickCount64();while(!check()){require(GetTickCount64()-start<7000,name);tick();}
}
static Bytes take(void* ch){
    Bytes b(MaxPacket);int n=ro_clip_take(ch,b.data(),int(b.size()));require(n>=0,"native worker");b.resize(n);return b;
}
static Bytes next(void* ch,Kind kind){
    Bytes b;until([&]{b=take(ch);return !b.empty() && b[0]==kind;},"outgoing clipboard event");return b;
}
static void submit(void* ch,const Bytes& b){require(ro_clip_push(ch,b.data(),int(b.size()))==0,"push");}
static void publish(HWND window,UINT format,const void* data,size_t size) {
    until([&]{return !!OpenClipboard(window);},"open clipboard");require(EmptyClipboard(),"empty clipboard");
    auto h=GlobalAlloc(GMEM_MOVEABLE,size);auto p=GlobalLock(h);memcpy(p,data,size);GlobalUnlock(h);
    require(SetClipboardData(format,h)!=nullptr,"set clipboard");CloseClipboard();
}
int wmain(){
    try {
        auto name=L"RemoteOpsClipboardTest"+std::to_wstring(GetCurrentProcessId());
#ifndef RO_CLIPBOARD_FIXTURE
        auto station=CreateWindowStationW(name.c_str(),0,MAXIMUM_ALLOWED,nullptr);
        require(station!=nullptr,"create private window station");
        require(SetProcessWindowStation(station),"select private window station");
        auto desktop=CreateDesktopW(L"Default",nullptr,nullptr,0,MAXIMUM_ALLOWED,nullptr);
        require(desktop && SetThreadDesktop(desktop),"private desktop");
        #endif
        require(SUCCEEDED(OleInitialize(nullptr)),"private OLE");
        auto owner=CreateWindowExW(0,L"STATIC",L"copy button",0,0,0,0,0,HWND_MESSAGE,nullptr,GetModuleHandleW(nullptr),nullptr);
        require(owner!=nullptr,"private clipboard owner");
        void* ch=ro_clip_new();require(ch!=nullptr,"clipboard worker");for(int i=0;i<50;++i)tick();
        const wchar_t local[]=L"测试文本\r\n按钮复制";
        publish(owner,CF_UNICODETEXT,local,sizeof(local));
        auto event=next(ch,Text);Reader src{event};require(wide(src.string())==local,"button copy");
        Bytes text{Text};putString(text,utf8(L"远端文本\r\nsecond line"));submit(ch,text);
        until([&]{
            if(!OpenClipboard(owner))return false;auto h=GetClipboardData(CF_UNICODETEXT);
            auto w=h?static_cast<const wchar_t*>(GlobalLock(h)):nullptr;bool ok=w && wcscmp(w,L"远端文本\r\nsecond line")==0;
            if(w)GlobalUnlock(h);CloseClipboard();return ok;
        },"paste unicode");
        for(int i=0;i<30;++i)tick();require(take(ch).empty(),"text feedback loop");
        puts("PASS Unicode text both ways, programmatic Copy, no echo; Windows clipboard API");

        const auto path=std::filesystem::temp_directory_path()/(name+L".bin");std::ofstream(path,std::ios::binary)<<"file contents";
        auto paths=path.wstring();paths.push_back(0);paths.push_back(0);
        Bytes drop(sizeof(DROPFILES)+paths.size()*sizeof(wchar_t));DROPFILES header{};header.pFiles=sizeof(header);header.fWide=TRUE;
        memcpy(drop.data(),&header,sizeof(header));memcpy(drop.data()+sizeof(header),paths.data(),paths.size()*sizeof(wchar_t));
        publish(owner,CF_HDROP,drop.data(),drop.size());
        auto offer=next(ch,Files);Reader files{offer};const auto id=files.number<uint64_t>();
        require(files.number<uint32_t>()==1 && files.number<uint64_t>()==13 && wide(files.string())==path.filename(),"local file metadata");
        Bytes request{Pull};put(request,id);put<uint32_t>(request,0);put<uint64_t>(request,5);put<uint64_t>(request,97);put<uint32_t>(request,8);
        submit(ch,request);auto block=next(ch,Block);require(block.size()==18 && block[9]==1 && memcmp(block.data()+10,"contents",8)==0,"range read");

        Bytes manifest{Files};put<uint64_t>(manifest,88);put<uint32_t>(manifest,2);
        for(auto file:{L"数据.bin",L"空文件.txt"}){put<uint64_t>(manifest,file[0]==L'空'?0:160000);putString(manifest,utf8(file));}
        submit(ch,manifest);
        auto descriptor=static_cast<CLIPFORMAT>(RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW));
        auto content=static_cast<CLIPFORMAT>(RegisterClipboardFormatW(CFSTR_FILECONTENTS));
        until([&]{return !!IsClipboardFormatAvailable(descriptor);},"virtual files on clipboard");
        IDataObject* object=nullptr;require(SUCCEEDED(OleGetClipboard(&object)),"OLE clipboard");
        FORMATETC fmt{descriptor,nullptr,DVASPECT_CONTENT,-1,TYMED_HGLOBAL};STGMEDIUM m{};
        require(SUCCEEDED(object->GetData(&fmt,&m)),"group descriptor");
        auto group=static_cast<FILEGROUPDESCRIPTORW*>(GlobalLock(m.hGlobal));
        require(group && group->cItems==2 && wcscmp(group->fgd[1].cFileName,L"空文件.txt")==0,"multi-file names");
        GlobalUnlock(m.hGlobal);ReleaseStgMedium(&m);require(take(ch).empty(),"lazy content");
        fmt={content,nullptr,DVASPECT_CONTENT,0,TYMED_ISTREAM};
        require(SUCCEEDED(object->GetData(&fmt,&m)),"OLE file stream");
        std::atomic<bool> stop{false};
        std::thread peer([&]{
            while(!stop){
                auto b=take(ch);if(b.empty()){Sleep(1);continue;}
                if(b[0]!=Pull)continue;Reader r{b};auto offered=r.number<uint64_t>();auto index=r.number<uint32_t>();auto off=r.number<uint64_t>();
                auto req=r.number<uint64_t>();auto n=r.number<uint32_t>();require(offered==88 && index==0 && n<=BlockSize,"scoped pull");
                Bytes data{Block};put(data,req);data.push_back(1);for(uint32_t i=0;i<n;++i)data.push_back(static_cast<BYTE>((off+i)%251));
                submit(ch,data);
            }
        });
        Bytes data(160000);ULONG read=0;auto hr=m.pstm->Read(data.data(),static_cast<ULONG>(data.size()),&read);
        stop=true;peer.join();require(SUCCEEDED(hr) && read==data.size(),"stream all blocks");
        for(size_t i=0;i<data.size();++i)require(data[i]==i%251,"content match");
        puts("PASS file selected by Explorer format; virtual multi-file paste on demand; exact bytes across chunk boundaries");
        LARGE_INTEGER zero{};m.pstm->Seek(zero,STREAM_SEEK_SET,nullptr);
        ro_clip_free(ch);
        hr=m.pstm->Read(data.data(),1,&read);require(FAILED(hr),"revoked stream blocked");
        ReleaseStgMedium(&m);object->Release();
        for(int i=0;i<20;++i)tick();
        std::filesystem::remove(path);
        puts("PASS revocation invalidates existing file streams");return 0;
    }catch(const std::exception& e){fprintf(stderr,"FAIL %s (Win32 %lu)\n",e.what(),GetLastError());return 1;}
}
