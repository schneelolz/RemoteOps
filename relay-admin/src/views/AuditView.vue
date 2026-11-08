<script setup lang="ts">
import { computed } from "vue";
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
        <p class="page-desc">查看最近 200 条管理事件，导出当前筛选结果。</p>
      </div>
      <button class="btn btn-secondary" @click="exportRows">导出 CSV</button>
    </header>
    <div class="tools-toolbar">
      <div class="input-group">
        <label for="audit-search" class="input-label">搜索日志</label
        ><input
          id="audit-search"
          v-model="c.state.filters.keyword"
          class="input"
          type="search"
        />
      </div>
      <label
        >操作类型<select v-model="c.state.filters.auditType" class="input">
          <option value="ALL">全部操作</option>
          <option v-for="action in actions" :key="action">{{ action }}</option>
        </select></label
      ><label
        >结果<select v-model="c.state.filters.auditResult" class="input">
          <option value="ALL">全部结果</option>
          <option value="SUCCESS">成功</option>
          <option value="FAILED">失败</option>
        </select></label
      >
    </div>
    <section class="table-container">
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
              <td>{{ a.time }}</td>
              <td>
                {{ a.operator }}
                <div class="table-secondary-text">{{ a.ip }}</div>
              </td>
              <td>{{ a.action }}</td>
              <td class="font-mono">{{ a.target }}</td>
              <td>{{ a.result === "SUCCESS" ? "成功" : "失败" }}</td>
              <td>{{ a.details }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <EmptyState v-else title="暂无匹配日志" />
    </section>
  </div>
</template>
