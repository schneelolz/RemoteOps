// 仅通过父进程管道接收已授权流的凭据、信令和画面。此进程没有键鼠入口。
#include <atomic>
#include <condition_variable>
#include <cctype>
#include <cstdio>
#include <deque>
#include <fcntl.h>
#include <io.h>
#include <mutex>
#include <sstream>
#include <thread>
#include <vector>
#include "libyuv.h"
#include "minirtc_connection.h"
#include "signal_sender.h"
#include "clock/system_clock.h"
#include "nlohmann/json.hpp"

using nlohmann::json;
using namespace minirtc;
using Bytes = std::vector<uint8_t>;
constexpr size_t max_signal = 128 * 1024;
constexpr size_t max_video = 2 * 1024 * 1024;
constexpr size_t max_raw = 1920 * 1080 * 4 + 8;

// 网络线程不等待父进程的管道写入。
class Output {
    std::mutex mutex;
    std::condition_variable changed;
    std::deque<std::pair<uint8_t, Bytes>> queue;
    bool stop = false;
    std::thread writer;
public:
    explicit Output(FILE* file) : writer([this, file] {
        for (;;) {
            std::unique_lock lock(mutex);
            changed.wait(lock, [&]{ return stop || !queue.empty(); });
            if (stop) break;
            auto item = std::move(queue.front());
            queue.pop_front();
            lock.unlock();
            uint32_t size = static_cast<uint32_t>(item.second.size());
            if (fputc(item.first, file) == EOF || fwrite(&size, 4, 1, file) != 1 ||
                fwrite(item.second.data(), 1, size, file) != size || fflush(file)) std::_Exit(3);
        }
    }) {}
    ~Output() {
        { std::lock_guard lock(mutex); stop = true; }
        changed.notify_one();
        writer.join();
    }
    bool put(uint8_t kind, const uint8_t* data = nullptr, size_t size = 0) {
        std::lock_guard lock(mutex);
        if (queue.size() >= (kind == 'V' ? 4u : 32u)) return false;
        Bytes copy;
        if (size) copy.assign(data, data + size);
        queue.emplace_back(kind, std::move(copy));
        changed.notify_one();
        return true;
    }
};

static bool relay_candidate(std::string line, const std::string& address) {
    if (!line.empty() && line.back() == '\n') line.pop_back();
    if (!line.empty() && line.back() == '\r') line.pop_back();
    if (line.find_first_of("\r\n") != std::string::npos || line.rfind("a=candidate:", 0) != 0) return false;
    std::istringstream fields(line);
    std::string foundation, protocol, ip, typ, kind;
    unsigned component = 0, port = 0;
    uint64_t priority = 0;
    return (fields >> foundation >> component >> protocol >> priority >> ip >> port >> typ >> kind)
            && component == 1 && (protocol == "udp" || protocol == "UDP")
            && ip == address && port >= 25000 && port <= 25063 && typ == "typ" && kind == "relay";
}

static bool secure_sdp(const std::string& sdp, const std::string& address) {
    std::istringstream lines(sdp);
    std::string line;
    bool fingerprint = false;
    while (std::getline(lines, line)) {
        if (!line.empty() && line.back() == '\r') line.pop_back();
        if (line.find('\r') != std::string::npos) return false;
        if (line.rfind("a=fingerprint:", 0) == 0) {
            const std::string prefix = "a=fingerprint:sha-256 ";
            if (fingerprint || line.rfind(prefix, 0) != 0 || line.size() != prefix.size() + 95) return false;
            for (size_t i = 0; i < 95; ++i) {
                const auto ch = static_cast<unsigned char>(line[prefix.size() + i]);
                if (i % 3 == 2 ? ch != ':' : !std::isxdigit(ch)) return false;
            }
            fingerprint = true;
        }
        if (line.rfind("a=candidate:", 0) == 0 && !relay_candidate(line, address)) return false;
    }
    return fingerprint;
}

static int session(Output& out, const json& config) {
    const bool sender = config.at("sender");
    const auto& g = config.at("grant");
    const auto address = g.at("address").get<std::string>();
    ConnectionInfo info{};
    info.transmission_id = config.at("stream");
    info.user_id = sender ? "agent" : "viewer";
    info.remote_user_id = sender ? "viewer" : "agent";
    info.enable_srtp = true;
    info.trickle_ice = true;
    info.turn_mode = MiniRtcTurnForceUdp;
    info.video_content_type = MiniRtcVideoContentType::ScreenContent;
    info.video_codec = MiniRtcVideoCodec::H264;
    info.video_quality = MiniRtcVideoQualityHigh;
    info.video_frame_rate = 30;
    info.video_degradation_preference = MiniRtcVideoDegradationPreference::Balanced;
    info.disable_receiver_padding_probing = true;
    IceServerConfiguration ice;
    ice.id = info.transmission_id;
    ice.expires_at = g.at("expires");
    ice.servers.push_back({address, g.at("port").get<uint16_t>(), true, false,
                          g.at("username"), g.at("password"), ice.expires_at});
    if (!ice.Fresh()) return 1;
    info.ice_config = std::move(ice);
    MediaStreamIds streams;
    streams.video.push_back("screen");
    ConnectionCallbacks callbacks{};
    callbacks.user_data = &out;
    std::atomic<bool> selected{false}, connected{false};
    const auto ready = [&] { if (connected.load() && selected.load()) out.put('R'); };
    callbacks.on_selected_route = [&](bool relay) { selected = relay; if (relay) ready(); else out.put('E'); };
    callbacks.on_connection_status = [&](MiniRtcConnectionStatus status, const char*, size_t, void*) {
        if (status == MiniRtcConnectionConnected) { connected = true; ready(); }
        if (status == MiniRtcConnectionFailed || status == MiniRtcConnectionClosed ||
            (status == MiniRtcConnectionDisconnected && connected.exchange(false))) out.put('E');
    };
    callbacks.on_net_status_report = [](const char*,size_t,MiniRtcTraversalMode,const MiniRtcNetTrafficStats*,const char*,size_t,void*){};
    callbacks.on_receive_encoded_video = [](const uint8_t* data, size_t n, int64_t, const char*, size_t,
                                             const char*, size_t, void* context) {
        if (!data || !n || n > max_video) return 1;
        return static_cast<Output*>(context)->put('V',data,n) ? 0 : 1;
    };
    auto signal = std::make_shared<CallbackSignalSender>([&](const std::string& msg) {
        return msg.size() <= max_signal && out.put('S',reinterpret_cast<const uint8_t*>(msg.data()),msg.size()) ? 0 : -1;
    });
    MiniRtcConnection peer(std::make_shared<SystemClock>(), signal, info, streams, callbacks);
    if (peer.Init() != 0) return 1;
    if (sender) {
        IceWorkMsg start{};
        start.type = IceWorkMsg::UserJoinTransmission;
        start.transmission_id = info.transmission_id;
        start.remote_user_id = info.remote_user_id;
        peer.ProcessIceWorkMsg(start);
    }
    bool got_sdp = false;
    Bytes nv12;
    for (;;) {
        int kind = fgetc(stdin);
        if (kind == EOF) break;
        uint32_t size = 0;
        if (fread(&size,4,1,stdin) != 1 || size > (kind == 'F' ? max_raw : max_signal)) return 1;
        Bytes data(size);
        if (fread(data.data(),1,size,stdin) != size) return 1;
        if (kind == 'S') {
            auto msg = json::parse(data);
            if (msg.at("transmission_id") != info.transmission_id || msg.at("remote_user_id") != info.user_id ||
                msg.at("user_id") != info.remote_user_id) return 1;
            IceWorkMsg work{};
            work.transmission_id = info.transmission_id;
            work.remote_user_id = info.remote_user_id;
            const auto type = msg.at("type").get<std::string>();
            const auto sdp = msg.at("sdp").get<std::string>();
            if (type == "new_candidate") {
                if (sdp != "a=end-of-candidates\r\n" && !relay_candidate(sdp, address)) continue;
                work.type = IceWorkMsg::NewCandidate;
                work.new_candidate = sdp;
                work.candidate_ufrag = msg.value("ufrag", std::string{});
            } else {
                if (got_sdp || type != (sender ? "answer" : "offer") || !secure_sdp(sdp,address)) return 1;
                got_sdp = true;
                work.type = sender ? IceWorkMsg::Answer : IceWorkMsg::Offer;
                work.remote_sdp = sdp;
            }
            peer.ProcessIceWorkMsg(work);
        } else if (kind == 'F' && sender && size >= 8) {
            if (!connected || !selected) continue;
            uint32_t w,h;
            memcpy(&w,data.data(),4);
            memcpy(&h,data.data()+4,4);
            if (!w || !h || w>1920 || h>1080 || (w|h)%2 || size != 8+w*h*4) return 1;
            nv12.resize(w*h*3/2);
            if (libyuv::ARGBToNV12(data.data()+8,w*4,nv12.data(),w,nv12.data()+w*h,w,w,h)) return 1;
            MiniRtcVideoFrame frame{};
            frame.width=w; frame.height=h; frame.size=nv12.size(); frame.data=reinterpret_cast<const char*>(nv12.data());
            peer.SendVideoFrame(&frame,"screen");
        } else return 1;
    }
    return 0;
}

int main() {
    // stdout 为专有二进制管道，隔离上游媒体库自己的日志。
    FILE* pipe = _fdopen(_dup(_fileno(stdout)), "wb");
    if (!pipe || _dup2(_fileno(stderr),_fileno(stdout)) != 0) return 2;
    _setmode(_fileno(pipe), _O_BINARY);
    _setmode(_fileno(stdin), _O_BINARY);
    try {
        uint32_t size;
        if (fgetc(stdin) != 'I' || fread(&size,4,1,stdin) != 1 || !size || size>4096) return 1;
        std::string init(size, '\0');
        if (fread(init.data(),1,size,stdin) != size) return 1;
        Output output(pipe);
        return session(output,json::parse(init));
    } catch (...) { return 1; }
}
