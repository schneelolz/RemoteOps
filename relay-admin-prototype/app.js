/**
 * RemoteOps Relay Admin - Production Management Console
 * Modernized, Clean, High-Density Operations Frontend
 */

// Global Application State
const state = {
  theme: localStorage.getItem('remoteops-theme') || 'dark',
  currentTab: 'overview', // 'overview', 'sessions', 'agents', 'identity', 'settings', 'audit'
  isLoggedIn: false,
  demoMode: false,
  adminUser: '',
  activeDrawerSession: null,
  activeDrawerAgent: null,
  activeModal: null, // 'terminateSession', 'emergencyStop', 'shutdownAgent', 'purgeClosedSessions'
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
  visibleColumns: {
    sessions: ['connectionStatus', 'agentHostname', 'controller', 'controlMode', 'lastHeartbeat'],
    agents: ['connectionStatus', 'hostname', 'controlCode', 'session', 'controlMode', 'lastHeartbeat']
  },
  api: {
    baseUrl: '',
    token: '',
    connected: false,
    loading: false,
    error: null
  }
};

// Data Containers (populated strictly by /api/admin/* or loadDemoData)
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

function isPrototypePreview() {
  return window.location.protocol === 'file:' ||
    window.location.search.includes('demo=true') ||
    window.location.search.includes('demo=1') ||
    localStorage.getItem('remoteops-demo-mode') === 'true';
}

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

function formatLeaseRemaining(value) {
  if (!value) return '未配置';
  const expire = new Date(value).getTime();
  if (Number.isNaN(expire)) return String(value);
  const diff = Math.floor((expire - Date.now()) / 1000);
  if (diff <= 0) return '已到期';
  const m = Math.floor(diff / 60);
  const s = diff % 60;
  return `剩余 ${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

function isLeaseActive(value) {
  if (!value) return false;
  const expire = new Date(value).getTime();
  if (Number.isNaN(expire)) return false;
  return expire > Date.now();
}

function formatMaskedUuid(uuid) {
  if (!uuid) return '-';
  const s = String(uuid);
  if (s.length <= 16) return s;
  return `${s.slice(0, 8)}...${s.slice(-4)}`;
}

function truncate(str, head = 8, tail = 6) {
  if (!str) return '-';
  if (str.length <= head + tail + 3) return str;
  return `${str.substring(0, head)}...${str.substring(str.length - tail)}`;
}

function permissionLabel(value) {
  const labels = {
    full_access: '完全控制',
    read_only: '只读',
    approval_required: '需审批',
    controller_approved: 'Controller 已批准',
  };
  return labels[value] || '未知';
}

function mcpControlModeLabel(value) {
  const labels = {
    full_access: '完全控制',
    step_by_step: '逐项确认',
    read_only: '只读',
    external_approval: '外部审批',
    expired: '已过期',
  };
  return labels[value] || '未知/已失联';
}

// Column Definitions
const sessionColumnDefinitions = [
  { key: 'connectionStatus', label: '连接状态' },
  { key: 'agentHostname', label: 'Agent 计算机名' },
  { key: 'agentMacAddress', label: 'Agent MAC 地址' },
  { key: 'controller', label: 'MCP 计算机名' },
  { key: 'controllerMacAddress', label: 'MCP MAC 地址' },
  { key: 'controlMode', label: '控制权限' },
  { key: 'lastHeartbeat', label: '最后心跳' },
  { key: 'sessionId', label: 'Session ID' },
  { key: 'agentId', label: 'Agent ID' },
  { key: 'humanController', label: 'Human Controller' },
  { key: 'ownerUuid', label: 'Owner UUID' },
  { key: 'connectTime', label: '连接时间' },
  { key: 'activity', label: '活动' }
];

const agentColumnDefinitions = [
  { key: 'connectionStatus', label: '连接状态' },
  { key: 'hostname', label: '计算机名' },
  { key: 'macAddress', label: 'MAC 地址' },
  { key: 'controlCode', label: '控制码' },
  { key: 'session', label: '当前 Session' },
  { key: 'lastHeartbeat', label: '最后心跳' },
  { key: 'controlMode', label: '控制权限' },
  { key: 'ready', label: '就绪状态' },
  { key: 'agentId', label: 'Agent ID' },
  { key: 'os', label: '操作系统' },
  { key: 'lease', label: '租约到期' },
  { key: 'paired', label: '配对记录' }
];

function toggleVisibleColumn(scope, key, checked) {
  const columns = state.visibleColumns[scope] || [];
  state.visibleColumns[scope] = checked
    ? [...new Set([...columns, key])]
    : columns.filter(column => column !== key);
  try {
    localStorage.setItem(`remoteops-visible-columns-v2-${scope}`, JSON.stringify(state.visibleColumns[scope]));
  } catch (_) {}
  renderApp();
}

function restoreVisibleColumns() {
  for (const scope of ['sessions', 'agents']) {
    try {
      const stored = JSON.parse(localStorage.getItem(`remoteops-visible-columns-v2-${scope}`) || 'null');
      if (Array.isArray(stored) && stored.length > 0) state.visibleColumns[scope] = stored;
    } catch (_) {}
  }
}

function renderColumnPicker(scope, definitions) {
  const visible = state.visibleColumns[scope] || [];
  return `
    <details class="column-picker">
      <summary class="btn btn-secondary btn-sm" title="自定义显示列">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 6h16M4 12h16M4 18h16"/><circle cx="8" cy="6" r="2"/><circle cx="15" cy="12" r="2"/><circle cx="10" cy="18" r="2"/></svg>
        <span>显示列</span>
      </summary>
      <div class="column-picker-menu">
        <div class="column-picker-title">自定义显示列</div>
        ${definitions.map(column => `
          <label class="column-option">
            <input type="checkbox" ${visible.includes(column.key) ? 'checked' : ''} onchange="toggleVisibleColumn('${scope}', '${column.key}', this.checked)" />
            <span>${column.label}</span>
          </label>
        `).join('')}
      </div>
    </details>
  `;
}

// RESTful Management API Fetch
async function apiFetch(path, options = {}) {
  const headers = { ...(options.headers || {}) };
  const config = {
    ...options,
    headers,
    credentials: 'same-origin'
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

  const contentType = response.headers.get('content-type') || '';
  if (!contentType.includes('application/json')) {
    return null;
  }
  return response.json();
}

function applyApiData(overview, identity, rawAgents, rawSessions, rawAudit) {
  relayInfo.address = state.demoMode
    ? 'relay-prod-ap-east-1'
    : escapeHtml(window.location.hostname || '127.0.0.1');
  relayInfo.host = state.demoMode
    ? 'relay.internal.remoteops:18081'
    : escapeHtml(window.location.host || '127.0.0.1');
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
    macAddress: escapeHtml(agent.mac_address || '-'),
    os: escapeHtml(agent.operating_system || '-'),
    status: agent.state === 'online' ? 'online' : 'offline',
    sessionId: agent.session_id ? escapeHtml(String(agent.session_id)) : 'None',
    codeStatus: agent.pairing_code_configured ? '控制码已生成' : '控制码未生成',
    codeConfigured: Boolean(agent.pairing_code_configured),
    lease: formatApiDate(agent.lease_expires_at),
    leaseExpiresAt: agent.lease_expires_at || '',
    heartbeat: formatApiDate(agent.last_seen),
    everPaired: Boolean(agent.ever_paired),
    ready: Boolean(agent.ready),
    permissionMode: agent.permission_mode,
    permissionLabel: escapeHtml(permissionLabel(agent.permission_mode)),
    mcpControlMode: agent.mcp_control_mode || null,
    mcpControlLabel: escapeHtml(mcpControlModeLabel(agent.mcp_control_mode)),
    supportsAgentShutdown: Boolean(agent.supports_agent_shutdown),
    generation: agent.connection_generation || 0
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
      agentMacAddress: escapeHtml(agent?.mac_address || session.mac_address || '-'),
      agentId: escapeHtml(String(session.agent_instance_id)),
      agentStatus: session.state === 'online' ? 'online' : 'offline',
      hostname: escapeHtml(session.hostname || '-'),
      os: escapeHtml(session.operating_system || '-'),
      role: escapeHtml(session.role || 'default'),
      controllerType: ai ? 'AI (MCP)' : human ? 'Human' : 'None',
      controllerName: escapeHtml(ai ? `AI (${String(ai.controller_instance_id).slice(0, 8)})` : human ? `Human (${String(human.controller_instance_id).slice(0, 8)})` : 'None'),
      controllerHostname: escapeHtml(ai?.hostname || ai?.controller_hostname || '-'),
      controllerMacAddress: escapeHtml(ai?.mac_address || ai?.controller_mac_address || '-'),
      controllerInstanceId: escapeHtml(String((ai || human)?.controller_instance_id || '-')),
      humanController: escapeHtml(human ? `Connected (${String(human.controller_instance_id).slice(0, 8)})` : 'None'),
      ownerUuid: escapeHtml(session.owner_id ? String(session.owner_id) : relayInfo.ownerUuid),
      permissionMode,
      permissionLabel: escapeHtml(permissionLabel(permissionMode)),
      relayPermissionLabel: escapeHtml(permissionLabel(permissionMode)),
      mcpControlMode: session.mcp_control_mode || ai?.mcp_control_mode || null,
      mcpControlLabel: escapeHtml(mcpControlModeLabel(session.mcp_control_mode || ai?.mcp_control_mode)),
      supportsAgentShutdown: Boolean(agent?.supports_agent_shutdown),
      agentGeneration: agent?.connection_generation || session.connection_generation || 0,
      connectTime: formatApiDate(session.last_seen),
      lastHeartbeat: formatApiDate(session.last_seen),
      pendingApprovals: session.pending_approvals || 0,
      inflightRequests: session.in_flight_requests || 0,
      generation: session.connection_generation || 0,
      leaseExpire: formatApiDate(session.lease_expires_at),
      isClosed: bindings.length === 0,
      status: bindings.length === 0 ? 'CLOSED' : (session.state === 'online' ? 'ACTIVE' : 'DEGRADED')
    };
  });

  auditLogs = (rawAudit || []).map(event => ({
    id: event.id,
    time: formatApiDate(event.timestamp),
    operator: escapeHtml(event.source || 'admin_api'),
    action: escapeHtml(String(event.action || '').toUpperCase()),
    target: escapeHtml(event.target || '-'),
    result: event.success ? 'SUCCESS' : 'FAILED',
    ip: escapeHtml(event.source && event.source.includes('.') ? event.source : '127.0.0.1'),
    details: escapeHtml(event.summary || '-')
  }));

  recentEvents.splice(0, recentEvents.length, ...(rawAudit || []).slice(0, 5).map(event => ({
    time: formatApiDate(event.timestamp).slice(11, 19),
    type: escapeHtml(String(event.action || 'SYSTEM').toUpperCase()),
    desc: escapeHtml(event.summary || String(event.action || '系统事件')),
    badge: event.success ? 'online' : 'danger'
  })));
}

function demoTimestamp(offsetMinutes) {
  return new Date(Date.now() + offsetMinutes * 60 * 1000).toISOString();
}

function loadDemoData() {
  const ownerId = '550e8400-e29b-41d4-a716-446655440000';
  const demoAgents = [
    {
      agent_instance_id: 'agt-4f54ec9c-97e0-4df1-a2c2-672f43a810ac',
      session_id: 'sess-6a21b7d1-4438-4c65-8b65-067290ff3e18',
      hostname: 'WORKSTATION-SHANGHAI-01',
      mac_address: '00:25:96:FF:FE:12',
      operating_system: 'Windows 11 Pro 24H2',
      state: 'online',
      pairing_code_configured: true,
      lease_expires_at: demoTimestamp(9),
      last_seen: demoTimestamp(0),
      connection_generation: 12,
      ready: true,
      ever_paired: true,
      permission_mode: 'read_only'
    },
    {
      agent_instance_id: 'agt-059f0307-d61a-432b-bf8d-a7af5031c319',
      session_id: 'sess-706a2171-a438-4c65-8b65-b672f43a819e',
      hostname: 'BUILD-SERVER-02',
      mac_address: '3C:52:82:7A:10:42',
      operating_system: 'Ubuntu 24.04 LTS',
      state: 'online',
      pairing_code_configured: false,
      lease_expires_at: demoTimestamp(-12),
      last_seen: demoTimestamp(-1),
      connection_generation: 4,
      ready: false,
      ever_paired: false,
      permission_mode: 'read_only'
    },
    {
      agent_instance_id: 'agt-f49c0a4c-52cc-4378-8655-347c0b672f43',
      session_id: null,
      hostname: 'FINANCE-LAPTOP-7',
      mac_address: null,
      operating_system: 'Windows 10 Enterprise',
      state: 'offline',
      pairing_code_configured: true,
      lease_expires_at: demoTimestamp(-28),
      last_seen: demoTimestamp(-28),
      connection_generation: 2,
      ready: false,
      ever_paired: true,
      permission_mode: 'read_only'
    },
    {
      agent_instance_id: 'agt-01440f8c-2cf1-4c03-8963-d5bc1c419a88',
      session_id: null,
      hostname: 'MAC-STUDIO-OPS',
      mac_address: '3C:52:82:7A:10:43',
      operating_system: 'macOS 15.6',
      state: 'online',
      pairing_code_configured: true,
      lease_expires_at: demoTimestamp(7),
      last_seen: demoTimestamp(0),
      connection_generation: 8,
      ready: true,
      ever_paired: true,
      permission_mode: 'full_access'
    }
  ];

  const demoSessions = [
    {
      session_id: 'sess-6a21b7d1-4438-4c65-8b65-067290ff3e18',
      agent_instance_id: demoAgents[0].agent_instance_id,
      hostname: demoAgents[0].hostname,
      operating_system: demoAgents[0].operating_system,
      state: 'online',
      role: 'default',
      permission_mode: 'full_access',
      owner_id: ownerId,
      controller_bindings: [{ kind: 'ai', controller_instance_id: 'ctrl-12345678', hostname: 'MCP-CONSOLE-01', mac_address: '3C:52:82:7A:10:41' }],
      pending_approvals: 1,
      in_flight_requests: 2,
      lease_expires_at: demoAgents[0].lease_expires_at,
      last_seen: demoAgents[0].last_seen,
      connection_generation: demoAgents[0].connection_generation
    },
    {
      session_id: 'sess-706a2171-a438-4c65-8b65-b672f43a819e',
      agent_instance_id: demoAgents[1].agent_instance_id,
      hostname: demoAgents[1].hostname,
      operating_system: demoAgents[1].operating_system,
      state: 'online',
      role: 'default',
      permission_mode: 'read_only',
      owner_id: ownerId,
      controller_bindings: [{ kind: 'human', controller_instance_id: 'ctrl-98765432' }],
      pending_approvals: 0,
      in_flight_requests: 0,
      lease_expires_at: demoTimestamp(12),
      last_seen: demoAgents[1].last_seen,
      connection_generation: demoAgents[1].connection_generation
    }
  ];

  const demoAudit = [
    {
      id: 'demo-1',
      timestamp: demoTimestamp(-3),
      source: 'admin',
      action: 'SESSION_CREATE',
      target: demoSessions[0].session_id,
      success: true,
      summary: 'AI Controller (MCP) 已接入并建立安全反向代理通道'
    },
    {
      id: 'demo-2',
      timestamp: demoTimestamp(-8),
      source: 'admin',
      action: 'AGENT_READY',
      target: demoAgents[0].hostname,
      success: true,
      summary: 'Agent 节点完成自检与 TLS 握手，进入就绪状态'
    },
    {
      id: 'demo-3',
      timestamp: demoTimestamp(-14),
      source: 'admin',
      action: 'APPROVAL_REQUIRED',
      target: demoSessions[0].session_id,
      success: true,
      summary: '检测到 1 项危险特权指令待管理员审批'
    },
    {
      id: 'demo-4',
      timestamp: demoTimestamp(-26),
      source: 'admin',
      action: 'AGENT_OFFLINE',
      target: demoAgents[2].hostname,
      success: false,
      summary: 'Agent 心跳租约过期，节点进入离线状态'
    }
  ];

  applyApiData(
    {
      owner_id: ownerId,
      version: '0.2.0',
      uptime_seconds: 27342,
      online_agents: 3,
      active_sessions: 2,
      connected_controllers: 2,
      pending_approvals: 1,
      in_flight_requests: 2
    },
    {
      owner_id: ownerId,
      ai_token_configured: true,
      ai_token_fingerprint: 'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
      human_token_configured: false
    },
    demoAgents,
    demoSessions,
    demoAudit
  );
  state.api.connected = true;
  state.api.error = null;
}

async function refreshRealData() {
  state.api.loading = true;
  state.api.error = null;

  if (state.demoMode) {
    try {
      loadDemoData();
    } finally {
      state.api.loading = false;
    }
    return;
  }

  if (!state.api.baseUrl) {
    state.api.baseUrl = window.location.origin;
  }

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
  state.demoMode = isPrototypePreview();
  if (state.demoMode) {
    state.adminUser = 'admin';
    await refreshRealData();
    state.isLoggedIn = true;
    const hashMatch = window.location.hash.match(/tab=([a-z]+)/);
    if (hashMatch && ['overview', 'sessions', 'agents', 'identity', 'settings', 'audit'].includes(hashMatch[1])) {
      state.currentTab = hashMatch[1];
    }
    renderApp();
    return;
  }

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
  if (btn) btn.classList.add('refreshing');
  try {
    await refreshRealData();
    showToast('控制台数据已刷新', 'success');
  } catch (error) {
    showToast(`刷新失败：${error.message}`, 'error');
  } finally {
    if (btn) btn.classList.remove('refreshing');
    renderApp();
  }
}

function showToast(message, type = 'success') {
  const container = document.getElementById('toast-container');
  if (!container) return;

  const toast = document.createElement('div');
  toast.className = `toast toast-${type}`;
  toast.innerText = message;
  container.appendChild(toast);

  setTimeout(() => {
    toast.style.opacity = '0';
    toast.style.transform = 'translateY(10px) scale(0.95)';
    toast.style.transition = 'all 0.2s ease';
    setTimeout(() => toast.remove(), 200);
  }, 2600);
}

function copyToClipboard(text, label = '内容', targetEl = null) {
  if (!text || text === '-') return;
  navigator.clipboard.writeText(text).then(() => {
    showToast(`已复制 ${label} 到剪贴板`, 'success');
    if (targetEl && targetEl.classList) {
      targetEl.classList.add('copied');
      setTimeout(() => targetEl.classList.remove('copied'), 1800);
    }
  }).catch(() => {
    showToast(`复制 ${label} 失败，请手动选择`, 'error');
  });
}

function toggleTheme() {
  state.theme = state.theme === 'dark' ? 'light' : 'dark';
  document.documentElement.setAttribute('data-theme', state.theme);
  try {
    localStorage.setItem('remoteops-theme', state.theme);
  } catch (_) {}
  renderApp();
}

function navigateTo(tabName) {
  state.currentTab = tabName;
  window.history.replaceState(null, '', `#tab=${tabName}`);
  renderApp();
}

function enterDemoMode() {
  state.demoMode = true;
  state.isLoggedIn = true;
  state.adminUser = 'admin';
  try {
    localStorage.setItem('remoteops-demo-mode', 'true');
  } catch (_) {}
  loadDemoData();
  renderApp();
  showToast('控制台数据已加载', 'info');
}

async function logout() {
  if (!state.demoMode) {
    try {
      await apiFetch('/api/admin/logout', { method: 'POST' });
    } catch (_) {}
  }
  state.isLoggedIn = false;
  state.demoMode = false;
  state.api.connected = false;
  state.api.token = '';
  state.currentTab = 'overview';
  try {
    localStorage.removeItem('remoteops-demo-mode');
  } catch (_) {}
  document.getElementById('drawer-backdrop')?.remove();
  document.getElementById('drawer-panel')?.remove();
  document.getElementById('modal-backdrop')?.remove();
  window.history.replaceState(null, '', `${window.location.pathname}`);
  renderApp();
  showToast('已安全退出登录', 'info');
}

// Drawer Controls
function openSessionDrawer(sessionId) {
  const sess = sessions.find(s => s.id === sessionId);
  if (!sess) return;
  state.activeDrawerAgent = null;
  state.activeDrawerSession = sess;
  renderDrawer();
}

function openAgentDrawer(agentId) {
  const agent = agents.find(item => item.id === agentId);
  if (!agent) return;
  if (agent.sessionId !== 'None') {
    openSessionDrawer(agent.sessionId);
    return;
  }
  // Open dedicated Agent drawer if no active session
  state.activeDrawerSession = null;
  state.activeDrawerAgent = agent;
  renderDrawer();
}

function closeDrawer() {
  state.activeDrawerSession = null;
  state.activeDrawerAgent = null;
  renderDrawer();
}

function closeSessionDrawer() {
  closeDrawer();
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

async function confirmTerminateSession() {
  if (!state.modalTargetData?.id) return;
  const sessionId = state.modalTargetData.id;
  closeModal();
  try {
    if (!state.demoMode) {
      await apiFetch(`/api/admin/sessions/${encodeURIComponent(sessionId)}/close`, { method: 'POST' });
      await refreshRealData();
    } else {
      const session = sessions.find(item => item.id === sessionId);
      if (session) {
        session.isClosed = true;
        session.status = 'CLOSED';
        session.controllerType = 'None';
        session.controllerHostname = '-';
        session.controllerInstanceId = '-';
        session.humanController = 'None';
        session.pendingApprovals = 0;
        session.inflightRequests = 0;
      }
    }
    showToast(`会话 ${truncate(sessionId, 6, 4)} 已安全关闭`, 'success');
    closeDrawer();
    renderApp();
  } catch (error) {
    showToast(`关闭会话失败：${error.message}`, 'error');
  }
}

async function confirmEmergencyStop() {
  if (!state.modalTargetData?.id) return;
  const sessionId = state.modalTargetData.id;
  closeModal();
  try {
    if (!state.demoMode) {
      await apiFetch(`/api/admin/sessions/${encodeURIComponent(sessionId)}/emergency-stop`, { method: 'POST' });
    }
    showToast(`🛑 紧急停止信号已发送至会话 ${truncate(sessionId, 6, 4)}`, 'error');
    closeDrawer();
    await refreshRealData();
    renderApp();
  } catch (error) {
    showToast(`紧急停止失败：${error.message}`, 'error');
  }
}

async function confirmShutdownAgent() {
  if (!state.modalTargetData?.id) return;
  const agentId = state.modalTargetData.id;
  const agentName = state.modalTargetData.agentName || '';
  const generation = Number(state.modalTargetData.generation || 0);
  closeModal();
  try {
    if (!state.demoMode) {
      await apiFetch(`/api/admin/agents/${encodeURIComponent(agentId)}/shutdown`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ agent_instance_id: agentId, connection_generation: generation })
      });
      await refreshRealData();
    } else {
      const agent = agents.find(item => item.id === agentId);
      if (agent) agent.status = 'offline';
      const session = sessions.find(item => item.agentId === agentId);
      if (session) session.agentStatus = 'offline';
    }
    showToast(`Agent ${agentName} 已优雅退出`, 'success');
    closeDrawer();
    renderApp();
  } catch (error) {
    showToast(`关闭 Agent 失败：${error.message}`, 'error');
  }
}

async function confirmPurgeClosedSessions() {
  const clearable = sessions.filter(session => session.isClosed && session.agentStatus !== 'online');
  if (clearable.length === 0) {
    closeModal();
    showToast('没有可清除的已关闭会话', 'info');
    return;
  }

  closeModal();
  try {
    if (state.demoMode) {
      sessions = sessions.filter(session => !clearable.includes(session));
    } else {
      await apiFetch('/api/admin/sessions/closed/clear', { method: 'POST' });
      await refreshRealData();
    }
    showToast(`已清除 ${clearable.length} 条已关闭会话记录`, 'success');
    renderApp();
  } catch (error) {
    showToast(`清除已关闭会话失败：${error.message}`, 'error');
  }
}

// Top Bar Component
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
            <span class="pulse-dot"></span>
            ${isOnline ? '在线 (Online)' : '未连接 (Offline)'}
          </span>
        </div>
      </div>

      <div class="topbar-right">
        <button id="topbar-refresh-btn" class="btn btn-secondary btn-sm" onclick="handleManualRefresh()" title="手动同步 Relay 最新状态">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/></svg>
          <span>刷新</span>
        </button>
        <button class="btn btn-secondary btn-sm" onclick="toggleTheme()" title="切换明亮/暗黑主题">
          ${state.theme === 'dark' ? '☀️ 亮色' : '🌙 深色'}
        </button>
        <div class="admin-pill">
          <span class="avatar">${escapeHtml((state.adminUser || 'A').charAt(0).toUpperCase())}</span>
          <span class="font-mono text-slate-300">${escapeHtml(state.adminUser || 'Admin')}</span>
        </div>
        <button class="btn btn-secondary btn-sm" onclick="navigateTo('settings')" title="修改管理控制台密码">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>
          <span>安全设置</span>
        </button>
        <button class="btn btn-ghost btn-sm text-slate-400 hover:text-red-400" onclick="logout()" title="安全退出">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9"/></svg>
        </button>
      </div>
    </header>
  `;
}

// Sidebar Component
function renderSidebar() {
  const activeCount = sessions.filter(s => s.status === 'ACTIVE').length;
  const agentCount = agents.length;
  const auditCount = auditLogs.length;

  const items = [
    { id: 'overview', name: '系统概览', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/></svg>' },
    { id: 'sessions', name: '会话管理', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16"/><circle cx="5" cy="19" r="1"/></svg>', badge: activeCount },
    { id: 'agents', name: 'Agent 节点', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>', badge: agentCount },
    { id: 'identity', name: 'Relay 身份凭据', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><circle cx="12" cy="11" r="3"/></svg>' },
    { id: 'settings', name: '安全设置', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z"/><path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06-1.9 1.9-.06-.06a1.7 1.7 0 0 0-1.88-.34 1.7 1.7 0 0 0-1.03 1.56V20h-2.7v-.09a1.7 1.7 0 0 0-1.03-1.56 1.7 1.7 0 0 0-1.88.34l-.06.06-1.9-1.9.06-.06A1.7 1.7 0 0 0 7.76 15a1.7 1.7 0 0 0-1.56-1.03H6v-2.7h.2A1.7 1.7 0 0 0 7.76 10a1.7 1.7 0 0 0-.34-1.88l-.06-.06 1.9-1.9.06.06a1.7 1.7 0 0 0 1.88.34 1.7 1.7 0 0 0 1.03-1.56V5h2.7v.09a1.7 1.7 0 0 0 1.03 1.56 1.7 1.7 0 0 0 1.88-.34l.06-.06 1.9 1.9-.06.06A1.7 1.7 0 0 0 19.4 10c.18.62.75 1.03 1.4 1.03h.2v2.7h-.2A1.7 1.7 0 0 0 19.4 15z"/></svg>' },
    { id: 'audit', name: '审计日志', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/></svg>', badge: auditCount }
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
        <div class="nav-section-title">运维监控</div>
        ${items.slice(0, 3).map(item => `
          <button type="button" class="nav-item ${state.currentTab === item.id ? 'active' : ''}" onclick="navigateTo('${item.id}')">
            ${item.icon}
            <span>${item.name}</span>
            ${item.badge !== undefined ? `<span class="nav-badge">${item.badge}</span>` : ''}
          </button>
        `).join('')}

        <div class="nav-section-title" style="margin-top:10px;">凭据与安全</div>
        ${items.slice(3).map(item => `
          <button type="button" class="nav-item ${state.currentTab === item.id ? 'active' : ''}" onclick="navigateTo('${item.id}')">
            ${item.icon}
            <span>${item.name}</span>
            ${item.badge !== undefined ? `<span class="nav-badge">${item.badge}</span>` : ''}
          </button>
        `).join('')}
      </nav>

      <div class="sidebar-footer">
        <div class="sidebar-footer-row">
          <span class="pulse-indicator">
            <span class="pulse-dot"></span>
            <span>Relay 运行正常</span>
          </span>
          <span class="badge badge-info" style="font-size:10.5px; padding:1px 6px;">v${escapeHtml(relayInfo.version)}</span>
        </div>
        <div style="font-size:11px; color:var(--text-muted); font-family:var(--font-mono); overflow:hidden; text-overflow:ellipsis; white-space:nowrap;">
          ${escapeHtml(relayInfo.host)}
        </div>
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
            <svg width="20" height="20" class="text-blue-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/></svg>
            <span>系统概览</span>
            <span class="page-title-sub">System Overview</span>
          </h1>
          <div class="page-desc">实时监控 Relay 集群运行指标、节点拓扑状态及近期安全审计事件流水</div>
        </div>
        <div style="display:flex; gap:8px;">
          <button class="btn btn-secondary btn-sm" onclick="navigateTo('sessions')">
            <span>查看全部会话 →</span>
          </button>
        </div>
      </div>

      <!-- Hero Stat Cards -->
      <div class="grid-4">
        <div class="card stat-card">
          <div class="stat-header">
            <span>在线 Agent 节点</span>
            <div class="stat-icon-wrap text-emerald-400">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>
            </div>
          </div>
          <div class="stat-value" style="color:var(--status-online-text);">${onlineAgents}</div>
          <div class="stat-footer">
            <span>已注册 Agent 共 ${agents.length} 台</span>
          </div>
        </div>

        <div class="card stat-card">
          <div class="stat-header">
            <span>活跃会话</span>
            <div class="stat-icon-wrap text-blue-400">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16"/><circle cx="5" cy="19" r="1"/></svg>
            </div>
          </div>
          <div class="stat-value" style="color:var(--color-brand);">${activeSess}</div>
          <div class="stat-footer">
            <span>关联 Controller: ${relayInfo.connectedControllers}</span>
          </div>
        </div>

        <div class="card stat-card">
          <div class="stat-header">
            <span>已连接 Controller</span>
            <div class="stat-icon-wrap text-purple-400">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><path d="m4.93 4.93 4.24 4.24M14.83 9.17l4.24-4.24M14.83 14.83l4.24 4.24M9.17 14.83l-4.24 4.24"/></svg>
            </div>
          </div>
          <div class="stat-value">${relayInfo.connectedControllers}</div>
          <div class="stat-footer">
            <span>进行中请求: ${relayInfo.inFlightRequests}</span>
          </div>
        </div>

        <div class="card stat-card">
          <div class="stat-header">
            <span>待处理审批</span>
            <div class="stat-icon-wrap text-amber-400">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
            </div>
          </div>
          <div class="stat-value" style="color:${relayInfo.pendingApprovals > 0 ? 'var(--status-degraded-text)' : 'var(--text-primary)'};">${relayInfo.pendingApprovals}</div>
          <div class="stat-footer">
            <span style="color:${relayInfo.pendingApprovals > 0 ? 'var(--status-degraded-text)' : 'var(--text-muted)'};">${relayInfo.pendingApprovals > 0 ? '⚠️ 存在等待管理员确认的特权指令' : '暂无待审批指令'}</span>
          </div>
        </div>
      </div>

      <!-- Relay Details & Events -->
      <div class="grid-2">
        <!-- Relay Info Card -->
        <div class="card">
          <div class="section-title">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>
            <span>Relay 运行状态与核心指标</span>
          </div>
          <div class="key-value-list" style="margin-top:14px;">
            <div class="kv-item">
              <span class="kv-label">Relay 服务状态</span>
              <span class="kv-value">
                <span class="badge ${state.api.connected ? 'badge-online' : 'badge-offline'}">
                  <span class="pulse-dot"></span>
                  ${state.api.connected ? '正常运行 (Online)' : '离线 (Offline)'}
                </span>
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">运行版本</span>
              <span class="kv-value font-mono"><span class="tag">v${escapeHtml(relayInfo.version)}</span></span>
            </div>
            <div class="kv-item">
              <span class="kv-label">连续运行时间 (Uptime)</span>
              <span class="kv-value font-mono">${escapeHtml(relayInfo.uptime)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">管理端地址</span>
              <span class="kv-value font-mono" style="color:var(--color-brand);">${escapeHtml(relayInfo.host)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">Owner 标识摘要</span>
              <span class="kv-value font-mono">
                <span class="copyable-text" onclick="copyToClipboard(${eventValue(relayInfo.ownerUuid)}, 'Owner UUID', this)" title="点击复制完整 Owner UUID">
                  ${escapeHtml(formatMaskedUuid(relayInfo.ownerUuid))}
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                </span>
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">已连接 Controller 数</span>
              <span class="kv-value font-mono">${relayInfo.connectedControllers}</span>
            </div>
          </div>
        </div>

        <!-- Recent Events Card -->
        <div class="card">
          <div class="section-title">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 14 14"/></svg>
            <span>最近实时事件流水</span>
            <span class="section-sub">Realtime Events</span>
          </div>
          <div style="display:flex; flex-direction:column; gap:10px; margin-top:14px;">
            ${recentEvents.length === 0 ? '<div class="table-muted-text" style="padding:16px 0;">暂无实时事件</div>' : recentEvents.map(evt => `
              <div style="display:flex; align-items:flex-start; gap:10px; font-size:12.5px; padding:8px 0; border-bottom:1px solid var(--border-subtle);">
                <span class="font-mono text-slate-400" style="font-size:11.5px; flex-shrink:0;">${escapeHtml(evt.time)}</span>
                <span class="badge badge-${evt.badge}" style="font-size:11px; padding:1px 6px; flex-shrink:0;">${escapeHtml(evt.type)}</span>
                <span style="color:var(--text-primary); flex:1; line-height:1.4;">${escapeHtml(evt.desc)}</span>
              </div>
            `).join('')}
          </div>
        </div>
      </div>

      <!-- Active Sessions Preview Table -->
      <div class="table-container">
        <div class="table-header-bar">
          <div class="table-title">
            <svg width="16" height="16" class="text-emerald-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 2v20M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6"/></svg>
            <span>活跃会话状态看板</span>
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

  const isFiltered = state.filters.keyword || state.filters.status !== 'ALL' || state.filters.controllerType !== 'ALL' || state.filters.permissionMode !== 'ALL';
  const activeCount = sessions.filter(session => session.status === 'ACTIVE').length;
  const closedCount = sessions.filter(session => session.isClosed).length;
  const clearableClosedCount = sessions.filter(session => session.isClosed && session.agentStatus !== 'online').length;

  return `
    <div class="content-container">
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg width="20" height="20" class="text-blue-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16"/><circle cx="5" cy="19" r="1"/></svg>
            <span>会话管理</span>
            <span class="page-title-sub">Session Management</span>
          </h1>
          <div class="page-desc">查看实时会话拓扑、权限控制模式、租约心跳，执行会话关闭与紧急停止</div>
        </div>
        <div class="page-header-actions">
          ${renderColumnPicker('sessions', sessionColumnDefinitions)}
          <button class="btn btn-secondary btn-sm" onclick="handleManualRefresh()">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/></svg>
            <span>刷新</span>
          </button>
        </div>
      </div>

      <!-- Filter Toolbar -->
      <div class="filter-toolbar session-filter-toolbar">
        <div class="filter-left">
          <div class="search-input-wrap">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>
            <input
              type="text"
              class="input font-mono"
              placeholder="搜索 Session ID / Agent..."
              value="${escapeHtml(state.filters.keyword)}"
              style="width: 240px;"
              oninput="state.filters.keyword = this.value; renderApp();"
            />
          </div>

          <select class="select" onchange="state.filters.status = this.value; renderApp();">
            <option value="ALL" ${state.filters.status === 'ALL' ? 'selected' : ''}>全部会话状态</option>
            <option value="ACTIVE" ${state.filters.status === 'ACTIVE' ? 'selected' : ''}>活跃 (Active)</option>
            <option value="DEGRADED" ${state.filters.status === 'DEGRADED' ? 'selected' : ''}>异常 / 降级</option>
            <option value="CLOSED" ${state.filters.status === 'CLOSED' ? 'selected' : ''}>已关闭</option>
          </select>

          <select class="select" onchange="state.filters.controllerType = this.value; renderApp();">
            <option value="ALL" ${state.filters.controllerType === 'ALL' ? 'selected' : ''}>全部 Controller 类型</option>
            <option value="AI" ${state.filters.controllerType === 'AI' ? 'selected' : ''}>AI (MCP)</option>
            <option value="Human" ${state.filters.controllerType === 'Human' ? 'selected' : ''}>Human Controller</option>
            <option value="None" ${state.filters.controllerType === 'None' ? 'selected' : ''}>无绑定</option>
          </select>

          <select class="select" onchange="state.filters.permissionMode = this.value; renderApp();">
            <option value="ALL" ${state.filters.permissionMode === 'ALL' ? 'selected' : ''}>全部权限模式</option>
            <option value="read_only" ${state.filters.permissionMode === 'read_only' ? 'selected' : ''}>只读模式</option>
            <option value="full_access" ${state.filters.permissionMode === 'full_access' ? 'selected' : ''}>完全控制</option>
          </select>

          ${isFiltered ? `
            <button class="btn btn-ghost btn-sm" onclick="state.filters = { keyword: '', status: 'ALL', controllerType: 'ALL', permissionMode: 'ALL' }; renderApp();">
              ✕ 重置筛选
            </button>
          ` : ''}
        </div>

        <div class="session-toolbar-summary">
          <span>活跃 <strong class="font-mono">${activeCount}</strong></span>
          <span>已关闭 <strong class="font-mono">${closedCount}</strong></span>
          <button class="btn btn-danger btn-sm" onclick="openModal('purgeClosedSessions')" ${clearableClosedCount === 0 ? 'disabled' : ''} title="清除长期离线且已无 Controller 绑定的记录">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6l-1 14H6L5 6m3 0V4h8v2"/><line x1="10" y1="11" x2="10" y2="17"/><line x1="14" y1="11" x2="14" y2="17"/></svg>
            清除已关闭 (${clearableClosedCount})
          </button>
          <span class="session-result-count">当前显示 <strong class="font-mono">${filteredSessions.length}</strong></span>
        </div>
      </div>

      <!-- Table -->
      <div class="table-container">
        ${renderSessionTable(filteredSessions, 'sessions')}
      </div>
    </div>
  `;
}

// Session Table Component
function renderSessionTable(tableSessions, scope = 'sessions') {
  if (!tableSessions || tableSessions.length === 0) {
    return renderEmptyState('暂无匹配会话', '当前筛选条件下没有 Session 记录');
  }

  const visible = state.visibleColumns[scope] || [];
  const hasColumn = key => visible.includes(key);

  return `
    <div class="table-wrapper">
      <table class="ops-table">
        <thead>
          <tr>
            ${hasColumn('connectionStatus') ? '<th>连接状态</th>' : ''}
            ${hasColumn('agentHostname') ? '<th>Agent 计算机名</th>' : ''}
            ${hasColumn('agentMacAddress') ? '<th>Agent MAC 地址</th>' : ''}
            ${hasColumn('controller') ? '<th>MCP 计算机名</th>' : ''}
            ${hasColumn('controllerMacAddress') ? '<th>MCP MAC 地址</th>' : ''}
            ${hasColumn('controlMode') ? '<th>控制权限</th>' : ''}
            ${hasColumn('lastHeartbeat') ? '<th>最后心跳</th>' : ''}
            ${hasColumn('sessionId') ? '<th>Session ID</th>' : ''}
            ${hasColumn('agentId') ? '<th>Agent ID</th>' : ''}
            ${hasColumn('humanController') ? '<th>Human Controller</th>' : ''}
            ${hasColumn('ownerUuid') ? '<th>Owner UUID</th>' : ''}
            ${hasColumn('connectTime') ? '<th>连接时间</th>' : ''}
            ${hasColumn('activity') ? '<th>活动</th>' : ''}
            <th style="text-align:right;">操作</th>
          </tr>
        </thead>
        <tbody>
          ${tableSessions.map(s => {
            const isOnline = s.agentStatus === 'online';
            return `
              <tr class="${s.isClosed ? 'session-row-closed' : ''}" onclick="openSessionDrawer(${eventValue(s.id)})">
                ${hasColumn('connectionStatus') ? `<td>
                  <span class="badge ${s.isClosed ? 'badge-offline' : (isOnline ? 'badge-online' : 'badge-offline')} ">
                    <span class="badge-dot"></span>
                    ${s.isClosed ? '已关闭' : (isOnline ? '在线' : '离线')}
                  </span>
                </td>` : ''}
                ${hasColumn('agentHostname') ? `<td>
                  <div class="table-primary-text">${escapeHtml(s.agentName)}</div>
                  <div class="table-secondary-text">${escapeHtml(s.os)}</div>
                </td>` : ''}
                ${hasColumn('agentMacAddress') ? `<td><span class="font-mono table-secondary-text">${escapeHtml(s.agentMacAddress === '-' ? '未提供' : s.agentMacAddress)}</span></td>` : ''}
                ${hasColumn('controller') ? `<td>
                  ${s.controllerType.includes('AI') ? `
                    <div class="controller-cell">
                      <span class="badge badge-purple">MCP</span>
                      <span class="table-primary-text font-mono">${escapeHtml(s.controllerHostname === '-' ? '未提供主机名' : s.controllerHostname)}</span>
                    </div>
                  ` : '<span class="table-muted-text">未连接</span>'}
                </td>` : ''}
                ${hasColumn('controllerMacAddress') ? `<td><span class="font-mono table-secondary-text">${escapeHtml(s.controllerMacAddress === '-' ? '未提供' : s.controllerMacAddress)}</span></td>` : ''}
                ${hasColumn('controlMode') ? `<td>
                  <div><span class="table-secondary-text">Relay：</span>${escapeHtml(s.permissionLabel)}</div>
                  <div><span class="table-secondary-text">MCP：</span>${escapeHtml(s.mcpControlLabel)}</div>
                </td>` : ''}
                ${hasColumn('lastHeartbeat') ? `<td><span class="font-mono table-secondary-text">${escapeHtml(s.lastHeartbeat)}</span></td>` : ''}
                ${hasColumn('sessionId') ? `<td>
                  <span class="copyable-text" onclick="event.stopPropagation(); copyToClipboard(${eventValue(s.id)}, 'Session ID', this);" title="点击复制完整 Session ID">
                    ${escapeHtml(truncate(s.id, 8, 6))}
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                  </span>
                </td>` : ''}
                ${hasColumn('agentId') ? `<td><span class="font-mono table-secondary-text">${escapeHtml(truncate(s.agentId, 8, 6))}</span></td>` : ''}
                ${hasColumn('humanController') ? `<td>
                  ${s.humanController !== 'None' ? `
                    <span class="badge badge-info">${escapeHtml(s.humanController)}</span>
                  ` : '<span class="table-muted-text">无</span>'}
                </td>` : ''}
                ${hasColumn('ownerUuid') ? `<td>
                  <span class="copyable-text" onclick="event.stopPropagation(); copyToClipboard(${eventValue(s.ownerUuid)}, 'Owner UUID', this);" title="点击复制 Owner UUID">
                    ${escapeHtml(formatMaskedUuid(s.ownerUuid))}
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                  </span>
                </td>` : ''}
                ${hasColumn('connectTime') ? `<td><span class="font-mono table-secondary-text">${escapeHtml(s.connectTime)}</span></td>` : ''}
                ${hasColumn('activity') ? `<td><span class="table-secondary-text">审批 ${s.pendingApprovals} · 请求 ${s.inflightRequests}</span></td>` : ''}
                <td style="text-align:right;" onclick="event.stopPropagation();">
                  <div class="btn-action-group">
                    <button class="btn btn-secondary btn-sm" onclick="openSessionDrawer(${eventValue(s.id)})">详情</button>
                    ${s.isClosed ? '<span class="table-muted-text session-closed-label">已关闭</span>' : `
                      <button class="btn btn-warning btn-sm" onclick="openModal('terminateSession', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })">关闭</button>
                      ${s.status === 'ACTIVE' ? `<button class="btn btn-danger btn-sm" onclick="openModal('emergencyStop', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })" title="强制切断会话并发送紧急停止">停止</button>` : ''}
                    `}
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

  const onlineCount = agents.filter(agent => agent.status === 'online').length;
  const readyCount = agents.filter(agent => agent.status === 'online' && agent.ready).length;
  const codeCount = agents.filter(agent => agent.codeConfigured && isLeaseActive(agent.leaseExpiresAt)).length;
  const visible = state.visibleColumns.agents || [];
  const hasColumn = key => visible.includes(key);

  return `
    <div class="content-container">
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg width="20" height="20" class="text-blue-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>
            <span>Agent 节点管理</span>
            <span class="page-title-sub">Agent Nodes</span>
          </h1>
          <div class="page-desc">实时监控客户端 Agent 计算机在线状态、控制码可用性及所属会话关联</div>
        </div>
        <div style="display:flex; gap:8px; align-items:center;">
          ${renderColumnPicker('agents', agentColumnDefinitions)}
          <button class="btn btn-secondary btn-sm" onclick="handleManualRefresh()">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/></svg>
            <span>刷新</span>
          </button>
        </div>
      </div>

      ${agents.length === 0 ? `
        <div class="table-container">
          ${renderEmptyState('暂无注册的 Agent', '当前 Relay 尚未接入任何客户端 Agent 节点')}
        </div>
      ` : `
        <!-- Summary Cards -->
        <div class="agent-summary" aria-label="Agent 状态汇总">
          <div class="agent-summary-item">
            <span class="agent-summary-value">${agents.length}</span>
            <span class="agent-summary-label">已登记计算机</span>
          </div>
          <div class="agent-summary-item agent-summary-item-online">
            <span class="agent-summary-value">${onlineCount}</span>
            <span class="agent-summary-label">当前在线节点</span>
          </div>
          <div class="agent-summary-item">
            <span class="agent-summary-value">${readyCount}</span>
            <span class="agent-summary-label">可接受控制</span>
          </div>
          <div class="agent-summary-item">
            <span class="agent-summary-value">${codeCount}</span>
            <span class="agent-summary-label">控制码有效</span>
          </div>
        </div>

        <div class="table-container agent-table-container">
          <div class="table-header-bar">
            <div class="table-title">
              <svg width="16" height="16" class="text-blue-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M3 9h18M9 21V9"/></svg>
              <span>已注册 Agent 节点列表</span>
              <span class="section-sub">${agents.length} 台设备</span>
            </div>
            <span class="table-helper-text">点击行查看节点详情</span>
          </div>
          <div class="table-wrapper">
            <table class="ops-table agent-ops-table">
              <thead>
                <tr>
                  ${hasColumn('connectionStatus') ? '<th>连接状态</th>' : ''}
                  ${hasColumn('hostname') ? '<th>计算机名</th>' : ''}
                  ${hasColumn('macAddress') ? '<th>MAC 地址</th>' : ''}
                  ${hasColumn('controlCode') ? '<th>控制码状态</th>' : ''}
                  ${hasColumn('session') ? '<th>当前 Session</th>' : ''}
                  ${hasColumn('controlMode') ? '<th>控制权限</th>' : ''}
                  ${hasColumn('lastHeartbeat') ? '<th>最后心跳</th>' : ''}
                  ${hasColumn('ready') ? '<th>就绪状态</th>' : ''}
                  ${hasColumn('agentId') ? '<th>Agent ID</th>' : ''}
                  ${hasColumn('os') ? '<th>操作系统</th>' : ''}
                  ${hasColumn('lease') ? '<th>租约到期</th>' : ''}
                  ${hasColumn('paired') ? '<th>配对记录</th>' : ''}
                  <th style="text-align:right;">操作</th>
                </tr>
              </thead>
              <tbody>
                ${agents.map(agt => {
                  const isOnline = agt.status === 'online';
                  const isCodeActive = agt.codeConfigured && isLeaseActive(agt.leaseExpiresAt);
                  const connectionLabel = !isOnline ? '离线' : agt.ready ? '在线' : '重连中';
                  return `
                    <tr class="agent-table-row" onclick="openAgentDrawer(${eventValue(agt.id)})">
                      ${hasColumn('connectionStatus') ? `<td><span class="badge ${isOnline && agt.ready ? 'badge-online' : isOnline ? 'badge-degraded' : 'badge-offline'}"><span class="badge-dot"></span>${connectionLabel}</span></td>` : ''}
                      ${hasColumn('hostname') ? `<td><div class="table-primary-text table-hostname">${escapeHtml(agt.hostname)}</div><div class="table-secondary-text">${escapeHtml(agt.os)}</div></td>` : ''}
                      ${hasColumn('macAddress') ? `<td><span class="font-mono table-secondary-text">${escapeHtml(agt.macAddress === '-' ? '未提供' : agt.macAddress)}</span></td>` : ''}
                      ${hasColumn('controlCode') ? `<td><div class="control-code-cell control-code-cell-${isCodeActive ? 'active' : 'inactive'}"><span class="control-code-status">${isCodeActive ? '可用' : agt.codeConfigured ? '已到期' : '未生成'}</span><span class="table-secondary-text font-mono">${agt.codeConfigured ? escapeHtml(formatLeaseRemaining(agt.leaseExpiresAt)) : '等待生成'}</span></div></td>` : ''}
                      ${hasColumn('session') ? `<td>${agt.sessionId !== 'None' ? `<span class="copyable-text" onclick="event.stopPropagation(); copyToClipboard(${eventValue(agt.sessionId)}, 'Session ID', this)">${escapeHtml(truncate(agt.sessionId, 8, 6))}<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg></span>` : '<span class="table-muted-text">暂无会话</span>'}</td>` : ''}
                      ${hasColumn('controlMode') ? `<td><div><span class="table-secondary-text">上限：</span>${escapeHtml(agt.permissionLabel)}</div><div><span class="table-secondary-text">MCP：</span>${escapeHtml(agt.mcpControlLabel)}</div></td>` : ''}
                      ${hasColumn('lastHeartbeat') ? `<td><span class="font-mono table-secondary-text">${escapeHtml(agt.heartbeat)}</span></td>` : ''}
                      ${hasColumn('ready') ? `<td><span class="ready-status ready-status-${agt.ready ? 'yes' : 'no'}"><span class="ready-status-dot"></span>${agt.ready ? '就绪可控' : '未就绪'}</span></td>` : ''}
                      ${hasColumn('agentId') ? `<td><span class="font-mono table-secondary-text">${escapeHtml(truncate(agt.id, 8, 6))}</span></td>` : ''}
                      ${hasColumn('os') ? `<td class="table-secondary-text">${escapeHtml(agt.os)}</td>` : ''}
                      ${hasColumn('lease') ? `<td><span class="font-mono table-secondary-text">${escapeHtml(agt.lease)}</span></td>` : ''}
                      ${hasColumn('paired') ? `<td class="table-secondary-text">${agt.everPaired ? '曾配对' : '尚未配对'}</td>` : ''}
                      <td style="text-align:right;"><button class="btn btn-secondary btn-sm" onclick="event.stopPropagation(); openAgentDrawer(${eventValue(agt.id)})">详情</button></td>
                    </tr>
                  `;
                }).join('')}
              </tbody>
            </table>
          </div>
        </div>
      `}
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
            <svg width="20" height="20" class="text-blue-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><circle cx="12" cy="11" r="3"/></svg>
            <span>Relay 身份与凭据</span>
            <span class="page-title-sub">Identity & Credentials</span>
          </h1>
          <div class="page-desc">查看 Relay 统一控制器 Owner UUID 身份及 Controller 凭据配置状态与指纹</div>
        </div>
      </div>

      <div class="identity-layout">
        <!-- Left Column: Owner 身份 Card -->
        <div class="card">
          <div class="section-title">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>
            <span>Owner 身份标识</span>
            <span class="section-sub">Controller Owner ID</span>
          </div>
          <p class="section-desc">
            Owner UUID 是此 Relay 部署绑定的全局唯一 Controller Owner 身份。AI Controller (MCP) 与 Human Controller 接入时必须严格匹配此标识。
          </p>

          <div style="display:flex; flex-direction:column; gap:16px;">
            <div class="input-group">
              <div class="input-label">完整 Owner UUID</div>
              <div class="code-box">
                <span class="font-mono text-break">${escapeHtml(relayInfo.ownerUuid || '-')}</span>
                <button class="btn btn-secondary btn-sm" onclick="copyToClipboard(${eventValue(relayInfo.ownerUuid)}, 'Owner UUID', this)">
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                  <span>复制</span>
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
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 2l-2 2m-7.61 7.61a5.5 5.5 0 1 1-7.778 7.778 5.5 5.5 0 0 1 7.777-7.777zm0 0L15.5 7.5m0 0l3 3L22 7l-3-3m-3.5 3.5L19 4"/></svg>
            <span>Controller 凭据状态</span>
            <span class="section-sub">Authentication Credentials</span>
          </div>
          <p class="section-desc">
            Relay 支持 AI Controller (MCP) 与 Human Controller 凭据鉴权。控制台仅提供只读配置核验与 SHA-256 指纹。
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

            <div class="key-value-list" style="margin-top:10px;">
              <div class="kv-item">
                <span class="kv-label">Token 指纹 (SHA-256)</span>
                <span class="kv-value font-mono">
                  ${relayInfo.aiTokenFingerprint ? `
                    <span class="copyable-text" onclick="copyToClipboard(${eventValue(relayInfo.aiTokenFingerprint)}, 'Token 指纹', this)" title="点击复制完整指纹">
                      <span class="text-break">${escapeHtml(relayInfo.aiTokenFingerprint)}</span>
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                    </span>
                  ` : '<span class="table-muted-text">-</span>'}
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
              供运维工程师人工直连控制或独立终端授权校验使用。
            </div>
          </div>

          <!-- Brief Note on Token Configuration -->
          <div class="token-management-note" style="margin-top:16px;">
            <svg width="15" height="15" style="flex-shrink:0; margin-top:2px;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>
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
            <svg width="20" height="20" class="text-blue-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z"/><path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06-1.9 1.9-.06-.06a1.7 1.7 0 0 0-1.88-.34 1.7 1.7 0 0 0-1.03 1.56V20h-2.7v-.09a1.7 1.7 0 0 0-1.03-1.56 1.7 1.7 0 0 0-1.88.34l-.06.06-1.9-1.9.06-.06A1.7 1.7 0 0 0 7.76 15a1.7 1.7 0 0 0-1.56-1.03H6v-2.7h.2A1.7 1.7 0 0 0 7.76 10a1.7 1.7 0 0 0-.34-1.88l-.06-.06 1.9-1.9.06.06a1.7 1.7 0 0 0 1.88.34 1.7 1.7 0 0 0 1.03-1.56V5h2.7v.09a1.7 1.7 0 0 0 1.03 1.56 1.7 1.7 0 0 0 1.88-.34l.06-.06 1.9 1.9-.06.06A1.7 1.7 0 0 0 19.4 10c.18.62.75 1.03 1.4 1.03h.2v2.7h-.2A1.7 1.7 0 0 0 19.4 15z"/></svg>
            <span>安全设置</span>
            <span class="page-title-sub">Security Settings</span>
          </h1>
          <div class="page-desc">修改管理控制台登录密码。密码修改成功后，所有已登录的会话均需使用新密码重新鉴权。</div>
        </div>
      </div>

      <div class="card settings-card">
        <div class="section-title">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>
          <span>修改管理管理员密码</span>
        </div>
        <p class="settings-intro">密码会以 Argon2/PBKDF2 加盐哈希形式保存至 Relay 状态文件，服务端绝不存储明文密码。</p>
        <div class="settings-policy">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 8h.01"/></svg>
          <span>安全合规要求：新密码长度至少需达到 12 个字符。</span>
        </div>
        <form onsubmit="handleChangePassword(event)" class="settings-form">
          <div class="input-group">
            <label class="input-label" for="current-admin-password">当前密码 (Current Password)</label>
            <input id="current-admin-password" type="password" class="input font-mono" autocomplete="current-password" required />
          </div>
          <div class="input-group">
            <label class="input-label" for="new-admin-password">新密码 (New Password)</label>
            <input id="new-admin-password" type="password" class="input font-mono" minlength="12" autocomplete="new-password" placeholder="至少 12 位密码" required />
          </div>
          <div class="input-group">
            <label class="input-label" for="confirm-admin-password">确认新密码 (Confirm New Password)</label>
            <input id="confirm-admin-password" type="password" class="input font-mono" minlength="12" autocomplete="new-password" placeholder="再次输入新密码" required />
          </div>
          <div id="password-change-error" style="display:none; padding:10px 12px; background:var(--status-danger-bg); border:1px solid var(--status-danger-border); border-radius:var(--radius-md); color:var(--status-danger-text); font-size:12.5px;"></div>
          <div class="settings-form-actions">
            <button id="password-change-submit" type="submit" class="btn btn-primary settings-submit">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z"/><path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06-1.9 1.9-.06-.06A1.7 1.7 0 0 0 15.96 18a1.7 1.7 0 0 0-1.03 1.56V20h-2.7v-.09A1.7 1.7 0 0 0 11.2 18a1.7 1.7 0 0 0-1.88.34l-.06.06-1.9-1.9.06-.06A1.7 1.7 0 0 0 7.76 15a1.7 1.7 0 0 0-1.56-1.03H6v-2.7h.2A1.7 1.7 0 0 0 7.76 10a1.7 1.7 0 0 0-.34-1.88l-.06-.06 1.9-1.9.06.06a1.7 1.7 0 0 0 1.88.34 1.7 1.7 0 0 0 1.03-1.56V5h2.7v.09a1.7 1.7 0 0 0 1.03 1.56 1.7 1.7 0 0 0 1.88-.34l.06-.06 1.9 1.9-.06.06A1.7 1.7 0 0 0 19.4 10c.18.62.75 1.03 1.4 1.03h.2v2.7h-.2A1.7 1.7 0 0 0 19.4 15z"/></svg>
              <span id="password-change-submit-text">确认修改密码</span>
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
    errorBox.innerText = '两次输入的新密码不一致，请核对后重试';
    errorBox.style.display = 'block';
    return;
  }
  button.disabled = true;
  document.getElementById('password-change-submit-text').innerText = '正在保存并使会话失效...';
  try {
    if (!state.demoMode) {
      await apiFetch('/api/admin/password', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ current_password: currentPassword, new_password: newPassword, confirm_password: confirmPassword })
      });
    }
    state.isLoggedIn = false;
    state.api.connected = false;
    state.api.token = '';
    state.currentTab = 'overview';
    renderApp();
    showToast('密码修改成功，请使用新密码重新登录', 'success');
  } catch (error) {
    button.disabled = false;
    document.getElementById('password-change-submit-text').innerText = '确认修改密码';
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
            <svg width="20" height="20" class="text-blue-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/></svg>
            <span>审计日志</span>
            <span class="page-title-sub">Audit Logs</span>
          </h1>
          <div class="page-desc">追溯管理员与 Controller 操作痕迹、会话启停、紧急停止与安全认证记录</div>
        </div>
        <button class="btn btn-secondary btn-sm" onclick="exportAuditLogs()">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>
          <span>导出日志 (CSV)</span>
        </button>
      </div>

      <!-- Filters -->
      <div class="filter-toolbar">
        <div class="filter-left">
          <select class="select" onchange="state.filters.auditType = this.value; renderApp();">
            <option value="ALL" ${state.filters.auditType === 'ALL' ? 'selected' : ''}>全部操作类型</option>
            <option value="AUTH_SUCCESS" ${state.filters.auditType === 'AUTH_SUCCESS' ? 'selected' : ''}>认证成功 (AUTH_SUCCESS)</option>
            <option value="AUTH_FAILURE" ${state.filters.auditType === 'AUTH_FAILURE' ? 'selected' : ''}>认证失败 (AUTH_FAILURE)</option>
            <option value="SESSION_CREATE" ${state.filters.auditType === 'SESSION_CREATE' ? 'selected' : ''}>创建会话 (SESSION_CREATE)</option>
            <option value="SESSION_CLOSE" ${state.filters.auditType === 'SESSION_CLOSE' ? 'selected' : ''}>关闭会话 (SESSION_CLOSE)</option>
            <option value="EMERGENCY_STOP" ${state.filters.auditType === 'EMERGENCY_STOP' ? 'selected' : ''}>紧急停止 (EMERGENCY_STOP)</option>
            <option value="PASSWORD_CHANGE" ${state.filters.auditType === 'PASSWORD_CHANGE' ? 'selected' : ''}>修改密码 (PASSWORD_CHANGE)</option>
          </select>

          <select class="select" onchange="state.filters.auditResult = this.value; renderApp();">
            <option value="ALL" ${state.filters.auditResult === 'ALL' ? 'selected' : ''}>全部操作结果</option>
            <option value="SUCCESS" ${state.filters.auditResult === 'SUCCESS' ? 'selected' : ''}>成功 (SUCCESS)</option>
            <option value="FAILED" ${state.filters.auditResult === 'FAILED' ? 'selected' : ''}>失败 (FAILED)</option>
          </select>

          ${(state.filters.auditType !== 'ALL' || state.filters.auditResult !== 'ALL') ? `
            <button class="btn btn-ghost btn-sm" onclick="state.filters.auditType = 'ALL'; state.filters.auditResult = 'ALL'; renderApp();">
              ✕ 重置筛选
            </button>
          ` : ''}
        </div>
        <div style="font-size:12.5px; color:var(--text-muted);">
          共展示 <span class="font-mono" style="color:var(--text-primary); font-weight:600;">${logs.length}</span> 条审计记录
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
                    <td class="font-mono table-secondary-text" style="font-size:12px;">${escapeHtml(log.time)}</td>
                    <td class="font-mono table-primary-text">${escapeHtml(log.operator)}</td>
                    <td>
                      <span class="tag" style="font-weight:600;">${escapeHtml(log.action)}</span>
                    </td>
                    <td class="font-mono table-secondary-text">${escapeHtml(log.target)}</td>
                    <td>
                      <span class="badge ${log.result === 'SUCCESS' ? 'badge-online' : 'badge-danger'}">
                        <span class="badge-dot"></span>
                        ${escapeHtml(log.result)}
                      </span>
                    </td>
                    <td class="font-mono table-secondary-text">${escapeHtml(log.ip)}</td>
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
  showToast('已成功导出审计日志 CSV 文件', 'success');
}

// View: Login View
function renderLoginView() {
  return `
    <div class="login-screen">
      <div class="login-card">
        <div class="login-brand">
          <div class="brand-badge" style="width:38px; height:38px; font-size:18px;">R</div>
          <div>
            <div class="login-kicker">REMOTEOPS RELAY</div>
            <h1>管理控制台</h1>
            <p>安全登录以查看 Relay 运行状态</p>
          </div>
        </div>

        <div class="login-security-note">
          <span class="pulse-dot"></span>
          <span>管理端连接已就绪</span>
          <span class="login-security-meta">同源会话 · HTTPS</span>
        </div>

        <form class="login-form" onsubmit="handleLogin(event)">
          <div class="input-group">
            <label class="input-label" for="login-username">管理员账号 (Username)</label>
            <input type="text" id="login-username" class="input font-mono" autocomplete="username" value="admin" required />
          </div>

          <div class="input-group">
            <div style="display:flex; justify-content:space-between; align-items:center;">
              <label class="input-label" for="login-password">管理密码 (Password)</label>
              <button type="button" class="btn btn-ghost btn-xs" onclick="togglePasswordVisibility()">
                <span id="password-toggle-text">显示</span>
              </button>
            </div>
            <input type="password" id="login-password" class="input font-mono" autocomplete="current-password" placeholder="输入管理员密码" required />
            <span class="login-field-note">浏览器不会持久化明文凭据。</span>
          </div>

          <div id="login-error-box" class="login-error-box"></div>

          <button type="submit" id="login-submit-btn" class="btn btn-primary login-submit-btn">
            <span>登录管理控制台</span>
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M5 12h14"/><path d="m13 6 6 6-6 6"/></svg>
          </button>
        </form>

        <div class="login-footer">
          <span>RemoteOps Relay</span>
          <span class="font-mono">ADMIN ACCESS</span>
        </div>
      </div>
    </div>
  `;
}

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
  btn.innerHTML = '<span>正在验证管理凭据...</span>';
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
    btn.innerHTML = '<span>登录管理控制台</span><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M5 12h14"/><path d="m13 6 6 6-6 6"/></svg>';
    state.isLoggedIn = true;
    state.currentTab = 'overview';
    window.history.replaceState(null, '', '#tab=overview');
    showToast('登录成功，已接入 Relay 控制台', 'success');
    renderApp();
  } catch (error) {
    btn.disabled = false;
    btn.innerHTML = '<span>登录管理控制台</span><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M5 12h14"/><path d="m13 6 6 6-6 6"/></svg>';
    errBox.style.display = 'block';
    errBox.innerText = `登录失败：${error.message}`;
  }
}

// Drawer Component (Session Details & Agent Details)
function renderDrawer() {
  let backdrop = document.getElementById('drawer-backdrop');
  let drawer = document.getElementById('drawer-panel');

  if (!state.activeDrawerSession && !state.activeDrawerAgent) {
    if (backdrop) backdrop.classList.remove('open');
    if (drawer) drawer.classList.remove('open');
    return;
  }

  if (!backdrop) {
    backdrop = document.createElement('div');
    backdrop.id = 'drawer-backdrop';
    backdrop.className = 'drawer-backdrop';
    backdrop.onclick = closeDrawer;
    document.body.appendChild(backdrop);
  }

  if (!drawer) {
    drawer = document.createElement('div');
    drawer.id = 'drawer-panel';
    drawer.className = 'drawer';
    document.body.appendChild(drawer);
  }

  // If Session Drawer
  if (state.activeDrawerSession) {
    const s = state.activeDrawerSession;
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
          <div class="font-mono text-slate-400" style="font-size:11.5px; margin-top:2px;">${escapeHtml(truncate(s.id, 10, 8))}</div>
        </div>
        <button class="btn btn-ghost btn-sm" onclick="closeDrawer()">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
        </button>
      </div>

      <div class="drawer-body">
        <!-- Section 0: Topology Visual Diagram -->
        <div class="topology-flow-card">
          <div class="section-title" style="font-size:12.5px;">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="22 12 18 12 15 21 9 3 6 12 2 12"/></svg>
            <span>实时链路拓扑 (Link Topology)</span>
          </div>
          <div class="topology-chain">
            <!-- Node 1: Controller -->
            <div class="topology-node active">
              <div class="topology-node-icon">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><path d="m4.93 4.93 4.24 4.24M14.83 9.17l4.24-4.24M14.83 14.83l4.24 4.24M9.17 14.83l-4.24 4.24"/></svg>
              </div>
              <div class="topology-node-title">${escapeHtml(s.controllerName)}</div>
              <div class="topology-node-sub font-mono">${escapeHtml(s.controllerHostname || 'MCP-Node')}</div>
            </div>

            <div class="topology-connector active"></div>

            <!-- Node 2: Relay Hub -->
            <div class="topology-node active">
              <div class="topology-node-icon" style="border-color:var(--color-brand); color:var(--color-brand);">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="2" width="20" height="8" rx="2" ry="2"/><rect x="2" y="14" width="20" height="8" rx="2" ry="2"/><line x1="6" y1="6" x2="6.01" y2="6"/><line x1="6" y1="18" x2="6.01" y2="18"/></svg>
              </div>
              <div class="topology-node-title">Relay Gateway</div>
              <div class="topology-node-sub font-mono">:18081</div>
            </div>

            <div class="topology-connector ${s.agentStatus === 'online' ? 'active' : ''}"></div>

            <!-- Node 3: Agent Node -->
            <div class="topology-node ${s.agentStatus === 'online' ? 'active' : ''}">
              <div class="topology-node-icon" style="${s.agentStatus === 'online' ? 'border-color:var(--status-online); color:var(--status-online-text);' : ''}">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>
              </div>
              <div class="topology-node-title">${escapeHtml(s.agentName)}</div>
              <div class="topology-node-sub">${s.agentStatus === 'online' ? '在线 Connected' : '离线 Offline'}</div>
            </div>
          </div>
        </div>

        <!-- Section 1: Basic Node Info -->
        <div class="card" style="padding:16px;">
          <div class="section-title">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>
            <span>基础与 Agent 节点信息</span>
          </div>
          <div class="key-value-list" style="margin-top:12px;">
            <div class="kv-item">
              <span class="kv-label">完整 Session ID</span>
              <span class="kv-value">
                <span class="copyable-text" onclick="copyToClipboard(${eventValue(s.id)}, 'Session ID', this)">
                  <span class="text-break">${escapeHtml(s.id)}</span>
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                </span>
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">Agent 计算机</span>
              <span class="kv-value font-semibold">${escapeHtml(s.agentName)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">Agent MAC 地址</span>
              <span class="kv-value font-mono">${escapeHtml(s.agentMacAddress === '-' ? '未提供' : s.agentMacAddress)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">Agent ID</span>
              <span class="kv-value font-mono table-secondary-text">${escapeHtml(s.agentId)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">操作系统</span>
              <span class="kv-value">${escapeHtml(s.os)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">连接代次</span>
              <span class="kv-value font-mono">Generation #${s.generation}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">租约到期时间</span>
              <span class="kv-value font-mono">${escapeHtml(s.leaseExpire)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">最近通信心跳</span>
              <span class="kv-value font-mono">${escapeHtml(s.lastHeartbeat)}</span>
            </div>
          </div>
        </div>

        <!-- Section 2: Controller & Permission -->
        <div class="card" style="padding:16px;">
          <div class="section-title">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/></svg>
            <span>Controller 绑定与权限</span>
          </div>
          <div class="key-value-list" style="margin-top:12px;">
            <div class="kv-item">
              <span class="kv-label">AI Controller (MCP)</span>
              <span class="kv-value font-semibold text-blue-400">${escapeHtml(s.controllerName)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">MCP 计算机名</span>
              <span class="kv-value font-semibold">${escapeHtml(s.controllerHostname === '-' ? '未提供主机名' : s.controllerHostname)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">MCP MAC 地址</span>
              <span class="kv-value font-mono">${escapeHtml(s.controllerMacAddress === '-' ? '未提供' : s.controllerMacAddress)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">Relay 有效远程权限</span>
              <span class="kv-value">
                <span class="control-mode control-mode-${s.permissionMode === 'full_access' ? 'full' : 'readonly'}">
                  <span class="control-mode-dot"></span>${escapeHtml(s.permissionLabel)}
                </span>
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">MCP 当前本地模式</span>
              <span class="kv-value">
                <span class="control-mode control-mode-${s.mcpControlMode === 'full_access' ? 'full' : 'readonly'}">
                  <span class="control-mode-dot"></span>${escapeHtml(s.mcpControlLabel)}
                </span>
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">Owner UUID</span>
              <span class="kv-value">
                <span class="copyable-text" onclick="copyToClipboard(${eventValue(s.ownerUuid)}, 'Owner UUID', this)">
                  ${escapeHtml(formatMaskedUuid(s.ownerUuid))}
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                </span>
              </span>
            </div>
          </div>
        </div>

        <!-- Section 3: Live Activity Metrics -->
        <div class="card" style="padding:16px;">
          <div class="section-title">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>
            <span>实时活动与指令指标</span>
          </div>
          <div class="grid-2" style="margin-top:12px; margin-bottom:0;">
            <div style="background:var(--bg-card-subtle); padding:12px; border-radius:var(--radius-md); border:1px solid var(--border-subtle);">
              <div style="font-size:11.5px; color:var(--text-muted);">待处理特权审批</div>
              <div style="font-size:22px; font-weight:700; font-family:var(--font-mono); color:${s.pendingApprovals > 0 ? 'var(--status-degraded-text)' : 'var(--text-primary)'};">${s.pendingApprovals}</div>
            </div>
            <div style="background:var(--bg-card-subtle); padding:12px; border-radius:var(--radius-md); border:1px solid var(--border-subtle);">
              <div style="font-size:11.5px; color:var(--text-muted);">进行中交互请求</div>
              <div style="font-size:22px; font-weight:700; font-family:var(--font-mono); color:var(--color-brand);">${s.inflightRequests}</div>
            </div>
          </div>
        </div>

        <!-- Section 4: Danger Zone -->
        <div class="danger-zone">
          <div class="danger-zone-title">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="7.86 2 16.14 2 22 7.86 22 16.14 16.14 22 7.86 22 2 16.14 2 7.86 7.86 2"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
            <span>危险运维操作区 (Danger Zone)</span>
          </div>
          <p style="font-size:12px; color:var(--text-secondary); line-height:1.4;">
            关闭会话将切断 Controller 控制权并清理会话上下文；紧急停止将向 Agent 发送强制阻断信号。所有高危动作均记入审计日志。
          </p>

          <div style="display:flex; flex-direction:column; gap:10px; margin-top:4px;">
            <div style="display:flex; justify-content:space-between; align-items:center;">
              <div>
                <div style="font-size:13px; font-weight:600; color:var(--text-primary);">关闭当前会话</div>
                <div style="font-size:11.5px; color:var(--text-muted);">断开 Controller 连接并清理会话状态</div>
              </div>
              <button class="btn btn-warning btn-sm" onclick="openModal('terminateSession', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })">关闭会话</button>
            </div>

            <div style="display:flex; justify-content:space-between; align-items:center; border-top:1px solid rgba(239,68,68,0.2); padding-top:10px;">
              <div>
                <div style="font-size:13px; font-weight:600; color:var(--status-danger-text);">🛑 紧急停止 (Emergency Stop)</div>
                <div style="font-size:11.5px; color:var(--text-muted);">强制阻断远程执行并立即断开链接</div>
              </div>
              <button class="btn btn-danger btn-sm" onclick="openModal('emergencyStop', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })">紧急停止</button>
            </div>
            ${s.supportsAgentShutdown ? `<div style="display:flex; justify-content:space-between; align-items:center; border-top:1px solid rgba(239,68,68,0.2); padding-top:10px;">
              <div>
                <div style="font-size:13px; font-weight:600; color:var(--status-danger-text);">关闭 Agent</div>
                <div style="font-size:11.5px; color:var(--text-muted);">优雅清理资源并退出 Agent 进程（不关机）</div>
              </div>
              <button class="btn btn-danger btn-sm" onclick="openModal('shutdownAgent', { id: ${eventValue(s.agentId)}, agentName: ${eventValue(s.agentName)}, generation: ${s.agentGeneration} })">关闭 Agent</button>
            </div>` : ''}
          </div>
        </div>
      </div>
    `;
  } else if (state.activeDrawerAgent) {
    // Agent Dedicated Drawer
    const a = state.activeDrawerAgent;
    const isOnline = a.status === 'online';
    const isCodeActive = a.codeConfigured && isLeaseActive(a.leaseExpiresAt);

    drawer.innerHTML = `
      <div class="drawer-header">
        <div>
          <div style="font-size:15px; font-weight:700; color:var(--text-primary); display:flex; align-items:center; gap:8px;">
            <span>Agent 节点详情</span>
            <span class="badge ${isOnline ? 'badge-online' : 'badge-offline'}">
              <span class="badge-dot"></span>
              ${isOnline ? '在线' : '离线'}
            </span>
          </div>
          <div class="font-mono text-slate-400" style="font-size:11.5px; margin-top:2px;">${escapeHtml(a.hostname)}</div>
        </div>
        <button class="btn btn-ghost btn-sm" onclick="closeDrawer()">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
        </button>
      </div>

      <div class="drawer-body">
        <div class="card" style="padding:16px;">
          <div class="section-title">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>
            <span>计算机硬件与系统参数</span>
          </div>
          <div class="key-value-list" style="margin-top:12px;">
            <div class="kv-item">
              <span class="kv-label">计算机名 (Hostname)</span>
              <span class="kv-value font-semibold">${escapeHtml(a.hostname)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">MAC 地址</span>
              <span class="kv-value font-mono">${escapeHtml(a.macAddress === '-' ? '未提供' : a.macAddress)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">Agent 实例 ID</span>
              <span class="kv-value">
                <span class="copyable-text" onclick="copyToClipboard(${eventValue(a.id)}, 'Agent ID', this)">
                  <span class="text-break">${escapeHtml(a.id)}</span>
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                </span>
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">操作系统</span>
              <span class="kv-value">${escapeHtml(a.os)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">Agent 权限上限</span>
              <span class="kv-value">
                <span class="control-mode control-mode-${a.permissionMode === 'full_access' ? 'full' : 'readonly'}">
                  <span class="control-mode-dot"></span>${escapeHtml(a.permissionLabel)}
                </span>
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">MCP 当前本地模式</span>
              <span class="kv-value">
                <span class="control-mode control-mode-${a.mcpControlMode === 'full_access' ? 'full' : 'readonly'}">
                  <span class="control-mode-dot"></span>${escapeHtml(a.mcpControlLabel)}
                </span>
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">控制码就绪</span>
              <span class="kv-value font-mono">
                ${isCodeActive ? '<span class="badge badge-online">有效可用</span>' : '<span class="badge badge-offline">未生效/已过期</span>'}
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">控制码租约到期</span>
              <span class="kv-value font-mono">${escapeHtml(a.lease)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">最后心跳时间</span>
              <span class="kv-value font-mono">${escapeHtml(a.heartbeat)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">曾经配对记录</span>
              <span class="kv-value">${a.everPaired ? '已配对记录' : '新节点尚未配对'}</span>
            </div>
          </div>
        </div>

        <div class="card" style="padding:16px;">
          <div class="section-title">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16"/></svg>
            <span>当前会话关联</span>
          </div>
          <div style="margin-top:12px; font-size:13px;">
            ${a.sessionId !== 'None' ? `
              <div style="display:flex; justify-content:space-between; align-items:center;">
                <span class="copyable-text font-mono">${escapeHtml(a.sessionId)}</span>
                <button class="btn btn-secondary btn-sm" onclick="openSessionDrawer(${eventValue(a.sessionId)})">打开该会话详情 →</button>
              </div>
            ` : `
              <div class="table-muted-text">该计算机当前处于空闲状态，未绑定到任何活跃 Session。当 AI Controller (MCP) 发起连接时将自动建立拓扑。</div>
            `}
          </div>
        </div>
        ${a.supportsAgentShutdown ? `<div class="danger-zone" style="margin-top:16px;">
          <div class="danger-zone-title"><span>关闭 Agent</span></div>
          <p style="font-size:12px; color:var(--text-secondary); line-height:1.4;">发送优雅退出指令，清理在途任务和持久资源后结束 Agent 进程；不会关闭操作系统。</p>
          <button class="btn btn-danger btn-sm" ${isOnline ? '' : 'disabled'} onclick="openModal('shutdownAgent', { id: ${eventValue(a.id)}, agentName: ${eventValue(a.hostname)}, generation: ${a.generation} })">关闭 Agent</button>
        </div>` : ''}
      </div>
    `;
  }

  setTimeout(() => {
    backdrop.classList.add('open');
    drawer.classList.add('open');
  }, 10);
}

// Modal Component (Terminate & Emergency Stop)
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
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
            <span>确认关闭会话</span>
          </div>
          <button class="btn btn-ghost btn-sm" onclick="closeModal()">✕</button>
        </div>
        <div class="modal-body">
          <p>您即将终止并关闭以下处于活跃状态的会话：</p>
          <div class="code-box">
            <span class="text-break font-mono" style="font-weight:600;">Session ID: ${escapeHtml(data?.id)}</span>
          </div>
          <div style="font-size:12.5px; color:var(--text-secondary); line-height:1.5;">
            目标 Agent 节点: <strong class="text-slate-200">${escapeHtml(data?.agentName)}</strong><br/>
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
        <div class="modal-header" style="background:rgba(239,68,68,0.08);">
          <div class="modal-title" style="color:var(--status-danger-text);">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="7.86 2 16.14 2 22 7.86 22 16.14 16.14 22 7.86 22 2 16.14 2 7.86 7.86 2"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
            <span>⚠️ 确认紧急停止 (Emergency Stop)</span>
          </div>
          <button class="btn btn-ghost btn-sm" onclick="closeModal()">✕</button>
        </div>
        <div class="modal-body">
          <div style="padding:10px 12px; background:var(--status-danger-bg); border:1px solid var(--status-danger-border); border-radius:var(--radius-md); color:var(--status-danger-text); font-size:12.5px; line-height:1.5;">
            <strong>高危风险警告：</strong> 此操作将通过 Relay 向 Agent 节点强推紧急停止信号，立即中断所有正在执行的远程操作并彻底断开会话！
          </div>
          <div style="font-size:13px;">
            目标会话: <span class="font-mono" style="font-weight:600; color:var(--text-primary);">${escapeHtml(truncate(data?.id, 8, 6))}</span> (${escapeHtml(data?.agentName)})
          </div>
          <div class="input-group">
            <label class="input-label" for="emergency-stop-confirmation" style="color:var(--status-danger-text); font-weight:600;">请输入大写 "STOP" 以解锁确认按钮：</label>
            <input
              id="emergency-stop-confirmation"
              type="text"
              class="input font-mono"
              placeholder="输入 STOP"
              oninput="document.getElementById('emg-stop-btn').disabled = (this.value.trim() !== 'STOP');"
              onkeydown="if (event.key === 'Enter' && this.value.trim() === 'STOP') confirmEmergencyStop();"
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
  } else if (mType === 'shutdownAgent') {
    const expected = String(data?.agentName || '').trim();
    modalContent = `
      <div class="modal" style="border-color:var(--status-danger-border);">
        <div class="modal-header" style="background:rgba(239,68,68,0.08);">
          <div class="modal-title" style="color:var(--status-danger-text);"><span>⚠️ 确认关闭 Agent</span></div>
          <button class="btn btn-ghost btn-sm" onclick="closeModal()">✕</button>
        </div>
        <div class="modal-body">
          <div style="padding:10px 12px; background:var(--status-danger-bg); border:1px solid var(--status-danger-border); border-radius:var(--radius-md); color:var(--status-danger-text); font-size:12.5px; line-height:1.5;">
            这是高风险管理操作。Agent 将中止在途任务、关闭持久 Shell/串口/文件资源并退出进程；不会关闭整台电脑。
          </div>
          <div style="font-size:13px; margin-top:12px;">目标主机：<strong>${escapeHtml(expected)}</strong></div>
          <div class="input-group">
            <label class="input-label" for="shutdown-agent-confirmation" style="color:var(--status-danger-text); font-weight:600;">请输入上面的完整主机名以确认：</label>
            <input id="shutdown-agent-confirmation" type="text" class="input font-mono" placeholder="输入主机名" oninput="document.getElementById('shutdown-agent-btn').disabled = (this.value.trim() !== ${eventValue(expected)});" onkeydown="if (event.key === 'Enter' && this.value.trim() === ${eventValue(expected)}) confirmShutdownAgent();" autofocus />
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary btn-sm" onclick="closeModal()">取消</button>
          <button id="shutdown-agent-btn" class="btn btn-danger btn-sm" disabled onclick="confirmShutdownAgent()">确认关闭 Agent</button>
        </div>
      </div>
    `;
  } else if (mType === 'purgeClosedSessions') {
    const clearableCount = sessions.filter(session => session.isClosed && session.agentStatus !== 'online').length;
    modalContent = `
      <div class="modal" style="border-color:var(--status-danger-border);">
        <div class="modal-header">
          <div class="modal-title" style="color:var(--status-danger-text);">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6l-1 14H6L5 6m3 0V4h8v2"/><line x1="10" y1="11" x2="10" y2="17"/><line x1="14" y1="11" x2="14" y2="17"/></svg>
            <span>清除已关闭会话</span>
          </div>
          <button class="btn btn-ghost btn-sm" onclick="closeModal()" aria-label="关闭">✕</button>
        </div>
        <div class="modal-body">
          <p>将清除 <strong style="color:var(--text-primary);">${clearableCount}</strong> 条长期离线且已无 Controller 绑定的 Session 记录。</p>
          <div class="token-management-note">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
            <span>仍在线的 Agent 身份不会被删除；清理后这些离线记录将不再出现在会话列表中。</span>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary btn-sm" onclick="closeModal()">取消</button>
          <button class="btn btn-danger btn-sm" onclick="confirmPurgeClosedSessions()">确认清除</button>
        </div>
      </div>
    `;
  }

  backdrop.innerHTML = modalContent;
  setTimeout(() => {
    backdrop.classList.add('open');
    const input = document.getElementById('emergency-stop-confirmation') || document.getElementById('shutdown-agent-confirmation');
    if (input) input.focus();
  }, 10);
}

// Error State Component
function renderErrorState(errMsg) {
  return `
    <div class="content-container">
      <div class="empty-state">
        <svg class="empty-state-icon" style="color:var(--status-danger-text); opacity:1;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
        <div class="empty-state-title" style="color:var(--status-danger-text);">Relay 管理接口请求异常</div>
        <div class="empty-state-desc font-mono">${escapeHtml(errMsg)}</div>
        <div style="display:flex; gap:10px; margin-top:14px;">
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

  if (state.activeDrawerSession || state.activeDrawerAgent) {
    renderDrawer();
  }
}

// Keyboard global handler for Escape key
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    if (state.activeModal) {
      closeModal();
    } else if (state.activeDrawerSession || state.activeDrawerAgent) {
      closeDrawer();
    }
  }
});

// Initialization on DOM Loaded
document.addEventListener('DOMContentLoaded', () => {
  if (!document.getElementById('toast-container')) {
    const tc = document.createElement('div');
    tc.id = 'toast-container';
    tc.className = 'toast-container';
    document.body.appendChild(tc);
  }

  document.documentElement.setAttribute('data-theme', state.theme);
  restoreVisibleColumns();
  restoreSession();
});
