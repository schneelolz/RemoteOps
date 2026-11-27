#pragma once
#include <windows.h>
#include <shlobj.h>
#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstring>
#include <deque>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

namespace roclip {
using Bytes = std::vector<unsigned char>;
constexpr size_t MaxPacket = 256 * 1024;
constexpr ULONG BlockSize = 64 * 1024;
enum Kind : unsigned char { Text=1, Files, Pull, Block, Clear, Hello, Update, ImageStart, ImageChunk };
struct Entry {std::wstring name; uint64_t size;};

// 私有线格式为固定宽度小端整数和带长度的 UTF-8 字符串。
template<class T> void put(Bytes& b,T n) {
    for(size_t i=0;i<sizeof(T);++i) b.push_back(static_cast<unsigned char>(n>>(8*i)));
}
struct Reader {
    const Bytes& b;
    size_t at=1;
    template<class T> T number() {
        if(at>b.size() || sizeof(T)>b.size()-at) throw std::invalid_argument("short packet");
        T value=0;
        for(size_t i=0;i<sizeof(T);++i) value|=static_cast<T>(b[at++])<<(8*i);
        return value;
    }
    std::string string() {
        auto n=number<uint32_t>();
        if(n>b.size()-at) throw std::invalid_argument("short string");
        std::string result(reinterpret_cast<const char*>(b.data()+at),n);
        at+=n;
        return result;
    }
};
inline std::wstring wide(const std::string& s) {
    if(s.empty())return {};
    int n=MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,s.data(),int(s.size()),nullptr,0);
    if(n<=0)throw std::invalid_argument("invalid UTF-8");
    std::wstring w(n,0);
    MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,s.data(),int(s.size()),w.data(),n);
    return w;
}
inline std::string utf8(const std::wstring& w) {
    if(w.empty())return {};
    int n=WideCharToMultiByte(CP_UTF8,WC_ERR_INVALID_CHARS,w.data(),int(w.size()),nullptr,0,nullptr,nullptr);
    if(n<=0)throw std::invalid_argument("invalid UTF-16");
    std::string s(n,0);
    WideCharToMultiByte(CP_UTF8,WC_ERR_INVALID_CHARS,w.data(),int(w.size()),s.data(),n,nullptr,nullptr);
    return s;
}
inline void putString(Bytes& b,const std::string& s) {
    put<uint32_t>(b,static_cast<uint32_t>(s.size())); b.insert(b.end(),s.begin(),s.end());
}
inline bool safeName(const std::wstring& s) {
    if(s.empty() || s.size()>240 || s.back()==L'.' || s.back()==L' ' ||
       s.find_first_of(L"\\/:*?\"<>|")!=s.npos ||
       std::any_of(s.begin(),s.end(),[](wchar_t c){return c<32;}))return false;
    auto stem=s.substr(0,s.find(L'.'));
    while(!stem.empty() && stem.back()==L' ')stem.pop_back();
    CharUpperBuffW(stem.data(),static_cast<DWORD>(stem.size()));
    if(stem==L"CON" || stem==L"PRN" || stem==L"AUX" || stem==L"NUL" || stem==L"CONIN$" || stem==L"CONOUT$")return false;
    return !(stem.size()==4 && (stem.substr(0,3)==L"COM" || stem.substr(0,3)==L"LPT") &&
        std::wstring(L"0123456789\u00b9\u00b2\u00b3").find(stem[3])!=std::wstring::npos);
}
struct Channel {
    std::mutex mutex;
    std::condition_variable changed;
    std::atomic<bool> live{true};
    std::atomic<uint64_t> remoteOffer{0};
    uint64_t serial=0;
    std::deque<Bytes> outgoing,incoming;
    std::map<uint64_t,std::optional<Bytes>> pending;
    HANDLE wake=CreateEventW(nullptr,FALSE,FALSE,nullptr);
    ~Channel(){if(wake)CloseHandle(wake);}
    void cancel() {
        std::lock_guard guard(mutex); live=false;remoteOffer=0;outgoing.clear();incoming.clear();
        changed.notify_all();SetEvent(wake);
    }
    bool send(Bytes b) {
        std::lock_guard guard(mutex);
        if(!live || outgoing.size()>=16 || b.size()>MaxPacket)return false;
        outgoing.push_back(std::move(b));return true;
    }
    bool receive(Bytes b) {
        if(b.empty() || b.size()>MaxPacket)return false;
        std::lock_guard guard(mutex);
        if(!live)return false;
        if(b.front()==Block) {
            Reader r{b};
            const auto req=r.number<uint64_t>();
            auto it=pending.find(req);
            if(it!=pending.end() && !it->second) {it->second=std::move(b);changed.notify_all();}
        } else {
            if(incoming.size()>=16)return false;
            incoming.push_back(std::move(b));SetEvent(wake);
        }
        return true;
    }
    HRESULT read(uint64_t offer,uint32_t index,uint64_t offset,void* dest,ULONG length,ULONG& got) {
        std::unique_lock guard(mutex);
        if(!live || remoteOffer!=offer || pending.size()>=16 || outgoing.size()>=16)return STG_E_REVERTED;
        const auto req=++serial;
        Bytes b{Pull};put(b,offer);put(b,index);put(b,offset);put(b,req);put<uint32_t>(b,length);
        auto it=pending.emplace(req,std::nullopt).first;
        outgoing.push_back(std::move(b));
        changed.wait_for(guard,std::chrono::seconds(12),[&]{return !live || remoteOffer!=offer || it->second.has_value();});
        auto reply=std::move(it->second);
        pending.erase(it);
        if(!live || remoteOffer!=offer || !reply || reply->size()<10 || (*reply)[9]!=1 || reply->size()-10!=length)return STG_E_READFAULT;
        memcpy(dest,reply->data()+10,length);got=length;
        return S_OK;
    }
};
IDataObject* makeFiles(std::shared_ptr<Channel> channel,uint64_t offer,std::vector<Entry> entries);
}
