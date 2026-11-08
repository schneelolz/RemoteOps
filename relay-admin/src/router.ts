import { createRouter, createWebHashHistory } from "vue-router";

// 创建 history 前归一化旧书签，避免路由先捕获旧 hash。
if (location.hash.startsWith("#tab=")) {
  const old = new URLSearchParams(location.hash.slice(1));
  const page = old.get("tab") || "overview";
  history.replaceState(
    null,
    "",
    `#/${page === "tools" ? "tools/" + (old.get("section") || "settings") : page}`,
  );
}

export const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/", redirect: "/overview" },
    {
      path: "/overview",
      name: "overview",
      component: () => import("./views/OverviewView.vue"),
    },
    {
      path: "/sessions",
      name: "sessions",
      component: () => import("./views/SessionsView.vue"),
    },
    {
      path: "/agents",
      name: "agents",
      component: () => import("./views/AgentsView.vue"),
    },
    {
      path: "/mcp",
      name: "mcp",
      component: () => import("./views/McpView.vue"),
    },
    { path: "/tools", redirect: "/tools/settings" },
    {
      path: "/tools/:section(list|settings|sync)",
      name: "tools",
      component: () => import("./views/ToolsView.vue"),
    },
    {
      path: "/identity",
      name: "identity",
      component: () => import("./views/IdentityView.vue"),
    },
    {
      path: "/settings",
      name: "settings",
      component: () => import("./views/SettingsView.vue"),
    },
    {
      path: "/audit",
      name: "audit",
      component: () => import("./views/AuditView.vue"),
    },
    {
      path: "/:pathMatch(.*)*",
      redirect: (to) => {
        if (!to.path.startsWith("/tab=")) return "/overview";
        const old = new URLSearchParams(to.path.slice(1));
        const page = old.get("tab");
        if (page === "tools") {
          const section = old.get("section") || "settings";
          return (
            "/tools/" +
            (["settings", "list", "sync"].includes(section)
              ? section
              : "settings")
          );
        }
        return [
          "overview",
          "sessions",
          "agents",
          "mcp",
          "identity",
          "settings",
          "audit",
        ].includes(page || "")
          ? "/" + page
          : "/overview";
      },
    },
  ],
});
