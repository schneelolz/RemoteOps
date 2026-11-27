<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { RefreshCw, Save } from "@lucide/vue";
import { apiFetch } from "../services/core.js";

const MAX_BYTES = 32768;
const markdown = ref("");
const revision = ref(0);
const updatedAt = ref<string | null>(null);
const loading = ref(false);
const saving = ref(false);
const error = ref("");
const saved = ref(false);

function byteLength(value: string) {
  return new globalThis.TextEncoder().encode(value).length;
}

const usedBytes = computed(() => byteLength(markdown.value));
const usedPercent = computed(() =>
  Math.min(100, Math.round((usedBytes.value / MAX_BYTES) * 100)),
);

async function load() {
  loading.value = true;
  error.value = "";
  try {
    const data = await apiFetch("/api/admin/global-guide");
    markdown.value = String(data?.markdown || "");
    revision.value = Number(data?.revision || 0);
    updatedAt.value = data?.updated_at || null;
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : "全局指引读取失败";
  } finally {
    loading.value = false;
  }
}

async function save() {
  if (saving.value) return;
  saving.value = true;
  error.value = "";
  saved.value = false;
  try {
    const data = await apiFetch("/api/admin/global-guide", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        markdown: markdown.value,
        expected_revision: revision.value,
      }),
    });
    revision.value = Number(data?.revision || 0);
    updatedAt.value = data?.updated_at || null;
    saved.value = true;
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : "全局指引保存失败";
  } finally {
    saving.value = false;
  }
}

onMounted(load);
</script>

<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">RemoteOps 全局指引</h1>
        <p class="page-desc">
          维护面向所有 MCP
          客户端的全局操作说明；工具中心的工具选择指南仍独立维护。
        </p>
      </div>
      <div class="page-header-actions">
        <button class="btn btn-secondary" :disabled="loading" @click="load">
          <RefreshCw :size="15" aria-hidden="true" />
          <span>{{ loading ? "读取中…" : "重新读取" }}</span>
        </button>
      </div>
    </header>

    <form class="card guide-form" @submit.prevent="save">
      <div class="card-header-row">
        <div class="guide-meta">
          <span class="tag">修订 #{{ revision }}</span>
          <span v-if="updatedAt" class="table-secondary-text">
            上次更新：{{ new Date(updatedAt).toLocaleString("zh-CN") }}
          </span>
        </div>
        <span class="table-secondary-text font-mono">
          {{ usedBytes.toLocaleString() }} /
          {{ MAX_BYTES.toLocaleString() }} 字节（{{ usedPercent }}%）
        </span>
      </div>

      <div class="byte-meter" aria-hidden="true">
        <div
          class="byte-meter-fill"
          :class="{ 'byte-meter-warn': usedPercent >= 85 }"
          :style="{ width: usedPercent + '%' }"
        ></div>
      </div>

      <div class="input-group">
        <label for="global-guide-markdown" class="input-label"
          >全局指引正文（Markdown）</label
        >
        <textarea
          id="global-guide-markdown"
          v-model="markdown"
          class="input font-mono guide-editor"
          rows="18"
          maxlength="32768"
          placeholder="在此编写 RemoteOps 全局操作指引与安全边界说明…"
          aria-label="RemoteOps 全局指引正文"
        />
      </div>

      <p v-if="error" class="mcp-error" role="alert">{{ error }}</p>
      <p v-if="saved" class="status-success" role="status">
        已保存。新 MCP 初始化或显式刷新时会读取该版本。
      </p>

      <div class="mcp-form-actions">
        <span class="mcp-note">提交时附带期望修订号，防止覆盖并发修改。</span>
        <button type="submit" class="btn btn-primary" :disabled="saving">
          <Save :size="16" aria-hidden="true" />
          <span>{{ saving ? "保存中…" : "保存全局指引" }}</span>
        </button>
      </div>
    </form>
  </div>
</template>
