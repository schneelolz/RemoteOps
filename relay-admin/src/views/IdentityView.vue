<script setup lang="ts">
import * as c from "../services/operations";
</script>
<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">Relay 身份凭据</h1>
        <p class="page-desc">查看身份与凭据配置状态。此页面不显示访问令牌。</p>
      </div>
    </header>
    <section class="card">
      <h2 class="section-title">Owner 身份</h2>
      <p class="font-mono text-break">{{ c.relayInfo.ownerUuid }}</p>
      <button
        class="btn btn-secondary"
        @click="c.copyToClipboard(c.relayInfo.ownerUuid, 'Owner UUID')"
      >
        复制 Owner UUID
      </button>
    </section>
    <section class="card">
      <h2 class="section-title">Controller 凭据</h2>
      <dl class="tools-facts">
        <dt>人工 Controller</dt>
        <dd>{{ c.relayInfo.humanTokenConfigured ? "已配置" : "未配置" }}</dd>
        <dt>旧 AI Token</dt>
        <dd>{{ c.relayInfo.aiTokenConfigured ? "已配置" : "未配置" }}</dd>
        <dt>旧 Token 接入</dt>
        <dd>
          {{
            c.relayInfo.allowLegacyToken === null
              ? "未读取"
              : c.relayInfo.allowLegacyToken
                ? "允许"
                : "已关闭"
          }}
        </dd>
        <dt>Token 指纹</dt>
        <dd class="font-mono">
          {{ c.relayInfo.aiTokenFingerprint || "未配置" }}
        </dd>
      </dl>
      <p class="mcp-note">独立 MCP 客户端身份和权限在“客户端接入”中管理。</p>
      <RouterLink class="btn btn-secondary" to="/mcp"
        >管理 MCP 客户端</RouterLink
      >
    </section>
  </div>
</template>
