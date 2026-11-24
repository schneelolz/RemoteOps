#include "clipboard_internal.hpp"
#include <new>

namespace roclip {
class FileStream final : public IStream {
    std::atomic<ULONG> refs{1};
    std::shared_ptr<Channel> channel;
    uint64_t offer,size,position;
    uint32_t index;
public:
    FileStream(std::shared_ptr<Channel> ch,uint64_t id,uint32_t item,uint64_t bytes,uint64_t start=0):
        channel(std::move(ch)),offer(id),size(bytes),position(start),index(item){}
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid,void** result) override {
        if(!result)return E_POINTER;*result=nullptr;
        if(iid!=IID_IUnknown && iid!=IID_ISequentialStream && iid!=IID_IStream)return E_NOINTERFACE;
        *result=static_cast<IStream*>(this);AddRef();return S_OK;
    }
    ULONG STDMETHODCALLTYPE AddRef() override {return ++refs;}
    ULONG STDMETHODCALLTYPE Release() override {auto n=--refs;if(!n)delete this;return n;}
    HRESULT STDMETHODCALLTYPE Read(void* buffer,ULONG requested,ULONG* actual) override {
        ULONG done=0;if(actual)*actual=0;
        if(!buffer && requested)return STG_E_INVALIDPOINTER;
        try {
            while(done<requested && position<size) {
                if(!channel->live || channel->remoteOffer!=offer)return STG_E_REVERTED;
                auto want=static_cast<ULONG>(std::min<uint64_t>(size-position,std::min<ULONG>(BlockSize,requested-done)));
                ULONG read=0;
                auto hr=channel->read(offer,index,position,static_cast<BYTE*>(buffer)+done,want,read);
                if(FAILED(hr))return hr;
                done+=read;position+=read;if(actual)*actual=done;
            }
        } catch(...) {return E_OUTOFMEMORY;}
        return done==requested?S_OK:S_FALSE;
    }
    HRESULT STDMETHODCALLTYPE Write(const void*,ULONG,ULONG*) override {return STG_E_ACCESSDENIED;}
    HRESULT STDMETHODCALLTYPE Seek(LARGE_INTEGER d,DWORD origin,ULARGE_INTEGER* result) override {
        uint64_t base=origin==STREAM_SEEK_SET?0:origin==STREAM_SEEK_CUR?position:size;
        if(origin>STREAM_SEEK_END)return STG_E_INVALIDFUNCTION;
        if(d.QuadPart<0) {
            uint64_t back=0-static_cast<uint64_t>(d.QuadPart);
            if(back>base)return STG_E_INVALIDFUNCTION;position=base-back;
        }else{
            if(uint64_t(d.QuadPart)>UINT64_MAX-base)return STG_E_INVALIDFUNCTION;position=base+uint64_t(d.QuadPart);
        }
        if(result)result->QuadPart=position;return S_OK;
    }
    HRESULT STDMETHODCALLTYPE SetSize(ULARGE_INTEGER) override{return STG_E_ACCESSDENIED;}
    HRESULT STDMETHODCALLTYPE CopyTo(IStream* dst,ULARGE_INTEGER limit,ULARGE_INTEGER* read,ULARGE_INTEGER* written) override {
        if(!dst)return E_POINTER;
        BYTE buf[16384];uint64_t total=0;if(read)read->QuadPart=0;if(written)written->QuadPart=0;
        while(total<limit.QuadPart){
            ULONG n=0,w=0;auto hr=Read(buf,static_cast<ULONG>(std::min<uint64_t>(sizeof(buf),limit.QuadPart-total)),&n);
            if(FAILED(hr))return hr;if(!n)break;
            if(read)read->QuadPart+=n;hr=dst->Write(buf,n,&w);if(written)written->QuadPart+=w;
            if(FAILED(hr))return hr;if(w!=n)return STG_E_WRITEFAULT;total+=n;
        }
        return total==limit.QuadPart?S_OK:S_FALSE;
    }
    HRESULT STDMETHODCALLTYPE Commit(DWORD) override{return S_OK;}
    HRESULT STDMETHODCALLTYPE Revert() override{return E_NOTIMPL;}
    HRESULT STDMETHODCALLTYPE LockRegion(ULARGE_INTEGER,ULARGE_INTEGER,DWORD) override{return STG_E_INVALIDFUNCTION;}
    HRESULT STDMETHODCALLTYPE UnlockRegion(ULARGE_INTEGER,ULARGE_INTEGER,DWORD) override{return STG_E_INVALIDFUNCTION;}
    HRESULT STDMETHODCALLTYPE Stat(STATSTG* info,DWORD) override{
        if(!info)return E_POINTER;*info={};info->type=STGTY_STREAM;info->cbSize.QuadPart=size;info->grfMode=STGM_READ;return S_OK;
    }
    HRESULT STDMETHODCALLTYPE Clone(IStream** copy) override{
        if(!copy)return E_POINTER;*copy=new(std::nothrow)FileStream(channel,offer,index,size,position);
        return *copy?S_OK:E_OUTOFMEMORY;
    }
};

class FilesObject final : public IDataObject {
    std::atomic<ULONG> refs{1};
    std::shared_ptr<Channel> channel;
    uint64_t offer;
    std::vector<Entry> files;
    CLIPFORMAT descriptors=static_cast<CLIPFORMAT>(RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW));
    CLIPFORMAT contents=static_cast<CLIPFORMAT>(RegisterClipboardFormatW(CFSTR_FILECONTENTS));
    CLIPFORMAT effect=static_cast<CLIPFORMAT>(RegisterClipboardFormatW(CFSTR_PREFERREDDROPEFFECT));
public:
    FilesObject(std::shared_ptr<Channel> ch,uint64_t id,std::vector<Entry> entries):
        channel(std::move(ch)),offer(id),files(std::move(entries)){}
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid,void** result) override {
        if(!result)return E_POINTER;*result=nullptr;
        if(iid!=IID_IUnknown && iid!=IID_IDataObject)return E_NOINTERFACE;
        *result=static_cast<IDataObject*>(this);AddRef();return S_OK;
    }
    ULONG STDMETHODCALLTYPE AddRef() override{return ++refs;}
    ULONG STDMETHODCALLTYPE Release() override{auto count=--refs;if(!count)delete this;return count;}
    HRESULT STDMETHODCALLTYPE QueryGetData(FORMATETC* f) override {
        if(!f)return E_POINTER;
        if(!channel->live || channel->remoteOffer!=offer)return STG_E_REVERTED;
        if(f->dwAspect!=DVASPECT_CONTENT)return DV_E_DVASPECT;
        if(f->cfFormat==contents && (f->tymed&TYMED_ISTREAM)){
            return f->lindex==-1 || (f->lindex>=0 && size_t(f->lindex)<files.size())?S_OK:DV_E_LINDEX;
        }
        if((f->cfFormat==descriptors || f->cfFormat==effect) && (f->tymed&TYMED_HGLOBAL))return S_OK;
        return DV_E_FORMATETC;
    }
    HRESULT STDMETHODCALLTYPE GetData(FORMATETC* f,STGMEDIUM* medium) override {
        if(!medium)return E_POINTER;*medium={};auto hr=QueryGetData(f);if(FAILED(hr))return hr;
        if(f->cfFormat==contents) {
            if(f->lindex<0)return DV_E_LINDEX;
            auto stream=new(std::nothrow)FileStream(channel,offer,f->lindex,files[f->lindex].size);
            if(!stream)return E_OUTOFMEMORY;medium->tymed=TYMED_ISTREAM;medium->pstm=stream;return S_OK;
        }
        size_t n=f->cfFormat==effect?sizeof(DWORD):offsetof(FILEGROUPDESCRIPTORW,fgd)+files.size()*sizeof(FILEDESCRIPTORW);
        HGLOBAL h=GlobalAlloc(GMEM_MOVEABLE|GMEM_ZEROINIT,n);
        if(!h)return E_OUTOFMEMORY;
        void* mem=GlobalLock(h);if(!mem){GlobalFree(h);return E_OUTOFMEMORY;}
        if(f->cfFormat==effect)*static_cast<DWORD*>(mem)=DROPEFFECT_COPY;
        else {
            auto group=static_cast<FILEGROUPDESCRIPTORW*>(mem);group->cItems=static_cast<UINT>(files.size());
            for(size_t i=0;i<files.size();++i) {
                auto& d=group->fgd[i];d.dwFlags=FD_FILESIZE|FD_ATTRIBUTES|FD_PROGRESSUI|FD_UNICODE;
                d.dwFileAttributes=FILE_ATTRIBUTE_NORMAL;d.nFileSizeLow=DWORD(files[i].size);d.nFileSizeHigh=DWORD(files[i].size>>32);
                wcscpy_s(d.cFileName,files[i].name.c_str());
            }
        }
        GlobalUnlock(h);medium->tymed=TYMED_HGLOBAL;medium->hGlobal=h;return S_OK;
    }
    HRESULT STDMETHODCALLTYPE EnumFormatEtc(DWORD direction,IEnumFORMATETC** out) override {
        if(!out)return E_POINTER;*out=nullptr;if(direction!=DATADIR_GET)return E_NOTIMPL;
        FORMATETC formats[]={{descriptors,nullptr,DVASPECT_CONTENT,-1,TYMED_HGLOBAL},{contents,nullptr,DVASPECT_CONTENT,-1,TYMED_ISTREAM},{effect,nullptr,DVASPECT_CONTENT,-1,TYMED_HGLOBAL}};
        return SHCreateStdEnumFmtEtc(3,formats,out);
    }
    HRESULT STDMETHODCALLTYPE GetDataHere(FORMATETC*,STGMEDIUM*) override{return E_NOTIMPL;}
    HRESULT STDMETHODCALLTYPE GetCanonicalFormatEtc(FORMATETC*,FORMATETC* out) override {if(out)out->ptd=nullptr;return E_NOTIMPL;}
    HRESULT STDMETHODCALLTYPE SetData(FORMATETC*,STGMEDIUM*,BOOL) override{return E_NOTIMPL;}
    HRESULT STDMETHODCALLTYPE DAdvise(FORMATETC*,DWORD,IAdviseSink*,DWORD*) override{return OLE_E_ADVISENOTSUPPORTED;}
    HRESULT STDMETHODCALLTYPE DUnadvise(DWORD) override{return OLE_E_ADVISENOTSUPPORTED;}
    HRESULT STDMETHODCALLTYPE EnumDAdvise(IEnumSTATDATA**) override{return OLE_E_ADVISENOTSUPPORTED;}
};
IDataObject* makeFiles(std::shared_ptr<Channel> channel,uint64_t offer,std::vector<Entry> entries) {
    return new(std::nothrow)FilesObject(std::move(channel),offer,std::move(entries));
}
}
