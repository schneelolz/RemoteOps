<script setup lang="ts">
import { computed } from "vue";
import {
  Monitor,
  Radio,
  Plug,
  Clock,
  ArrowRight,
  Copy,
  Activity,
  Server,
} from "@lucide/vue";
import * as c from "../services/operations";
import SessionsTable from "../components/SessionsTable.vue";
import EmptyState from "../components/EmptyState.vue";

const metrics = computed(() => [
  {
    label: "在线 Agent",
    value: c.relayInfo.onlineAgents,
    sub: `共 ${c.agents.length} 台已登记节点`,
    tone: "online",
    icon: Monitor,
    to: "/agents",
  },
  {
    label: "活跃会话",
    value: c.relayInfo.activeSessions,
    sub: `总计 ${c.sessions.length} 条会话记录`,
    tone: "info",
    icon: Radio,
    to: "/sessions",
  },
  {
    label: "已连接 Controller",
    value: c.relayInfo.connectedControllers,
    sub: "人工控制端与 MCP 客户端",
    tone: "purple",
    icon: Plug,
    to: "/sessions",
  },
  {
    label: "待审批请求",
    value: c.relayInfo.pendingApprovals,
    sub:
      c.relayInfo.pendingApprovals > 0
        ? "存在等待人工确认的操作"
        : "当前无挂起审批",
    tone: c.relayInfo.pendingApprovals > 0 ? "degraded" : "neutral",
    icon: Clock,
    to: "/sessions",
  },
]);

const activeSessions = computed(() =>
  c.sessions.filter((s) => s.status === "ACTIVE").slice(0, 5),
);
const emptySessions = computed(() => c.sessionEmptyState(false));

function onMetricClick(to: string) {
  if (to === "/sessions") {
    c.clearSessionFilters();
  }
}
</script>
<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">系统概览</h1>
        <p class="page-desc">
          实时监控 Relay 运行状态、在线节点、活跃运维会话及最近安全事件。
        </p>
      </div>
      <div class="page-header-actions">
        <RouterLink to="/agents" class="btn btn-secondary btn-sm"
          >管理节点</RouterLink
        >
        <RouterLink to="/mcp" class="btn btn-primary btn-sm"
          >MCP 接入管理</RouterLink
        >
      </div>
    </header>

    <div class="metrics-grid">
      <RouterLink
        v-for="metric in metrics"
        :key="metric.label"
        :to="metric.to"
        class="card stat-card stat-card-link"
        @click="onMetricClick(metric.to)"
      >
        <div class="stat-header">
          <span>{{ metric.label }}</span>
          <span
            class="stat-icon-wrap"
            :class="'stat-icon-' + metric.tone"
            aria-hidden="true"
          >
            <component :is="metric.icon" :size="16" />
          </span>
        </div>
        <strong class="stat-value">{{ metric.value }}</strong>
        <div class="stat-footer">
          <span>{{ metric.sub }}</span>
          <ArrowRight :size="14" class="stat-arrow" aria-hidden="true" />
        </div>
      </RouterLink>
    </div>

    <div class="overview-split">
      <section class="card">
        <div class="card-header-row">
          <h2 class="section-title">
            <Server :size="18" aria-hidden="true" />
            <span>Relay 运行状态</span>
          </h2>
          <span
            class="badge"
            :class="c.state.api.connected ? 'badge-online' : 'badge-danger'"
          >
            <span class="badge-dot" aria-hidden="true"></span>
            {{ c.state.api.connected ? "在线" : "异常" }}
          </span>
        </div>
        <div class="key-value-list">
          <div class="kv-item">
            <span class="kv-label">运行版本</span>
            <span class="kv-value font-mono">{{ c.relayInfo.version }}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">持续运行时间</span>
            <span class="kv-value">{{ c.relayInfo.uptime }}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">进行中请求</span>
            <span class="kv-value font-mono">{{
              c.relayInfo.inFlightRequests
            }}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">Owner 身份</span>
            <button
              v-if="c.relayInfo.ownerUuid"
              type="button"
              class="copyable-text"
              title="点击复制完整 Owner UUID"
              @click="c.copyToClipboard(c.relayInfo.ownerUuid, 'Owner UUID')"
            >
              <span>{{ c.formatMaskedUuid(c.relayInfo.ownerUuid) }}</span>
              <Copy :size="13" aria-hidden="true" />
            </button>
            <span v-else class="kv-value font-mono">-</span>
          </div>
        </div>
      </section>

      <section class="card">
        <div class="card-header-row">
          <h2 class="section-title">
            <Activity :size="18" aria-hidden="true" />
            <span>最近事件</span>
          </h2>
          <RouterLink to="/audit" class="btn btn-ghost btn-sm"
            >全部审计</RouterLink
          >
        </div>
        <ul v-if="c.recentEvents.length" class="event-list">
          <li v-for="(event, i) in c.recentEvents" :key="i" class="event-row">
            <span
              class="badge badge-compact"
              :class="
                event.badge === 'online' ? 'badge-online' : 'badge-danger'
              "
            >
              {{ event.type }}
            </span>
            <span class="event-desc">{{ event.desc }}</span>
            <time class="event-time font-mono">{{ event.time }}</time>
          </li>
        </ul>
        <EmptyState
          v-else
          title="暂无最近事件"
          description="系统产生连接、审批或配置变更后将显示在此处。"
        />
      </section>
    </div>

    <section class="table-container">
      <div class="table-header-bar">
        <div>
          <h2 class="section-title">活跃会话</h2>
          <p class="table-helper-text">
            当前正在进行中的远程运维连接（最多显示 5 条）
          </p>
        </div>
        <RouterLink to="/sessions" class="btn btn-secondary btn-sm"
          >查看全部</RouterLink
        >
      </div>
      <SessionsTable
        :rows="activeSessions"
        :empty-title="emptySessions.title"
        :empty-description="emptySessions.description"
      />
    </section>
  </div>
</template>
