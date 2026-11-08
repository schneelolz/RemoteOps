<script setup lang="ts">
import { computed } from "vue";
import * as c from "../services/operations";
import ColumnPicker from "../components/ColumnPicker.vue";
import SessionsTable from "../components/SessionsTable.vue";
const rows = computed(() =>
  c.sessions.filter(
    (s) =>
      (!c.state.filters.keyword ||
        [s.id, s.agentName, s.controllerHostname].some((v) =>
          String(v)
            .toLowerCase()
            .includes(c.state.filters.keyword.toLowerCase()),
        )) &&
      (c.state.filters.status === "ALL" ||
        s.status === c.state.filters.status) &&
      (c.state.filters.controllerType === "ALL" ||
        s.controllerType === c.state.filters.controllerType) &&
      (c.state.filters.permissionMode === "ALL" ||
        s.permissionMode === c.state.filters.permissionMode),
  ),
);
</script>
<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">会话管理</h1>
        <p class="page-desc">查看连接状态、授权范围和会话活动。</p>
      </div>
      <button
        class="btn btn-secondary"
        @click="c.openModal('purgeClosedSessions')"
      >
        清除已关闭记录
      </button>
    </header>
    <div class="tools-toolbar">
      <div class="input-group">
        <label class="input-label" for="session-search">搜索会话</label
        ><input
          id="session-search"
          v-model="c.state.filters.keyword"
          class="input"
          type="search"
          placeholder="会话、Agent 或客户端"
        />
      </div>
      <label
        >状态<select v-model="c.state.filters.status" class="input">
          <option value="ALL">全部状态</option>
          <option value="ACTIVE">活跃</option>
          <option value="DEGRADED">连接异常</option>
          <option value="CLOSED">已关闭</option>
        </select></label
      ><label
        >控制器<select v-model="c.state.filters.controllerType" class="input">
          <option value="ALL">全部控制器</option>
          <option>AI (MCP)</option>
          <option>Human</option>
          <option>None</option>
        </select></label
      ><label
        >权限<select v-model="c.state.filters.permissionMode" class="input">
          <option value="ALL">全部权限</option>
          <option value="approval_required">逐项确认</option>
          <option value="full_access">完全控制</option>
        </select></label
      ><ColumnPicker
        scope="sessions"
        :definitions="c.sessionColumnDefinitions"
      />
    </div>
    <section class="table-container"><SessionsTable :rows="rows" /></section>
  </div>
</template>
