#pragma once
#include "host_grant.hpp"
#include "controller_session.h"
#include "display_stream_id.h"
#include "stream_names.h"
#include <array>
#include <atomic>
#include <cstdio>
#include <cstring>
#include <functional>
#include <memory>
#include <openssl/evp.h>
#include <stdexcept>

namespace remoteops {
using json = nlohmann::json;
namespace cd = crossdesk;
inline std::string hex(const unsigned char* data, size_t size) {
    constexpr char digits[] = "0123456789abcdef";
    std::string result;
    for (size_t i = 0; i < size; ++i) {
        result.push_back(digits[data[i] >> 4]);
        result.push_back(digits[data[i] & 15]);
    }
    return result;
}

struct HostPeer {
    explicit HostPeer(std::string token) : grant(std::move(token)) {}
    ~HostPeer() { stop(); }
    void stop() { grant.revoke(); if (peer) { MiniRtcDestroyPeer(&peer); peer = nullptr; } }
    remoteops::HostGrant grant;
    MiniRtcParams params{};
    MiniRtcPeerPtr* peer = nullptr;
    std::mutex mutex;
    std::string identity;
    std::string viewer_id;
    std::function<bool(const std::string&, const std::string&, const json&)> command;
    std::atomic<bool> disconnected{false};
    std::unique_ptr<EVP_PKEY, decltype(&EVP_PKEY_free)> key{
        EVP_PKEY_Q_keygen(nullptr, nullptr, "ED25519"), EVP_PKEY_free};

    // 现场密钥只存于当前流进程。使用上游的 Ed25519 设备登记协议。
    static int exchange(MiniRtcSignalExchangeStage stage, const char* data, size_t size,
                        char* out, size_t capacity, void* user) noexcept {
        try {
            auto& host = *static_cast<HostPeer*>(user);
            auto message = json::parse(data, data + size);
            std::array<unsigned char, 32> pub{};
            size_t n = pub.size();
            if (!host.key || EVP_PKEY_get_raw_public_key(host.key.get(), pub.data(), &n) != 1) return -1;
            const auto publicKey = hex(pub.data(), n);
            json reply;
            if (stage == MiniRtcSignalExchangeStage::PrepareLogin) {
                reply = message;
                reply["user_id"] = "";
                reply.erase("reconnect_token");
                reply["device_key"] = {{"version", 1}, {"public_key", publicKey}};
            } else if (message.value("type", "") == "login_challenge") {
                const auto nonce = message.at("nonce").get<std::string>();
                if (nonce.size() != 64 || message.at("user_id") != "") return -1;
                const auto text = json::array({"crossdesk.device-login.v1", "", publicKey, nonce}).dump();
                std::array<unsigned char, 64> signature{};
                n = signature.size();
                std::unique_ptr<EVP_MD_CTX, decltype(&EVP_MD_CTX_free)> ctx(EVP_MD_CTX_new(), EVP_MD_CTX_free);
                if (!ctx || EVP_DigestSignInit(ctx.get(), nullptr, nullptr, nullptr, host.key.get()) != 1 ||
                    EVP_DigestSign(ctx.get(), signature.data(), &n,
                        reinterpret_cast<const unsigned char*>(text.data()), text.size()) != 1) return -1;
                reply = {{"type","login"},{"user_id",""},{"ice_config_version",1},{"session_resume_version",1},
                    {"device_key",{{"version",1},{"public_key",publicKey},
                    {"nonce",nonce},{"signature",hex(signature.data(),n)}}}};
            } else return 0;
            const auto wire = reply.dump();
            if (wire.size() > capacity) return -1;
            std::memcpy(out, wire.data(), wire.size());
            return static_cast<int>(wire.size());
        } catch (...) { return -1; }
    }

    static void received(const char* data, size_t n, const char* remote, size_t remoteLen,
                         const char* stream, size_t streamLen, void* user) noexcept {
        try {
            if (!data || n > 4096 || !remote || !remoteLen || remoteLen > 64 || !stream) return;
            const std::string_view channel(stream, streamLen);
            if (channel != cd::kControlStream && channel != cd::kWebControlStream) return;
            const auto req = json::parse(data, data+n, nullptr, false);
            if (!req.is_object() || req.value("type", "") != "automation" ||
                req.value("version", 0) != 1 || !req.at("id").is_number_unsigned()) return;
            auto& host = *static_cast<HostPeer*>(user);
            const std::string who(remote, remoteLen);
            const auto method = req.at("method").get<std::string>();
            const auto args = req.at("arguments");
            bool ok = false;
            if (method == "remoteops_claim") {
                ok = host.grant.claim(who, args.at("ticket").get<std::string>());
                if (ok) {
                    std::lock_guard lock(host.mutex);
                    host.viewer_id = who;
                }
            }
            else if (host.command) ok = host.command(who, method, args);
            const auto reply = json{{"type","automation_result"},{"version",1},{"id",req.at("id")},
                {"ok",ok},{ok ? "result" : "error", ok ? json::object() : json("PERMISSION_DENIED")}}.dump();
            MiniRtcSendReliableDataFrameToPeer(host.peer, reply.data(), reply.size(), cd::kControlStream, remote, remoteLen);
        } catch (...) { /* 拒绝无法解析的消息；现场不执行任何动作。 */ }
    }

    void start(const std::string& address, int port, MiniRtcTurnMode route) {
        std::snprintf(params.signal_server_ip, sizeof(params.signal_server_ip), "%s", address.c_str());
        params.signal_server_port = port;
        params.user_id = "";
        params.user_data = this;
        params.enable_srtp = true;
        params.video_quality = MiniRtcVideoQualityHigh;
        params.video_frame_rate = 30;
        params.turn_mode = route;
        params.on_signal_exchange = exchange;
        params.on_signal_status = [](MiniRtcSignalStatus, const char*, size_t, void*){};
        params.on_connection_status = [](MiniRtcConnectionStatus status, const char* peerId, size_t n, void* ptr) {
            if (status != MiniRtcConnectionDisconnected && status != MiniRtcConnectionFailed &&
                status != MiniRtcConnectionClosed) return;
            auto& host = *static_cast<HostPeer*>(ptr);
            bool matches;
            { std::lock_guard lock(host.mutex); matches = peerId && n && host.viewer_id == std::string_view(peerId, n); }
            if (matches) { host.grant.revoke(); host.disconnected = true; }
        };
        params.on_net_status_report = [](const char* id, size_t n, MiniRtcTraversalMode, const MiniRtcNetTrafficStats*,
                                          const char*, size_t, void* u) {
            if (id && n <= 256 && std::string_view(id,n).find('@') == 9) {
                auto& h = *static_cast<HostPeer*>(u);
                std::lock_guard lock(h.mutex);
                h.identity.assign(id,n);
            }
        };
        params.on_receive_data_buffer = received;
        peer = MiniRtcCreatePeer(&params);
        if (!peer || MiniRtcAddDataStream(peer, cd::kControlStream, true) ||
            MiniRtcAddVideoStream(peer, cd::MakeDisplayStreamId(0).c_str()) || MiniRtcInit(peer))
            throw std::runtime_error("现场 RTC 初始化失败");
    }
};
}
