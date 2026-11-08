<script setup lang="ts">
import { computed, nextTick, watch } from "vue";
import { useRoute } from "vue-router";
import * as c from "../services/operations";
import type { Catalog, ManagedTool, SyncStatus } from "../models";
import EmptyState from "../components/EmptyState.vue";
const route = useRoute(),
  tc = c.state.toolCenter;
const section = computed(() => String(route.params.section || "settings"));
const catalog = computed(() => tc.snapshot as Catalog | null);
const disabled = computed(() => tc.busy || tc.loading || !catalog.value);
const tools = computed(() =>
  (catalog.value?.tools || []).filter(
    (t) =>
      (!tc.keyword ||
        `${t.name} ${t.id} ${t.purpose}`
          .toLowerCase()
          .includes(tc.keyword.toLowerCase())) &&
      (tc.filter === "all" ||
        (tc.filter === "enabled" ? t.enabled : !t.enabled)),
  ),
);
const toolPages = computed(() =>
  Math.max(1, Math.ceil(tools.value.length / tc.listPageSize)),
);
const toolPage = computed(() =>
  Math.min(toolPages.value, Math.max(1, tc.listPage)),
);
const visibleTools = computed(() =>
  tools.value.slice(
    (toolPage.value - 1) * tc.listPageSize,
    toolPage.value * tc.listPageSize,
  ),
);
watch(
  () => [tc.keyword, tc.filter, tc.listPageSize],
  () => {
    tc.listPage = 1;
  },
);
watch(
  () => [tc.nodeKeyword, tc.nodeFilter],
  async () => {
    await nextTick();
    document.querySelector(".tools-sync-scroll")?.scrollTo({ top: 0 });
  },
);
function changeToolPage(delta: number) {
  tc.listPage = Math.max(1, Math.min(toolPages.value, toolPage.value + delta));
}
const labels: Record<SyncStatus, string> = {
  synced: "已同步",
  pending: "等待确认",
  update_pending: "目录待更新",
  apply_failed: "应用失败",
  offline: "节点离线",
  upgrade_required: "需要升级 Agent",
};
const notes: Record<SyncStatus, string> = {
  synced: "已应用当前目录",
  pending: "尚未收到本次连接的应用确认",
  update_pending: "等待应用最新目录",
  apply_failed: "目录应用失败，请检查 Agent",
  offline: "连接未就绪，重新连接后确认",
  upgrade_required: "当前 Agent 不支持工具目录同步",
};
const nodes = computed(() =>
  (catalog.value?.nodes || []).map((n) => ({
    ...n,
    sync_status: c.toolNodeStatus(n, catalog.value?.revision) as SyncStatus,
  })),
);
const counts = computed(() =>
  Object.fromEntries(
    Object.keys(labels).map((key) => [
      key,
      nodes.value.filter((n) => n.sync_status === key).length,
    ]),
  ),
);
const filtered = computed(() =>
  nodes.value.filter(
    (n) =>
      (tc.nodeFilter === "all" || n.sync_status === tc.nodeFilter) &&
      (!tc.nodeKeyword ||
        `${n.hostname} ${n.agent_instance_id}`
          .toLowerCase()
          .includes(tc.nodeKeyword.toLowerCase())),
  ),
);
const pages = computed(() =>
  Math.max(1, Math.ceil(filtered.value.length / 25)),
);
const page = computed(() => Math.max(1, Math.min(tc.nodePage, pages.value)));
const rows = computed(() =>
  filtered.value.slice((page.value - 1) * 25, page.value * 25),
);
function changePage(delta: number) {
  tc.nodePage = Math.max(1, Math.min(pages.value, page.value + delta));
  document.querySelector(".tools-sync-scroll")?.scrollTo({ top: 0 });
}
function packages(t: ManagedTool) {
  return t.versions.flatMap((v) => v.packages);
}
function platformStatus(t: ManagedTool) {
  const p = packages(t);
  return p.some((p) =>
    p.tests.some(
      (x) => x.status === "passed" && (!p.tool.requires_desktop || x.desktop),
    ),
  )
    ? "有通过记录"
    : p.some((p) => p.tests.some((x) => x.status === "failed"))
      ? "有失败记录"
      : "尚未验证";
}
</script>
<template>
  <div
    class="content-container tool-center"
    :class="{ 'tools-sync-page': section === 'sync' }"
  >
    <nav class="tools-mobile-nav" aria-label="工具中心子菜单">
      <RouterLink
        v-for="s in c.TOOL_SECTIONS"
        :key="s[0]"
        :to="'/tools/' + s[0]"
        class="btn"
        :class="section === s[0] ? 'btn-primary' : 'btn-secondary'"
        >{{ s[1] }}</RouterLink
      >
    </nav>
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">
          {{
            section === "settings"
              ? "全局设置"
              : section === "sync"
                ? "节点同步状态"
                : "工具列表"
          }}
        </h1>
        <p class="page-desc">
          {{
            section === "settings"
              ? "管理工具中心总开关与全局使用指引。"
              : section === "sync"
                ? "查看 Agent 应用工具目录配置的情况。"
                : "维护工具资料、版本包与平台测试。"
          }}
        </p>
      </div>
      <div class="action-row">
        <button
          v-if="section === 'list'"
          class="btn btn-primary"
          :disabled="disabled"
          @click="c.openToolCenterEditor()"
        >
          新增工具</button
        ><button
          class="btn btn-secondary"
          :disabled="tc.busy || tc.loading"
          @click="c.refreshToolsData()"
        >
          {{ tc.loading ? "正在读取…" : "刷新" }}
        </button>
      </div>
    </header>
    <p v-if="tc.error" class="mcp-error" role="alert">{{ tc.error }}</p>
    <template v-if="section === 'settings'"
      ><section class="card tools-policy">
        <div>
          <h2 class="section-title">工具分发与运行</h2>
          <p class="section-desc">
            {{
              catalog?.enabled
                ? "已开启，Agent 可按现有授权运行已启用的工具。"
                : "当前关闭；可维护资料及测试记录，开启后才允许分发、启动与调用。"
            }}
          </p>
          <p class="mcp-note">
            目录修订
            {{
              catalog?.revision ?? "尚未读取"
            }}。未验证不代表不兼容，测试记录只覆盖实际系统与接口。
          </p>
        </div>
        <label class="tool-permission"
          ><input
            type="checkbox"
            role="switch"
            aria-label="工具中心全局开关"
            :checked="catalog?.enabled"
            :disabled="disabled"
            @change="
              c.toolCenterMutation(
                {
                  action: 'set_policy',
                  enabled: ($event.target as HTMLInputElement).checked,
                },
                '全局开关已更新',
              )
            "
          />{{ catalog?.enabled ? "已开启" : "已关闭" }}</label
        >
      </section>
      <section class="card tools-guide">
        <div class="tools-guide-heading">
          <h2 class="section-title">全局介绍与选择指南</h2>
          <button
            v-if="tc.guideDraft === null"
            class="btn btn-secondary"
            :disabled="disabled"
            @click="c.editToolGuide()"
          >
            编辑说明
          </button>
        </div>
        <pre v-if="tc.guideDraft === null" class="tools-guide-text">{{
          catalog?.guide_markdown || "尚未填写全局说明。"
        }}</pre>
        <form v-else @submit="c.saveToolGuide">
          <label class="input-label" for="tool-guide-input">全局说明</label
          ><textarea
            id="tool-guide-input"
            v-model="tc.guideDraft"
            class="input tools-guide-input"
            rows="14"
            :disabled="tc.busy"
          />
          <p
            v-if="tc.guideError"
            id="tool-guide-error"
            class="mcp-error"
            role="alert"
            tabindex="-1"
          >
            {{ tc.guideError }}
          </p>
          <div class="tools-guide-actions">
            <button
              type="button"
              class="btn btn-secondary"
              :disabled="tc.busy || !!tc.guideDraft"
              @click="c.useToolGuideTemplate()"
            >
              填入模板</button
            ><button
              type="button"
              class="btn btn-secondary"
              :disabled="tc.busy"
              @click="c.cancelToolGuide()"
            >
              取消</button
            ><button type="submit" class="btn btn-primary" :disabled="tc.busy">
              {{ tc.busy ? "正在保存…" : "保存说明" }}
            </button>
          </div>
        </form>
      </section></template
    >
    <template v-else-if="section === 'sync'"
      ><section class="tools-sync-section">
        <h2 id="tools-sync-title" class="section-title">工具目录同步</h2>
        <p class="section-desc">
          目录版本 {{ catalog?.revision ?? "尚未读取" }}。已同步表示 Agent
          已应用当前工具配置；工具包运行时按需下载。
        </p>
        <div class="tools-sync-summary">
          <span v-for="(label, key) in labels" :key="key"
            >{{ label }}<strong>{{ counts[key] }}</strong></span
          >
        </div>
        <div class="tools-toolbar">
          <div class="input-group">
            <label for="tools-node-search" class="input-label">搜索节点</label
            ><input
              id="tools-node-search"
              class="input"
              type="search"
              :value="tc.nodeKeyword"
              placeholder="节点名称或标识"
              @input="
                c.filterToolNodes(
                  ($event.target as HTMLInputElement).value,
                  'nodeKeyword',
                )
              "
            />
          </div>
          <label
            >同步状态<select
              id="tools-node-filter"
              class="input"
              :value="tc.nodeFilter"
              @change="
                c.filterToolNodes(
                  ($event.target as HTMLSelectElement).value,
                  'nodeFilter',
                )
              "
            >
              <option value="all">全部状态</option>
              <option v-for="(label, key) in labels" :key="key" :value="key">
                {{ label }}
              </option>
            </select></label
          >
        </div>
        <div class="table-container tools-sync-container">
          <div
            class="table-wrapper tools-sync-scroll"
            role="region"
            aria-label="节点目录同步列表"
            tabindex="0"
          >
            <table class="ops-table tools-sync-table">
              <thead>
                <tr>
                  <th>节点</th>
                  <th>状态</th>
                  <th>已应用目录版本</th>
                  <th>说明</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="n in rows" :key="n.agent_instance_id">
                  <td>
                    <strong>{{ n.hostname || n.agent_instance_id }}</strong>
                  </td>
                  <td>
                    <span
                      class="badge"
                      :class="
                        n.sync_status === 'synced'
                          ? 'badge-online'
                          : n.sync_status === 'apply_failed'
                            ? 'badge-danger'
                            : 'badge-info'
                      "
                      >{{ labels[n.sync_status] }}</span
                    >
                  </td>
                  <td>{{ n.applied_revision ?? "尚未确认" }}</td>
                  <td>{{ notes[n.sync_status] }}</td>
                </tr>
                <tr v-if="!rows.length">
                  <td colspan="4">
                    {{ tc.loading ? "正在读取…" : "暂无匹配节点" }}
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <footer class="tools-pagination">
            <span role="status"
              >共 {{ filtered.length }} 个节点 · 第 {{ page }} /
              {{ pages }} 页</span
            >
            <div>
              <button
                class="btn btn-secondary"
                :disabled="page <= 1"
                @click="changePage(-1)"
              >
                上一页</button
              ><button
                class="btn btn-secondary"
                :disabled="page >= pages"
                @click="changePage(1)"
              >
                下一页
              </button>
            </div>
          </footer>
        </div>
      </section></template
    >
    <template v-else
      ><div class="tools-toolbar">
        <div class="input-group">
          <label for="tools-search" class="input-label">搜索工具</label
          ><input
            id="tools-search"
            v-model="tc.keyword"
            class="input"
            type="search"
            placeholder="名称、标识或用途"
          />
        </div>
        <label
          >启用状态<select id="tools-filter" v-model="tc.filter" class="input">
            <option value="all">全部工具</option>
            <option value="enabled">已启用</option>
            <option value="disabled">未启用</option>
          </select></label
        ><button
          class="btn btn-secondary"
          :disabled="disabled"
          @click="c.openToolPackageImport()"
        >
          上传 / 导入包
        </button>
      </div>
      <section class="table-container">
        <div v-if="tools.length" class="table-wrapper">
          <table class="ops-table tools-table">
            <thead>
              <tr>
                <th>工具／用途</th>
                <th>版本与平台</th>
                <th>文件校验</th>
                <th>平台测试</th>
                <th>启用状态</th>
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="t in visibleTools" :key="t.id">
                <td>
                  <strong>{{ t.name }}</strong>
                  <div class="table-secondary-text">{{ t.id }}</div>
                  <p class="tools-purpose">{{ t.purpose }}</p>
                </td>
                <td>
                  {{ t.versions.length }} 个版本 / {{ packages(t).length }} 个包
                  <div class="table-secondary-text">
                    {{
                      [
                        ...new Set(
                          packages(t).flatMap((p) =>
                            p.tool.platforms.map((x) => x.os + " / " + x.arch),
                          ),
                        ),
                      ].join("、")
                    }}
                  </div>
                </td>
                <td>
                  <span
                    class="badge"
                    :class="
                      packages(t).length &&
                      packages(t).every((p) => p.file_verified)
                        ? 'badge-online'
                        : 'badge-info'
                    "
                    >{{
                      packages(t).length &&
                      packages(t).every((p) => p.file_verified)
                        ? "校验通过"
                        : "待校验"
                    }}</span
                  >
                </td>
                <td>
                  <span class="badge badge-info">{{ platformStatus(t) }}</span>
                </td>
                <td>
                  <label class="tool-permission"
                    ><input
                      type="checkbox"
                      :aria-label="'启用' + t.name"
                      :checked="t.enabled"
                      :disabled="disabled"
                      @change="
                        c.toolCenterMutation(
                          {
                            action: 'set_tool_enabled',
                            tool_id: t.id,
                            enabled: ($event.target as HTMLInputElement)
                              .checked,
                          },
                          '工具启用状态已更新',
                        )
                      "
                    />{{ t.enabled ? "已启用" : "未启用" }}</label
                  >
                </td>
                <td>
                  <div class="action-row">
                    <button
                      class="btn btn-secondary btn-sm"
                      @click="c.selectManagedTool(t.id)"
                    >
                      详情</button
                    ><button
                      class="btn btn-secondary btn-sm"
                      :disabled="disabled"
                      @click="c.openToolCenterEditor(t.id)"
                    >
                      编辑
                    </button>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <EmptyState
          v-else
          :title="tc.loading ? '正在读取工具目录…' : '暂无匹配工具'"
          description="调整筛选条件，或上传工具包后登记资料。"
        />
        <footer
          v-if="tools.length"
          class="tools-pagination tools-list-pagination"
        >
          <span role="status"
            >共 {{ tools.length }} 个工具 · 第 {{ toolPage }} /
            {{ toolPages }} 页</span
          >
          <div>
            <label class="page-size-label"
              >每页<select
                v-model.number="tc.listPageSize"
                class="input"
                aria-label="每页工具数"
              >
                <option :value="20">20 项</option>
                <option :value="50">50 项</option>
                <option :value="100">100 项</option>
              </select></label
            ><button
              class="btn btn-secondary"
              :disabled="toolPage <= 1"
              @click="changeToolPage(-1)"
            >
              上一页</button
            ><button
              class="btn btn-secondary"
              :disabled="toolPage >= toolPages"
              @click="changeToolPage(1)"
            >
              下一页
            </button>
          </div>
        </footer>
      </section>
      <details v-if="catalog?.audit?.length" class="card tools-audit">
        <summary>最近工具管理记录</summary>
        <ul>
          <li v-for="(a, i) in catalog.audit.slice(0, 20)" :key="i">
            {{ c.formatApiDate(a.created_at) }} · {{ a.actor }} ·
            {{ a.action }} · {{ a.result }}（修订 {{ a.revision }}）
          </li>
        </ul>
      </details></template
    >
  </div>
</template>
