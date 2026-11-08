import { reactive, nextTick } from "vue";
import {
  state,
  relayInfo,
  sessions,
  agents,
  auditLogs,
  recentEvents,
} from "./store.js";
import { invalidateMcpView, refreshMcpData, clearMcpSecret } from "./mcp.js";
import { invalidateToolsView, refreshToolsData } from "./tools.js";
let navigation = { push: (_path) => {}, replace: (_path) => {} };
export function configureNavigation(router) {
  navigation = router;
}
function formatApiDate(value) {
  if (!value) return "-";
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? String(value)
    : date.toLocaleString("zh-CN", { hour12: false });
}

function formatUptime(seconds) {
  if (seconds === undefined || seconds === null || Number.isNaN(seconds))
    return "-";
  const sec = Number(seconds);
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = sec % 60;
  if (d > 0) return `${d}天 ${h}小时 ${m}分`;
  if (h > 0) return `${h}小时 ${m}分 ${s}秒`;
  return `${m}分 ${s}秒`;
}

function formatLeaseRemaining(value) {
  if (!value) return "未配置";
  const expire = new Date(value).getTime();
  if (Number.isNaN(expire)) return String(value);
  const diff = Math.floor((expire - Date.now()) / 1000);
  if (diff <= 0) return "已到期";
  const m = Math.floor(diff / 60);
  const s = diff % 60;
  return `剩余 ${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

function isLeaseActive(value) {
  if (!value) return false;
  const expire = new Date(value).getTime();
  if (Number.isNaN(expire)) return false;
  return expire > Date.now();
}

function formatMaskedUuid(uuid) {
  if (!uuid) return "-";
  const s = String(uuid);
  if (s.length <= 16) return s;
  return `${s.slice(0, 8)}...${s.slice(-4)}`;
}

function truncate(str, head = 8, tail = 6) {
  if (!str) return "-";
  if (str.length <= head + tail + 3) return str;
  return `${str.substring(0, head)}...${str.substring(str.length - tail)}`;
}

function permissionLabel(value) {
  const labels = {
    full_access: "完全控制",
    read_only: "只读",
    approval_required: "需审批",
    controller_approved: "Controller 已批准",
  };
  return labels[value] || "未知";
}

function mcpControlModeLabel(value) {
  const labels = {
    full_access: "完全控制",
    step_by_step: "逐项确认",
    read_only: "只读",
    external_approval: "外部审批",
    expired: "已过期",
  };
  return labels[value] || "未知/已失联";
}

const sessionColumnDefinitions = [
  { key: "connectionStatus", label: "连接状态" },
  { key: "agentHostname", label: "Agent 计算机名" },
  { key: "agentMacAddress", label: "Agent MAC 地址" },
  { key: "controller", label: "MCP 计算机名" },
  { key: "controllerMacAddress", label: "MCP MAC 地址" },
  { key: "controlMode", label: "控制权限" },
  { key: "lastHeartbeat", label: "最后心跳" },
  { key: "sessionId", label: "Session ID" },
  { key: "agentId", label: "Agent ID" },
  { key: "humanController", label: "Human Controller" },
  { key: "ownerUuid", label: "Owner UUID" },
  { key: "connectTime", label: "连接时间" },
  { key: "activity", label: "活动" },
];

const agentColumnDefinitions = [
  { key: "connectionStatus", label: "连接状态" },
  { key: "hostname", label: "计算机名" },
  { key: "macAddress", label: "MAC 地址" },
  { key: "controlCode", label: "控制码" },
  { key: "session", label: "当前 Session" },
  { key: "lastHeartbeat", label: "最后心跳" },
  { key: "controlMode", label: "控制权限" },
  { key: "ready", label: "就绪状态" },
  { key: "agentId", label: "Agent ID" },
  { key: "os", label: "操作系统" },
  { key: "lease", label: "租约到期" },
  { key: "paired", label: "配对记录" },
];

function toggleVisibleColumn(scope, key, checked) {
  const columns = state.visibleColumns[scope] || [];
  state.visibleColumns[scope] = checked
    ? [...new Set([...columns, key])]
    : columns.filter((column) => column !== key);
  try {
    localStorage.setItem(
      `remoteops-visible-columns-v2-${scope}`,
      JSON.stringify(state.visibleColumns[scope]),
    );
  } catch (_) {}
}

function restoreVisibleColumns() {
  for (const scope of ["sessions", "agents"]) {
    try {
      const stored = JSON.parse(
        localStorage.getItem(`remoteops-visible-columns-v2-${scope}`) || "null",
      );
      if (Array.isArray(stored) && stored.length > 0)
        state.visibleColumns[scope] = stored;
    } catch (_) {}
  }
}

async function apiFetch(path, options = {}) {
  const headers = { ...(options.headers || {}) };
  const config = {
    ...options,
    headers,
    credentials: "same-origin",
  };

  const response = await fetch(path, config);
  if (!response.ok) {
    let message = `HTTP ${response.status}`;
    try {
      const body = await response.json();
      message = body.error || body.message || message;
    } catch (_) {}
    throw new Error(message);
  }

  const contentType = response.headers.get("content-type") || "";
  if (!contentType.includes("application/json")) {
    return null;
  }
  return response.json();
}

function applyApiData(overview, identity, rawAgents, rawSessions, rawAudit) {
  relayInfo.address = state.demoMode
    ? "relay-prod-ap-east-1"
    : String(window.location.hostname || "127.0.0.1");
  relayInfo.host = state.demoMode
    ? "relay.internal.remoteops:18081"
    : String(window.location.host || "127.0.0.1");
  relayInfo.ownerUuid = String(
    String(identity?.owner_id || overview?.owner_id || ""),
  );
  relayInfo.allowLegacyToken =
    typeof identity?.allow_legacy_token === "boolean"
      ? identity.allow_legacy_token
      : null;
  relayInfo.aiTokenConfigured = Boolean(identity?.ai_token_configured);
  relayInfo.aiTokenFingerprint = String(
    String(identity?.ai_token_fingerprint || ""),
  );
  relayInfo.humanTokenConfigured = Boolean(identity?.human_token_configured);
  relayInfo.version = String(String(overview?.version || "-"));
  relayInfo.uptimeSeconds = overview?.uptime_seconds || 0;
  relayInfo.uptime = formatUptime(overview?.uptime_seconds || 0);
  relayInfo.onlineAgents = overview?.online_agents ?? 0;
  relayInfo.activeSessions = overview?.active_sessions ?? 0;
  relayInfo.connectedControllers = overview?.connected_controllers ?? 0;
  relayInfo.pendingApprovals = overview?.pending_approvals ?? 0;
  relayInfo.inFlightRequests = overview?.in_flight_requests ?? 0;

  const agentById = new Map(
    (rawAgents || []).map((agent) => [String(agent.agent_instance_id), agent]),
  );

  agents.splice(
    0,
    agents.length,
    ...(rawAgents || []).map((agent) => ({
      id: String(String(agent.agent_instance_id)),
      hostname: String(agent.hostname || "-"),
      macAddress: String(agent.mac_address || "-"),
      os: String(agent.operating_system || "-"),
      status: agent.state === "online" ? "online" : "offline",
      sessionId: agent.session_id ? String(String(agent.session_id)) : "None",
      codeStatus: agent.pairing_code_configured
        ? "控制码已生成"
        : "控制码未生成",
      codeConfigured: Boolean(agent.pairing_code_configured),
      lease: formatApiDate(agent.lease_expires_at),
      leaseExpiresAt: agent.lease_expires_at || "",
      heartbeat: formatApiDate(agent.last_seen),
      everPaired: Boolean(agent.ever_paired),
      ready: Boolean(agent.ready),
      permissionMode: agent.permission_mode,
      permissionLabel: String(permissionLabel(agent.permission_mode)),
      mcpControlMode: agent.mcp_control_mode || null,
      mcpControlLabel: String(mcpControlModeLabel(agent.mcp_control_mode)),
      supportsAgentShutdown: Boolean(agent.supports_agent_shutdown),
      generation: agent.connection_generation || 0,
    })),
  );

  reconcileAgentSelection();
  sessions.splice(
    0,
    sessions.length,
    ...(rawSessions || []).map((session) => {
      const bindings = session.controller_bindings || [];
      const ai = bindings.find((binding) => binding.kind === "ai");
      const human = bindings.find((binding) => binding.kind === "human");
      const agent = agentById.get(String(session.agent_instance_id));
      const permissionMode = session.permission_mode || "approval_required";
      return {
        id: String(String(session.session_id)),
        agentName: String(agent?.hostname || session.hostname || "Unknown"),
        agentMacAddress: String(
          agent?.mac_address || session.mac_address || "-",
        ),
        agentId: String(String(session.agent_instance_id)),
        agentStatus: session.state === "online" ? "online" : "offline",
        hostname: String(session.hostname || "-"),
        os: String(session.operating_system || "-"),
        role: String(session.role || "default"),
        controllerType: ai ? "AI (MCP)" : human ? "Human" : "None",
        controllerName: String(
          ai
            ? `AI (${String(ai.controller_instance_id).slice(0, 8)})`
            : human
              ? `Human (${String(human.controller_instance_id).slice(0, 8)})`
              : "None",
        ),
        controllerHostname: String(
          ai?.hostname || ai?.controller_hostname || "-",
        ),
        controllerMacAddress: String(
          ai?.mac_address || ai?.controller_mac_address || "-",
        ),
        controllerInstanceId: String(
          String((ai || human)?.controller_instance_id || "-"),
        ),
        humanController: String(
          human
            ? `Connected (${String(human.controller_instance_id).slice(0, 8)})`
            : "None",
        ),
        ownerUuid: String(
          session.owner_id ? String(session.owner_id) : relayInfo.ownerUuid,
        ),
        permissionMode,
        permissionLabel: String(permissionLabel(permissionMode)),
        relayPermissionLabel: String(permissionLabel(permissionMode)),
        mcpControlMode:
          session.mcp_control_mode || ai?.mcp_control_mode || null,
        mcpControlLabel: String(
          mcpControlModeLabel(session.mcp_control_mode || ai?.mcp_control_mode),
        ),
        supportsAgentShutdown: Boolean(agent?.supports_agent_shutdown),
        agentGeneration:
          agent?.connection_generation || session.connection_generation || 0,
        connectTime: formatApiDate(session.last_seen),
        lastHeartbeat: formatApiDate(session.last_seen),
        pendingApprovals: session.pending_approvals || 0,
        inflightRequests: session.in_flight_requests || 0,
        generation: session.connection_generation || 0,
        leaseExpire: formatApiDate(session.lease_expires_at),
        isClosed: bindings.length === 0,
        status:
          bindings.length === 0
            ? "CLOSED"
            : session.state === "online"
              ? "ACTIVE"
              : "DEGRADED",
      };
    }),
  );
  auditLogs.splice(
    0,
    auditLogs.length,
    ...(rawAudit || []).map((event) => ({
      id: event.id,
      time: formatApiDate(event.timestamp),
      operator: String(event.source || "admin_api"),
      action: String(String(event.action || "").toUpperCase()),
      target: String(event.target || "-"),
      result: event.success ? "SUCCESS" : "FAILED",
      ip: String(
        event.source && event.source.includes(".") ? event.source : "127.0.0.1",
      ),
      details: String(event.summary || "-"),
    })),
  );

  recentEvents.splice(
    0,
    recentEvents.length,
    ...(rawAudit || []).slice(0, 5).map((event) => ({
      time: formatApiDate(event.timestamp).slice(11, 19),
      type: String(String(event.action || "SYSTEM").toUpperCase()),
      desc: String(event.summary || String(event.action || "系统事件")),
      badge: event.success ? "online" : "danger",
    })),
  );
}

async function refreshRealData() {
  state.api.loading = true;
  state.api.error = null;

  if (!state.api.baseUrl) {
    state.api.baseUrl = window.location.origin;
  }

  try {
    const [overview, identity, rawAgents, rawSessions, rawAudit] =
      await Promise.all([
        apiFetch("/api/admin/overview"),
        apiFetch("/api/admin/identity"),
        apiFetch("/api/admin/agents"),
        apiFetch("/api/admin/sessions"),
        apiFetch("/api/admin/audit?limit=200"),
      ]);
    applyApiData(overview, identity, rawAgents, rawSessions, rawAudit);
    state.api.connected = true;
    state.api.error = null;
  } catch (error) {
    state.api.connected = false;
    state.api.error = error.message;
    throw error;
  } finally {
    state.api.loading = false;
  }
}

function applyTheme() {
  const dark =
    state.theme === "dark" ||
    (state.theme === "system" &&
      window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
  document.documentElement.style.colorScheme = dark ? "dark" : "light";
}

function setTheme(value) {
  if (!["system", "light", "dark"].includes(value)) return;
  state.theme = value;
  try {
    localStorage.setItem("remoteops-theme", value);
  } catch (_) {}
  applyTheme();
}

function openSessionDrawer(sessionId) {
  const sess = sessions.find((s) => s.id === sessionId);
  if (!sess) return;
  state.activeDrawerAgent = null;
  state.activeDrawerSession = sess;
}

function openAgentDrawer(agentId) {
  const agent = agents.find((item) => item.id === agentId);
  if (!agent) return;
  if (agent.sessionId !== "None") {
    openSessionDrawer(agent.sessionId);
    return;
  }
  // Open dedicated Agent drawer if no active session
  state.activeDrawerSession = null;
  state.activeDrawerAgent = agent;
}

function closeDrawer() {
  state.activeDrawerSession = null;
  state.activeDrawerAgent = null;
}

/** @param {string} modalName @param {import("../console-types").ModalTarget|null} data */
function openModal(modalName, data = null) {
  if (state.shutdownBusy || state.toolCenter?.busy) return;
  clearMcpSecret();
  if (modalName === "shutdownAgent") {
    const agent = agents.find((item) => item.id === data?.id);
    if (
      !canShutdownAgent(agent) ||
      Number(agent.generation) !== Number(data.generation)
    ) {
      showToast("节点连接已变化，请刷新后重试", "error");
      return;
    }
    data = { targets: [snapshotAgent(agent)], results: null };
  }
  state.activeModal = modalName;
  state.modalTargetData = data;
  state.stopInputText = "";
}

function closeModal() {
  if (state.shutdownBusy || state.toolCenter.busy) return;
  clearMcpSecret();
  const tc = state.toolCenter;
  const returning =
    tc.returnToDetails &&
    state.activeModal?.startsWith("toolCenter") &&
    tc.snapshot?.tools.some((tool) => tool.id === tc.selected);
  tc.returnToDetails = false;
  state.activeModal = returning ? "toolDetails" : null;
  state.modalTargetData = null;
  state.stopInputText = "";
}

async function confirmTerminateSession() {
  if (!state.modalTargetData?.id) return;
  const sessionId = state.modalTargetData.id;
  closeModal();
  try {
    if (!state.demoMode) {
      await apiFetch(
        `/api/admin/sessions/${encodeURIComponent(sessionId)}/close`,
        { method: "POST" },
      );
      await refreshRealData();
    } else {
      const session = sessions.find((item) => item.id === sessionId);
      if (session) {
        session.isClosed = true;
        session.status = "CLOSED";
        session.controllerType = "None";
        session.controllerHostname = "-";
        session.controllerInstanceId = "-";
        session.humanController = "None";
        session.pendingApprovals = 0;
        session.inflightRequests = 0;
      }
    }
    showToast(`会话 ${truncate(sessionId, 6, 4)} 已安全关闭`, "success");
    closeDrawer();
  } catch (error) {
    showToast(`关闭会话失败：${error.message}`, "error");
  }
}

async function confirmEmergencyStop() {
  if (!state.modalTargetData?.id) return;
  const sessionId = state.modalTargetData.id;
  closeModal();
  try {
    if (!state.demoMode) {
      await apiFetch(
        `/api/admin/sessions/${encodeURIComponent(sessionId)}/emergency-stop`,
        { method: "POST" },
      );
    }
    showToast(
      `🛑 紧急停止信号已发送至会话 ${truncate(sessionId, 6, 4)}`,
      "error",
    );
    closeDrawer();
    await refreshRealData();
  } catch (error) {
    showToast(`紧急停止失败：${error.message}`, "error");
  }
}

function canShutdownAgent(agent) {
  return Boolean(
    agent &&
    agent.status === "online" &&
    agent.supportsAgentShutdown &&
    Number(agent.generation) > 0,
  );
}

function snapshotAgent(agent) {
  return {
    id: agent.id,
    generation: Number(agent.generation),
    hostname: agent.hostname,
  };
}

function reconcileAgentSelection() {
  for (const [id, generation] of state.selectedAgents) {
    const current = agents.find((agent) => agent.id === id);
    if (!canShutdownAgent(current) || Number(current.generation) !== generation)
      state.selectedAgents.delete(id);
  }
}

function filteredAgents() {
  const keyword = state.agentKeyword.trim().toLowerCase();
  return agents.filter(
    (agent) =>
      (state.agentStatus === "ALL" || agent.status === state.agentStatus) &&
      (!keyword ||
        [agent.hostname, agent.id, agent.os].some((value) =>
          String(value).toLowerCase().includes(keyword),
        )),
  );
}

function selectAgent(id, checked) {
  const agent = agents.find((item) => item.id === id);
  if (checked && canShutdownAgent(agent))
    state.selectedAgents.set(id, Number(agent.generation));
  else state.selectedAgents.delete(id);
}

function selectVisibleAgents(checked) {
  filteredAgents()
    .filter(canShutdownAgent)
    .forEach((agent) => {
      if (checked) state.selectedAgents.set(agent.id, Number(agent.generation));
      else state.selectedAgents.delete(agent.id);
    });
}

function openAgentShutdown(all = false) {
  reconcileAgentSelection();
  const targets = agents
    .filter(
      (agent) =>
        canShutdownAgent(agent) && (all || state.selectedAgents.has(agent.id)),
    )
    .map(snapshotAgent);
  if (targets.length)
    openModal("shutdownAgents", { targets, results: null, all });
}

async function confirmShutdownAgent() {
  if (
    state.shutdownBusy ||
    !state.modalTargetData?.targets?.length ||
    state.modalTargetData.results
  )
    return;
  const data = state.modalTargetData;
  state.shutdownBusy = true;
  data.results = [];

  // 固定确认时的连接代次，逐个请求避免误关新连接或瞬间放大负载。
  try {
    for (const target of data.targets) {
      const current = agents.find((agent) => agent.id === target.id);
      try {
        if (
          !canShutdownAgent(current) ||
          Number(current.generation) !== target.generation
        )
          throw new Error("连接已变化或离线，请刷新后重新选择");
        if (!state.demoMode) {
          await apiFetch(
            `/api/admin/agents/${encodeURIComponent(target.id)}/shutdown`,
            {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({
                agent_instance_id: target.id,
                connection_generation: target.generation,
              }),
            },
          );
        } else {
          current.status = "offline";
        }
        state.selectedAgents.delete(target.id);
        data.results.push({ ...target, ok: true, message: "关闭指令已接受" });
      } catch (error) {
        data.results.push({ ...target, ok: false, message: error.message });
      }
    }
    if (!state.demoMode) {
      try {
        await refreshRealData();
      } catch (error) {
        data.refreshError = error.message;
      }
    }
    reconcileAgentSelection();
  } finally {
    state.shutdownBusy = false;
  }
}

async function confirmPurgeClosedSessions() {
  const clearable = sessions.filter(
    (session) => session.isClosed && session.agentStatus !== "online",
  );
  if (clearable.length === 0) {
    closeModal();
    showToast("没有可清除的已关闭会话", "info");
    return;
  }

  closeModal();
  try {
    if (state.demoMode) {
      sessions.splice(
        0,
        sessions.length,
        ...sessions.filter((session) => !clearable.includes(session)),
      );
    } else {
      await apiFetch("/api/admin/sessions/closed/clear", { method: "POST" });
      await refreshRealData();
    }
    showToast(`已清除 ${clearable.length} 条已关闭会话记录`, "success");
  } catch (error) {
    showToast(`清除已关闭会话失败：${error.message}`, "error");
  }
}

/** @type {import("../console-types").Toast[]} */
const toasts = reactive([]);

function showToast(message, type = "success") {
  const id = crypto.randomUUID();
  if (toasts.length >= 3) toasts.shift();
  toasts.push({ id, message, type });
  setTimeout(() => {
    const i = toasts.findIndex((t) => t.id === id);
    if (i >= 0) toasts.splice(i, 1);
  }, 4500);
}

async function copyToClipboard(text, label = "内容") {
  try {
    await navigator.clipboard.writeText(String(text));
    showToast(`${label}已复制`);
  } catch {
    showToast("复制失败，请手动复制", "error");
  }
}

function navigateTo(page) {
  void navigation.push(
    page === "tools" ? `/tools/${state.toolCenter.page}` : `/${page}`,
  );
}

function setAgentFilter(field, value) {
  if (["agentKeyword", "agentStatus"].includes(field)) state[field] = value;
}

async function refreshCurrent() {
  try {
    await refreshRealData();
    if (state.currentTab === "tools") await refreshToolsData();
    if (state.currentTab === "mcp") await refreshMcpData();
  } catch {
    showToast("读取失败，请检查连接后重试", "error");
  }
}

async function activatePage(page, section = "settings") {
  invalidateMcpView();
  invalidateToolsView();
  closeDrawer();
  state.currentTab = page;
  state.toolCenter.page = section;
  if (!state.isLoggedIn) return;
  if (page === "mcp") await refreshMcpData();
  if (page === "tools") await refreshToolsData();
}

async function restoreSession() {
  state.api.baseUrl = window.location.origin;
  try {
    const result = await apiFetch("/api/admin/session");
    if (!result.authenticated) return;
    state.adminUser = result.username || "";
    state.isLoggedIn = true;
    await refreshCurrent();
  } catch {
    state.api.connected = false;
  }
}

async function login(username, password) {
  const result = await apiFetch("/api/admin/login", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ username, password }),
  });
  state.adminUser = result.username || username;
  state.isLoggedIn = true;
  await refreshCurrent();
}

async function logout() {
  invalidateMcpView(true);
  invalidateToolsView();
  closeDrawer();
  state.isLoggedIn = false;
  state.activeModal = null;
  state.modalTargetData = null;
  state.api.connected = false;
  state.api.token = "";
  try {
    await apiFetch("/api/admin/logout", { method: "POST" });
  } catch {}
  void navigation.replace("/overview");
}

async function changePassword(
  current_password,
  new_password,
  confirm_password,
) {
  await apiFetch("/api/admin/password", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ current_password, new_password, confirm_password }),
  });
  invalidateMcpView(true);
  invalidateToolsView();
  closeDrawer();
  state.isLoggedIn = false;
  state.activeModal = null;
  state.modalTargetData = null;
  state.api.connected = false;
  showToast("密码修改成功，请重新登录");
}

export {
  formatApiDate,
  formatUptime,
  formatLeaseRemaining,
  isLeaseActive,
  formatMaskedUuid,
  truncate,
  permissionLabel,
  mcpControlModeLabel,
  sessionColumnDefinitions,
  agentColumnDefinitions,
  toggleVisibleColumn,
  restoreVisibleColumns,
  apiFetch,
  applyApiData,
  refreshRealData,
  applyTheme,
  setTheme,
  openSessionDrawer,
  openAgentDrawer,
  closeDrawer,
  openModal,
  closeModal,
  confirmTerminateSession,
  confirmEmergencyStop,
  canShutdownAgent,
  snapshotAgent,
  reconcileAgentSelection,
  filteredAgents,
  selectAgent,
  selectVisibleAgents,
  openAgentShutdown,
  confirmShutdownAgent,
  confirmPurgeClosedSessions,
  toasts,
  showToast,
  copyToClipboard,
  navigateTo,
  setAgentFilter,
  refreshCurrent,
  activatePage,
  restoreSession,
  login,
  logout,
  changePassword,
};
