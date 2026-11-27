#pragma once

#include <chrono>
#include <cstdint>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <openssl/crypto.h>

namespace remoteops {
// 每个对象只属于一条 RemoteOps 桌面流。RTC 连接成功本身不授予观看或输入。
class HostGrant {
public:
    using Clock = std::chrono::steady_clock;
    explicit HostGrant(std::string ticket)
        : ticket_(std::move(ticket)), deadline_(Clock::now() + lease),
          admission_deadline_(Clock::now() + std::chrono::seconds(30)) {}
    ~HostGrant() { erase_ticket(); }

    // 只有来自 Agent 已认证控制链的心跳可以续租。过期是终态。
    bool heartbeat() {
        std::lock_guard lock(mutex_);
        if (!live()) return false;
        deadline_ = Clock::now() + lease;
        return true;
    }

    bool claim(std::string_view peer, std::string_view ticket) {
        std::lock_guard lock(mutex_);
        if (!live() || Clock::now() >= admission_deadline_ || !peer_.empty() || peer.empty() || peer.size() > 64 ||
            ticket_.size() != 36 || ticket.size() != ticket_.size()) return false;
        if (CRYPTO_memcmp(ticket_.data(), ticket.data(), ticket.size()) != 0) return false;
        peer_.assign(peer);
        erase_ticket();
        return true;
    }

    // Agent 释放旧输入并确认新模式后推进代次。观察也会使旧输入作废。
    bool mode(uint64_t epoch, bool input) {
        std::lock_guard lock(mutex_);
        if (!live() || epoch <= epoch_ || (input && peer_.empty())) return false;
        epoch_ = epoch;
        input_ = input;
        return true;
    }

    // 在执行线程最终检查。锁内只执行一次有界的本地动作或帧入队。
    template<class Apply>
    bool dispatch(std::string_view peer, std::optional<uint64_t> input_epoch, Apply&& apply) {
        std::lock_guard lock(mutex_);
        if (!live() || peer_.empty() || peer != peer_ ||
            (input_epoch && (!input_ || *input_epoch != epoch_))) return false;
        std::forward<Apply>(apply)();
        return true;
    }

    void revoke() {
        std::lock_guard lock(mutex_);
        closed_ = true;
        input_ = false;
        peer_.clear();
        erase_ticket();
    }

private:
    static constexpr auto lease = std::chrono::seconds(5);
    bool live() const { return !closed_ && Clock::now() < deadline_; }
    std::mutex mutex_;
    std::string ticket_, peer_;
    void erase_ticket() {
        OPENSSL_cleanse(ticket_.data(), ticket_.size());
        ticket_.clear();
    }
    Clock::time_point deadline_, admission_deadline_;
    uint64_t epoch_ = 0;
    bool closed_ = false, input_ = false;
};
}
