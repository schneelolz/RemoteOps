#pragma once
// 用进程私有的数据槽替代系统唯一的剪贴板，避免改写正在使用本机的人的内容。
// COM 数据对象、Shell 的文件清单和 IStream、磁盘读写与消息处理均运行生产实现。
#include "../native/clipboard_internal.hpp"
namespace fixture {
inline std::recursive_mutex guard;
inline UINT format=0;inline HANDLE memory=nullptr;inline IDataObject* object=nullptr;
inline std::atomic<DWORD> sequence{1};
inline void clear(){
    if(memory){GlobalFree(memory);memory=nullptr;}
    if(object){object->Release();object=nullptr;}
    format=0;++sequence;
}
inline BOOL open(HWND){return guard.try_lock();}
inline BOOL close(){guard.unlock();return TRUE;}
inline BOOL empty(){clear();return TRUE;}
inline HANDLE set(UINT f,HANDLE h){memory=h;format=f;++sequence;return h;}
inline HANDLE get(UINT f){return f==format?memory:nullptr;}
inline DWORD seq(){return sequence.load();}
inline BOOL available(UINT f){
    std::lock_guard lock(guard);
    if(format==f)return TRUE;if(!object)return FALSE;
    FORMATETC fmt{static_cast<CLIPFORMAT>(f),nullptr,DVASPECT_CONTENT,-1,TYMED_HGLOBAL};
    return object->QueryGetData(&fmt)==S_OK;
}
inline HRESULT oleSet(IDataObject* obj){
    std::lock_guard lock(guard);if(obj)obj->AddRef();clear();object=obj;return S_OK;
}
inline HRESULT oleGet(IDataObject** obj){
    std::lock_guard lock(guard);*obj=object;if(!object)return DV_E_FORMATETC;object->AddRef();return S_OK;
}
inline HRESULT current(IDataObject* obj){std::lock_guard lock(guard);return object==obj?S_OK:S_FALSE;}
}
#define OpenClipboard fixture::open
#define CloseClipboard fixture::close
#define EmptyClipboard fixture::empty
#define SetClipboardData fixture::set
#define GetClipboardData fixture::get
#define GetClipboardSequenceNumber fixture::seq
#define IsClipboardFormatAvailable fixture::available
#define OleSetClipboard fixture::oleSet
#define OleGetClipboard fixture::oleGet
#define OleIsCurrentClipboard fixture::current
