<script setup lang="ts">
import { nextTick, onMounted, onBeforeUnmount, watch, ref } from "vue";
import { useRoute, RouterLink, RouterView } from "vue-router";
import { router } from "./router";
import { version as frontendVersion } from "../package.json";
import {
  LayoutDashboard,
  Radio,
  Monitor,
  Plug,
  Boxes,
  FileText,
  Network,
  KeyRound,
  ShieldCheck,
  ScrollText,
  RefreshCw,
  LogOut,
  Server,
} from "@lucide/vue";
import * as c from "./services/operations";
import LoginView from "./views/LoginView.vue";
import ActionDialogs from "./components/ActionDialogs.vue";
import DetailDrawer from "./components/DetailDrawer.vue";

const route = useRoute(),
  ready = ref(false);

const menus = [
  ["overview", "系统概览"],
  ["sessions", "会话管理"],
  ["agents", "Agent 节点"],
  ["mcp", "MCP 客户端接入"],
  ["tools", "工具中心"],
  ["global-guide", "全局指引"],
  ["tcp-forward", "TCP 转发策略"],
  ["identity", "Relay 身份凭据"],
  ["settings", "安全设置"],
  ["audit", "审计日志"],
] as const;

const sectionHeaders: Record<number, string> = {
  0: "运维监控",
  3: "能力与策略",
  7: "系统与安全",
};

const icons = [
  LayoutDashboard,
  Radio,
  Monitor,
  Plug,
  Boxes,
  FileText,
  Network,
  KeyRound,
  ShieldCheck,
  ScrollText,
];

function leave() {
  c.invalidateMcpView(true);
  c.invalidateToolsView();
}
function systemTheme() {
  if (c.state.theme === "system") c.applyTheme();
}
function returnToPage(event: PageTransitionEvent) {
  if (event.persisted) void c.refreshCurrent();
}
watch(
  () => route.fullPath,
  async () => {
    await c.activatePage(
      String(route.name || "overview"),
      String(route.params.section || "settings"),
    );
    await nextTick();
    document.querySelector<HTMLElement>("main h1")?.focus();
  },
);
router.beforeEach((to, from) => {
  if (to.fullPath === from.fullPath) return true;
  if (c.state.toolCenter.busy || c.state.shutdownBusy) {
    c.showToast("操作进行中，请等待结果后切换页面", "info");
    return false;
  }
  if (
    (c.state.toolCenter.guideDraft !== null ||
      (c.state.toolCenter.draft && c.state.activeModal === "toolCenterEdit")) &&
    !confirm("有未保存的修改，确定离开？")
  )
    return false;
  return true;
});
onMounted(async () => {
  c.applyTheme();
  c.restoreVisibleColumns();
  await c.restoreSession();
  ready.value = true;
  await c.activatePage(
    String(route.name || "overview"),
    String(route.params.section || "settings"),
  );
  window.addEventListener("pagehide", leave);
  window.addEventListener("pageshow", returnToPage);
  window
    .matchMedia("(prefers-color-scheme: dark)")
    .addEventListener("change", systemTheme);
});
onBeforeUnmount(() => {
  window.removeEventListener("pagehide", leave);
  window.removeEventListener("pageshow", returnToPage);
  window
    .matchMedia("(prefers-color-scheme: dark)")
    .removeEventListener("change", systemTheme);
});
</script>
<template>
  <div v-if="!ready" class="app-boot" role="status">
    <div class="brand-badge brand-badge-lg" aria-hidden="true">
      <svg width="24" height="24" viewBox="0 0 32 32" fill="none">
        <path
          d="M10 24V8h7a5 5 0 0 1 0 10h-4l5 6h-3.5l-4.5-6H13v6h-3z"
          fill="currentColor"
        />
      </svg>
    </div>
    <p class="app-boot-text">正在连接 RemoteOps Relay 管理服务…</p>
  </div>
  <LoginView v-else-if="!c.state.isLoggedIn" />
  <template v-else>
    <aside class="sidebar">
      <header class="sidebar-header">
        <div class="brand-badge" aria-hidden="true">
          <svg width="18" height="18" viewBox="0 0 32 32" fill="none">
            <path
              d="M10 24V8h7a5 5 0 0 1 0 10h-4l5 6h-3.5l-4.5-6H13v6h-3z"
              fill="currentColor"
            />
          </svg>
        </div>
        <div class="brand-lockup">
          <div class="brand-title">RemoteOps Relay</div>
          <div class="brand-sub">管理控制台</div>
        </div>
      </header>
      <nav class="sidebar-nav" aria-label="管理导航">
        <template v-for="(menu, i) in menus" :key="menu[0]">
          <div v-if="sectionHeaders[i]" class="nav-section-title">
            {{ sectionHeaders[i] }}
          </div>
          <RouterLink
            class="nav-item"
            :class="{ active: route.name === menu[0] }"
            :to="menu[0] === 'tools' ? '/tools/settings' : '/' + menu[0]"
          >
            <component :is="icons[i]" :size="18" aria-hidden="true" />
            <span class="nav-label">{{ menu[1] }}</span>
            <span
              v-if="menu[0] === 'agents' && c.agents.length"
              class="nav-badge"
              >{{ c.agents.length }}</span
            >
            <span
              v-else-if="menu[0] === 'sessions' && c.relayInfo.activeSessions"
              class="nav-badge"
              >{{ c.relayInfo.activeSessions }}</span
            >
          </RouterLink>
          <div
            v-if="menu[0] === 'tools' && route.name === 'tools'"
            class="tools-subnav"
          >
            <RouterLink
              v-for="section in c.TOOL_SECTIONS"
              :key="section[0]"
              class="nav-item"
              :class="{ active: route.params.section === section[0] }"
              :to="'/tools/' + section[0]"
              >{{ section[1] }}</RouterLink
            >
          </div>
        </template>
      </nav>
      <footer class="sidebar-footer">
        <div class="sidebar-footer-row">
          <span
            class="badge"
            :class="c.state.api.connected ? 'badge-online' : 'badge-danger'"
          >
            <span class="badge-dot" aria-hidden="true"></span>
            {{ c.state.api.connected ? "Relay 运行正常" : "连接异常" }}
          </span>
        </div>
        <p class="sidebar-version">
          后端 {{ c.relayInfo.version }} · 前端 {{ frontendVersion }}
        </p>
      </footer>
    </aside>
    <div class="main-wrapper">
      <header class="topbar">
        <div class="topbar-left">
          <div class="relay-meta">
            <div class="relay-icon-chip" aria-hidden="true">
              <Server :size="16" />
            </div>
            <div class="relay-name-box">
              <strong class="relay-name">{{ c.relayInfo.address }}</strong>
              <span class="relay-addr font-mono">{{ c.relayInfo.host }}</span>
            </div>
          </div>
        </div>
        <div class="topbar-actions">
          <button
            class="btn btn-secondary btn-sm"
            :disabled="c.state.api.loading"
            @click="c.refreshCurrent()"
          >
            <RefreshCw
              :size="15"
              :class="{ 'icon-spin': c.state.api.loading }"
              aria-hidden="true"
            />
            <span>{{ c.state.api.loading ? "刷新中…" : "刷新" }}</span>
          </button>
          <select
            class="select theme-select"
            aria-label="界面主题"
            :value="c.state.theme"
            @change="c.setTheme(($event.target as HTMLSelectElement).value)"
          >
            <option value="system">跟随系统</option>
            <option value="light">亮色主题</option>
            <option value="dark">暗色主题</option>
          </select>
          <div class="admin-pill" :title="'当前管理员：' + c.state.adminUser">
            <span class="avatar" aria-hidden="true">{{
              (c.state.adminUser || "A").slice(0, 1).toUpperCase()
            }}</span>
            <span class="admin-pill-name">{{ c.state.adminUser }}</span>
          </div>
          <button
            class="btn btn-ghost btn-sm"
            title="退出登录"
            @click="c.logout()"
          >
            <LogOut :size="15" aria-hidden="true" />
            <span>退出</span>
          </button>
        </div>
      </header>
      <main class="content-area" id="main-content">
        <div
          v-if="c.state.api.error"
          class="mcp-error content-banner"
          role="alert"
        >
          <span>管理服务读取失败，请检查网络连接后刷新重试。</span>
        </div>
        <RouterView />
      </main>
    </div>
    <ActionDialogs /><DetailDrawer />
  </template>
  <div
    id="toast-container"
    class="toast-container"
    role="status"
    aria-live="polite"
  >
    <div
      v-for="toast in c.toasts"
      :key="toast.id"
      class="toast show"
      :class="'toast-' + toast.type"
    >
      {{ toast.message }}
    </div>
  </div>
</template>
