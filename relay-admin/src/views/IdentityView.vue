<script setup lang="ts">
import { Copy, Fingerprint, KeyRound, ShieldCheck } from "@lucide/vue";
import * as c from "../services/operations";
</script>
<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">Relay 身份凭据</h1>
        <p class="page-desc">
          查看 Relay
          所有者身份与控制器凭据配置状态。出于安全边界，此页面不展示访问令牌原文。
        </p>
      </div>
    </header>

    <div class="identity-layout">
      <section class="card identity-card">
        <h2 class="section-title">
          <Fingerprint :size="18" aria-hidden="true" />
          <span>Owner 身份标识</span>
        </h2>
        <p class="section-desc">
          所有人工控制端和 MCP 客户端通过此 Owner UUID 在 Relay 中隔离会话边界。
        </p>
        <div class="code-box">
          <span class="font-mono text-break">{{
            c.relayInfo.ownerUuid || "尚未读取"
          }}</span>
          <button
            class="btn btn-secondary btn-sm"
            :disabled="!c.relayInfo.ownerUuid"
            @click="c.copyToClipboard(c.relayInfo.ownerUuid, 'Owner UUID')"
          >
            <Copy :size="14" aria-hidden="true" />
            <span>复制</span>
          </button>
        </div>
        <div class="identity-meta-row">
          <span>实例地址：{{ c.relayInfo.address }}</span>
          <span class="font-mono">{{ c.relayInfo.host }}</span>
        </div>
      </section>

      <section class="card identity-card">
        <h2 class="section-title">
          <KeyRound :size="18" aria-hidden="true" />
          <span>Controller 凭据状态</span>
        </h2>
        <div class="key-value-list">
          <div class="kv-item">
            <span class="kv-label">人工 Controller</span>
            <span
              class="badge"
              :class="
                c.relayInfo.humanTokenConfigured
                  ? 'badge-online'
                  : 'badge-offline'
              "
            >
              <span class="badge-dot" aria-hidden="true"></span>
              {{ c.relayInfo.humanTokenConfigured ? "已配置" : "未配置" }}
            </span>
          </div>
          <div class="kv-item">
            <span class="kv-label">旧 AI Token</span>
            <span
              class="badge"
              :class="
                c.relayInfo.aiTokenConfigured ? 'badge-info' : 'badge-offline'
              "
            >
              {{ c.relayInfo.aiTokenConfigured ? "已配置" : "未配置" }}
            </span>
          </div>
          <div class="kv-item">
            <span class="kv-label">旧 Token 接入策略</span>
            <span
              class="badge"
              :class="
                c.relayInfo.allowLegacyToken === null
                  ? 'badge-offline'
                  : c.relayInfo.allowLegacyToken
                    ? 'badge-degraded'
                    : 'badge-online'
              "
            >
              {{
                c.relayInfo.allowLegacyToken === null
                  ? "未读取"
                  : c.relayInfo.allowLegacyToken
                    ? "允许"
                    : "已关闭"
              }}
            </span>
          </div>
          <div class="kv-item">
            <span class="kv-label">Token 指纹</span>
            <span class="kv-value font-mono">{{
              c.relayInfo.aiTokenFingerprint || "未配置"
            }}</span>
          </div>
        </div>
        <div class="token-management-note">
          <ShieldCheck :size="16" aria-hidden="true" />
          <span
            >独立 MCP 客户端身份和工具管理权限请在“MCP
            客户端接入”中集中管理。</span
          >
        </div>
        <div>
          <RouterLink class="btn btn-secondary" to="/mcp"
            >前往管理 MCP 客户端</RouterLink
          >
        </div>
      </section>
    </div>
  </div>
</template>
