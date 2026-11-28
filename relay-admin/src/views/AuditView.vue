<script setup lang="ts">
import { computed } from "vue";
import { Download } from "@lucide/vue";
import * as c from "../services/operations";
import EmptyState from "../components/EmptyState.vue";

const rows = computed(() =>
  c.auditLogs.filter(
    (a) =>
      (!c.state.filters.keyword ||
        [a.operator, a.target, a.details].some((v) =>
          String(v)
            .toLowerCase()
            .includes(c.state.filters.keyword.toLowerCase()),
        )) &&
      (c.state.filters.auditType === "ALL" ||
        a.action === c.state.filters.auditType) &&
      (c.state.filters.auditResult === "ALL" ||
        a.result === c.state.filters.auditResult),
  ),
);
const actions = computed(() => [...new Set(c.auditLogs.map((a) => a.action))]);

function exportRows() {
  const quote = (value: unknown) =>
    '"' + String(value ?? "").replace(/"/g, '""') + '"';
  const csv = [
    ["时间", "操作人", "操作", "目标", "结果", "来源", "详情"],
    ...rows.value.map((a) => [
      a.time,
      a.operator,
      a.action,
      a.target,
      a.result,
      a.ip,
      a.details,
    ]),
  ]
    .map((row) =>
      row
        .map((v) => quote(/^[=+@\-\t\r]/.test(String(v)) ? "'" + v : v))
        .join(","),
    )
    .join("\r\n");
  const url = URL.createObjectURL(
    new Blob(["\uFEFF" + csv], { type: "text/csv;charset=utf-8" }),
  );
  const a = document.createElement("a");
  a.href = url;
  a.download = "remoteops-audit.csv";
  a.click();
  URL.revokeObjectURL(url);
}
</script>
<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">审计日志</h1>
        <p class="page-desc">
          查看最近 200 条管理与安全事件，支持按条件筛选并导出 CSV。
        </p>
      </div>
      <div class="page-header-actions">
        <button
          class="btn btn-secondary"
          :disabled="!rows.length"
          @click="exportRows"
        >
          <Download :size="16" aria-hidden="true" />
          <span>导出 CSV</span>
        </button>
      </div>
    </header>

    <div class="filter-toolbar tools-toolbar">
      <div class="input-group toolbar-search">
        <label for="audit-search" class="input-label">搜索日志</label>
        <input
          id="audit-search"
          v-model="c.state.filters.keyword"
          class="input"
          type="search"
          placeholder="搜索操作人、目标或详情…"
        />
      </div>
      <div class="input-group toolbar-select">
        <label for="audit-type-filter" class="input-label">操作类型</label>
        <select
          id="audit-type-filter"
          v-model="c.state.filters.auditType"
          class="select"
        >
          <option value="ALL">全部操作</option>
          <option v-for="action in actions" :key="action">{{ action }}</option>
        </select>
      </div>
      <div class="input-group toolbar-select">
        <label for="audit-result-filter" class="input-label">执行结果</label>
        <select
          id="audit-result-filter"
          v-model="c.state.filters.auditResult"
          class="select"
        >
          <option value="ALL">全部结果</option>
          <option value="SUCCESS">成功</option>
          <option value="FAILED">失败</option>
        </select>
      </div>
    </div>

    <section class="table-container">
      <div class="table-header-bar">
        <span class="table-helper-text"
          >共 <strong>{{ rows.length }}</strong> 条审计记录</span
        >
      </div>
      <div v-if="rows.length" class="table-wrapper">
        <table class="ops-table audit-table">
          <thead>
            <tr>
              <th>时间</th>
              <th>操作人／来源</th>
              <th>操作</th>
              <th>目标</th>
              <th>结果</th>
              <th>详情</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(a, i) in rows" :key="i">
              <td class="font-mono">{{ a.time }}</td>
              <td>
                <div class="table-primary-text">{{ a.operator }}</div>
                <div class="table-secondary-text font-mono">{{ a.ip }}</div>
              </td>
              <td>
                <span class="tag">{{ a.action }}</span>
              </td>
              <td class="font-mono">{{ a.target }}</td>
              <td>
                <span
                  class="badge"
                  :class="
                    a.result === 'SUCCESS' ? 'badge-online' : 'badge-danger'
                  "
                >
                  <span class="badge-dot" aria-hidden="true"></span>
                  {{ a.result === "SUCCESS" ? "成功" : "失败" }}
                </span>
              </td>
              <td>{{ a.details }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <EmptyState
        v-else
        title="暂无匹配日志"
        description="尝试清除筛选关键字或切换操作类型。"
      />
    </section>
  </div>
</template>
