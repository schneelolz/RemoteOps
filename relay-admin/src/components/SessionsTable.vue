<script setup lang="ts">
import * as c from "../services/operations";
import EmptyState from "./EmptyState.vue";

type Session = (typeof c.sessions)[number];
defineProps<{
  rows: Session[];
  emptyTitle?: string;
  emptyDescription?: string;
  showClearFilters?: boolean;
}>();
defineEmits<{ clearFilters: [] }>();

const statusMeta: Record<string, { label: string; cls: string }> = {
  ACTIVE: { label: "活跃", cls: "badge-online" },
  WAITING: { label: "待配对", cls: "badge-info" },
  DEGRADED: { label: "连接异常", cls: "badge-degraded" },
  CLOSED: { label: "已关闭", cls: "badge-offline" },
};

function value(row: Session, key: string) {
  const controllerDisplay =
    row.controllerHostname && row.controllerHostname !== "-"
      ? row.controllerHostname
      : row.controllerName && row.controllerName !== "None"
        ? row.controllerName
        : "-";
  const values: Record<string, unknown> = {
    connectionStatus: row.status,
    agentHostname: row.agentName,
    agentMacAddress: row.agentMacAddress,
    controller: controllerDisplay,
    controllerMacAddress: row.controllerMacAddress,
    controlMode: `${row.permissionLabel} / MCP ${row.mcpControlLabel}`,
    lastHeartbeat: row.lastHeartbeat,
    sessionId: row.id,
    agentId: row.agentId,
    humanController: row.humanController,
    ownerUuid: row.ownerUuid,
    connectTime: row.connectTime,
    activity: `审批 ${row.pendingApprovals} / 请求 ${row.inflightRequests}`,
  };
  return values[key] ?? "-";
}

function isMonoKey(key: string) {
  return (
    key.toLowerCase().includes("id") ||
    key.toLowerCase().includes("mac") ||
    key === "ownerUuid"
  );
}
</script>
<template>
  <div v-if="rows.length" class="table-wrapper">
    <table class="ops-table">
      <thead>
        <tr>
          <th v-for="key in c.state.visibleColumns.sessions" :key="key">
            {{
              c.sessionColumnDefinitions.find((col) => col.key === key)?.label
            }}
          </th>
          <th>操作</th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="row in rows"
          :key="row.id"
          :class="{ 'session-row-closed': row.isClosed }"
          @click="c.openSessionDrawer(row.id)"
        >
          <td v-for="key in c.state.visibleColumns.sessions" :key="key">
            <span
              v-if="key === 'connectionStatus'"
              class="badge"
              :class="statusMeta[row.status]?.cls || 'badge-offline'"
            >
              <span class="badge-dot" aria-hidden="true"></span>
              {{ statusMeta[row.status]?.label || row.status }}
            </span>
            <span
              v-else-if="key === 'controlMode'"
              class="control-mode"
              :class="
                row.permissionMode === 'full_access'
                  ? 'control-mode-full'
                  : 'control-mode-readonly'
              "
            >
              <span class="control-mode-dot" aria-hidden="true"></span>
              {{ value(row, key) }}
            </span>
            <strong
              v-else-if="key === 'agentHostname'"
              class="table-primary-text"
            >
              {{ value(row, key) }}
            </strong>
            <span v-else :class="{ 'font-mono': isMonoKey(key) }">
              {{ value(row, key) }}
            </span>
          </td>
          <td @click.stop>
            <div class="action-row">
              <button
                class="btn btn-secondary btn-sm"
                @click="c.openSessionDrawer(row.id)"
              >
                详情
              </button>
              <button
                class="btn btn-secondary btn-sm"
                :disabled="row.isClosed || row.status === 'WAITING'"
                @click="
                  c.openModal('terminateSession', {
                    id: row.id,
                    agentName: row.agentName,
                  })
                "
              >
                关闭会话
              </button>
            </div>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
  <EmptyState
    v-else
    :title="emptyTitle || '暂无匹配会话'"
    :description="
      emptyDescription || '调整筛选条件或等待 Agent 与 Controller 建立连接。'
    "
  >
    <button
      v-if="showClearFilters"
      type="button"
      class="btn btn-secondary btn-sm empty-state-action"
      @click="$emit('clearFilters')"
    >
      清除筛选
    </button>
  </EmptyState>
</template>
