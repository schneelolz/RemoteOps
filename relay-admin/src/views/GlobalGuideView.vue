<script setup lang="ts">
import { onMounted, ref } from "vue";
import { apiFetch } from "../services/core.js";

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
          这里维护整个 RemoteOps 的操作说明，工具中心的工具选择说明独立管理。
        </p>
      </div>
      <button class="btn btn-secondary" :disabled="loading" @click="load">
        {{ loading ? "读取中…" : "刷新" }}
      </button>
    </header>
    <form class="card settings-form" @submit.prevent="save">
      <div class="table-secondary-text">
        当前修订 {{ revision
        }}<span v-if="updatedAt">
          · {{ new Date(updatedAt).toLocaleString("zh-CN") }}</span
        >
      </div>
      <textarea
        v-model="markdown"
        class="input"
        rows="18"
        maxlength="32768"
        aria-label="RemoteOps 全局指引正文"
      />
      <p class="table-secondary-text">
        {{ byteLength(markdown) }} / 32768 字节
      </p>
      <p v-if="error" class="mcp-error" role="alert">{{ error }}</p>
      <p v-if="saved" class="status-success" role="status">
        已保存。新 MCP 初始化或显式刷新时会读取该版本。
      </p>
      <button class="btn btn-primary" :disabled="saving">
        {{ saving ? "保存中…" : "保存全局指引" }}
      </button>
    </form>
  </div>
</template>
