<script setup lang="ts">
import * as c from "../services/operations";
import SessionsTable from "../components/SessionsTable.vue";
import EmptyState from "../components/EmptyState.vue";
</script>
<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">系统概览</h1>
        <p class="page-desc">Relay 运行情况、当前连接及最近事件。</p>
      </div>
    </header>
    <div class="metrics-grid">
      <section
        v-for="metric in [
          ['在线 Agent', c.relayInfo.onlineAgents],
          ['活跃会话', c.relayInfo.activeSessions],
          ['已连接 Controller', c.relayInfo.connectedControllers],
          ['待审批请求', c.relayInfo.pendingApprovals],
        ]"
        :key="String(metric[0])"
        class="card metric-card"
      >
        <div class="metric-label">{{ metric[0] }}</div>
        <strong class="metric-value">{{ metric[1] }}</strong>
      </section>
    </div>
    <section class="card">
      <h2 class="section-title">Relay 状态</h2>
      <dl class="tools-facts">
        <dt>运行版本</dt>
        <dd>{{ c.relayInfo.version }}</dd>
        <dt>运行时间</dt>
        <dd>{{ c.relayInfo.uptime }}</dd>
        <dt>进行中请求</dt>
        <dd>{{ c.relayInfo.inFlightRequests }}</dd>
        <dt>Owner</dt>
        <dd class="font-mono">
          {{ c.formatMaskedUuid(c.relayInfo.ownerUuid) }}
        </dd>
      </dl>
    </section>
    <section class="table-container">
      <div class="table-header-bar">
        <h2 class="section-title">活跃会话</h2>
        <RouterLink to="/sessions" class="btn btn-secondary"
          >查看全部</RouterLink
        >
      </div>
      <SessionsTable
        :rows="c.sessions.filter((s) => s.status === 'ACTIVE').slice(0, 5)"
      />
    </section>
    <section class="card">
      <h2 class="section-title">最近事件</h2>
      <ul v-if="c.recentEvents.length" class="event-list">
        <li v-for="(event, i) in c.recentEvents" :key="i">
          <time>{{ event.time }}</time> <strong>{{ event.type }}</strong>
          {{ event.desc }}
        </li>
      </ul>
      <EmptyState v-else title="暂无最近事件" />
    </section>
  </div>
</template>
