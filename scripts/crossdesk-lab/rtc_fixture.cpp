// 真实 CrossDesk 控制器和现场 RTC 对端，只向测试色块送帧，不访问现场桌面。
#include "controller_session.h"
#include "display_stream_id.h"
#include "host_peer.hpp"
#include "stream_names.h"
#include <algorithm>
#include <array>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <iostream>
#include <memory>
#include <mutex>
#include <openssl/evp.h>
#include <stdexcept>
#include <thread>
#include <windows.h>

using namespace std::chrono_literals;
using json = nlohmann::json;
using Clock = std::chrono::steady_clock;
namespace cd = crossdesk;

namespace {
void require(bool ok, const char* message) { if (!ok) throw std::runtime_error(message); }

class AgentPipe {
public:
    std::string tickets() const {
        std::string line;
        char ch;
        DWORD n;
        while (line.size() <= 256 && ReadFile(pipe_, &ch, 1, &n, nullptr) && n == 1) {
            if (ch == '\n') return line;
            line += ch;
        }
        throw std::runtime_error("缺少当前测试流的授权");
    }

    void poll(remoteops::HostGrant& grant) {
        DWORD available;
        if (!PeekNamedPipe(pipe_, nullptr, 0, nullptr, &available, nullptr)) {
            grant.revoke();
            throw std::runtime_error("Agent 控制链已结束");
        }
        char bytes[32];
        DWORD count;
        if (available) {
            require(ReadFile(pipe_, bytes, std::min<DWORD>(available, sizeof(bytes)), &count, nullptr) && count,
                    "Agent 控制链读取失败");
            for (DWORD i = 0; i < count; ++i) {
                switch (bytes[i]) {
                case 'H': require(grant.heartbeat(), "现场租约失效"); break;
                case 'E': require(grant.mode(2, true), "接管代次错误"); current = 'E'; break;
                case 'O': require(grant.mode(3, false), "释放代次错误"); current = 'O'; break;
                case 'X': grant.revoke(); current = 'X'; break;
                case 'A': current = 'A'; break;
                default: throw std::runtime_error("Agent 控制消息无效");
                }
            }
        }
    }
    char current = 0;
private:
    HANDLE pipe_ = GetStdHandle(STD_INPUT_HANDLE);
};

struct TestHost : remoteops::HostPeer {
    ~TestHost() { stop(); }
    explicit TestHost(std::string ticket) : HostPeer(std::move(ticket)) {
        command = [this](const std::string& who, const std::string& method, const json& args) {
            return method == "test_marker" && grant.dispatch(who, args.at("epoch").get<uint64_t>(), [&]{++changes;});
        };
    }
    std::atomic<unsigned> changes{0};
    // 两种测试亮度只由现场已接受的输入切换。RTC 发送与原生解码均走真实实现。
    bool send_marker() {
        std::string destination;
        { std::lock_guard lock(mutex); destination = viewer_id; }
        if (destination.empty()) return false;
        return grant.dispatch(destination, std::nullopt, [&] {
            constexpr unsigned width = 1280, height = 720;
            std::vector<uint8_t> pixels(width * height * 3 / 2, 128);
            std::fill_n(pixels.begin(), width * height, changes == 0 ? 48 : 192);
            MiniRtcVideoFrame frame{};
            frame.width = width;
            frame.height = height;
            frame.data = reinterpret_cast<const char*>(pixels.data());
            frame.size = pixels.size();
            MiniRtcSendVideoFrameToPeer(peer, &frame, cd::MakeDisplayStreamId(0).c_str(),
                                        destination.data(), destination.size());
        });
    }
};

void run(AgentPipe& agent, int port, const std::string& hostTicket, const std::string& viewerTicket) {
    TestHost host(hostTicket);
    host.start("127.0.0.1", port, std::getenv("REMOTEOPS_CROSSDESK_FIXTURE_TURN") ? MiniRtcTurnForceUdp : MiniRtcTurnDisabled);
    cd::ControllerSession viewer;
    bool revoked = false;
    auto wait = [&](auto condition, const char* message) {
        const auto end = Clock::now() + 15s;
        do {
            if (!revoked) agent.poll(host.grant);
            viewer.Tick();
            if (condition()) return;
            require(!viewer.Finished(), message);
            std::this_thread::sleep_for(5ms);
        } while (Clock::now() < end);
        throw std::runtime_error(message);
    };
    auto step = [&](const char* name, char expected) {
        agent.current = 0;
        std::cout << name << std::endl;
        wait([&]{return agent.current == expected;}, "Relay 未确认现场交接");
    };
    std::string device, password;
    wait([&] {
        std::lock_guard lock(host.mutex);
        if (host.identity.empty()) return false;
        device = host.identity.substr(0,9);
        password = host.identity.substr(10);
        return true;
    }, "现场信令登记失败");
    cd::SessionOptions options;
    options.host = "127.0.0.1";
    options.port = port;
    options.log_directory = ".";
    viewer.Connect(options, device, password);
    wait([&]{ return viewer.Connected(); }, "CrossDesk RTC 未连接");
    auto call = [&](const std::string& name, json args) {
        const auto id = viewer.RequestRemote(name, std::move(args));
        require(id != 0, "控制通道未就绪");
        std::optional<json> result;
        wait([&]{ result = viewer.TakeRemoteResult(id); return bool(result); }, "控制请求未获现场回执");
        return result->at("ok") == true;
    };
    require(!call("test_marker", {{"epoch",1}}), "建链时错误地获得输入");
    require(!host.send_marker(), "未领取观看许可已发送帧");
    require(call("remoteops_claim", {{"ticket",viewerTicket}}), "本流观看许可未生效");
    require(!call("remoteops_claim", {{"ticket",viewerTicket}}), "观看票据被重复使用");
    require(!call("test_marker", {{"epoch",1}}), "观察者可以修改色块");
    auto observe = [&](int luminance) {
        auto due = Clock::now();
        wait([&] {
            if (Clock::now() >= due) {
                require(host.send_marker(), "现场未接受已授权的观看");
                due = Clock::now() + 50ms;
            }
            const auto frame = viewer.TakeFrame();
            if (!frame || frame->width != 1280 || frame->height != 720 ||
                frame->nv12.size() < 1280 * 720 * 3 / 2) return false;
            return std::abs(int(frame->nv12[360 * 1280 + 640]) - luminance) <= 12;
        }, "未收到对应操作的 720p 解码画面");
    };
    observe(48);
    step("take", 'E');
    step("taken", 'A');
    require(call("test_marker", {{"epoch",2}}), "已授权输入未送达");
    observe(192);
    const auto expected = std::getenv("REMOTEOPS_CROSSDESK_FIXTURE_TURN") ? MiniRtcTraversalRelay : MiniRtcTraversalP2P;
    wait([&] {
        host.send_marker();
        const auto net = viewer.Status().at("network");
        return net.at("traversal") == int(expected) && net.at("srtp") == true;
    }, "实际选中的媒体路径或 SRTP 状态不符");
    step("release", 'O');
    require(!call("test_marker", {{"epoch",2}}), "交还后接受了旧输入");
    step("released", 'A');
    step("close", 'X');
    revoked = true;
    require(!host.send_marker(), "撤销观看后仍可发送帧");
    require(!call("remoteops_claim", {{"ticket",viewerTicket}}) && !call("test_marker", {{"epoch",2}}),
            "撤权后仍可操作");
    require(host.changes == 1, "现场执行次数错误");
    viewer.Disconnect();
}
}

int main(int argc, char** argv) {
    if (argc != 2) return 2;
    try {
        MiniRtcConfigureLogging(MiniRtcLogWarn, [](const MiniRtcLogRecord* r, void*) {
            if (r && r->source_file) {
                // 只保留位置。SDK 的原始错误可能包含 ICE 凭据或设备信息。
                const std::string_view path(r->source_file);
                const auto pos = path.find_last_of("/\\");
                std::cerr << "RTC diagnostic " << path.substr(pos == path.npos ? 0 : pos + 1)
                          << ' ' << r->source_line << '\n';
            }
        }, nullptr);
        AgentPipe agent;
        const auto tickets = json::parse(agent.tickets());
        run(agent, std::stoi(argv[1]), tickets.at("host"), tickets.at("viewer"));
        std::cerr << "CrossDesk " << (std::getenv("REMOTEOPS_CROSSDESK_FIXTURE_TURN") ? "TURN/UDP" : "P2P")
                  << ": 720p decoded, scoped input and revocation PASS\n";
        std::cout << "done" << std::endl;
        return 0;
    } catch (const std::runtime_error& error) {
        std::cerr << error.what() << '\n';
        return 1;
    } catch (...) {
        std::cerr << "CrossDesk RTC fixture failed\n";
        return 1;
    }
}
