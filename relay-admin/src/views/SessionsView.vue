<script setup lang="ts">
import { computed } from "vue";
import * as c from "../services/operations";
import ColumnPicker from "../components/ColumnPicker.vue";
import SessionsTable from "../components/SessionsTable.vue";

const rows = computed(() => c.filteredSessions());
const empty = computed(() => c.sessionEmptyState(true));
const showWaitingNotice = computed(
  () =>
    rows.value.length > 0 &&
    !c.hasActiveSessionFilters() &&
    c.isControllerWaitingForPairing(),
);
</script>
<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">会话管理</h1>
        <p class="page-desc">
          查看连接状态、授权范围和会话活动。点击任意行可查看完整连接详情。
        </p>
      </div>
      <div class="page-header-actions">
        <button
          class="btn btn-secondary"
          @click="c.openModal('purgeClosedSessions')"
        >
          清除已关闭记录
        </button>
      </div>
    </header>

    <div class="filter-toolbar tools-toolbar">
      <div class="input-group toolbar-search">
        <label class="input-label" for="session-search">搜索会话</label>
        <input
          id="session-search"
          v-model="c.state.filters.keyword"
          class="input"
          type="search"
          placeholder="搜索会话 ID、Agent、控制端名称或完整 UUID…"
        />
      </div>
      <div class="input-group toolbar-select">
        <label class="input-label" for="session-status-filter">状态</label>
        <select
          id="session-status-filter"
          v-model="c.state.filters.status"
          class="select"
        >
          <option value="ALL">全部状态</option>
          <option value="ACTIVE">活跃</option>
          <option value="WAITING">待配对</option>
          <option value="DEGRADED">连接异常</option>
          <option value="CLOSED">已关闭</option>
        </select>
      </div>
      <div class="input-group toolbar-select">
        <label class="input-label" for="session-controller-filter"
          >控制器</label
        >
        <select
          id="session-controller-filter"
          v-model="c.state.filters.controllerType"
          class="select"
        >
          <option value="ALL">全部控制器</option>
          <option>AI (MCP)</option>
          <option>Human</option>
          <option>None</option>
        </select>
      </div>
      <div class="input-group toolbar-select">
        <label class="input-label" for="session-permission-filter">权限</label>
        <select
          id="session-permission-filter"
          v-model="c.state.filters.permissionMode"
          class="select"
        >
          <option value="ALL">全部权限</option>
          <option value="approval_required">逐项确认</option>
          <option value="full_access">完全控制</option>
        </select>
      </div>
      <ColumnPicker
        scope="sessions"
        :definitions="c.sessionColumnDefinitions"
      />
    </div>

    <section class="table-container">
      <div class="table-header-bar">
        <span class="table-helper-text"
          >共匹配 <strong>{{ rows.length }}</strong> /
          {{ c.sessions.length }} 条会话</span
        >
        <span v-if="showWaitingNotice" class="badge badge-info">
          <span class="badge-dot" aria-hidden="true"></span>
          控制端已连接，等待配对设备
        </span>
      </div>
      <SessionsTable
        :rows="rows"
        :empty-title="empty.title"
        :empty-description="empty.description"
        :show-clear-filters="empty.showClearFilters"
        @clear-filters="c.clearSessionFilters()"
      />
    </section>
  </div>
</template>
