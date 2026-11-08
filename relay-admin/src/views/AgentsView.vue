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
type Agent = (typeof c.agents)[number];
function value(a: Agent, key: string) {
  const values: Record<string, unknown> = {
    connectionStatus: a.status === "online" ? "在线" : "离线",
    hostname: a.hostname,
    macAddress: a.macAddress,
    controlCode:
      a.codeConfigured && c.isLeaseActive(a.leaseExpiresAt)
        ? "有效可用"
        : "未生效／已过期",
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
</script>
<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">Agent 节点</h1>
        <p class="page-desc">
          节点连接、控制码状态与授权上限。关闭 Agent 不会关闭服务器。
        </p>
      </div>
      <div class="action-row">
        <button
          class="btn btn-secondary"
          :disabled="!c.state.selectedAgents.size || c.state.shutdownBusy"
          @click="c.openAgentShutdown(false)"
        >
          关闭所选（{{ c.state.selectedAgents.size }}）</button
        ><button
          class="btn btn-danger"
          :disabled="!c.agents.some(c.canShutdownAgent) || c.state.shutdownBusy"
          @click="c.openAgentShutdown(true)"
        >
          关闭全部在线 Agent
        </button>
      </div>
    </header>
    <div class="tools-toolbar">
      <div class="input-group">
        <label class="input-label" for="agent-search">搜索节点</label
        ><input
          id="agent-search"
          v-model="c.state.agentKeyword"
          class="input"
          type="search"
          placeholder="节点名称、标识或操作系统"
        />
      </div>
      <label
        >节点状态<select v-model="c.state.agentStatus" class="input">
          <option value="ALL">全部状态</option>
          <option value="online">在线</option>
          <option value="offline">离线</option>
        </select></label
      ><ColumnPicker scope="agents" :definitions="c.agentColumnDefinitions" />
    </div>
    <section class="table-container">
      <div v-if="rows.length" class="table-wrapper">
        <table class="ops-table">
          <thead>
            <tr>
              <th>
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
            <tr v-for="a in rows" :key="a.id">
              <td>
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
                {{ value(a, key) }}
              </td>
              <td>
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
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <EmptyState v-else title="没有匹配的节点" />
    </section>
  </div>
</template>
