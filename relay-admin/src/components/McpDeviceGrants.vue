<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { agents, apiFetch } from "../services/operations";

const props = defineProps<{ installation: string; name: string }>();
const emit = defineEmits<{ close: [] }>();
const dialog = ref<HTMLDialogElement>();
const grants = ref<string[]>([]);
const selected = ref("");
const ready = ref(false);
const busy = ref(false);
const error = ref("");
const available = computed(() =>
  agents.filter((a) => !grants.value.includes(a.id)),
);
const endpoint =
  "/api/admin/mcp/clients/" +
  encodeURIComponent(props.installation) +
  "/device-grants";

async function reload() {
  ready.value = false;
  error.value = "";
  try {
    const result = await apiFetch(endpoint);
    if (!Array.isArray(result?.agent_instance_ids))
      throw new Error("无法读取设备授权列表");
    grants.value = result.agent_instance_ids;
    ready.value = true;
  } catch (e) {
    error.value = e instanceof Error ? e.message : "读取失败";
  }
}

async function change(device: string, enabled: boolean) {
  if (busy.value || !ready.value || !device) return;
  busy.value = true;
  error.value = "";
  try {
    await apiFetch(endpoint + "/" + encodeURIComponent(device), {
      method: enabled ? "PUT" : "DELETE",
    });
    selected.value = "";
    await reload();
  } catch (e) {
    ready.value = false;
    error.value = e instanceof Error ? e.message : "更新失败，请重新读取授权";
  } finally {
    busy.value = false;
  }
}

onMounted(() => {
  dialog.value?.showModal();
  void reload();
});
</script>

<template>
  <dialog
    ref="dialog"
    class="card device-grants"
    aria-labelledby="device-grants-title"
    @close="emit('close')"
  >
    <header class="card-header-row">
      <div>
        <h2 id="device-grants-title" class="section-title">设备授权</h2>
        <strong>{{ name }}</strong>
        <div class="font-mono table-secondary-text">{{ installation }}</div>
      </div>
      <button class="btn btn-secondary" @click="dialog?.close()">关闭</button>
    </header>
    <p>
      现场已允许默认完全控制时，此安装用控制码配对下列设备即可进入完全控制。撤销会立即结束由该授权生效的完全控制。
    </p>
    <p v-if="error" role="alert" class="mcp-error">
      {{ error }}
      <button class="btn btn-secondary btn-sm" @click="reload">重新读取</button>
    </p>
    <template v-if="ready">
      <ul v-if="grants.length">
        <li v-for="id in grants" :key="id">
          <div>
            <strong>{{
              agents.find((a) => a.id === id)?.hostname || "已授权设备"
            }}</strong>
            <div class="font-mono table-secondary-text">{{ id }}</div>
          </div>
          <button
            class="btn btn-secondary btn-sm"
            :disabled="busy"
            @click="change(id, false)"
          >
            撤销授权
          </button>
        </li>
      </ul>
      <p v-else>此安装尚未获得设备授权。</p>
      <label class="input-group"
        >添加设备
        <select v-model="selected" class="select" :disabled="busy">
          <option value="">选择设备</option>
          <option v-for="agent in available" :key="agent.id" :value="agent.id">
            {{ agent.hostname }} · {{ agent.id }}
          </option>
        </select>
      </label>
      <button
        class="btn btn-primary"
        :disabled="busy || !selected"
        @click="change(selected, true)"
      >
        授权所选设备
      </button>
    </template>
    <p v-else-if="!error" role="status">正在读取授权…</p>
  </dialog>
</template>

<style scoped>
.device-grants {
  width: min(34rem, calc(100vw - 2rem));
  max-height: 85vh;
  margin: auto;
  overflow-y: auto;
  color: inherit;
}
.device-grants .card-header-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: start;
}
.device-grants::backdrop {
  background: rgb(0 0 0 / 42%);
}
.device-grants p,
.device-grants label,
.device-grants ul {
  margin-block: 1rem;
}
.device-grants ul {
  padding: 0;
  list-style: none;
}
.device-grants li {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: 0.5rem;
  padding-block: 0.5rem;
}
.device-grants .font-mono {
  overflow-wrap: anywhere;
}
.device-grants select {
  width: 100%;
  min-width: 0;
}
</style>
