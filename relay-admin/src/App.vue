<script setup lang="ts">
import { nextTick, onMounted, onBeforeUnmount, watch, ref } from "vue";
import { useRoute, RouterLink, RouterView } from "vue-router";
import { router } from "./router";
import {
  LayoutDashboard,
  Radio,
  Monitor,
  Plug,
  Boxes,
  KeyRound,
  ShieldCheck,
  ScrollText,
  RefreshCw,
  LogOut,
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
  ["identity", "Relay 身份凭据"],
  ["settings", "安全设置"],
  ["audit", "审计日志"],
];
const icons = [
  LayoutDashboard,
  Radio,
  Monitor,
  Plug,
  Boxes,
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
  <div v-if="!ready" class="empty-state" role="status">正在连接管理服务…</div>
  <LoginView v-else-if="!c.state.isLoggedIn" />
  <template v-else>
    <aside class="sidebar">
      <header class="sidebar-header">
        <div class="brand-badge">R</div>
        <div>
          <div class="brand-title">RemoteOps Relay</div>
          <div class="brand-sub">管理控制台</div>
        </div>
      </header>
      <nav class="sidebar-nav" aria-label="管理导航">
        <template v-for="(menu, i) in menus" :key="menu[0]"
          ><div v-if="i === 0 || i === 3" class="nav-section-title">
            {{ i === 0 ? "运维监控" : "凭据与安全" }}
          </div>
          <RouterLink
            class="nav-item"
            :class="{ active: route.name === menu[0] }"
            :to="menu[0] === 'tools' ? '/tools/settings' : '/' + menu[0]"
            ><component :is="icons[i]" :size="18" aria-hidden="true" /><span>{{
              menu[1]
            }}</span
            ><span v-if="menu[0] === 'agents'" class="nav-badge">{{
              c.agents.length
            }}</span></RouterLink
          >
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
          </div></template
        >
      </nav>
      <footer class="sidebar-footer">
        <span
          class="badge"
          :class="c.state.api.connected ? 'badge-online' : 'badge-danger'"
          >{{ c.state.api.connected ? "Relay 运行正常" : "连接异常" }}</span
        >
        <p class="mcp-note">后端 {{ c.relayInfo.version }} · 前端 0.1.1</p>
      </footer>
    </aside>
    <div class="main-wrapper">
      <header class="topbar">
        <div>
          <strong>{{ c.relayInfo.address }}</strong>
          <div class="table-secondary-text">{{ c.relayInfo.host }}</div>
        </div>
        <div class="topbar-actions">
          <button
            class="btn btn-secondary"
            :disabled="c.state.api.loading"
            @click="c.refreshCurrent()"
          >
            <RefreshCw :size="16" aria-hidden="true" />{{
              c.state.api.loading ? "刷新中…" : "刷新"
            }}</button
          ><select
            class="input theme-select"
            aria-label="界面主题"
            :value="c.state.theme"
            @change="c.setTheme(($event.target as HTMLSelectElement).value)"
          >
            <option value="system">跟随系统</option>
            <option value="light">亮色</option>
            <option value="dark">暗色</option></select
          ><span class="admin-pill">{{ c.state.adminUser }}</span
          ><button
            class="btn btn-secondary"
            title="退出登录"
            @click="c.logout()"
          >
            <LogOut :size="16" aria-hidden="true" />退出
          </button>
        </div>
      </header>
      <main class="content-area" id="main-content">
        <p v-if="c.state.api.error" class="mcp-error" role="alert">
          管理服务读取失败，请刷新重试。
        </p>
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
