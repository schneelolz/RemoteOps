<script setup lang="ts">
import { onMounted, ref } from "vue";
import { apiFetch } from "../services/core.js";

interface Rule {
  id: string;
  host: string;
  port: number;
  agent_id: string | null;
  installation_id: string | null;
  purpose: string;
  expires_at: string | null;
}
const revision = ref(0);
const rules = ref<Rule[]>([]);
const contact = ref("");
const updatedAt = ref<string | null>(null);
const loading = ref(false);
const saving = ref(false);
const loaded = ref(false);
const error = ref("");
const message = ref("");

async function load() {
  if (loading.value || saving.value) return;
  loading.value = true;
  error.value = "";
  try {
    const data = await apiFetch("/api/admin/tcp-forward-policy");
    rules.value = structuredClone(data.rules);
    contact.value = data.approval_contact;
    revision.value = data.revision;
    updatedAt.value = data.updated_at;
    loaded.value = true;
  } catch (cause) {
    error.value =
      cause instanceof Error ? cause.message : "策略读取失败，请重试";
  } finally {
    loading.value = false;
  }
}
function add() {
  rules.value.push({
    id: crypto.randomUUID(),
    host: "",
    port: 80,
    agent_id: null,
    installation_id: null,
    purpose: "",
    expires_at: null,
  });
  message.value = "";
}
async function save() {
  if (!loaded.value || saving.value) return;
  saving.value = true;
  error.value = "";
  message.value = "";
  try {
    const policy = {
      revision: revision.value,
      rules: rules.value.map((rule) => ({
        ...rule,
        host: rule.host.trim(),
        agent_id: rule.agent_id?.trim() || null,
        installation_id: rule.installation_id?.trim() || null,
        purpose: rule.purpose.trim(),
        expires_at: rule.expires_at
          ? new Date(rule.expires_at).toISOString()
          : null,
      })),
      approval_contact: contact.value,
      updated_at: updatedAt.value,
    };
    const result = await apiFetch("/api/admin/tcp-forward-policy", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ expected_revision: revision.value, policy }),
    });
    revision.value = result.revision;
    updatedAt.value = result.updated_at;
    rules.value = structuredClone(result.rules);
    message.value =
      "策略已生效。撤销或到期的目标转发会关闭，其余会话继续运行。";
  } catch (cause) {
    error.value =
      (cause instanceof Error ? cause.message : "策略保存失败") +
      "。草稿已保留；如提示修订冲突，请核对当前策略后重新合并。";
  } finally {
    saving.value = false;
  }
}
function dateValue(value: string | null) {
  if (!value) return "";
  const date = new Date(value);
  date.setMinutes(date.getMinutes() - date.getTimezoneOffset());
  return date.toISOString().slice(0, 16);
}
function changeDate(rule: Rule, event: Event) {
  const value = (event.target as HTMLInputElement).value;
  rule.expires_at = value ? new Date(value).toISOString() : null;
}
onMounted(load);
</script>

<template>
  <div class="content-container tcp-policy">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">TCP 转发策略</h1>
        <p class="page-desc">
          批准现场可访问的固定目标。保存后即时生效，修改策略无需重启中继。
        </p>
      </div>
      <button
        class="btn btn-secondary"
        :disabled="loading || saving"
        @click="load"
      >
        {{ loading ? "读取中…" : "重新读取并替换草稿" }}
      </button>
    </header>
    <p v-if="loading" role="status">正在读取已批准目标…</p>
    <form v-if="loaded" class="card settings-form" @submit.prevent="save">
      <div class="policy-meta">
        当前修订 {{ revision
        }}<span v-if="updatedAt">
          · {{ new Date(updatedAt).toLocaleString("zh-CN") }}</span
        >
      </div>
      <label class="policy-field"
        >申请联系说明
        <textarea
          v-model="contact"
          class="input"
          rows="2"
          maxlength="1024"
          required
        />
      </label>
      <p class="policy-help">
        控制端被拒绝时会收到此说明，以及目标、端口、用途、现场和安装身份等申请字段。
      </p>
      <div v-if="rules.length === 0" class="policy-empty" role="status">
        当前没有已批准目标，所有新 TCP
        转发都会被拒绝。添加一条规则后保存即可批准访问。
      </div>
      <fieldset
        v-for="(rule, index) in rules"
        :key="rule.id"
        class="policy-rule"
        :disabled="saving"
      >
        <legend>目标规则 {{ index + 1 }}</legend>
        <div class="policy-grid">
          <label class="policy-field"
            >目标主机或网段<input
              v-model="rule.host"
              class="input"
              required
              maxlength="253"
              placeholder="192.168.10.254 或 192.168.10.0/24"
          /></label>
          <label class="policy-field"
            >目标端口<input
              v-model.number="rule.port"
              class="input"
              type="number"
              required
              min="1"
              max="65535"
          /></label>
          <label class="policy-field"
            >现场 Agent 身份<input
              v-model="rule.agent_id"
              class="input"
              placeholder="留空表示所有已授权现场"
          /></label>
          <label class="policy-field"
            >MCP 安装身份<input
              v-model="rule.installation_id"
              class="input"
              placeholder="留空表示所有已授权安装"
          /></label>
          <label class="policy-field"
            >用途<input
              v-model="rule.purpose"
              class="input"
              required
              maxlength="1024"
              placeholder="说明批准本目标的用途"
          /></label>
          <label class="policy-field"
            >到期时间<input
              class="input"
              type="datetime-local"
              :value="dateValue(rule.expires_at)"
              @change="changeDate(rule, $event)"
            /><span class="policy-help">留空表示长期有效</span></label
          >
        </div>
        <button
          type="button"
          class="btn btn-secondary"
          @click="rules.splice(index, 1)"
        >
          移除此规则
        </button>
      </fieldset>
      <button
        type="button"
        class="btn btn-secondary"
        :disabled="saving || rules.length >= 256"
        @click="add"
      >
        添加目标规则
      </button>
      <p class="policy-help">
        移除规则或缩小范围后，失去授权的监听和连接会关闭。回环监听、会话审批、目标地址安全校验及资源上限同时生效。
      </p>
      <button class="btn btn-primary" :disabled="saving || loading">
        {{ saving ? "保存中…" : "保存并立即生效" }}
      </button>
    </form>
    <p v-if="error" class="mcp-error" role="alert">{{ error }}</p>
    <p v-if="message" role="status" aria-live="polite">{{ message }}</p>
  </div>
</template>

<style scoped>
.tcp-policy form {
  display: grid;
  gap: 16px;
}
.policy-rule {
  min-width: 0;
  padding: 16px;
  border: 1px solid var(--border-default);
  border-radius: 8px;
}
.policy-rule legend {
  padding-inline: 6px;
  font-weight: 600;
}
.policy-grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  gap: 16px;
  margin-bottom: 16px;
}
.policy-field {
  display: grid;
  gap: 8px;
  min-width: 0;
  font-weight: 500;
}
.policy-help,
.policy-meta {
  color: var(--text-secondary);
  font-size: 13px;
  line-height: 1.6;
  font-weight: 400;
}
.policy-empty {
  padding-block: 20px;
  line-height: 1.7;
}
.tcp-policy .input {
  width: 100%;
  min-width: 0;
  box-sizing: border-box;
}
.tcp-policy .btn {
  min-height: 44px;
  justify-self: start;
  white-space: normal;
}
@media (max-width: 640px) {
  .policy-grid {
    grid-template-columns: minmax(0, 1fr);
  }
  .tcp-policy .page-header {
    align-items: stretch;
    gap: 16px;
    flex-direction: column;
  }
}
</style>
