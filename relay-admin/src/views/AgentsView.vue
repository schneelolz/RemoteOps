<script setup lang="ts">
import { computed } from "vue";
import * as c from "../services/operations";
import ColumnPicker from "../components/ColumnPicker.vue";
import EmptyState from "../components/EmptyState.vue";

const rows = computed(() => c.filteredAgents());
const eligible = computed(() => rows.value.filter(c.canShutdownAgent));
const allChecked = computed(
  () =>
    eligible.value.length > 0 &&
    eligible.value.every((a) => c.state.selectedAgents.has(a.id)),
);
const onlineCount = computed(
  () => c.agents.filter((a) => a.status === "online").length,
);

type Agent = (typeof c.agents)[number];

function isCodeActive(a: Agent) {
  return a.codeConfigured && c.isLeaseActive(a.leaseExpiresAt);
}

function value(a: Agent, key: string) {
  const values: Record<string, unknown> = {
    connectionStatus: a.status === "online" ? "在线" : "离线",
    hostname: a.hostname,
    macAddress: a.macAddress,
    controlCode: isCodeActive(a) ? "有效可用" : "未生效／已过期",
    session: a.sessionId,
    controlMode: `${a.permissionLabel} / MCP ${a.mcpControlLabel}`,
    ready: a.ready ? "已就绪" : "未就绪",
    agentId: a.id,
    os: a.os,
    lease: a.lease,
    lastHeartbeat: a.heartbeat,
    paired: a.everPaired ? "已配对" : "尚未配对",
  };
  return values[key] ?? "-";
}

function isMonoKey(key: string) {
  return ["macAddress", "session", "agentId"].includes(key);
}
</script>
<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">Agent 节点</h1>
        <p class="page-desc">
          管理现场节点连接、临时控制码与授权上限。关闭 Agent
          进程不会关闭宿主机。
        </p>
      </div>
      <div class="page-header-actions">
        <button
          class="btn btn-secondary"
          :disabled="!c.state.selectedAgents.size || c.state.shutdownBusy"
          @click="c.openAgentShutdown(false)"
        >
          关闭所选（{{ c.state.selectedAgents.size }}）
        </button>
        <button
          class="btn btn-danger"
          :disabled="!c.agents.some(c.canShutdownAgent) || c.state.shutdownBusy"
          @click="c.openAgentShutdown(true)"
        >
          关闭全部在线 Agent
        </button>
      </div>
    </header>

    <div class="filter-toolbar tools-toolbar">
      <div class="input-group toolbar-search">
        <label class="input-label" for="agent-search">搜索节点</label>
        <input
          id="agent-search"
          v-model="c.state.agentKeyword"
          class="input"
          type="search"
          placeholder="搜索计算机名、Agent ID 或操作系统…"
        />
      </div>
      <div class="input-group toolbar-select">
        <label class="input-label" for="agent-status-filter">节点状态</label>
        <select
          id="agent-status-filter"
          v-model="c.state.agentStatus"
          class="select"
        >
          <option value="ALL">全部状态</option>
          <option value="online">在线</option>
          <option value="offline">离线</option>
        </select>
      </div>
      <ColumnPicker scope="agents" :definitions="c.agentColumnDefinitions" />
    </div>

    <section class="table-container">
      <div class="table-header-bar">
        <span class="table-helper-text">
          在线 <strong>{{ onlineCount }}</strong> 台 · 当前显示
          <strong>{{ rows.length }}</strong> / {{ c.agents.length }} 台节点
        </span>
      </div>
      <div v-if="rows.length" class="table-wrapper">
        <table class="ops-table">
          <thead>
            <tr>
              <th class="agent-selection-cell">
                <input
                  aria-label="选择当前筛选内可关闭节点"
                  type="checkbox"
                  :checked="allChecked"
                  :disabled="!eligible.length"
                  @change="
                    c.selectVisibleAgents(
                      ($event.target as HTMLInputElement).checked,
                    )
                  "
                />
              </th>
              <th v-for="key in c.state.visibleColumns.agents" :key="key">
                {{
                  c.agentColumnDefinitions.find((col) => col.key === key)?.label
                }}
              </th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="a in rows"
              :key="a.id"
              :class="{ 'selected-row': c.state.selectedAgents.has(a.id) }"
              @click="c.openAgentDrawer(a.id)"
            >
              <td class="agent-selection-cell" @click.stop>
                <input
                  type="checkbox"
                  :aria-label="'选择 ' + a.hostname"
                  :checked="c.state.selectedAgents.has(a.id)"
                  :disabled="!c.canShutdownAgent(a) || c.state.shutdownBusy"
                  @change="
                    c.selectAgent(
                      a.id,
                      ($event.target as HTMLInputElement).checked,
                    )
                  "
                />
              </td>
              <td v-for="key in c.state.visibleColumns.agents" :key="key">
                <span
                  v-if="key === 'connectionStatus'"
                  class="badge"
                  :class="
                    a.status === 'online' ? 'badge-online' : 'badge-offline'
                  "
                >
                  <span class="badge-dot" aria-hidden="true"></span>
                  {{ a.status === "online" ? "在线" : "离线" }}
                </span>
                <strong
                  v-else-if="key === 'hostname'"
                  class="table-primary-text"
                >
                  {{ a.hostname }}
                </strong>
                <span
                  v-else-if="key === 'controlCode'"
                  class="badge"
                  :class="isCodeActive(a) ? 'badge-info' : 'badge-offline'"
                >
                  {{ value(a, key) }}
                </span>
                <span
                  v-else-if="key === 'ready'"
                  class="ready-status"
                  :class="a.ready ? 'ready-status-yes' : 'ready-status-no'"
                >
                  <span class="ready-status-dot" aria-hidden="true"></span>
                  {{ value(a, key) }}
                </span>
                <span
                  v-else-if="key === 'controlMode'"
                  class="control-mode"
                  :class="
                    a.permissionMode === 'full_access'
                      ? 'control-mode-full'
                      : 'control-mode-readonly'
                  "
                >
                  <span class="control-mode-dot" aria-hidden="true"></span>
                  {{ value(a, key) }}
                </span>
                <span v-else :class="{ 'font-mono': isMonoKey(key) }">
                  {{ value(a, key) }}
                </span>
              </td>
              <td @click.stop>
                <div class="action-row">
                  <button
                    class="btn btn-secondary btn-sm"
                    @click="c.openAgentDrawer(a.id)"
                  >
                    详情
                  </button>
                  <button
                    class="btn btn-secondary btn-sm"
                    :disabled="!c.canShutdownAgent(a)"
                    @click="
                      c.openModal('shutdownAgent', {
                        id: a.id,
                        generation: a.generation,
                      })
                    "
                  >
                    关闭 Agent
                  </button>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <EmptyState
        v-else
        title="没有匹配的节点"
        description="调整筛选条件，或等待现场 Agent 主动连接到 Relay。"
      />
    </section>
  </div>
</template>
