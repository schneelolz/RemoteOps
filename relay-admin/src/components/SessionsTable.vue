<script setup lang="ts">
import * as c from "../services/operations";
import EmptyState from "./EmptyState.vue";
type Session = (typeof c.sessions)[number];
defineProps<{ rows: Session[] }>();
function value(row: Session, key: string) {
  const values: Record<string, unknown> = {
    connectionStatus: row.status,
    agentHostname: row.agentName,
    agentMacAddress: row.agentMacAddress,
    controller: row.controllerHostname,
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
        <tr v-for="row in rows" :key="row.id">
          <td v-for="key in c.state.visibleColumns.sessions" :key="key">
            <span
              :class="
                key === 'connectionStatus'
                  ? 'badge'
                  : key.toLowerCase().includes('id')
                    ? 'font-mono'
                    : ''
              "
              >{{ value(row, key) }}</span
            >
          </td>
          <td>
            <button
              class="btn btn-secondary btn-sm"
              @click="c.openSessionDrawer(row.id)"
            >
              详情
            </button>
            <button
              class="btn btn-secondary btn-sm"
              :disabled="row.isClosed"
              @click="
                c.openModal('terminateSession', {
                  id: row.id,
                  agentName: row.agentName,
                })
              "
            >
              关闭会话
            </button>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
  <EmptyState
    v-else
    title="暂无匹配会话"
    description="调整筛选条件或等待 Agent 与 Controller 连接。"
  />
</template>
