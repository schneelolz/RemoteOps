/**
 * RemoteOps Relay Admin - Production Management Console
 */

// Global Application State
const state = {
  theme: 'dark',
  currentTab: 'overview', // 'overview', 'sessions', 'agents', 'identity', 'settings', 'audit'
  isLoggedIn: false,
  adminUser: '',
  activeDrawerSession: null,
  activeModal: null, // 'terminateSession', 'emergencyStop'
  modalTargetData: null,
  stopInputText: '',
  filters: {
    keyword: '',
    status: 'ALL',
    controllerType: 'ALL',
    permissionMode: 'ALL',
    auditType: 'ALL',
    auditResult: 'ALL'
  },
  api: {
    baseUrl: '',
    token: '',
    connected: false,
    loading: false,
    error: null
  }
};

// Data Containers (populated strictly by /api/admin/*)
const relayInfo = {
  address: '-',
  host: '-',
  version: '-',
  uptime: '-',
  uptimeSeconds: 0,
  onlineAgents: 0,
  activeSessions: 0,
  connectedControllers: 0,
  pendingApprovals: 0,
  inFlightRequests: 0,
  ownerUuid: '',
  aiTokenConfigured: false,
  aiTokenFingerprint: '',
  humanTokenConfigured: false
};

let sessions = [];
const agents = [];
let auditLogs = [];
const recentEvents = [];

// All values originating in Relay/API responses or operator input pass through this
// helper before being interpolated into an HTML template. Event-handler arguments
// additionally use encodeURIComponent below so quotes cannot escape an attribute.
function escapeHtml(value) {
  return String(value ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function safeEventValue(value) {
  return encodeURIComponent(String(value ?? ''));
}

function eventValue(value) {
  return `decodeURIComponent('${safeEventValue(value)}')`;
}

function formatApiDate(value) {
  if (!value) return '-';
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString('zh-CN', { hour12: false });
}

function formatUptime(seconds) {
  if (seconds === undefined || seconds === null || Number.isNaN(seconds)) return '-';
  const sec = Number(seconds);
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = sec % 60;
  if (d > 0) return `${d}天 ${h}小时 ${m}分`;
  if (h > 0) return `${h}小时 ${m}分 ${s}秒`;
  return `${m}分 ${s}秒`;
}

function formatMaskedUuid(uuid) {
  if (!uuid) return '-';
  const str = String(uuid).trim();
  if (str.length <= 12) return str;
  return `${str.slice(0, 6)}...${str.slice(-4)}`;
}

function truncate(str, head = 8, tail = 6) {
  if (!str) return '';
  if (str.length <= head + tail + 3) return str;
  return `${str.substring(0, head)}...${str.substring(str.length - tail)}`;
}

function permissionLabel(value) {
  return {
    read_only: '只读模式',
    approval_required: '写操作需审批',
    controller_approved: 'Controller 已批准',
    full_access: 'Owner 全权限'
  }[value] || value || '-';
}

async function apiFetch(path, options = {}) {
  const headers = { ...(options.headers || {}) };
  if (state.api.token) headers.Authorization = `Bearer ${state.api.token}`;
  const response = await fetch(`${state.api.baseUrl}${path}`, {
    ...options,
    credentials: 'include',
    headers
  });
  if (!response.ok) {
    let detail = `HTTP ${response.status}`;
    try { detail = (await response.json()).error || detail; } catch (_) { /* response may not be JSON */ }
    throw new Error(detail);
  }
  return response.json();
}

function applyApiData(overview, identity, rawAgents, rawSessions, rawAudit) {
  relayInfo.address = escapeHtml(window.location.hostname || '127.0.0.1');
  relayInfo.host = escapeHtml(window.location.host || '127.0.0.1');
  relayInfo.ownerUuid = escapeHtml(String(identity?.owner_id || overview?.owner_id || ''));
  relayInfo.aiTokenConfigured = Boolean(identity?.ai_token_configured);
  relayInfo.aiTokenFingerprint = escapeHtml(String(identity?.ai_token_fingerprint || ''));
  relayInfo.humanTokenConfigured = Boolean(identity?.human_token_configured);
  relayInfo.version = escapeHtml(String(overview?.version || '-'));
  relayInfo.uptimeSeconds = overview?.uptime_seconds || 0;
  relayInfo.uptime = formatUptime(overview?.uptime_seconds || 0);
  relayInfo.onlineAgents = overview?.online_agents ?? 0;
  relayInfo.activeSessions = overview?.active_sessions ?? 0;
  relayInfo.connectedControllers = overview?.connected_controllers ?? 0;
  relayInfo.pendingApprovals = overview?.pending_approvals ?? 0;
  relayInfo.inFlightRequests = overview?.in_flight_requests ?? 0;

  const agentById = new Map((rawAgents || []).map(agent => [String(agent.agent_instance_id), agent]));

  agents.splice(0, agents.length, ...(rawAgents || []).map(agent => ({
    id: escapeHtml(String(agent.agent_instance_id)),
    hostname: escapeHtml(agent.hostname || '-'),
    os: escapeHtml(agent.operating_system || '-'),
    status: agent.state === 'online' ? 'online' : 'offline',
    sessionId: agent.session_id ? escapeHtml(String(agent.session_id)) : 'None',
    codeStatus: agent.pairing_code_configured ? '控制码已生成' : '控制码未生成',
    lease: formatApiDate(agent.lease_expires_at),
    heartbeat: formatApiDate(agent.last_seen),
    everPaired: Boolean(agent.ever_paired),
    ready: Boolean(agent.ready),
    permissionMode: agent.permission_mode,
    permissionLabel: escapeHtml(permissionLabel(agent.permission_mode))
  })));

  sessions = (rawSessions || []).map(session => {
    const bindings = session.controller_bindings || [];
    const ai = bindings.find(binding => binding.kind === 'ai');
    const human = bindings.find(binding => binding.kind === 'human');
    const agent = agentById.get(String(session.agent_instance_id));
    const permissionMode = session.permission_mode || 'approval_required';
    return {
      id: escapeHtml(String(session.session_id)),
      agentName: escapeHtml(agent?.hostname || session.hostname || 'Unknown'),
      agentId: escapeHtml(String(session.agent_instance_id)),
      agentStatus: session.state === 'online' ? 'online' : 'offline',
      hostname: escapeHtml(session.hostname || '-'),
      os: escapeHtml(session.operating_system || '-'),
      role: escapeHtml(session.role || 'default'),
      controllerType: ai ? 'AI (MCP)' : human ? 'Human' : 'None',
      controllerName: escapeHtml(ai ? `AI (${String(ai.controller_instance_id).slice(0, 8)})` : human ? `Human (${String(human.controller_instance_id).slice(0, 8)})` : 'None'),
      controllerInstanceId: escapeHtml(String((ai || human)?.controller_instance_id || '-')),
      humanController: escapeHtml(human ? `Connected (${String(human.controller_instance_id).slice(0, 8)})` : 'None'),
      ownerUuid: escapeHtml(session.owner_id ? String(session.owner_id) : relayInfo.ownerUuid),
      permissionMode,
      permissionLabel: escapeHtml(permissionLabel(permissionMode)),
      connectTime: formatApiDate(session.last_seen),
      lastHeartbeat: formatApiDate(session.last_seen),
      pendingApprovals: session.pending_approvals || 0,
      inflightRequests: session.in_flight_requests || 0,
      generation: session.connection_generation || 0,
      leaseExpire: formatApiDate(session.lease_expires_at),
      status: session.state === 'online' ? 'ACTIVE' : 'DEGRADED'
    };
  });

  auditLogs = (rawAudit || []).map(event => ({
    id: event.id,
    time: formatApiDate(event.timestamp),
    operator: escapeHtml(event.source || 'admin_api'),
    action: escapeHtml(String(event.action || '').toUpperCase()),
    target: escapeHtml(event.target || '-'),
    result: event.success ? 'SUCCESS' : 'FAILED',
    ip: escapeHtml(event.source && event.source.includes('.') ? event.source : '-'),
    details: escapeHtml(event.summary || '-')
  }));

  recentEvents.splice(0, recentEvents.length, ...(rawAudit || []).slice(0, 5).map(event => ({
    time: formatApiDate(event.timestamp).slice(11, 19),
    type: escapeHtml(String(event.action || 'SYSTEM').toUpperCase()),
    desc: escapeHtml(event.summary || String(event.action || '系统事件')),
    badge: event.success ? 'online' : 'danger'
  })));
}

async function refreshRealData() {
  if (!state.api.baseUrl) {
    state.api.baseUrl = window.location.origin;
  }
  state.api.loading = true;
  state.api.error = null;
  try {
    const [overview, identity, rawAgents, rawSessions, rawAudit] = await Promise.all([
      apiFetch('/api/admin/overview'),
      apiFetch('/api/admin/identity'),
      apiFetch('/api/admin/agents'),
      apiFetch('/api/admin/sessions'),
      apiFetch('/api/admin/audit?limit=200')
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

async function restoreSession() {
  state.api.baseUrl = window.location.origin;
  try {
    const session = await apiFetch('/api/admin/session');
    if (!session.authenticated) throw new Error('管理 Session 已失效');
    if (session.username) state.adminUser = session.username;
    await refreshRealData();
    state.isLoggedIn = true;
    const hashMatch = window.location.hash.match(/tab=([a-z]+)/);
    if (hashMatch && ['overview', 'sessions', 'agents', 'identity', 'settings', 'audit'].includes(hashMatch[1])) {
      state.currentTab = hashMatch[1];
    }
  } catch (_) {
    state.isLoggedIn = false;
    state.api.connected = false;
  }
  renderApp();
}

async function handleManualRefresh() {
  const btn = document.getElementById('topbar-refresh-btn');
  if (btn) btn.disabled = true;
  try {
    await refreshRealData();
    showToast('已刷新 Relay 最新状态', 'success');
  } catch (error) {
    showToast(`刷新失败：${error.message}`, 'error');
  } finally {
    if (btn) btn.disabled = false;
    renderApp();
  }
}

// Toast System
function showToast(message, type = 'success') {
  const container = document.getElementById('toast-container');
  if (!container) return;

  const toast = document.createElement('div');
  toast.className = `toast toast-${type}`;

  const iconSvg = type === 'success'
    ? `<svg class="w-4 h-4 text-emerald-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="20 6 9 17 4 12"/></svg>`
    : `<svg class="w-4 h-4 text-red-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>`;

  toast.innerHTML = iconSvg;
  const toastText = document.createElement('span');
  toastText.textContent = String(message ?? '');
  toast.appendChild(toastText);
  container.appendChild(toast);

  setTimeout(() => {
    toast.style.opacity = '0';
    toast.style.transform = 'translateY(10px)';
    toast.style.transition = 'all 0.2s ease';
    setTimeout(() => toast.remove(), 200);
  }, 2600);
}

// Clipboard Helper
function copyToClipboard(text, label = '内容') {
  navigator.clipboard.writeText(text).then(() => {
    showToast(`已复制 ${label} 到剪贴板`, 'success');
  }).catch(() => {
    showToast(`复制失败，请手动选择复制`, 'error');
  });
}

// Theme Switcher
function toggleTheme() {
  state.theme = state.theme === 'dark' ? 'light' : 'dark';
  document.documentElement.setAttribute('data-theme', state.theme);
  renderApp();
  showToast(`已切换至 ${state.theme === 'dark' ? '深色' : '浅色'} 主题`, 'info');
}

// Tab Navigation
function navigateTo(tabName) {
  state.currentTab = tabName;
  state.activeDrawerSession = null;
  state.activeModal = null;
  state.modalTargetData = null;
  window.history.replaceState(null, '', `#tab=${encodeURIComponent(tabName)}`);
  renderApp();
  if (state.api.connected) refreshRealData().then(renderApp).catch(() => renderApp());
}

function logout() {
  fetch(`${state.api.baseUrl}/api/admin/logout`, { method: 'POST', credentials: 'same-origin' }).catch(() => {});
  state.isLoggedIn = false;
  state.api.token = '';
  state.api.connected = false;
  state.activeDrawerSession = null;
  state.activeModal = null;
  state.modalTargetData = null;
  document.getElementById('drawer-backdrop')?.remove();
  document.getElementById('drawer-panel')?.remove();
  document.getElementById('modal-backdrop')?.remove();
  document.getElementById('modal-panel')?.remove();
  window.history.replaceState(null, '', `${window.location.pathname}${window.location.search}`);
  renderApp();
  showToast('已退出登录', 'info');
}

// Drawer Controls
function openSessionDrawer(sessionId) {
  const sess = sessions.find(s => s.id === sessionId);
  if (!sess) return;
  state.activeDrawerSession = sess;
  renderDrawer();
}

function closeSessionDrawer() {
  state.activeDrawerSession = null;
  renderDrawer();
}

// Modal Controls
function openModal(modalName, data = null) {
  state.activeModal = modalName;
  state.modalTargetData = data;
  state.stopInputText = '';
  renderModal();
}

function closeModal() {
  state.activeModal = null;
  state.modalTargetData = null;
  state.stopInputText = '';
  renderModal();
}

// Actions: Terminate Session (Real API Only)
async function confirmTerminateSession() {
  const targetId = state.modalTargetData?.id;
  if (!targetId) return;

  try {
    const result = await apiFetch(`/api/admin/sessions/${encodeURIComponent(targetId)}/close`, { method: 'POST' });
    showToast(result.message || '会话已关闭', result.success ? 'success' : 'error');
    closeModal();
    closeSessionDrawer();
    await refreshRealData();
    renderApp();
  } catch (error) {
    showToast(`关闭会话失败：${error.message}`, 'error');
  }
}

// Actions: Emergency Stop (Real API Only)
async function confirmEmergencyStop() {
  const targetId = state.modalTargetData?.id;
  if (!targetId) return;

  try {
    const result = await apiFetch(`/api/admin/sessions/${encodeURIComponent(targetId)}/emergency-stop`, { method: 'POST' });
    showToast(result.message || '已发送紧急停止请求', result.success ? 'success' : 'error');
    closeModal();
    closeSessionDrawer();
    await refreshRealData();
    renderApp();
  } catch (error) {
    showToast(`紧急停止失败：${error.message}`, 'error');
  }
}

// Render Top Bar
function renderTopBar() {
  const isOnline = state.api.connected;
  return `
    <header class="topbar">
      <div class="topbar-left">
        <div class="relay-meta">
          <div class="relay-name-box">
            <span class="relay-name">${escapeHtml(relayInfo.address)}</span>
            <span class="relay-addr font-mono">${escapeHtml(relayInfo.host)}</span>
          </div>
          <span class="badge ${isOnline ? 'badge-online' : 'badge-offline'}">
            <span class="badge-dot"></span>
            ${isOnline ? '在线 (Online)' : '未连接 (Offline)'}
          </span>
        </div>
      </div>

      <div class="topbar-right">
        <button id="topbar-refresh-btn" class="btn btn-secondary btn-sm" onclick="handleManualRefresh()" title="刷新 Relay 状态">
          <svg class="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/></svg>
          刷新
        </button>
        <button class="btn btn-secondary btn-sm" onclick="toggleTheme()" title="切换亮/暗色主题">
          ${state.theme === 'dark' ? '☀️ 亮色' : '🌙 深色'}
        </button>
        <div class="admin-pill">
          <span class="avatar">A</span>
          <span class="font-mono text-slate-300">${escapeHtml(state.adminUser || 'Admin')}</span>
        </div>
        <button class="btn btn-secondary btn-sm" onclick="navigateTo('settings')" title="修改管理页面密码">
          <svg class="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>
          安全设置
        </button>
        <button class="btn btn-ghost btn-sm text-slate-400 hover:text-red-400" onclick="logout()" title="退出登录">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9"/></svg>
        </button>
      </div>
    </header>
  `;
}

// Render Sidebar
function renderSidebar() {
  const activeCount = sessions.filter(s => s.status === 'ACTIVE').length;
  const agentCount = agents.length;
  const auditCount = auditLogs.length;

  const items = [
    { id: 'overview', name: '概览', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/></svg>' },
    { id: 'sessions', name: '会话管理', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16"/><circle cx="5" cy="19" r="1"/></svg>', badge: activeCount },
    { id: 'agents', name: 'Agent 节点', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>', badge: agentCount },
    { id: 'identity', name: 'Relay 身份凭据', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><circle cx="12" cy="11" r="3"/></svg>' },
    { id: 'settings', name: '安全设置', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z"/><path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06-1.9 1.9-.06-.06a1.7 1.7 0 0 0-1.88-.34 1.7 1.7 0 0 0-1.03 1.56V20h-2.7v-.09a1.7 1.7 0 0 0-1.03-1.56 1.7 1.7 0 0 0-1.88.34l-.06.06-1.9-1.9.06-.06A1.7 1.7 0 0 0 7.76 15a1.7 1.7 0 0 0-1.56-1.03H6v-2.7h.2A1.7 1.7 0 0 0 7.76 10a1.7 1.7 0 0 0-.34-1.88l-.06-.06 1.9-1.9.06.06a1.7 1.7 0 0 0 1.88.34 1.7 1.7 0 0 0 1.03-1.56V5h2.7v.09a1.7 1.7 0 0 0 1.03 1.56 1.7 1.7 0 0 0 1.88-.34l.06-.06 1.9 1.9-.06.06A1.7 1.7 0 0 0 19.4 10c.18.62.75 1.03 1.4 1.03h.2v2.7h-.2A1.7 1.7 0 0 0 19.4 15z"/></svg>' },
    { id: 'audit', name: '审计日志', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/><polyline points="10 9 9 9 8 9"/></svg>', badge: auditCount }
  ];

  return `
    <aside class="sidebar">
      <div class="sidebar-header">
        <div class="brand-badge">R</div>
        <div>
          <div class="brand-title">RemoteOps Relay</div>
          <div class="brand-sub">Admin Console</div>
        </div>
      </div>

      <nav class="sidebar-nav">
        ${items.map(item => `
          <button type="button" class="nav-item ${state.currentTab === item.id ? 'active' : ''}" ${state.currentTab === item.id ? 'aria-current="page"' : ''} onclick="navigateTo('${item.id}')">
            ${item.icon}
            <span>${item.name}</span>
            ${item.badge !== undefined ? `<span class="nav-badge">${item.badge}</span>` : ''}
          </button>
        `).join('')}
      </nav>

      <div class="sidebar-footer">
        <div style="display:flex; justify-content:space-between; align-items:center;">
          <span>版本: ${escapeHtml(relayInfo.version)}</span>
          <span class="badge badge-info" style="padding:1px 6px;">Technical Preview</span>
        </div>
        <div style="color:var(--text-muted); font-size:12px;">主机: ${escapeHtml(relayInfo.host)}</div>
      </div>
    </aside>
  `;
}

// Views: Overview
function renderOverviewView() {
  if (state.api.error) return renderErrorState(`管理 API 请求失败：${state.api.error}`);

  const activeSess = sessions.filter(s => s.status === 'ACTIVE').length;
  const onlineAgents = agents.filter(a => a.status === 'online').length;

  return `
    <div class="content-container">
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/></svg>
            <span>系统概览</span>
            <span class="page-title-sub">System Overview</span>
          </h1>
          <div class="page-desc">实时监控 Relay 运行指标、集群连接状态及近期安全审计事件</div>
        </div>
        <div style="display:flex; gap:8px;">
          <button class="btn btn-secondary btn-sm" onclick="navigateTo('sessions')">
            查看所有会话
          </button>
        </div>
      </div>

      <!-- Stat Cards -->
      <div class="grid-4">
        <div class="card stat-card">
          <div class="stat-header">
            <span>在线 Agent 节点</span>
            <svg class="w-4 h-4 text-emerald-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/></svg>
          </div>
          <div class="stat-value">${onlineAgents}</div>
          <div class="stat-footer">
            <span>已注册 Agent 共 ${agents.length} 个</span>
          </div>
        </div>

        <div class="card stat-card">
          <div class="stat-header">
            <span>活跃会话</span>
            <svg class="w-4 h-4 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16"/></svg>
          </div>
          <div class="stat-value">${activeSess}</div>
          <div class="stat-footer">
            <span>关联 Controller: ${relayInfo.connectedControllers}</span>
          </div>
        </div>

        <div class="card stat-card">
          <div class="stat-header">
            <span>已连接 Controller</span>
            <svg class="w-4 h-4 text-indigo-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><path d="m4.93 4.93 4.24 4.24M14.83 9.17l4.24-4.24M14.83 14.83l4.24 4.24M9.17 14.83l-4.24 4.24"/></svg>
          </div>
          <div class="stat-value">${relayInfo.connectedControllers}</div>
          <div class="stat-footer">
            <span>进行中请求: ${relayInfo.inFlightRequests}</span>
          </div>
        </div>

        <div class="card stat-card">
          <div class="stat-header">
            <span>待处理审批</span>
            <svg class="w-4 h-4 text-amber-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
          </div>
          <div class="stat-value" style="color:${relayInfo.pendingApprovals > 0 ? 'var(--status-degraded-text)' : 'var(--text-primary)'};">${relayInfo.pendingApprovals}</div>
          <div class="stat-footer">
            <span style="color:${relayInfo.pendingApprovals > 0 ? 'var(--status-degraded-text)' : 'var(--text-muted)'};">${relayInfo.pendingApprovals > 0 ? '存在等待确认的指令' : '暂无待审批指令'}</span>
          </div>
        </div>
      </div>

      <!-- Relay Details & Events -->
      <div class="grid-2">
        <!-- Relay Info Card -->
        <div class="card">
          <div class="section-title">
            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>
            <span>Relay 运行状态与指标</span>
          </div>
          <div class="key-value-list" style="margin-top:12px;">
            <div class="kv-item">
              <span class="kv-label">Relay 服务状态</span>
              <span class="kv-value">
                <span class="badge ${state.api.connected ? 'badge-online' : 'badge-offline'}">
                  <span class="badge-dot"></span>
                  ${state.api.connected ? '正常运行 (Online)' : '离线 (Offline)'}
                </span>
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">运行版本</span>
              <span class="kv-value font-mono">${escapeHtml(relayInfo.version)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">连续运行时间</span>
              <span class="kv-value font-mono">${escapeHtml(relayInfo.uptime)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">管理端地址</span>
              <span class="kv-value font-mono text-blue-400">${escapeHtml(relayInfo.host)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">Owner 标识</span>
              <span class="kv-value font-mono">${escapeHtml(formatMaskedUuid(relayInfo.ownerUuid))}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">已连接 Controller 数</span>
              <span class="kv-value font-mono">${relayInfo.connectedControllers}</span>
            </div>
          </div>
        </div>

        <!-- Recent Events -->
        <div class="card">
          <div class="section-title">
            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 14 14"/></svg>
            <span>最近实时事件</span>
            <span class="section-sub">Realtime Events</span>
          </div>
          <div style="display:flex; flex-direction:column; gap:10px; margin-top:12px;">
            ${recentEvents.length === 0 ? '<div style="color:var(--text-muted); font-size:13px; padding:12px 0;">暂无实时事件</div>' : recentEvents.map(evt => `
              <div style="display:flex; align-items:flex-start; gap:10px; font-size:13px; padding:6px 0; border-bottom:1px solid var(--border-subtle);">
                <span class="font-mono text-slate-400" style="font-size:12px; flex-shrink:0;">${escapeHtml(evt.time)}</span>
                <span class="badge badge-${evt.badge}" style="font-size:12px; padding:1px 6px; flex-shrink:0;">${escapeHtml(evt.type)}</span>
                <span style="color:var(--text-primary); flex:1; font-size:13px;">${escapeHtml(evt.desc)}</span>
              </div>
            `).join('')}
          </div>
        </div>
      </div>

      <!-- Active Sessions Preview Table -->
      <div class="table-container">
        <div class="table-header-bar">
          <div class="table-title">
            <svg class="w-4 h-4 text-emerald-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 2v20M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6"/></svg>
            <span>活跃会话状态</span>
            <span class="section-sub">Active Sessions</span>
          </div>
          <button class="btn btn-secondary btn-sm" onclick="navigateTo('sessions')">查看全部 ${activeSess} 个活跃会话 →</button>
        </div>
        ${renderSessionTable(sessions.filter(s => s.status === 'ACTIVE').slice(0, 5))}
      </div>
    </div>
  `;
}

// Views: Session Management
function renderSessionsView() {
  if (state.api.error) return renderErrorState(`管理 API 请求失败：${state.api.error}`);

  let filteredSessions = [...sessions];

  if (state.filters.keyword) {
    const q = state.filters.keyword.toLowerCase();
    filteredSessions = filteredSessions.filter(s => s.id.toLowerCase().includes(q) || s.agentName.toLowerCase().includes(q) || s.agentId.toLowerCase().includes(q));
  }
  if (state.filters.status !== 'ALL') {
    filteredSessions = filteredSessions.filter(s => s.status === state.filters.status);
  }
  if (state.filters.controllerType !== 'ALL') {
    filteredSessions = filteredSessions.filter(s => s.controllerType.includes(state.filters.controllerType));
  }
  if (state.filters.permissionMode !== 'ALL') {
    filteredSessions = filteredSessions.filter(s => s.permissionMode === state.filters.permissionMode);
  }

  return `
    <div class="content-container">
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16"/><circle cx="5" cy="19" r="1"/></svg>
            <span>会话管理</span>
            <span class="page-title-sub">Session Management</span>
          </h1>
          <div class="page-desc">查看所有实时会话拓扑、权限模式、心跳租约，执行会话关闭与紧急停止</div>
        </div>
        <div style="display:flex; gap:8px;">
          <button class="btn btn-secondary btn-sm" onclick="handleManualRefresh()">
            <svg class="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/></svg>
            刷新列表
          </button>
        </div>
      </div>

      <!-- Filter Toolbar -->
      <div class="filter-toolbar">
        <div class="filter-left">
          <input
            type="text"
            class="input font-mono"
            placeholder="搜索 Session ID / Agent..."
            value="${escapeHtml(state.filters.keyword)}"
            style="width: 240px;"
            oninput="state.filters.keyword = this.value; renderApp();"
          />

          <select class="select" onchange="state.filters.status = this.value; renderApp();">
            <option value="ALL" ${state.filters.status === 'ALL' ? 'selected' : ''}>全部状态</option>
            <option value="ACTIVE" ${state.filters.status === 'ACTIVE' ? 'selected' : ''}>活跃 (Active)</option>
            <option value="DEGRADED" ${state.filters.status === 'DEGRADED' ? 'selected' : ''}>异常 / 降级</option>
          </select>

          <select class="select" onchange="state.filters.controllerType = this.value; renderApp();">
            <option value="ALL" ${state.filters.controllerType === 'ALL' ? 'selected' : ''}>全部 Controller 类型</option>
            <option value="AI" ${state.filters.controllerType === 'AI' ? 'selected' : ''}>AI (MCP)</option>
            <option value="Human" ${state.filters.controllerType === 'Human' ? 'selected' : ''}>Human Controller</option>
            <option value="None" ${state.filters.controllerType === 'None' ? 'selected' : ''}>无绑定</option>
          </select>

          <select class="select" onchange="state.filters.permissionMode = this.value; renderApp();">
            <option value="ALL" ${state.filters.permissionMode === 'ALL' ? 'selected' : ''}>全部权限模式</option>
            <option value="approval_required" ${state.filters.permissionMode === 'approval_required' ? 'selected' : ''}>写操作需审批</option>
            <option value="read_only" ${state.filters.permissionMode === 'read_only' ? 'selected' : ''}>只读模式</option>
            <option value="controller_approved" ${state.filters.permissionMode === 'controller_approved' ? 'selected' : ''}>Controller 已批准</option>
            <option value="full_access" ${state.filters.permissionMode === 'full_access' ? 'selected' : ''}>Owner 全权限</option>
          </select>

          ${(state.filters.keyword || state.filters.status !== 'ALL' || state.filters.controllerType !== 'ALL' || state.filters.permissionMode !== 'ALL') ? `
            <button class="btn btn-ghost btn-sm" onclick="state.filters = { keyword: '', status: 'ALL', controllerType: 'ALL', permissionMode: 'ALL' }; renderApp();">
              ✕ 重置筛选
            </button>
          ` : ''}
        </div>

        <div style="font-size:12.5px; color:var(--text-muted);">
          共找到 <span class="font-mono text-slate-200">${filteredSessions.length}</span> 个会话
        </div>
      </div>

      <!-- Table -->
      <div class="table-container">
        ${renderSessionTable(filteredSessions)}
      </div>
    </div>
  `;
}

// Session Table Component
function renderSessionTable(tableSessions) {
  if (!tableSessions || tableSessions.length === 0) {
    return renderEmptyState('暂无活跃会话', '当前 Relay 没有正在运行的 Session 记录');
  }

  return `
    <div class="table-wrapper">
      <table class="ops-table">
        <thead>
          <tr>
            <th>状态</th>
            <th>Session ID</th>
            <th>Agent 名称 / ID</th>
            <th>AI Controller (MCP)</th>
            <th>Human Controller</th>
            <th>Owner UUID</th>
            <th>权限模式</th>
            <th>最后心跳</th>
            <th style="text-align:right;">操作</th>
          </tr>
        </thead>
        <tbody>
          ${tableSessions.map(s => {
            const isOnline = s.agentStatus === 'online';
            return `
              <tr onclick="openSessionDrawer(${eventValue(s.id)})">
                <td>
                  <span class="badge ${isOnline ? 'badge-online' : 'badge-offline'}">
                    <span class="badge-dot"></span>
                    ${isOnline ? 'Active' : 'Offline'}
                  </span>
                </td>
                <td>
                  <span class="copyable-text" onclick="event.stopPropagation(); copyToClipboard(${eventValue(s.id)}, 'Session ID');" title="点击复制完整 Session ID">
                    ${escapeHtml(truncate(s.id, 8, 6))}
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                  </span>
                </td>
                <td>
                  <div style="font-weight:600; color:var(--text-primary); font-size:13px;">${escapeHtml(s.agentName)}</div>
                  <div class="font-mono" style="font-size:12px; color:var(--text-muted);">${escapeHtml(truncate(s.agentId, 8, 6))}</div>
                </td>
                <td>
                  ${s.controllerType.includes('AI') ? `
                    <div style="display:flex; align-items:center; gap:4px;">
                      <span class="badge badge-info" style="font-size:11px;">MCP</span>
                      <span style="font-size:12.5px; color:var(--text-primary);">${escapeHtml(s.controllerName)}</span>
                    </div>
                  ` : `<span style="color:var(--text-muted); font-size:12.5px;">未连接</span>`}
                </td>
                <td>
                  ${s.humanController !== 'None' ? `
                    <span class="badge badge-info" style="font-size:11.5px;">${escapeHtml(s.humanController)}</span>
                  ` : `<span style="color:var(--text-muted); font-size:12.5px;">无</span>`}
                </td>
                <td>
                  <span class="copyable-text" onclick="event.stopPropagation(); copyToClipboard(${eventValue(s.ownerUuid)}, 'Owner UUID');" title="点击复制 Owner UUID">
                    ${escapeHtml(formatMaskedUuid(s.ownerUuid))}
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                  </span>
                </td>
                <td>
                  <span class="tag" style="${s.permissionMode === 'full_access' ? 'border-color:var(--status-danger-border); color:var(--status-danger-text);' : ''}">${escapeHtml(s.permissionLabel)}</span>
                </td>
                <td>
                  <span class="font-mono text-slate-400">${escapeHtml(s.lastHeartbeat)}</span>
                </td>
                <td style="text-align:right;" onclick="event.stopPropagation();">
                  <div style="display:inline-flex; gap:6px;">
                    <button class="btn btn-secondary btn-sm" onclick="openSessionDrawer(${eventValue(s.id)})">详情</button>
                    <button class="btn btn-warning btn-sm" onclick="openModal('terminateSession', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })">关闭</button>
                    <button class="btn btn-danger btn-sm" onclick="openModal('emergencyStop', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })">🛑 紧急停止</button>
                  </div>
                </td>
              </tr>
            `;
          }).join('')}
        </tbody>
      </table>
    </div>
  `;
}

// Views: Agent Management
function renderAgentsView() {
  if (state.api.error) return renderErrorState(`管理 API 请求失败：${state.api.error}`);

  return `
    <div class="content-container">
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>
            <span>Agent 节点管理</span>
            <span class="page-title-sub">Agent Nodes</span>
          </h1>
          <div class="page-desc">查看已注册的 Agent 实例、控制码状态、操作系统及心跳租约</div>
        </div>
        <button class="btn btn-secondary btn-sm" onclick="handleManualRefresh()">
          <svg class="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/></svg>
          刷新
        </button>
      </div>

      <div class="table-container">
        ${agents.length === 0 ? renderEmptyState('暂无注册的 Agent', '当前 Relay 尚未接入任何客户端 Agent 节点') : `
          <div class="table-wrapper">
            <table class="ops-table">
              <thead>
                <tr>
                  <th>状态</th>
                  <th>Agent ID</th>
                  <th>主机名 (Hostname)</th>
                  <th>操作系统</th>
                  <th>绑定 Session ID</th>
                  <th>控制码状态</th>
                  <th>最近心跳</th>
                  <th>就绪状态</th>
                </tr>
              </thead>
              <tbody>
                ${agents.map(agt => `
                  <tr>
                    <td>
                      <span class="badge ${agt.status === 'online' ? 'badge-online' : 'badge-offline'}">
                        <span class="badge-dot"></span>
                        ${agt.status === 'online' ? 'Online' : 'Offline'}
                      </span>
                    </td>
                    <td class="font-mono text-slate-200 font-semibold">
                      <span class="copyable-text" onclick="copyToClipboard(${eventValue(agt.id)}, 'Agent ID')">
                        ${escapeHtml(truncate(agt.id, 8, 6))}
                        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                      </span>
                    </td>
                    <td class="font-mono text-slate-300">${escapeHtml(agt.hostname)}</td>
                    <td style="color:var(--text-muted); font-size:12.5px;">${escapeHtml(agt.os)}</td>
                    <td class="font-mono">
                      ${agt.sessionId !== 'None' ? `<span class="copyable-text" onclick="copyToClipboard(${eventValue(agt.sessionId)}, 'Session ID')">${escapeHtml(truncate(agt.sessionId, 8, 6))}</span>` : '<span style="color:var(--text-muted);">-</span>'}
                    </td>
                    <td>
                      <span class="tag">${escapeHtml(agt.codeStatus)}</span>
                    </td>
                    <td class="font-mono text-slate-400">${escapeHtml(agt.heartbeat)}</td>
                    <td>
                      <span class="badge ${agt.ready ? 'badge-online' : 'badge-degraded'}">
                        ${agt.ready ? 'Ready' : 'Not Ready'}
                      </span>
                    </td>
                  </tr>
                `).join('')}
              </tbody>
            </table>
          </div>
        `}
      </div>
    </div>
  `;
}

// Views: Relay Identity & Credentials
function renderIdentityView() {
  if (state.api.error) return renderErrorState(`管理 API 请求失败：${state.api.error}`);

  return `
    <div class="content-container">
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><circle cx="12" cy="11" r="3"/></svg>
            <span>Relay 身份与凭据</span>
            <span class="page-title-sub">Identity & Credentials</span>
          </h1>
          <div class="page-desc">查看 Relay 核心身份 Owner UUID 及 Controller 凭据配置状态</div>
        </div>
      </div>

      <div class="identity-layout">
        <!-- Left Column: Owner 身份 Card -->
        <div class="card">
          <div class="section-title">
            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>
            <span>Owner 身份标识</span>
            <span class="section-sub">Controller Owner ID</span>
          </div>
          <p class="section-desc">
            Owner UUID 是此 Relay 部署绑定的全局唯一 Controller Owner 标识。AI Controller (MCP) 与 Human Controller 接入时必须匹配此身份。
          </p>

          <div style="display:flex; flex-direction:column; gap:14px;">
            <div class="input-group">
              <div class="input-label">完整 Owner UUID</div>
              <div class="code-box">
                <span class="font-mono text-break">${escapeHtml(relayInfo.ownerUuid || '-')}</span>
                <button class="btn btn-secondary btn-sm" onclick="copyToClipboard(${eventValue(relayInfo.ownerUuid)}, 'Owner UUID')">
                  <svg class="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                  复制 UUID
                </button>
              </div>
            </div>

            <div class="identity-meta-row">
              <span>脱敏摘要: <strong class="font-mono text-slate-300">${escapeHtml(formatMaskedUuid(relayInfo.ownerUuid))}</strong></span>
              <span>管理端地址: <strong class="font-mono text-slate-300">${escapeHtml(relayInfo.host)}</strong></span>
            </div>
          </div>
        </div>

        <!-- Right Column: Controller 凭据 Card -->
        <div class="card">
          <div class="section-title">
            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 2l-2 2m-7.61 7.61a5.5 5.5 0 1 1-7.778 7.778 5.5 5.5 0 0 1 7.777-7.777zm0 0L15.5 7.5m0 0l3 3L22 7l-3-3m-3.5 3.5L19 4"/></svg>
            <span>Controller 凭据状态</span>
            <span class="section-sub">Authentication Credentials</span>
          </div>
          <p class="section-desc">
            Relay 支持 AI Controller (MCP) 与 Human Controller 凭据鉴权。后台仅展示只读配置状态与 SHA-256 指纹。
          </p>

          <!-- AI Controller Token Section -->
          <div class="credential-item">
            <div class="credential-title-group">
              <span style="font-weight:600; font-size:13.5px; color:var(--text-primary);">AI Controller Token (MCP)</span>
              <span class="badge ${relayInfo.aiTokenConfigured ? 'badge-online' : 'badge-offline'}">
                <span class="badge-dot"></span>
                ${relayInfo.aiTokenConfigured ? '已配置 (Configured)' : '未配置 (Not Configured)'}
              </span>
            </div>

            <div class="key-value-list" style="margin-top:8px;">
              <div class="kv-item">
                <span class="kv-label">Token 指纹 (SHA-256)</span>
                <span class="kv-value font-mono">
                  ${relayInfo.aiTokenFingerprint ? `
                    <span class="copyable-text" onclick="copyToClipboard(${eventValue(relayInfo.aiTokenFingerprint)}, 'Token 指纹')" title="点击复制完整指纹">
                      <span class="text-break">${escapeHtml(relayInfo.aiTokenFingerprint)}</span>
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                    </span>
                  ` : '<span style="color:var(--text-muted);">-</span>'}
                </span>
              </div>
            </div>
          </div>

          <!-- Human Controller Token Compact Info Row -->
          <div class="credential-item" style="margin-top:16px; border-top:1px solid var(--border-subtle); padding-top:14px;">
            <div class="credential-title-group">
              <span style="font-weight:600; font-size:13.5px; color:var(--text-primary);">Human Controller Token</span>
              <span class="badge ${relayInfo.humanTokenConfigured ? 'badge-online' : 'badge-offline'}">
                <span class="badge-dot"></span>
                ${relayInfo.humanTokenConfigured ? '已配置 (Configured)' : '未配置 (Not Configured)'}
              </span>
            </div>
            <div class="credential-note" style="margin-top:6px;">
              供运维操作员人工直连控制或独立授权校验使用。
            </div>
          </div>

          <!-- Brief Note on Token Configuration -->
          <div class="token-management-note" style="margin-top:16px;">
            <svg class="w-4 h-4 text-slate-400" style="flex-shrink:0; margin-top:2px;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>
            <span>Controller 认证令牌由 Relay 服务端启动环境变量或配置文件统一维护，控制台仅提供只读状态与指纹核验。</span>
          </div>
        </div>
      </div>
    </div>
  `;
}

// Views: Security Settings
function renderSettingsView() {
  return `
    <div class="content-container">
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z"/><path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06-1.9 1.9-.06-.06a1.7 1.7 0 0 0-1.88-.34 1.7 1.7 0 0 0-1.03 1.56V20h-2.7v-.09a1.7 1.7 0 0 0-1.03-1.56 1.7 1.7 0 0 0-1.88.34l-.06.06-1.9-1.9.06-.06A1.7 1.7 0 0 0 7.76 15a1.7 1.7 0 0 0-1.56-1.03H6v-2.7h.2A1.7 1.7 0 0 0 7.76 10a1.7 1.7 0 0 0-.34-1.88l-.06-.06 1.9-1.9.06.06a1.7 1.7 0 0 0 1.88.34 1.7 1.7 0 0 0 1.03-1.56V5h2.7v.09a1.7 1.7 0 0 0 1.03 1.56 1.7 1.7 0 0 0 1.88-.34l.06-.06 1.9 1.9-.06.06A1.7 1.7 0 0 0 19.4 10c.18.62.75 1.03 1.4 1.03h.2v2.7h-.2A1.7 1.7 0 0 0 19.4 15z"/></svg>
            <span>安全设置</span>
            <span class="page-title-sub">Security Settings</span>
          </h1>
          <div class="page-desc">修改管理页面登录密码。密码修改成功后所有已登录会话都需要使用新密码重新登录。</div>
        </div>
      </div>

      <div class="card settings-card">
        <div class="section-title">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>
          <span>修改管理密码</span>
        </div>
        <p class="settings-intro">密码会以随机盐哈希形式保存至 Relay 状态文件，服务端不保存明文。</p>
        <div class="settings-policy">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 8h.01"/></svg>
          <span>新密码至少需要 12 个字符。</span>
        </div>
        <form onsubmit="handleChangePassword(event)" class="settings-form">
          <div class="input-group">
            <label class="input-label" for="current-admin-password">当前密码</label>
            <input id="current-admin-password" type="password" class="input font-mono" autocomplete="current-password" required />
          </div>
          <div class="input-group">
            <label class="input-label" for="new-admin-password">新密码</label>
            <input id="new-admin-password" type="password" class="input font-mono" minlength="12" autocomplete="new-password" required />
          </div>
          <div class="input-group">
            <label class="input-label" for="confirm-admin-password">确认新密码</label>
            <input id="confirm-admin-password" type="password" class="input font-mono" minlength="12" autocomplete="new-password" required />
          </div>
          <div id="password-change-error" style="display:none; padding:8px 10px; background:var(--status-danger-bg); border:1px solid var(--status-danger-border); border-radius:4px; color:var(--status-danger-text); font-size:12.5px;"></div>
          <div class="settings-form-actions">
            <button id="password-change-submit" type="submit" class="btn btn-primary settings-submit">
              <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z"/><path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06-1.9 1.9-.06-.06A1.7 1.7 0 0 0 15.96 18a1.7 1.7 0 0 0-1.03 1.56V20h-2.7v-.09A1.7 1.7 0 0 0 11.2 18a1.7 1.7 0 0 0-1.88.34l-.06.06-1.9-1.9.06-.06A1.7 1.7 0 0 0 7.76 15a1.7 1.7 0 0 0-1.56-1.03H6v-2.7h.2A1.7 1.7 0 0 0 7.76 10a1.7 1.7 0 0 0-.34-1.88l-.06-.06 1.9-1.9.06.06a1.7 1.7 0 0 0 1.88.34 1.7 1.7 0 0 0 1.03-1.56V5h2.7v.09a1.7 1.7 0 0 0 1.03 1.56 1.7 1.7 0 0 0 1.88-.34l.06-.06 1.9 1.9-.06.06A1.7 1.7 0 0 0 19.4 10c.18.62.75 1.03 1.4 1.03h.2v2.7h-.2A1.7 1.7 0 0 0 19.4 15z"/></svg>
              <span id="password-change-submit-text">修改密码</span>
            </button>
          </div>
        </form>
      </div>
    </div>
  `;
}

async function handleChangePassword(event) {
  event.preventDefault();
  const button = document.getElementById('password-change-submit');
  const errorBox = document.getElementById('password-change-error');
  const currentPassword = document.getElementById('current-admin-password').value;
  const newPassword = document.getElementById('new-admin-password').value;
  const confirmPassword = document.getElementById('confirm-admin-password').value;
  errorBox.style.display = 'none';
  if (newPassword !== confirmPassword) {
    errorBox.innerText = '两次输入的新密码不一致';
    errorBox.style.display = 'block';
    return;
  }
  button.disabled = true;
  document.getElementById('password-change-submit-text').innerText = '正在修改...';
  try {
    await apiFetch('/api/admin/password', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ current_password: currentPassword, new_password: newPassword, confirm_password: confirmPassword })
    });
    state.isLoggedIn = false;
    state.api.connected = false;
    state.api.token = '';
    state.currentTab = 'overview';
    renderApp();
    showToast('密码修改成功，请使用新密码重新登录', 'success');
  } catch (error) {
    button.disabled = false;
    document.getElementById('password-change-submit-text').innerText = '修改密码';
    errorBox.innerText = `密码修改失败：${error.message}`;
    errorBox.style.display = 'block';
  }
}

// Views: Audit Logs
function renderAuditView() {
  if (state.api.error) return renderErrorState(`管理 API 请求失败：${state.api.error}`);

  let logs = [...auditLogs];

  if (state.filters.auditType && state.filters.auditType !== 'ALL') {
    logs = logs.filter(l => l.action === state.filters.auditType);
  }
  if (state.filters.auditResult && state.filters.auditResult !== 'ALL') {
    logs = logs.filter(l => l.result === state.filters.auditResult);
  }

  return `
    <div class="content-container">
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>
            <span>审计日志</span>
            <span class="page-title-sub">Audit Logs</span>
          </h1>
          <div class="page-desc">追溯管理员与 Controller 操作痕迹、会话启停、紧急停止与安全认证记录</div>
        </div>
        <button class="btn btn-secondary btn-sm" onclick="exportAuditLogs()">
          <svg class="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>
          导出日志 (CSV)
        </button>
      </div>

      <!-- Filters -->
      <div class="filter-toolbar">
        <div class="filter-left">
          <select class="select" onchange="state.filters.auditType = this.value; renderApp();">
            <option value="ALL" ${state.filters.auditType === 'ALL' ? 'selected' : ''}>全部操作类型</option>
            <option value="AUTH_SUCCESS" ${state.filters.auditType === 'AUTH_SUCCESS' ? 'selected' : ''}>认证成功 (AUTH_SUCCESS)</option>
            <option value="AUTH_FAILURE" ${state.filters.auditType === 'AUTH_FAILURE' ? 'selected' : ''}>认证失败 (AUTH_FAILURE)</option>
            <option value="SESSION_CLOSE" ${state.filters.auditType === 'SESSION_CLOSE' ? 'selected' : ''}>关闭会话 (SESSION_CLOSE)</option>
            <option value="EMERGENCY_STOP" ${state.filters.auditType === 'EMERGENCY_STOP' ? 'selected' : ''}>紧急停止 (EMERGENCY_STOP)</option>
            <option value="PASSWORD_CHANGE" ${state.filters.auditType === 'PASSWORD_CHANGE' ? 'selected' : ''}>修改密码 (PASSWORD_CHANGE)</option>
          </select>

          <select class="select" onchange="state.filters.auditResult = this.value; renderApp();">
            <option value="ALL" ${state.filters.auditResult === 'ALL' ? 'selected' : ''}>全部结果</option>
            <option value="SUCCESS" ${state.filters.auditResult === 'SUCCESS' ? 'selected' : ''}>成功 (SUCCESS)</option>
            <option value="FAILED" ${state.filters.auditResult === 'FAILED' ? 'selected' : ''}>失败 (FAILED)</option>
          </select>
        </div>
        <div style="font-size:12.5px; color:var(--text-muted);">
          共展示 <span class="font-mono text-slate-200">${logs.length}</span> 条审计记录
        </div>
      </div>

      <div class="table-container">
        ${logs.length === 0 ? renderEmptyState('暂无审计记录', '当前没有任何匹配的审计事件') : `
          <div class="table-wrapper">
            <table class="ops-table">
              <thead>
                <tr>
                  <th>时间戳 (Timestamp)</th>
                  <th>操作源 (Source)</th>
                  <th>操作类型</th>
                  <th>目标对象 (Target)</th>
                  <th>结果</th>
                  <th>来源 IP</th>
                  <th>操作详情</th>
                </tr>
              </thead>
              <tbody>
                ${logs.map(log => `
                  <tr>
                    <td class="font-mono text-slate-400" style="font-size:12.5px;">${escapeHtml(log.time)}</td>
                    <td class="font-mono text-slate-200">${escapeHtml(log.operator)}</td>
                    <td>
                      <span class="tag" style="font-weight:600;">${escapeHtml(log.action)}</span>
                    </td>
                    <td class="font-mono">${escapeHtml(log.target)}</td>
                    <td>
                      <span class="badge ${log.result === 'SUCCESS' ? 'badge-online' : 'badge-danger'}">
                        <span class="badge-dot"></span>
                        ${escapeHtml(log.result)}
                      </span>
                    </td>
                    <td class="font-mono text-slate-400">${escapeHtml(log.ip)}</td>
                    <td style="color:var(--text-primary); font-size:13px;">${escapeHtml(log.details)}</td>
                  </tr>
                `).join('')}
              </tbody>
            </table>
          </div>
        `}
      </div>
    </div>
  `;
}

// Export Audit Logs
function exportAuditLogs() {
  if (auditLogs.length === 0) {
    showToast('暂无审计日志可导出', 'error');
    return;
  }
  const csvContent = "data:text/csv;charset=utf-8,\uFEFF"
    + ["时间,操作源,操作类型,目标对象,结果,IP,操作详情"].concat(
        auditLogs.map(e => `"${e.time}","${e.operator}","${e.action}","${e.target}","${e.result}","${e.ip}","${e.details.replace(/"/g, '""')}"`)
      ).join("\n");

  const encodedUri = encodeURI(csvContent);
  const link = document.createElement("a");
  link.setAttribute("href", encodedUri);
  link.setAttribute("download", `relay_audit_${Date.now()}.csv`);
  document.body.appendChild(link);
  link.click();
  document.body.removeChild(link);
  showToast('已导出审计日志 CSV 文件', 'success');
}

// View: Login View
function renderLoginView() {
  return `
    <div class="login-screen">
      <div class="login-card">
        <div class="login-brand">
          <div class="brand-badge" style="width:36px; height:36px; font-size:18px;">R</div>
          <div>
            <h1>RemoteOps Relay Admin</h1>
            <p>内部运维管理控制台</p>
          </div>
        </div>

        <form onsubmit="handleLogin(event)" style="display:flex; flex-direction:column; gap:14px;">
          <div class="input-group">
            <label class="input-label" for="login-username">用户名 (Username)</label>
            <input type="text" id="login-username" class="input font-mono" autocomplete="username" value="admin" required />
          </div>

          <div class="input-group">
            <div style="display:flex; justify-content:space-between; align-items:center;">
              <label class="input-label" for="login-password">密码 (Password)</label>
              <button type="button" class="btn btn-ghost btn-sm" style="padding:0; font-size:12px;" onclick="togglePasswordVisibility()">
                <span id="password-toggle-text">显示</span>
              </button>
            </div>
            <input type="password" id="login-password" class="input font-mono" autocomplete="current-password" required />
            <span style="font-size:12px; color:var(--text-muted);">使用 HTTPS 同源登录，无需填写 Relay 地址或 MCP Token。</span>
          </div>

          <div id="login-error-box" style="display:none; padding:8px 10px; background:var(--status-danger-bg); border:1px solid var(--status-danger-border); border-radius:4px; color:var(--status-danger-text); font-size:12.5px;"></div>

          <button type="submit" id="login-submit-btn" class="btn btn-primary" style="margin-top:6px; height:36px;">
            进入控制台
          </button>
        </form>

        <div style="border-top:1px solid var(--border-subtle); padding-top:12px; font-size:12px; color:var(--text-muted); text-align:center;">
          RemoteOps Relay 内部服务
        </div>
      </div>
    </div>
  `;
}

// Login Interactivity
function togglePasswordVisibility() {
  const input = document.getElementById('login-password');
  const toggleText = document.getElementById('password-toggle-text');
  if (!input) return;
  if (input.type === 'password') {
    input.type = 'text';
    toggleText.innerText = '隐藏';
  } else {
    input.type = 'password';
    toggleText.innerText = '显示';
  }
}

async function handleLogin(e) {
  e.preventDefault();
  const btn = document.getElementById('login-submit-btn');
  const errBox = document.getElementById('login-error-box');
  const username = document.getElementById('login-username').value;
  const password = document.getElementById('login-password').value;

  btn.disabled = true;
  btn.innerText = '正在验证管理凭据...';
  errBox.style.display = 'none';

  state.api.baseUrl = window.location.origin;
  state.api.token = '';
  try {
    const result = await apiFetch('/api/admin/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password })
    });
    state.adminUser = result.username || username;
    await refreshRealData();
    btn.disabled = false;
    btn.innerText = '进入控制台';
    state.isLoggedIn = true;
    state.currentTab = 'overview';
    window.history.replaceState(null, '', '#tab=overview');
    showToast('登录成功，已接入 Relay 控制台', 'success');
    renderApp();
  } catch (error) {
    btn.disabled = false;
    btn.innerText = '进入控制台';
    errBox.style.display = 'block';
    errBox.innerText = `登录失败：${error.message}`;
  }
}

// Drawer Component (Session Details)
function renderDrawer() {
  let backdrop = document.getElementById('drawer-backdrop');
  let drawer = document.getElementById('drawer-panel');

  if (!state.activeDrawerSession) {
    if (backdrop) backdrop.classList.remove('open');
    if (drawer) drawer.classList.remove('open');
    return;
  }

  const s = state.activeDrawerSession;

  if (!backdrop) {
    backdrop = document.createElement('div');
    backdrop.id = 'drawer-backdrop';
    backdrop.className = 'drawer-backdrop';
    backdrop.onclick = closeSessionDrawer;
    document.body.appendChild(backdrop);
  }

  if (!drawer) {
    drawer = document.createElement('div');
    drawer.id = 'drawer-panel';
    drawer.className = 'drawer';
    document.body.appendChild(drawer);
  }

  drawer.innerHTML = `
    <div class="drawer-header">
      <div>
        <div style="font-size:15px; font-weight:700; color:var(--text-primary); display:flex; align-items:center; gap:8px;">
          <span>Session 详情</span>
          <span class="badge ${s.agentStatus === 'online' ? 'badge-online' : 'badge-offline'}">
            <span class="badge-dot"></span>
            ${s.agentStatus === 'online' ? 'Active' : 'Offline'}
          </span>
        </div>
        <div class="font-mono text-slate-400" style="font-size:12px; margin-top:3px;">${escapeHtml(truncate(s.id, 10, 8))}</div>
      </div>
      <button class="btn btn-ghost btn-sm" onclick="closeSessionDrawer()">
        <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
      </button>
    </div>

    <div class="drawer-body">
      <!-- Section 1: Basic Info -->
      <div>
        <div class="section-title">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>
          <span>基础与 Agent 节点信息</span>
        </div>
        <div class="key-value-list">
          <div class="kv-item">
            <span class="kv-label">完整 Session ID</span>
            <span class="kv-value">
              <span class="copyable-text" onclick="copyToClipboard(${eventValue(s.id)}, 'Session ID')">
                <span class="text-break">${escapeHtml(s.id)}</span>
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
              </span>
            </span>
          </div>
          <div class="kv-item">
            <span class="kv-label">Agent 节点</span>
            <span class="kv-value font-semibold text-slate-200">${escapeHtml(s.agentName)} (${escapeHtml(truncate(s.agentId, 8, 6))})</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">主机名 / OS</span>
            <span class="kv-value font-mono text-slate-300">${escapeHtml(s.hostname)} / ${escapeHtml(s.os)}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">会话角色 (Role)</span>
            <span class="kv-value font-mono text-slate-300">${escapeHtml(s.role)}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">连接代次 (Generation)</span>
            <span class="kv-value font-mono">Gen #${s.generation}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">租约到期时间</span>
            <span class="kv-value font-mono">${escapeHtml(s.leaseExpire)}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">最近通信时间</span>
            <span class="kv-value font-mono">${escapeHtml(s.lastHeartbeat)}</span>
          </div>
        </div>
      </div>

      <!-- Section 2: Controller Info -->
      <div>
        <div class="section-title">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/></svg>
          <span>Controller 绑定与拓扑</span>
        </div>
        <div class="key-value-list">
          <div class="kv-item">
            <span class="kv-label">AI Controller (MCP)</span>
            <span class="kv-value font-semibold text-blue-400">${escapeHtml(s.controllerName)}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">Owner UUID</span>
            <span class="kv-value">
              <span class="copyable-text" onclick="copyToClipboard(${eventValue(s.ownerUuid)}, 'Owner UUID')">
                ${escapeHtml(formatMaskedUuid(s.ownerUuid))}
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
              </span>
            </span>
          </div>
        </div>
      </div>

      <!-- Section 3: Activity Info -->
      <div>
        <div class="section-title">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>
          <span>实时活动与指令指标</span>
        </div>
        <div class="grid-2" style="margin-bottom:0;">
          <div class="card" style="padding:12px;">
            <div style="font-size:12px; color:var(--text-muted);">待处理审批</div>
            <div style="font-size:24px; font-weight:700; font-family:var(--font-mono); color:${s.pendingApprovals > 0 ? 'var(--status-degraded-text)' : 'var(--text-primary)'};">${s.pendingApprovals}</div>
          </div>
          <div class="card" style="padding:12px;">
            <div style="font-size:12px; color:var(--text-muted);">进行中请求</div>
            <div style="font-size:24px; font-weight:700; font-family:var(--font-mono);">${s.inflightRequests}</div>
          </div>
        </div>
      </div>

      <!-- Section 4: Danger Zone -->
      <div class="danger-zone">
        <div class="danger-zone-title">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="7.86 2 16.14 2 22 7.86 22 16.14 16.14 22 7.86 22 2 16.14 2 7.86 7.86 2"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
          <span>危险运维操作 (Danger Zone)</span>
        </div>
        <p style="font-size:12.5px; color:var(--text-secondary); line-height:1.5;">
          关闭会话将切断 Controller 控制权并清理会话状态；紧急停止将强制终止远程操作。所有操作均会被记录至审计日志。
        </p>

        <div style="display:flex; flex-direction:column; gap:8px; margin-top:6px;">
          <div style="display:flex; justify-content:space-between; align-items:center;">
            <div>
              <div style="font-size:13px; font-weight:600; color:var(--text-primary);">关闭当前会话</div>
              <div style="font-size:12px; color:var(--text-muted);">断开 Controller 连接并清理会话状态</div>
            </div>
            <button class="btn btn-warning btn-sm" onclick="openModal('terminateSession', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })">关闭会话</button>
          </div>

          <div style="display:flex; justify-content:space-between; align-items:center; border-top:1px solid rgba(239,68,68,0.2); padding-top:10px;">
            <div>
              <div style="font-size:13px; font-weight:600; color:var(--status-danger-text);">🛑 紧急停止 (Emergency Stop)</div>
              <div style="font-size:12px; color:var(--text-muted);">立即终止正在执行的远程操作并强制断开</div>
            </div>
            <button class="btn btn-danger btn-sm" onclick="openModal('emergencyStop', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })">紧急停止</button>
          </div>
        </div>
      </div>
    </div>
  `;

  setTimeout(() => {
    backdrop.classList.add('open');
    drawer.classList.add('open');
  }, 10);
}

// Modal Component
function renderModal() {
  let backdrop = document.getElementById('modal-backdrop');

  if (!state.activeModal) {
    if (backdrop) backdrop.classList.remove('open');
    return;
  }

  if (!backdrop) {
    backdrop = document.createElement('div');
    backdrop.id = 'modal-backdrop';
    backdrop.className = 'modal-backdrop';
    document.body.appendChild(backdrop);
  }

  const mType = state.activeModal;
  const data = state.modalTargetData;

  let modalContent = '';

  if (mType === 'terminateSession') {
    modalContent = `
      <div class="modal">
        <div class="modal-header">
          <div class="modal-title" style="color:var(--status-degraded-text);">
            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
            <span>确认关闭会话</span>
          </div>
          <button class="btn btn-ghost btn-sm" onclick="closeModal()">✕</button>
        </div>
        <div class="modal-body">
          <p>您即将关闭以下活跃会话：</p>
          <div class="code-box">
            <span class="text-break font-mono">Session ID: ${escapeHtml(data?.id)}</span>
          </div>
          <div style="font-size:13px; color:var(--text-muted); line-height:1.5;">
            目标 Agent: <strong class="text-slate-200">${escapeHtml(data?.agentName)}</strong><br/>
            关闭会话后，当前连接中的 AI / Human Controller 将立即失去控制权，未完成的交互指令将被中断。
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary btn-sm" onclick="closeModal()">取消</button>
          <button class="btn btn-warning btn-sm" onclick="confirmTerminateSession()">确认关闭会话</button>
        </div>
      </div>
    `;
  } else if (mType === 'emergencyStop') {
    modalContent = `
      <div class="modal" style="border-color:var(--status-danger-border);">
        <div class="modal-header" style="background:rgba(239,68,68,0.1);">
          <div class="modal-title" style="color:var(--status-danger-text);">
            <svg class="w-5 h-5 text-red-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="7.86 2 16.14 2 22 7.86 22 16.14 16.14 22 7.86 22 2 16.14 2 7.86 7.86 2"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
            <span>⚠️ 确认紧急停止 (Emergency Stop)</span>
          </div>
          <button class="btn btn-ghost btn-sm" onclick="closeModal()">✕</button>
        </div>
        <div class="modal-body">
          <div style="padding:10px 12px; background:var(--status-danger-bg); border:1px solid var(--status-danger-border); border-radius:var(--radius-md); color:var(--status-danger-text); font-size:13px; line-height:1.5;">
            <strong>高危风险提示：</strong> 此操作将通过 Relay 向 Agent 节点发送紧急停止请求，立即终止正在执行的远程操作并强制断开当前会话！
          </div>
          <div style="font-size:13px;">
            目标会话: <span class="font-mono text-slate-200">${escapeHtml(truncate(data?.id, 8, 6))}</span> (${escapeHtml(data?.agentName)})
          </div>
          <div class="input-group">
            <label class="input-label" for="emergency-stop-confirmation" style="color:var(--status-danger-text);">请输入 "STOP" 以解锁确认按钮：</label>
            <input
              id="emergency-stop-confirmation"
              type="text"
              class="input font-mono"
              placeholder="输入 STOP"
              oninput="document.getElementById('emg-stop-btn').disabled = (this.value.trim() !== 'STOP');"
              autofocus
            />
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary btn-sm" onclick="closeModal()">放弃操作</button>
          <button id="emg-stop-btn" class="btn btn-danger btn-sm" disabled onclick="confirmEmergencyStop()">
            🛑 确认紧急停止
          </button>
        </div>
      </div>
    `;
  }

  backdrop.innerHTML = modalContent;
  setTimeout(() => backdrop.classList.add('open'), 10);
}

// Error State
function renderErrorState(errMsg) {
  return `
    <div class="content-container">
      <div class="empty-state">
        <svg class="empty-state-icon text-red-500" style="opacity:1;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
        <div class="empty-state-title" style="color:var(--status-danger-text);">Relay 服务请求异常</div>
        <div class="empty-state-desc font-mono" style="font-size:12.5px;">${escapeHtml(errMsg)}</div>
        <div style="display:flex; gap:10px; margin-top:12px;">
          <button class="btn btn-primary btn-sm" onclick="handleManualRefresh()">重试连接</button>
        </div>
      </div>
    </div>
  `;
}

// Empty State Component
function renderEmptyState(title, desc) {
  return `
    <div class="empty-state">
      <svg class="empty-state-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="3" y="3" width="18" height="18" rx="2"/><line x1="9" y1="9" x2="15" y2="15"/><line x1="15" y1="9" x2="9" y2="15"/></svg>
      <div class="empty-state-title">${escapeHtml(title)}</div>
      <div class="empty-state-desc">${escapeHtml(desc)}</div>
    </div>
  `;
}

// Main App Render
function renderApp() {
  const app = document.getElementById('app');
  if (!app) return;

  if (!state.isLoggedIn) {
    app.innerHTML = renderLoginView();
    return;
  }

  let contentHtml = '';
  switch (state.currentTab) {
    case 'overview': contentHtml = renderOverviewView(); break;
    case 'sessions': contentHtml = renderSessionsView(); break;
    case 'agents': contentHtml = renderAgentsView(); break;
    case 'identity': contentHtml = renderIdentityView(); break;
    case 'settings': contentHtml = renderSettingsView(); break;
    case 'audit': contentHtml = renderAuditView(); break;
    default: contentHtml = renderOverviewView();
  }

  app.innerHTML = `
    ${renderSidebar()}
    <div class="main-wrapper">
      ${renderTopBar()}
      <main class="content-area">
        ${contentHtml}
      </main>
    </div>
  `;

  if (state.activeDrawerSession) {
    renderDrawer();
  }
}

// Initialization on DOM Loaded
document.addEventListener('DOMContentLoaded', () => {
  if (!document.getElementById('toast-container')) {
    const tc = document.createElement('div');
    tc.id = 'toast-container';
    tc.className = 'toast-container';
    document.body.appendChild(tc);
  }

  restoreSession();
});
