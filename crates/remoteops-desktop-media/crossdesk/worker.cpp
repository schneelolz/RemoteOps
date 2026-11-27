// 每个已配对桌面流独占进程。媒体组件通过匿名管道与父进程交换消息。
#include "host_peer.hpp"
#include "libyuv.h"
#include <fcntl.h>
#include <io.h>
#include <windows.h>
#include <chrono>
#include <vector>
using namespace std::chrono_literals;
using json = nlohmann::json;
using Bytes = std::vector<uint8_t>;
using Clock = std::chrono::steady_clock;

namespace {
struct Packet { char type; Bytes data; };
constexpr uint32_t max_frame = 1920*1080*4+8;
Packet read_packet() {
    const int type = fgetc(stdin);
    uint32_t size = 0;
    if (type == EOF || fread(&size,4,1,stdin)!=1 ||
        size > (type=='F' ? max_frame : 4096)) throw std::runtime_error("invalid IPC header");
    Packet p{static_cast<char>(type), Bytes(size)};
    if (size && fread(p.data.data(),1,size,stdin)!=size) throw std::runtime_error("closed IPC");
    return p;
}
bool waiting() {
    DWORD n=0;
    if (!PeekNamedPipe(GetStdHandle(STD_INPUT_HANDLE),nullptr,0,nullptr,&n,nullptr))
        throw std::runtime_error("parent closed");
    return n!=0;
}
void write_packet(FILE* output,char type,const void* data=nullptr,uint32_t size=0) {
    if (fputc(type,output)==EOF || fwrite(&size,4,1,output)!=1 ||
        (size && fwrite(data,1,size,output)!=size) || fflush(output)) throw std::runtime_error("closed IPC");
}
bool valid_room(const std::string& room) {
    return room.size()==16 && room[9]=='@' &&
        room.find_first_not_of("0123456789") == 9 &&
        room.find_first_not_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",10)==std::string::npos;
}

void serve(FILE* output,const json& options,const std::string& address,int port) {
    const auto ticket=options.at("ticket").get<std::string>();
    if (ticket.size()!=36) throw std::runtime_error("invalid ticket");
    const bool sender=options.at("sender").get<bool>();
    const auto turn=options.value("force_turn", false);
    auto last_heartbeat=Clock::now();
    std::unique_ptr<remoteops::HostPeer> host;
    crossdesk::ControllerSession viewer;
    bool offered=false, joined=false, claimed=false, announced=false;
    uint64_t claim_request=0;
    if (sender) {
        host=std::make_unique<remoteops::HostPeer>(ticket);
        host->start(address,port,turn ? MiniRtcTurnForceUdp : MiniRtcTurnAutoUdpTcp);
    }
    Bytes nv12;
    while (Clock::now()-last_heartbeat<5s) {
        if (host) {
            if (host->disconnected) break;
            std::string room;
            if (!offered) { std::lock_guard lock(host->mutex); room=host->identity; }
            if (!room.empty()) {
                if (!valid_room(room)) throw std::runtime_error("invalid host registration");
                write_packet(output,'S',room.data(),static_cast<uint32_t>(room.size())); offered=true;
            }
            if (!announced) {
                bool ready;
                { std::lock_guard lock(host->mutex); ready=!host->viewer_id.empty(); }
                if (ready) { write_packet(output,'R'); announced=true; }
            }
        } else if (joined) {
            viewer.Tick();
            if (viewer.Finished()) break;
            if (viewer.Connected() && !claim_request && !claimed) {
                claim_request=viewer.RequestRemote("remoteops_claim",{{"ticket",ticket}});
                if (!claim_request) break;
            }
            if (claim_request) {
                if (auto reply=viewer.TakeRemoteResult(claim_request)) {
                    if (!reply->value("ok", false)) break;
                    claimed=true; claim_request=0;
                    viewer.SetVideoOutput(true,true);
                }
            }
            if (claimed) {
                if (!announced) {
                    auto net=viewer.Status().at("network");
                    const int route=net.at("traversal").get<int>();
                    if (net.value("srtp",false) && (route==MiniRtcTraversalP2P || route==MiniRtcTraversalRelay)) {
                        if (turn && route!=MiniRtcTraversalRelay) break;
                        write_packet(output,route==MiniRtcTraversalRelay ? 'T' : 'P'); announced=true;
                    }
                }
                if (announced) {
                    if (auto frame=viewer.TakeEncodedVideo(); frame && !frame->data.empty() && frame->data.size()<=2*1024*1024)
                        write_packet(output,'V',frame->data.data(),static_cast<uint32_t>(frame->data.size()));
                }
            }
        }
        if (!waiting()) { Sleep(2); continue; }
        auto p=read_packet();
        if (p.type=='H' && p.data.empty()) {
            if (host && !host->grant.heartbeat()) break;
            last_heartbeat=Clock::now(); continue;
        }
        if (p.type=='S' && !sender && !joined) {
            const std::string room(p.data.begin(),p.data.end());
            if (!valid_room(room)) break;
            crossdesk::SessionOptions settings; settings.host=address; settings.port=port;
            settings.log_directory=options.at("log_directory").get<std::string>();
            viewer.Connect(settings,room.substr(0,9),room.substr(10)); joined=true;
        } else if (p.type=='F' && host && p.data.size()>=8) {
            if (!announced) continue;
            uint32_t w,h; std::memcpy(&w,p.data.data(),4); std::memcpy(&h,p.data.data()+4,4);
            if (!w || !h || w>1920 || h>1080 || ((w|h)&1) || p.data.size()!=8+w*h*4) break;
            std::string remote;
            {std::lock_guard lock(host->mutex);remote=host->viewer_id;}
            nv12.resize(w*h*3/2);
            if (libyuv::ARGBToNV12(p.data.data()+8,w*4,nv12.data(),w,nv12.data()+w*h,w,w,h)) break;
            MiniRtcVideoFrame frame{}; frame.width=w; frame.height=h; frame.size=nv12.size();
            frame.data=reinterpret_cast<const char*>(nv12.data());
            if (!host->grant.dispatch(remote,std::nullopt,[&]{
                MiniRtcSendVideoFrameToPeer(host->peer,&frame,crossdesk::MakeDisplayStreamId(0).c_str(),remote.data(),remote.size());
            })) break;
        } else break;
    }
    write_packet(output,'E');
}
}

int main() {
    FILE* output=_fdopen(_dup(_fileno(stdout)),"wb");
    if (!output || _dup2(_fileno(stderr),_fileno(stdout))!=0) return 2;
    _setmode(_fileno(output),_O_BINARY); _setmode(_fileno(stdin),_O_BINARY); setvbuf(stdin,nullptr,_IONBF,0);
    try {
        MiniRtcConfigureLogging(MiniRtcLogOff,nullptr,nullptr);
        auto p=read_packet(); if (p.type!='I') return 2;
        const auto options=json::parse(p.data);
        const auto host=options.at("host").get<std::string>();
        const int port=options.at("port").get<int>();
        if (host.empty() || host.size()>253 || host.find_first_not_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-:")!=std::string::npos ||
            port<1 || port>65535) return 2;
        serve(output,options,host,port); return 0;
    } catch (...) { return 1; }
}
