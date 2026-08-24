/**
 * RemoteOps Relay Admin - Application Logic & Prototype State Machine
 */

// Global Prototype State
const state = {
  theme: 'dark',
  currentTab: 'overview', // 'login', 'overview', 'sessions', 'agents', 'identity', 'audit', 'settings'
  isLoggedIn: false,
  adminUser: 'ops_admin_schnee',
  activeDrawerSession: null,
  activeModal: null, // 'terminateSession', 'emergencyStop', 'rotateToken', 'tokenResult'
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
  prototypeScenario: 'normal', // 'normal', 'relay_degraded', 'relay_offline', 'empty_sessions', 'empty_agents', 'filter_no_results', 'loading_skeleton', 'api_error'
  api: {
    baseUrl: '',
    token: '',
    connected: false,
    loading: false,
    error: null
  }
};

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
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN', { hour12: false });
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
    credentials: 'same-origin',
    headers
  });
  if (!response.ok) {
    let detail = `HTTP ${response.status}`;
    try { detail = (await response.json()).error || detail; } catch (_) { /* response may not be JSON */ }
    throw new Error(detail);
  }
  return response.json();
}

function applyApiData(overview, identity, agents, sessions, audit) {
  mockRelayInfo.address = escapeHtml(window.location.hostname || '127.0.0.1');
  mockRelayInfo.aiTokenCreatedAt = '-';
  mockRelayInfo.aiTokenLastRotated = '-';
  mockRelayInfo.ownerUuid = escapeHtml(String(identity.owner_id || overview.owner_id || ''));
  mockRelayInfo.aiTokenConfigured = Boolean(identity.ai_token_configured);
  mockRelayInfo.aiTokenFingerprint = escapeHtml(identity.ai_token_fingerprint || '-');
  mockRelayInfo.version = escapeHtml(overview.version || '-');
  mockRelayInfo.uptime = `${Math.floor((overview.uptime_seconds || 0) / 86400)}d ${Math.floor((overview.uptime_seconds || 0) % 86400 / 3600)}h`;
  mockRelayInfo.currentConnections = overview.connected_controllers || 0;
  mockRelayInfo.maxConnections = Math.max(overview.connected_controllers || 0, 1);

  const agentById = new Map(agents.map(agent => [String(agent.agent_instance_id), agent]));
  mockAgents.splice(0, mockAgents.length, ...agents.map(agent => ({
    id: escapeHtml(String(agent.agent_instance_id)),
    hostname: escapeHtml(agent.hostname),
    os: escapeHtml(agent.operating_system),
    status: agent.state === 'online' ? 'online' : 'offline',
    sessionId: agent.session_id ? escapeHtml(String(agent.session_id)) : 'None',
    codeStatus: agent.pairing_code_configured ? '控制码已生成' : '控制码未生成',
    lease: formatApiDate(agent.lease_expires_at),
    heartbeat: formatApiDate(agent.last_seen),
    pairCount: agent.ever_paired ? 1 : 0
  })));

  mockSessions = sessions.map(session => {
    const bindings = session.controller_bindings || [];
    const ai = bindings.find(binding => binding.kind === 'ai');
    const human = bindings.find(binding => binding.kind === 'human');
    const agent = agentById.get(String(session.agent_instance_id));
    const permissionMode = session.permission_mode || 'approval_required';
    return {
      id: escapeHtml(String(session.session_id)),
      agentName: escapeHtml(agent?.hostname || session.hostname),
      agentId: escapeHtml(String(session.agent_instance_id)),
      agentStatus: session.state === 'online' ? 'online' : 'offline',
      hostname: escapeHtml(session.hostname),
      os: escapeHtml(session.operating_system),
      capabilities: [],
      controllerType: ai ? 'AI (MCP)' : human ? 'Human' : 'None',
      controllerName: escapeHtml(ai ? `AI Controller (${String(ai.controller_instance_id).slice(0, 8)})` : human ? `Human Controller (${String(human.controller_instance_id).slice(0, 8)})` : 'None'),
      controllerInstanceId: escapeHtml(String((ai || human)?.controller_instance_id || '-')),
      humanController: escapeHtml(human ? `Connected (${String(human.controller_instance_id).slice(0, 8)})` : 'None'),
      ownerUuid: escapeHtml(session.owner_id ? String(session.owner_id) : mockRelayInfo.ownerUuid),
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

  mockAuditLogs = audit.map(event => ({
    id: event.id,
    time: formatApiDate(event.timestamp),
    operator: escapeHtml(event.source || 'admin_api'),
    action: escapeHtml(String(event.action || '').toUpperCase()),
    target: escapeHtml(event.target || '-'),
    result: event.success ? 'SUCCESS' : 'FAILED',
    ip: '-',
      details: escapeHtml(event.summary || '-'),
  }));
  mockEvents.splice(0, mockEvents.length, ...audit.slice(-5).reverse().map(event => ({
    time: formatApiDate(event.timestamp).slice(11, 19),
    type: escapeHtml(String(event.action || 'SYSTEM').toUpperCase()),
    desc: escapeHtml(event.summary || String(event.action || '系统事件')),
    badge: event.success ? 'online' : 'danger'
  })));
}

async function refreshRealData() {
  if (!state.api.baseUrl) return;
  state.api.loading = true;
  state.api.error = null;
  try {
    const [overview, identity, agents, sessions, audit] = await Promise.all([
      apiFetch('/api/admin/overview'),
      apiFetch('/api/admin/identity'),
      apiFetch('/api/admin/agents'),
      apiFetch('/api/admin/sessions'),
      apiFetch('/api/admin/audit?limit=200')
    ]);
    applyApiData(overview, identity, agents, sessions, audit);
    state.api.connected = true;
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
    if (session.username) state.adminUser = escapeHtml(session.username);
    await refreshRealData();
    state.isLoggedIn = true;
  } catch (_) {
    state.isLoggedIn = false;
    state.api.connected = false;
  }
  renderApp();
}

// Mock Relay Data
const mockRelayInfo = {
  name: 'relay-prod-ap-east-1',
  address: 'relay.prod.internal',
  adminPort: 18081,
  servicePort: 7443,
  version: 'v1.4.2-prod',
  uptime: '14d 08h 32m',
  currentConnections: 38,
  maxConnections: 200,
  lastHeartbeat: '2026-08-22 23:52:14',
  ownerUuid: '550e8400-e29b-41d4-a716-446655440000',
  aiTokenConfigured: true,
  aiTokenFingerprint: 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
  aiTokenCreatedAt: '2026-08-01 10:00:00',
  aiTokenLastRotated: '2026-08-15 14:20:00'
};

// Mock Sessions
let mockSessions = [
  {
    id: 'sess-8f3a9b1c-4e20-410a-b9c1-7a2d8e4f5001',
    agentName: 'prod-k8s-node-01',
    agentId: 'agt-99214b',
    agentStatus: 'online',
    hostname: 'node-01.infra.internal',
    os: 'Windows 11 24H2 (x64)',
    capabilities: ['bash', 'read_file', 'write_file', 'network_probe', 'k8s_ops'],
    controllerType: 'AI (MCP)',
    controllerName: 'AI Controller (Claude-3.5-Sonnet)',
    controllerInstanceId: 'mcp-worker-084a',
    humanController: 'None',
    ownerUuid: '550e8400-e29b-41d4-a716-446655440000',
    permissionMode: 'RequireApproval',
    permissionLabel: '写操作需审批',
    connectTime: '2026-08-22 21:15:02',
    lastHeartbeat: '2s 前',
    pendingApprovals: 1,
    inflightRequests: 1,
    generation: 3,
    leaseExpire: '00:44:12',
    status: 'ACTIVE'
  },
  {
    id: 'sess-3c7d2e8f-9a10-482b-c3e4-8b1a5f6e7002',
    agentName: 'db-primary-pg16',
    agentId: 'agt-55102a',
    agentStatus: 'online',
    hostname: 'db-master.prod.internal',
    os: 'Windows Server 2022 (x64)',
    capabilities: ['bash', 'psql_exec', 'file_backup'],
    controllerType: 'AI (MCP)',
    controllerName: 'AI Controller (GPT-4o)',
    controllerInstanceId: 'mcp-worker-110b',
    humanController: 'Connected (ops_schnee)',
    ownerUuid: '550e8400-e29b-41d4-a716-446655440000',
    permissionMode: 'ReadOnly',
    permissionLabel: '只读模式',
    connectTime: '2026-08-22 22:40:19',
    lastHeartbeat: '1s 前',
    pendingApprovals: 0,
    inflightRequests: 0,
    generation: 1,
    leaseExpire: '01:12:00',
    status: 'ACTIVE'
  },
  {
    id: 'sess-1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4003',
    agentName: 'ci-runner-arm64-03',
    agentId: 'agt-77889c',
    agentStatus: 'online',
    hostname: 'runner-03.ci.internal',
    os: 'Windows 11 23H2 (x64)',
    capabilities: ['bash', 'docker_build', 'git'],
    controllerType: 'AI (MCP)',
    controllerName: 'AI Controller (Gemini-1.5-Pro)',
    controllerInstanceId: 'mcp-worker-209c',
    humanController: 'None',
    ownerUuid: '550e8400-e29b-41d4-a716-446655440000',
    permissionMode: 'FullAccess',
    permissionLabel: 'Owner 全权限',
    connectTime: '2026-08-22 23:05:40',
    lastHeartbeat: '5s 前',
    pendingApprovals: 1,
    inflightRequests: 2,
    generation: 5,
    leaseExpire: '00:28:10',
    status: 'ACTIVE'
  },
  {
    id: 'sess-9e8d7c6b-5a4f-4e3d-2c1b-0a9f8e7d6004',
    agentName: 'edge-gateway-shanghai',
    agentId: 'agt-33441d',
    agentStatus: 'offline',
    hostname: 'gw-sh.edge.internal',
    os: 'Windows Server 2019 (x64)',
    capabilities: ['bash', 'iptables', 'tcpdump'],
    controllerType: 'None',
    controllerName: 'None',
    controllerInstanceId: '-',
    humanController: 'None',
    ownerUuid: '550e8400-e29b-41d4-a716-446655440000',
    permissionMode: 'RequireApproval',
    permissionLabel: '写操作需审批',
    connectTime: '2026-08-22 18:30:00',
    lastHeartbeat: '12m 前 (超时)',
    pendingApprovals: 0,
    inflightRequests: 0,
    generation: 2,
    leaseExpire: '已过期',
    status: 'DEGRADED'
  },
  {
    id: 'sess-4f5e6d7c-8b9a-4012-3456-789abcdef005',
    agentName: 'sec-scanner-sandbox',
    agentId: 'agt-11223e',
    agentStatus: 'online',
    hostname: 'sandbox.sec.internal',
    os: 'Windows 10 22H2 (x64)',
    capabilities: ['bash', 'gdb', 'radare2'],
    controllerType: 'Human',
    controllerName: 'Human Controller (sec_audit_team)',
    controllerInstanceId: 'hc-cli-9902',
    humanController: 'Connected (sec_audit_team)',
    ownerUuid: '550e8400-e29b-41d4-a716-446655440000',
    permissionMode: 'FullAccess',
    permissionLabel: 'Owner 全权限',
    connectTime: '2026-08-22 23:45:11',
    lastHeartbeat: '3s 前',
    pendingApprovals: 0,
    inflightRequests: 0,
    generation: 1,
    leaseExpire: '01:50:00',
    status: 'ACTIVE'
  }
];

// Mock Agents
const mockAgents = [
  { id: 'agt-99214b', hostname: 'node-01.infra.internal', os: 'Windows 11 24H2 (x64)', status: 'online', sessionId: 'sess-8f3a9b1c-...', codeStatus: '已配对至会话', lease: '00:44:12', heartbeat: '2s 前', pairCount: 14 },
  { id: 'agt-55102a', hostname: 'db-master.prod.internal', os: 'Windows Server 2022 (x64)', status: 'online', sessionId: 'sess-3c7d2e8f-...', codeStatus: '已配对至会话', lease: '01:12:00', heartbeat: '1s 前', pairCount: 3 },
  { id: 'agt-77889c', hostname: 'runner-03.ci.internal', os: 'Windows 11 23H2 (x64)', status: 'online', sessionId: 'sess-1a2b3c4d-...', codeStatus: '已配对至会话', lease: '00:28:10', heartbeat: '5s 前', pairCount: 89 },
  { id: 'agt-33441d', hostname: 'gw-sh.edge.internal', os: 'Windows Server 2019 (x64)', status: 'offline', sessionId: 'sess-9e8d7c6b-...', codeStatus: '控制码已失效', lease: '已过期', heartbeat: '12m 前', pairCount: 5 },
  { id: 'agt-11223e', hostname: 'sandbox.sec.internal', os: 'Windows 10 22H2 (x64)', status: 'online', sessionId: 'sess-4f5e6d7c-...', codeStatus: '已配对至会话', lease: '01:50:00', heartbeat: '3s 前', pairCount: 1 },
  { id: 'agt-66778f', hostname: 'cache-redis-cluster-01', os: 'Windows Server 2022 (x64)', status: 'online', sessionId: 'None', codeStatus: '控制码已生成，租约剩余 08:42', lease: '08:42', heartbeat: '4s 前', pairCount: 12 },
  { id: 'agt-88990a', hostname: 'log-collector-fluentd', os: 'Windows Server 2022 (x64)', status: 'online', sessionId: 'None', codeStatus: '控制码已生成，租约剩余 05:18', lease: '05:18', heartbeat: '6s 前', pairCount: 42 },
  { id: 'agt-22334b', hostname: 'backup-agent-s3', os: 'Windows 10 22H2 (x64)', status: 'offline', sessionId: 'None', codeStatus: '控制码未生成', lease: '-', heartbeat: '3h 前', pairCount: 8 }
];

// Mock Audit Logs
let mockAuditLogs = [
  { id: 101, time: '2026-08-22 23:51:10', operator: 'ops_admin_schnee', action: 'SESSION_VIEW', target: 'sess-8f3a9b1c', result: 'SUCCESS', ip: '10.240.12.88', details: '查看 Session 详情抽屉' },
  { id: 102, time: '2026-08-22 23:45:11', operator: 'sec_audit_team', action: 'CONTROLLER_BIND', target: 'agt-11223e', result: 'SUCCESS', ip: '10.240.14.12', details: 'Human Controller 连接成功' },
  { id: 103, time: '2026-08-22 23:05:40', operator: 'mcp-worker-209c', action: 'SESSION_CREATE', target: 'agt-77889c', result: 'SUCCESS', ip: '10.240.8.101', details: 'AI Controller (Gemini) 配对成功' },
  { id: 104, time: '2026-08-22 22:50:02', operator: 'ops_admin_schnee', action: 'EMERGENCY_STOP', target: 'sess-0012ab', result: 'SUCCESS', ip: '10.240.12.88', details: '管理员强制紧急停止远程执行' },
  { id: 105, time: '2026-08-22 22:10:15', operator: 'ops_admin_schnee', action: 'TOKEN_ROTATE', target: 'AI_CONTROLLER_TOKEN', result: 'SUCCESS', ip: '10.240.12.88', details: '轮换 MCP AI Controller Token' },
  { id: 106, time: '2026-08-22 21:15:02', operator: 'mcp-worker-084a', action: 'SESSION_CREATE', target: 'agt-99214b', result: 'SUCCESS', ip: '10.240.8.55', details: 'AI Controller (Claude) 配对成功' },
  { id: 107, time: '2026-08-22 20:01:44', operator: 'unknown_client', action: 'ADMIN_LOGIN', target: 'Relay Admin', result: 'FAILED', ip: '192.168.1.99', details: 'Token 凭证校验失败' }
];

// Events Feed for Overview
const mockEvents = [
  { time: '23:45:11', type: 'CONTROLLER', desc: 'Human Controller (sec_audit_team) 连接至 agt-11223e', badge: 'info' },
  { time: '23:05:40', type: 'SESSION', desc: 'AI Controller (Gemini) 创建新会话 sess-1a2b3c4d', badge: 'online' },
  { time: '22:50:02', type: 'DANGER', desc: '管理员触发紧急停止：终止会话 sess-0012ab 所有指令', badge: 'danger' },
  { time: '22:40:19', type: 'AGENT', desc: 'Agent db-primary-pg16 完成心跳校验并就绪', badge: 'online' },
  { time: '22:10:15', type: 'SECURITY', desc: 'AI Controller Token 完成密钥轮换', badge: 'degraded' }
];

// Helper: Truncate strings
function truncate(str, head = 8, tail = 6) {
  if (!str) return '';
  if (str.length <= head + tail + 3) return str;
  return `${str.substring(0, head)}...${str.substring(str.length - tail)}`;
}

function getSessionMetrics(sessions = mockSessions) {
  const activeSessions = sessions.filter(s => s.status === 'ACTIVE' && s.agentStatus === 'online');
  const connectedAi = activeSessions.filter(s => s.controllerType.includes('AI')).length;
  const connectedHuman = activeSessions.filter(s => s.humanController !== 'None').length;
  return {
    activeSessions,
    connectedControllers: connectedAi + connectedHuman,
    connectedAi,
    connectedHuman,
    pendingApprovals: activeSessions.reduce((total, session) => total + session.pendingApprovals, 0)
  };
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
  showToast(`已切换至 ${state.theme === 'dark' ? '深色' : '浅色'} 主题`, 'info');
}

// Tab Navigation
function navigateTo(tabName) {
  state.currentTab = tabName;
  state.activeDrawerSession = null;
  state.activeModal = null;
  state.modalTargetData = null;
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
  renderApp();
  showToast('已退出登录', 'info');
}

// Prototype Scenario Switcher
function setPrototypeScenario(scenario) {
  state.prototypeScenario = scenario;
  showToast(`已应用场景: ${scenario}`, 'info');
  renderApp();
}

// Drawer Controls
function openSessionDrawer(sessionId) {
  const sess = mockSessions.find(s => s.id === sessionId);
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

// Actions: Terminate Session
async function confirmTerminateSession() {
  const targetId = state.modalTargetData?.id;
  if (!targetId) return;

  if (state.api.connected) {
    try {
      const result = await apiFetch(`/api/admin/sessions/${encodeURIComponent(targetId)}/close`, { method: 'POST' });
      showToast(result.message || '会话已关闭', result.success ? 'success' : 'error');
      await refreshRealData();
    } catch (error) {
      showToast(`关闭会话失败：${error.message}`, 'error');
      return;
    }
  } else {
    mockSessions = mockSessions.filter(s => s.id !== targetId);
  }
  closeModal();
  closeSessionDrawer();
  showToast(`会话 ${truncate(targetId, 6, 4)} 已安全关闭`, 'success');

  if (!state.api.connected) mockAuditLogs.unshift({
    id: Date.now(),
    time: new Date().toISOString().replace('T', ' ').substring(0, 19),
    operator: state.adminUser,
    action: 'SESSION_TERMINATE',
    target: targetId,
    result: 'SUCCESS',
    ip: '10.240.12.88',
    details: '管理员关闭会话并清理 Controller 绑定'
  });

  renderApp();
}

// Actions: Emergency Stop
async function confirmEmergencyStop() {
  const targetId = state.modalTargetData?.id;
  if (!targetId) return;

  if (state.api.connected) {
    try {
      const result = await apiFetch(`/api/admin/sessions/${encodeURIComponent(targetId)}/emergency-stop`, { method: 'POST' });
      showToast(result.message || '已发送紧急停止请求', 'error');
      await refreshRealData();
    } catch (error) {
      showToast(`紧急停止失败：${error.message}`, 'error');
      return;
    }
  } else {
    mockSessions = mockSessions.filter(s => s.id !== targetId);
  }
  closeModal();
  closeSessionDrawer();
  showToast(`🚨 紧急停止已生效：已终止目标 Agent 上所有远程操作并断开会话`, 'error');

  if (!state.api.connected) mockAuditLogs.unshift({
    id: Date.now(),
    time: new Date().toISOString().replace('T', ' ').substring(0, 19),
    operator: state.adminUser,
    action: 'EMERGENCY_STOP',
    target: targetId,
    result: 'SUCCESS',
    ip: '10.240.12.88',
    details: '触发紧急停止 (STOP) 强制终止所有指令'
  });

  renderApp();
}

// Actions: Rotate Token
function confirmRotateToken() {
  const newToken = 'mcp_sec_' + Math.random().toString(36).substring(2, 12) + '_' + Date.now().toString(36);
  mockRelayInfo.aiTokenLastRotated = new Date().toISOString().replace('T', ' ').substring(0, 19);
  mockRelayInfo.aiTokenFingerprint = 'sha256_' + Math.random().toString(36).substring(2, 10) + '...';

  closeModal();
  openModal('tokenResult', { newToken });

  mockAuditLogs.unshift({
    id: Date.now(),
    time: new Date().toISOString().replace('T', ' ').substring(0, 19),
    operator: state.adminUser,
    action: 'TOKEN_ROTATE',
    target: 'AI_CONTROLLER_TOKEN',
    result: 'SUCCESS',
    ip: '10.240.12.88',
    details: '完成 AI Controller Token 密钥轮换'
  });
  renderApp();
}

// Render Top Bar
function renderTopBar() {
  const relayStatus = state.prototypeScenario === 'relay_offline'
    ? { text: 'Offline (离线)', class: 'badge-offline', dot: 'bg-slate-400' }
    : state.prototypeScenario === 'relay_degraded'
    ? { text: 'Degraded (高负载)', class: 'badge-degraded', dot: 'bg-amber-400' }
    : { text: 'Online (在线)', class: 'badge-online', dot: 'bg-emerald-400' };

  return `
    <header class="topbar">
      <div class="topbar-left">
        <div class="relay-meta">
          <div class="relay-name-box">
            <span class="relay-name">${mockRelayInfo.name}</span>
            <span class="relay-addr font-mono">${mockRelayInfo.address}:${mockRelayInfo.adminPort}</span>
          </div>
          <span class="badge ${relayStatus.class}">
            <span class="badge-dot"></span>
            ${relayStatus.text}
          </span>
        </div>
      </div>

      <div class="topbar-right">
        <div class="topbar-meta-item">
          <svg class="w-3.5 h-3.5 text-slate-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>
          <span>更新于 ${new Date().toLocaleTimeString()}</span>
        </div>
        <button class="btn btn-secondary btn-sm" onclick="showToast('已刷新 Relay 最新状态', 'info'); renderApp();">
          <svg class="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/></svg>
          刷新
        </button>
        <button class="btn btn-secondary btn-sm" onclick="toggleTheme()" title="切换亮/暗色主题">
          ${state.theme === 'dark' ? '☀️ 亮色' : '🌙 深色'}
        </button>
        <div class="admin-pill">
          <span class="avatar">A</span>
          <span class="font-mono text-slate-300">${escapeHtml(state.adminUser)}</span>
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

// Render Prototype Scenario Ribbon
function renderScenarioRibbon() {
  return `
    <div class="prototype-ribbon">
      <div style="display:flex; align-items:center; gap:8px;">
        <span style="font-weight:700; color:#a5b4fc;">⚡ 原型状态快速切换器 (Prototype State Switcher):</span>
        <span>用于模拟系统异常、极端边界与各测试画面</span>
      </div>
      <div style="display:flex; align-items:center; gap:8px;">
        <label>当前模拟场景：</label>
        <select onchange="setPrototypeScenario(this.value)">
          <option value="normal" ${state.prototypeScenario === 'normal' ? 'selected' : ''}>✅ 正常运行 (Normal State)</option>
          <option value="relay_degraded" ${state.prototypeScenario === 'relay_degraded' ? 'selected' : ''}>⚠️ Relay 性能降级 (Degraded)</option>
          <option value="relay_offline" ${state.prototypeScenario === 'relay_offline' ? 'selected' : ''}>❌ Relay 离线 (Offline / Disconnected)</option>
          <option value="empty_sessions" ${state.prototypeScenario === 'empty_sessions' ? 'selected' : ''}>📭 无活跃 Session (No Active Sessions)</option>
          <option value="empty_agents" ${state.prototypeScenario === 'empty_agents' ? 'selected' : ''}>🤖 Relay 无 Agent 注册 (No Agents)</option>
          <option value="filter_no_results" ${state.prototypeScenario === 'filter_no_results' ? 'selected' : ''}>🔍 筛选无匹配结果 (Filter Empty)</option>
          <option value="loading_skeleton" ${state.prototypeScenario === 'loading_skeleton' ? 'selected' : ''}>⏳ API 请求中 (Loading Skeleton)</option>
          <option value="api_error" ${state.prototypeScenario === 'api_error' ? 'selected' : ''}>💥 API 请求失败 500 (Network / API Error)</option>
        </select>
      </div>
    </div>
  `;
}

// Render Sidebar
function renderSidebar() {
  const metrics = getSessionMetrics(state.prototypeScenario === 'empty_sessions' ? [] : mockSessions);
  const activeCount = metrics.activeSessions.length;
  const agentCount = state.prototypeScenario === 'empty_agents' ? 0 : mockAgents.length;
  const auditCount = mockAuditLogs.length;

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
          <div class="nav-item ${state.currentTab === item.id ? 'active' : ''}" onclick="navigateTo('${item.id}')">
            ${item.icon}
            <span>${item.name}</span>
            ${item.badge !== undefined ? `<span class="nav-badge">${item.badge}</span>` : ''}
          </div>
        `).join('')}
      </nav>

      <div class="sidebar-footer">
        <div style="display:flex; justify-content:space-between; align-items:center;">
          <span>版本: ${mockRelayInfo.version}</span>
          <span class="badge badge-online" style="padding:0 4px; font-size:9px;">STABLE</span>
        </div>
        <div style="color:var(--text-muted); font-size:10px;">业务端口 :${mockRelayInfo.servicePort} (TLS)</div>
      </div>
    </aside>
  `;
}

// Views: Overview
function renderOverviewView() {
  if (state.api.error) return renderErrorState(`管理 API 请求失败：${escapeHtml(state.api.error)}`);
  if (state.prototypeScenario === 'api_error') {
    return renderErrorState('获取 Relay 概览数据失败: Connection Refused (500)');
  }
  if (state.prototypeScenario === 'loading_skeleton') {
    return renderLoadingSkeleton();
  }

  const metrics = getSessionMetrics(state.prototypeScenario === 'empty_sessions' ? [] : mockSessions);
  const onlineAgents = state.prototypeScenario === 'empty_agents'
    ? 0
    : mockAgents.filter(agent => agent.status === 'online').length;
  const activeSess = metrics.activeSessions.length;
  const connectedControllers = metrics.connectedControllers;
  const pendingApprovals = metrics.pendingApprovals;

  return `
    <div>
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/></svg>
            系统概览 (System Overview)
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
          <div class="stat-value">${onlineAgents} <span style="font-size:13px; font-weight:400; color:var(--text-muted);">/ 8 注册</span></div>
          <div class="stat-footer">
            <span class="stat-trend-up">↑ 2</span> 较上次刷新增加
          </div>
        </div>

        <div class="card stat-card">
          <div class="stat-header">
            <span>活跃会话 (Active Sessions)</span>
            <svg class="w-4 h-4 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16"/></svg>
          </div>
          <div class="stat-value">${activeSess}</div>
          <div class="stat-footer">
            <span style="color:var(--text-secondary);">持平</span> 包含 ${metrics.connectedAi} 个 AI 会话
          </div>
        </div>

        <div class="card stat-card">
          <div class="stat-header">
            <span>已连接 Controller</span>
            <svg class="w-4 h-4 text-indigo-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><path d="m4.93 4.93 4.24 4.24M14.83 9.17l4.24-4.24M14.83 14.83l4.24 4.24M9.17 14.83l-4.24 4.24"/></svg>
          </div>
          <div class="stat-value">${connectedControllers}</div>
          <div class="stat-footer">
            <span class="tag">AI: ${metrics.connectedAi}</span> <span class="tag">Human: ${metrics.connectedHuman}</span>
          </div>
        </div>

        <div class="card stat-card">
          <div class="stat-header">
            <span>待处理审批 (Approvals)</span>
            <svg class="w-4 h-4 text-amber-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
          </div>
          <div class="stat-value" style="color:var(--status-degraded-text);">${pendingApprovals}</div>
          <div class="stat-footer">
            <span style="color:var(--status-degraded-text);">高危指令待确认</span>
          </div>
        </div>
      </div>

      <!-- Relay Details & Events -->
      <div class="grid-2">
        <!-- Relay Info Card -->
        <div class="card">
          <div class="section-title">
            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>
            Relay 运行状态与端口指标
          </div>
          <div class="key-value-list" style="margin-top:12px;">
            <div class="kv-item">
              <span class="kv-label">Relay 运行状态</span>
              <span class="kv-value">
                <span class="badge ${state.prototypeScenario === 'relay_offline' ? 'badge-offline' : state.prototypeScenario === 'relay_degraded' ? 'badge-degraded' : 'badge-online'}">
                  <span class="badge-dot"></span>
                  ${state.prototypeScenario === 'relay_offline' ? 'Offline' : state.prototypeScenario === 'relay_degraded' ? 'Degraded' : 'Online'}
                </span>
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">运行版本 / 架构</span>
              <span class="kv-value font-mono">${mockRelayInfo.version} (linux/amd64)</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">连续运行时间 (Uptime)</span>
              <span class="kv-value font-mono">${mockRelayInfo.uptime}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">业务 TLS 端口 (Agent/MCP)</span>
              <span class="kv-value font-mono text-blue-400">:${mockRelayInfo.servicePort}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">管理 API 端口 (Admin)</span>
              <span class="kv-value font-mono">:${mockRelayInfo.adminPort} (Local/Private only)</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">当前连接数占用</span>
              <span class="kv-value font-mono">${mockRelayInfo.currentConnections} / ${mockRelayInfo.maxConnections} (${Math.round(mockRelayInfo.currentConnections/mockRelayInfo.maxConnections*100)}%)</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">最后心跳更新</span>
              <span class="kv-value font-mono">${mockRelayInfo.lastHeartbeat}</span>
            </div>
          </div>
        </div>

        <!-- Recent Events -->
        <div class="card">
          <div class="section-title">
            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 14 14"/></svg>
            最近实时事件 (Realtime Events)
          </div>
          <div style="display:flex; flex-direction:column; gap:10px; margin-top:12px;">
            ${mockEvents.map(evt => `
              <div style="display:flex; align-items:flex-start; gap:10px; font-size:12px; padding:6px 0; border-bottom:1px solid var(--border-subtle);">
                <span class="font-mono text-slate-400" style="font-size:11px; flex-shrink:0;">${evt.time}</span>
                <span class="badge badge-${evt.badge}" style="font-size:10px; padding:1px 5px; flex-shrink:0;">${evt.type}</span>
                <span style="color:var(--text-primary); flex:1; font-size:12px;">${evt.desc}</span>
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
            活跃会话状态 (Active Sessions)
          </div>
          <button class="btn btn-secondary btn-sm" onclick="navigateTo('sessions')">查看全部 ${metrics.activeSessions.length} 个活跃会话 →</button>
        </div>
        ${renderSessionTable(metrics.activeSessions.slice(0, 3))}
      </div>
    </div>
  `;
}

// Views: Session Management
function renderSessionsView() {
  if (state.api.error) return renderErrorState(`管理 API 请求失败：${escapeHtml(state.api.error)}`);
  if (state.prototypeScenario === 'api_error') {
    return renderErrorState('加载会话列表失败: Relay API 响应 500');
  }
  if (state.prototypeScenario === 'loading_skeleton') {
    return renderLoadingSkeleton();
  }

  let sessions = [...mockSessions];

  if (state.prototypeScenario === 'empty_sessions') {
    sessions = [];
  } else if (state.prototypeScenario === 'filter_no_results') {
    sessions = [];
  } else {
    // Apply Filters
    if (state.filters.keyword) {
      const q = state.filters.keyword.toLowerCase();
      sessions = sessions.filter(s => s.id.toLowerCase().includes(q) || s.agentName.toLowerCase().includes(q) || s.agentId.toLowerCase().includes(q));
    }
    if (state.filters.status !== 'ALL') {
      sessions = sessions.filter(s => s.status === state.filters.status);
    }
    if (state.filters.controllerType !== 'ALL') {
      sessions = sessions.filter(s => s.controllerType.includes(state.filters.controllerType));
    }
    if (state.filters.permissionMode !== 'ALL') {
      sessions = sessions.filter(s => s.permissionMode === state.filters.permissionMode);
    }
  }

  return `
    <div>
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16"/><circle cx="5" cy="19" r="1"/></svg>
            会话管理 (Session Management)
          </h1>
          <div class="page-desc">核心运维控制台：查看所有实时会话拓扑、权限模式、心跳租约，执行关闭与紧急停止</div>
        </div>
        <div style="display:flex; gap:8px;">
          <button class="btn btn-secondary btn-sm" onclick="showToast('已拉取最新会话列表', 'info'); renderApp();">
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
            style="width: 220px;"
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
            <option value="RequireApproval" ${state.filters.permissionMode === 'RequireApproval' ? 'selected' : ''}>写操作需审批</option>
            <option value="ReadOnly" ${state.filters.permissionMode === 'ReadOnly' ? 'selected' : ''}>只读模式</option>
            <option value="ControllerApproved" ${state.filters.permissionMode === 'ControllerApproved' ? 'selected' : ''}>控制器确认</option>
            <option value="FullAccess" ${state.filters.permissionMode === 'FullAccess' ? 'selected' : ''}>Owner 全权限</option>
          </select>

          ${(state.filters.keyword || state.filters.status !== 'ALL' || state.filters.controllerType !== 'ALL' || state.filters.permissionMode !== 'ALL') ? `
            <button class="btn btn-ghost btn-sm" onclick="state.filters = { keyword: '', status: 'ALL', controllerType: 'ALL', permissionMode: 'ALL' }; renderApp();">
              ✕ 重置筛选
            </button>
          ` : ''}
        </div>

        <div style="font-size:12px; color:var(--text-muted);">
          共找到 <span class="font-mono text-slate-200">${sessions.length}</span> 个会话
        </div>
      </div>

      <!-- Table -->
      <div class="table-container">
        ${renderSessionTable(sessions)}
      </div>
    </div>
  `;
}

// Session Table Component
function renderSessionTable(sessions) {
  if (sessions.length === 0) {
    if (state.prototypeScenario === 'filter_no_results') {
      return renderEmptyState('没有匹配的筛选结果', '尝试放宽筛选条件或重置搜索词');
    }
    return renderEmptyState('暂无活跃会话', '当前 Relay 没有正在运行的 Session');
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
          ${sessions.map(s => {
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
                  <div style="font-weight:600; color:var(--text-primary); font-size:12.5px;">${escapeHtml(s.agentName)}</div>
                  <div class="font-mono" style="font-size:11px; color:var(--text-muted);">${escapeHtml(s.agentId)}</div>
                </td>
                <td>
                  ${s.controllerType.includes('AI') ? `
                    <div style="display:flex; align-items:center; gap:4px;">
                      <span class="badge badge-info" style="font-size:10px;">MCP</span>
                      <span style="font-size:11.5px; color:var(--text-primary);">${escapeHtml(s.controllerName.replace('AI Controller ', ''))}</span>
                    </div>
                  ` : `<span style="color:var(--text-muted); font-size:11.5px;">未连接</span>`}
                </td>
                <td>
                  ${s.humanController !== 'None' ? `
                    <span class="badge badge-info" style="font-size:10.5px;">${escapeHtml(s.humanController)}</span>
                  ` : `<span style="color:var(--text-muted); font-size:11.5px;">无</span>`}
                </td>
                <td>
                  <span class="copyable-text" onclick="event.stopPropagation(); copyToClipboard(${eventValue(s.ownerUuid)}, 'Owner UUID');" title="点击复制 Owner UUID">
                    ${escapeHtml(truncate(s.ownerUuid, 6, 4))}
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                  </span>
                </td>
                <td>
                  <span class="tag" style="${s.permissionMode === 'FullAccess' ? 'border-color:var(--status-danger-border); color:var(--status-danger-text);' : ''}">${escapeHtml(s.permissionLabel)}</span>
                </td>
                <td>
                  <span class="font-mono text-slate-400">${escapeHtml(s.lastHeartbeat)}</span>
                </td>
                <td style="text-align:right;" onclick="event.stopPropagation();">
                  <div style="display:inline-flex; gap:6px;">
                    <button class="btn btn-secondary btn-sm" onclick="openSessionDrawer(${eventValue(s.id)})">详情</button>
                    ${isOnline ? `
                      <button class="btn btn-warning btn-sm" onclick="openModal('terminateSession', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })">关闭</button>
                      <button class="btn btn-danger btn-sm" onclick="openModal('emergencyStop', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })">🛑 紧急停止</button>
                    ` : `
                      <button class="btn btn-secondary btn-sm" disabled title="Agent 离线，无法立即清理远端会话">清理记录</button>
                      <button class="btn btn-danger btn-sm" disabled title="Agent 离线且没有可执行的紧急操作">🛑 紧急停止</button>
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
  if (state.api.error) return renderErrorState(`管理 API 请求失败：${escapeHtml(state.api.error)}`);
  if (state.prototypeScenario === 'api_error') {
    return renderErrorState('获取 Agent 列表失败: 500 Internal Server Error');
  }
  if (state.prototypeScenario === 'loading_skeleton') {
    return renderLoadingSkeleton();
  }

  let agents = state.prototypeScenario === 'empty_agents' ? [] : mockAgents;

  return `
    <div>
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>
            Agent 节点管理 (Agent Management)
          </h1>
          <div class="page-desc">查看已注册的 Agent 实例、控制码租约状态、操作系统及心跳状态</div>
        </div>
        <button class="btn btn-secondary btn-sm" onclick="showToast('已刷新 Agent 状态', 'info');">刷新</button>
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
                  <th>控制码状态 (脱敏安全展示)</th>
                  <th>心跳</th>
                  <th>配对历史</th>
                  <th style="text-align:right;">操作</th>
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
                    <td class="font-mono text-slate-200 font-semibold">${escapeHtml(agt.id)}</td>
                    <td class="font-mono text-slate-300">${escapeHtml(agt.hostname)}</td>
                    <td style="color:var(--text-muted); font-size:11.5px;">${escapeHtml(agt.os)}</td>
                    <td class="font-mono">
                      ${agt.sessionId !== 'None' ? `<span class="copyable-text" onclick="copyToClipboard(${eventValue(agt.sessionId)}, 'Session ID')">${escapeHtml(agt.sessionId)}</span>` : '<span style="color:var(--text-muted);">-</span>'}
                    </td>
                    <td>
                      <span class="tag" style="background:var(--bg-input);">${escapeHtml(agt.codeStatus)}</span>
                    </td>
                    <td class="font-mono text-slate-400">${escapeHtml(agt.heartbeat)}</td>
                    <td class="font-mono text-slate-400">${agt.pairCount} 次</td>
                    <td style="text-align:right;">
                      <button class="btn btn-secondary btn-sm" onclick="showToast(${eventValue(`Agent ${agt.id} 详细诊断就绪`)}, 'info');">诊断</button>
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
  return `
    <div>
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><circle cx="12" cy="11" r="3"/></svg>
            Relay 身份与凭据 (Relay Identity & Credentials)
          </h1>
          <div class="page-desc">管理 Relay 核心身份 Owner UUID 及 AI Controller (MCP) 管理凭据</div>
        </div>
      </div>

      <div style="display:flex; flex-direction:column; gap:18px;">
        <!-- Owner UUID Card -->
        <div class="card">
          <div class="section-title">
            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>
            Owner UUID (统一 Controller Owner 身份)
          </div>
          <p style="font-size:12px; color:var(--text-secondary); margin-bottom:14px; line-height:1.5;">
            Owner UUID 是本 Relay 的全局唯一统一 Controller Owner 身份。无论是 <strong>AI Controller (MCP)</strong> 还是 <strong>Human Controller</strong>，在发起会话连接时都必须提供与此一致的 Owner UUID。
          </p>

          <div style="display:flex; flex-direction:column; gap:10px;">
            <div class="input-group">
              <label class="input-label">完整 Owner UUID</label>
              <div class="code-box">
                <span>${escapeHtml(mockRelayInfo.ownerUuid)}</span>
                <button class="btn btn-secondary btn-sm" onclick="copyToClipboard(${eventValue(mockRelayInfo.ownerUuid)}, 'Owner UUID')">
                  <svg class="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                  复制 UUID
                </button>
              </div>
            </div>

            <div style="display:flex; gap:16px; font-size:12px; color:var(--text-muted); margin-top:4px;">
              <span>脱敏摘要: <strong class="font-mono text-slate-300">550e...0000</strong></span>
              <span>关联业务地址: <strong class="font-mono text-slate-300">${escapeHtml(mockRelayInfo.address)}:${mockRelayInfo.servicePort}</strong></span>
            </div>
          </div>
        </div>

        <!-- AI Controller Token (MCP) Card -->
        <div class="card">
          <div class="section-title">
            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 2l-2 2m-7.61 7.61a5.5 5.5 0 1 1-7.778 7.778 5.5 5.5 0 0 1 7.777-7.777zm0 0L15.5 7.5m0 0l3 3L22 7l-3-3m-3.5 3.5L19 4"/></svg>
            AI Controller Token (MCP 通信凭据)
          </div>
          <p style="font-size:12px; color:var(--text-secondary); margin-bottom:14px; line-height:1.5;">
            该 Token 供 AI Agent / MCP Server 连接 Relay 执行操作使用。为保障安全，后台不长期明文存储该凭据，只展示其 SHA-256 指纹。轮换后新 Token 仅在弹窗中展示一次。
          </p>

          <div class="key-value-list" style="margin-bottom:16px;">
            <div class="kv-item">
              <span class="kv-label">配置状态</span>
              <span class="kv-value">
                <span class="badge badge-online">
                  <span class="badge-dot"></span>
                  已配置 (Configured)
                </span>
              </span>
            </div>
            <div class="kv-item">
              <span class="kv-label">Token 指纹 (SHA-256)</span>
              <span class="kv-value font-mono text-slate-300">${escapeHtml(mockRelayInfo.aiTokenFingerprint)}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">创建时间</span>
              <span class="kv-value font-mono">${mockRelayInfo.aiTokenCreatedAt}</span>
            </div>
            <div class="kv-item">
              <span class="kv-label">最近轮换时间</span>
              <span class="kv-value font-mono">${mockRelayInfo.aiTokenLastRotated}</span>
            </div>
          </div>

          <div style="display:flex; gap:10px;">
            <button class="btn btn-warning btn-sm" disabled title="第一阶段不提供 Token 轮换接口">
              <svg class="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/></svg>
              Token 轮换（暂未开放）
            </button>
            <button class="btn btn-danger btn-sm" disabled title="第一阶段不提供 Token 撤销接口">
              Token 撤销（暂未开放）
            </button>
          </div>
        </div>

        <!-- Human Controller Token Card -->
        <div class="card" style="opacity:0.75;">
          <div class="section-title">
            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="8.5" cy="7" r="4"/></svg>
            Human Controller Token
          </div>
          <div style="font-size:12px; color:var(--text-muted); line-height:1.6;">
            当前阶段状态：<strong class="tag">未启用 / 暂不管理</strong>。<br/>
            第一版重点由 MCP 通过 AI Controller Token 管理远程操作，Human Controller 凭据采用静态白名单或直连控制码，暂不引入复杂独立流转。
          </div>
        </div>
      </div>
    </div>
  `;
}

// Views: Security Settings
function renderSettingsView() {
  return `
    <div>
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z"/><path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06-1.9 1.9-.06-.06a1.7 1.7 0 0 0-1.88-.34 1.7 1.7 0 0 0-1.03 1.56V20h-2.7v-.09a1.7 1.7 0 0 0-1.03-1.56 1.7 1.7 0 0 0-1.88.34l-.06.06-1.9-1.9.06-.06A1.7 1.7 0 0 0 7.76 15a1.7 1.7 0 0 0-1.56-1.03H6v-2.7h.2A1.7 1.7 0 0 0 7.76 10a1.7 1.7 0 0 0-.34-1.88l-.06-.06 1.9-1.9.06.06a1.7 1.7 0 0 0 1.88.34 1.7 1.7 0 0 0 1.03-1.56V5h2.7v.09a1.7 1.7 0 0 0 1.03 1.56 1.7 1.7 0 0 0 1.88-.34l.06-.06 1.9 1.9-.06.06A1.7 1.7 0 0 0 19.4 10c.18.62.75 1.03 1.4 1.03h.2v2.7h-.2A1.7 1.7 0 0 0 19.4 15z"/></svg>
            安全设置 (Security Settings)
          </h1>
          <div class="page-desc">修改管理页面密码。修改成功后所有已登录设备都需要重新登录。</div>
        </div>
      </div>

      <div class="card settings-card">
        <div class="section-title">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>
          修改管理页面密码
        </div>
        <p class="settings-intro">密码会以随机盐哈希形式保存到 Relay 状态文件，不会保存明文。</p>
        <div class="settings-policy">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 8h.01"/></svg>
          <span>新密码至少需要 12 个字符。</span>
        </div>
        <form onsubmit="handleChangePassword(event)" class="settings-form">
          <div class="input-group">
            <label class="input-label">当前密码</label>
            <input id="current-admin-password" type="password" class="input font-mono" autocomplete="current-password" required />
          </div>
          <div class="input-group">
            <label class="input-label">新密码</label>
            <input id="new-admin-password" type="password" class="input font-mono" minlength="12" autocomplete="new-password" required />
          </div>
          <div class="input-group">
            <label class="input-label">确认新密码</label>
            <input id="confirm-admin-password" type="password" class="input font-mono" minlength="12" autocomplete="new-password" required />
          </div>
          <div id="password-change-error" style="display:none; padding:8px 10px; background:var(--status-danger-bg); border:1px solid var(--status-danger-border); border-radius:4px; color:var(--status-danger-text); font-size:11.5px;"></div>
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
  if (state.api.error) return renderErrorState(`管理 API 请求失败：${escapeHtml(state.api.error)}`);
  if (state.prototypeScenario === 'api_error') {
    return renderErrorState('审计日志检索异常: 500 DB Query Timeout');
  }

  let logs = [...mockAuditLogs];

  if (state.filters.auditType && state.filters.auditType !== 'ALL') {
    logs = logs.filter(l => l.action === state.filters.auditType);
  }
  if (state.filters.auditResult && state.filters.auditResult !== 'ALL') {
    logs = logs.filter(l => l.result === state.filters.auditResult);
  }

  return `
    <div>
      <div class="page-header">
        <div>
          <h1 class="page-title">
            <svg class="w-5 h-5 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>
            审计日志 (Audit Logs)
          </h1>
          <div class="page-desc">全面追溯管理员与 Controller 操作痕迹、会话启停、紧急停止与密钥变更</div>
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
            <option value="SESSION_TERMINATE" ${state.filters.auditType === 'SESSION_TERMINATE' ? 'selected' : ''}>关闭会话 (SESSION_TERMINATE)</option>
            <option value="EMERGENCY_STOP" ${state.filters.auditType === 'EMERGENCY_STOP' ? 'selected' : ''}>紧急停止 (EMERGENCY_STOP)</option>
            <option value="TOKEN_ROTATE" ${state.filters.auditType === 'TOKEN_ROTATE' ? 'selected' : ''}>Token 轮换 (TOKEN_ROTATE)</option>
            <option value="ADMIN_LOGIN" ${state.filters.auditType === 'ADMIN_LOGIN' ? 'selected' : ''}>管理员登录 (ADMIN_LOGIN)</option>
          </select>

          <select class="select" onchange="state.filters.auditResult = this.value; renderApp();">
            <option value="ALL" ${state.filters.auditResult === 'ALL' ? 'selected' : ''}>全部结果</option>
            <option value="SUCCESS" ${state.filters.auditResult === 'SUCCESS' ? 'selected' : ''}>成功 (SUCCESS)</option>
            <option value="FAILED" ${state.filters.auditResult === 'FAILED' ? 'selected' : ''}>失败 (FAILED)</option>
          </select>
        </div>
        <div style="font-size:12px; color:var(--text-muted);">
          展示最近 <span class="font-mono text-slate-200">${logs.length}</span> 条审计流水
        </div>
      </div>

      <div class="table-container">
        <div class="table-wrapper">
          <table class="ops-table">
            <thead>
              <tr>
                <th>时间戳 (Timestamp)</th>
                <th>操作者 (Operator)</th>
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
                  <td class="font-mono text-slate-400" style="font-size:11.5px;">${escapeHtml(log.time)}</td>
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
                  <td style="color:var(--text-primary); font-size:12px;">${escapeHtml(log.details)}</td>
                </tr>
              `).join('')}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  `;
}

// Export Audit Logs
function exportAuditLogs() {
  const csvContent = "data:text/csv;charset=utf-8,"
    + ["Time,Operator,Action,Target,Result,IP,Details"].concat(
        mockAuditLogs.map(e => `"${e.time}","${e.operator}","${e.action}","${e.target}","${e.result}","${e.ip}","${e.details}"`)
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
            <p>内部运维安全控制台</p>
          </div>
        </div>

        <form onsubmit="handleLogin(event)" style="display:flex; flex-direction:column; gap:14px;">
          <div class="input-group">
            <label class="input-label">用户名 (Username)</label>
            <input type="text" id="login-username" class="input font-mono" autocomplete="username" required />
          </div>

          <div class="input-group">
            <div style="display:flex; justify-content:space-between; align-items:center;">
              <label class="input-label">密码 (Password)</label>
              <button type="button" class="btn btn-ghost btn-sm" style="padding:0; font-size:11px;" onclick="togglePasswordVisibility()">
                <span id="password-toggle-text">显示</span>
              </button>
            </div>
            <input type="password" id="login-password" class="input font-mono" autocomplete="current-password" required />
            <span style="font-size:10.5px; color:var(--text-muted);">当前页面使用 HTTPS 同源登录，不需要填写 Relay 地址或 MCP Token。</span>
          </div>

          <div id="login-error-box" style="display:none; padding:8px 10px; background:var(--status-danger-bg); border:1px solid var(--status-danger-border); border-radius:4px; color:var(--status-danger-text); font-size:11.5px;"></div>

          <button type="submit" id="login-submit-btn" class="btn btn-primary" style="margin-top:6px; height:36px;">
            进入控制台
          </button>
        </form>

        <div style="border-top:1px solid var(--border-subtle); padding-top:12px; font-size:11px; color:var(--text-muted); text-align:center;">
          默认监听私有网络 / 本地端口 18081
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
    await apiFetch('/api/admin/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password })
    });
    await refreshRealData();
    btn.disabled = false;
    btn.innerText = '进入控制台';
    state.isLoggedIn = true;
    state.currentTab = 'overview';
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
          <div style="font-size:14px; font-weight:700; color:var(--text-primary); display:flex; align-items:center; gap:8px;">
          <span>Session 详情</span>
          <span class="badge ${s.agentStatus === 'online' ? 'badge-online' : 'badge-offline'}">
            <span class="badge-dot"></span>
            ${s.agentStatus === 'online' ? 'Active' : 'Offline'}
          </span>
        </div>
        <div class="font-mono text-slate-400" style="font-size:11px; margin-top:2px;">${escapeHtml(truncate(s.id, 10, 8))}</div>
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
          基础与 Agent 节点信息
        </div>
        <div class="key-value-list">
          <div class="kv-item">
            <span class="kv-label">完整 Session ID</span>
            <span class="kv-value">
              <span class="copyable-text" onclick="copyToClipboard(${eventValue(s.id)}, 'Session ID')">
                ${escapeHtml(s.id)}
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
              </span>
            </span>
          </div>
          <div class="kv-item">
            <span class="kv-label">Agent 节点</span>
            <span class="kv-value font-semibold text-slate-200">${escapeHtml(s.agentName)} (${escapeHtml(s.agentId)})</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">主机名 / OS</span>
            <span class="kv-value font-mono text-slate-300">${escapeHtml(s.hostname)} / ${escapeHtml(s.os)}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">能力列表 (Capabilities)</span>
            <span class="kv-value" style="display:flex; flex-wrap:wrap; gap:4px; justify-content:flex-end;">
              ${s.capabilities.map(c => `<span class="tag">${escapeHtml(c)}</span>`).join('')}
            </span>
          </div>
          <div class="kv-item">
            <span class="kv-label">连接代次 (Generation)</span>
            <span class="kv-value font-mono">Gen #${s.generation}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">创建时间 / 租约到期</span>
            <span class="kv-value font-mono">${s.connectTime} / 剩余 ${s.leaseExpire}</span>
          </div>
        </div>
      </div>

      <!-- Section 2: Controller Info -->
      <div>
        <div class="section-title">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/></svg>
          Controller 绑定与拓扑
        </div>
        <div class="key-value-list">
          <div class="kv-item">
            <span class="kv-label">AI Controller (MCP)</span>
            <span class="kv-value font-semibold text-blue-400">${escapeHtml(s.controllerName)}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">Controller 实例 ID</span>
            <span class="kv-value font-mono">${escapeHtml(s.controllerInstanceId)}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">Human Controller</span>
            <span class="kv-value">${escapeHtml(s.humanController)}</span>
          </div>
          <div class="kv-item">
            <span class="kv-label">Owner UUID</span>
            <span class="kv-value">
              <span class="copyable-text" onclick="copyToClipboard(${eventValue(s.ownerUuid)}, 'Owner UUID')">
                ${escapeHtml(s.ownerUuid)}
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
              </span>
            </span>
          </div>
          <div class="kv-item">
            <span class="kv-label">当前权限模式</span>
            <span class="kv-value">
              <span class="tag" style="color:var(--status-info-text); border-color:var(--status-info-border);">${escapeHtml(s.permissionLabel)}</span>
            </span>
          </div>
        </div>
      </div>

      <!-- Section 3: Activity Info -->
      <div>
        <div class="section-title">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>
          实时活动与指令执行
        </div>
        <div class="grid-2" style="margin-bottom:0;">
          <div class="card" style="padding:10px;">
            <div style="font-size:11px; color:var(--text-muted);">待处理审批</div>
            <div style="font-size:20px; font-weight:700; font-family:var(--font-mono); color:${s.pendingApprovals > 0 ? 'var(--status-degraded-text)' : 'var(--text-primary)'};">${s.pendingApprovals}</div>
          </div>
          <div class="card" style="padding:10px;">
            <div style="font-size:11px; color:var(--text-muted);">进行中请求</div>
            <div style="font-size:20px; font-weight:700; font-family:var(--font-mono);">${s.inflightRequests}</div>
          </div>
        </div>
      </div>

      <!-- Section 4: Danger Zone -->
      <div class="danger-zone">
        <div class="danger-zone-title">
          <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="7.86 2 16.14 2 22 7.86 22 16.14 16.14 22 7.86 22 2 16.14 2 7.86 7.86 2"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
          危险运维操作 (Danger Zone)
        </div>
        <p style="font-size:11.5px; color:var(--text-secondary); line-height:1.4;">
          操作将直接影响远程 Agent 与已绑定的 Controller。所有危险操作均会被记录至审计日志。
        </p>

        <div style="display:flex; flex-direction:column; gap:8px; margin-top:4px;">
          <div style="display:flex; justify-content:space-between; align-items:center;">
            <div>
              <div style="font-size:12px; font-weight:600; color:var(--text-primary);">${s.agentStatus === 'online' ? '关闭当前会话' : '清理离线会话记录'}</div>
              <div style="font-size:11px; color:var(--text-muted);">${s.agentStatus === 'online' ? '断开 Controller 连接并清理会话状态' : 'Agent 离线，清理操作需等待 Relay 状态确认'}</div>
            </div>
            <button class="btn btn-warning btn-sm" ${s.agentStatus === 'online' ? `onclick="openModal('terminateSession', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })"` : 'disabled title="Agent 离线，当前原型不执行清理"'}>${s.agentStatus === 'online' ? '关闭会话' : '暂不可用'}</button>
          </div>

          <div style="display:flex; justify-content:space-between; align-items:center; border-top:1px solid rgba(239,68,68,0.2); padding-top:8px;">
            <div>
              <div style="font-size:12px; font-weight:600; color:var(--status-danger-text);">🛑 紧急停止 (Emergency Stop)</div>
              <div style="font-size:11px; color:var(--text-muted);">${s.agentStatus === 'online' ? '立即终止正在执行的远程操作并强制断开' : 'Agent 离线且没有可立即执行的远程操作'}</div>
            </div>
            <button class="btn btn-danger btn-sm" ${s.agentStatus === 'online' ? `onclick="openModal('emergencyStop', { id: ${eventValue(s.id)}, agentName: ${eventValue(s.agentName)} })"` : 'disabled title="Agent 离线，无法立即执行"'}>紧急停止</button>
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
  let modal = document.getElementById('modal-panel');

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
            确认关闭会话 (Terminate Session)
          </div>
          <button class="btn btn-ghost btn-sm" onclick="closeModal()">✕</button>
        </div>
        <div class="modal-body">
          <p>您即将关闭以下活跃会话：</p>
          <div class="code-box">
            <span>Session ID: ${escapeHtml(data?.id)}</span>
          </div>
          <div style="font-size:12px; color:var(--text-muted); line-height:1.5;">
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
            ⚠️ 确认紧急停止 (Emergency Stop)
          </div>
          <button class="btn btn-ghost btn-sm" onclick="closeModal()">✕</button>
        </div>
        <div class="modal-body">
          <div style="padding:10px; background:var(--status-danger-bg); border:1px solid var(--status-danger-border); border-radius:4px; color:var(--status-danger-text); font-size:12px; line-height:1.5;">
            <strong>高危风险提示：</strong> 此操作将通过 Relay 向 Agent 节点发送紧急停止请求，立即终止正在执行的远程操作并强制断开当前会话，且不可自动恢复！
          </div>
          <div>
            目标会话: <span class="font-mono text-slate-200">${escapeHtml(data?.id)}</span> (${escapeHtml(data?.agentName)})
          </div>
          <div class="input-group">
            <label class="input-label" style="color:var(--status-danger-text);">请输入 "STOP" 以解锁确认按钮：</label>
            <input
              type="text"
              class="input font-mono"
              placeholder="输入 STOP"
              oninput="document.getElementById('emg-stop-btn').disabled = (this.value !== 'STOP');"
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
  } else if (mType === 'rotateToken') {
    modalContent = `
      <div class="modal">
        <div class="modal-header">
          <div class="modal-title">
            <svg class="w-4 h-4 text-amber-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/></svg>
            确认轮换 AI Controller Token
          </div>
          <button class="btn btn-ghost btn-sm" onclick="closeModal()">✕</button>
        </div>
        <div class="modal-body">
          <p style="line-height:1.5;">轮换 Token 后，旧 Token 将立刻失效。所有使用旧 Token 的 MCP Server 必须重新配置新 Token 方可连接。</p>
          <div style="font-size:12px; color:var(--text-muted);">新 Token 仅在生成后的弹窗中展示一次，请及时复制保存。</div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary btn-sm" onclick="closeModal()">取消</button>
          <button class="btn btn-warning btn-sm" onclick="confirmRotateToken()">生成并轮换新 Token</button>
        </div>
      </div>
    `;
  } else if (mType === 'tokenResult') {
    modalContent = `
      <div class="modal">
        <div class="modal-header">
          <div class="modal-title" style="color:var(--status-online-text);">
            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="20 6 9 17 4 12"/></svg>
            Token 轮换成功 (One-time Secret)
          </div>
          <button class="btn btn-ghost btn-sm" onclick="closeModal()">✕</button>
        </div>
        <div class="modal-body">
          <p style="font-size:12px;">以下是新生成的 AI Controller Token（仅展示一次）：</p>
          <div class="code-box">
            <span style="color:#6ee7b7;">${escapeHtml(data?.newToken)}</span>
            <button class="btn btn-secondary btn-sm" onclick="copyToClipboard(${eventValue(data?.newToken)}, '新 Token')">复制</button>
          </div>
          <div style="font-size:11.5px; color:var(--status-degraded-text); line-height:1.4;">
            ⚠️ 关闭此弹窗后将无法再次查看完整明文。请立刻更新 MCP Server 配置文件。
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-primary btn-sm" onclick="closeModal()">我已保存并关闭</button>
        </div>
      </div>
    `;
  }

  backdrop.innerHTML = modalContent;
  setTimeout(() => backdrop.classList.add('open'), 10);
}

// Loading Skeleton
function renderLoadingSkeleton() {
  return `
    <div>
      <div class="page-header">
        <div class="skeleton" style="width:240px; height:28px;"></div>
      </div>
      <div class="grid-4">
        <div class="card skeleton" style="height:100px;"></div>
        <div class="card skeleton" style="height:100px;"></div>
        <div class="card skeleton" style="height:100px;"></div>
        <div class="card skeleton" style="height:100px;"></div>
      </div>
      <div class="card skeleton" style="height:320px; width:100%;"></div>
    </div>
  `;
}

// Error State
function renderErrorState(errMsg) {
  return `
    <div class="empty-state">
      <svg class="empty-state-icon text-red-500" style="opacity:1;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
      <div class="empty-state-title" style="color:var(--status-danger-text);">Relay 服务请求异常</div>
      <div class="empty-state-desc font-mono" style="font-size:11.5px;">${escapeHtml(errMsg)}</div>
      <div style="display:flex; gap:10px; margin-top:8px;">
        <button class="btn btn-secondary btn-sm" onclick="setPrototypeScenario('normal')">恢复正常状态</button>
        <button class="btn btn-primary btn-sm" onclick="showToast('正在重试...', 'info'); refreshRealData().then(renderApp).catch(() => renderApp());">重试连接</button>
      </div>
    </div>
  `;
}

// Empty State Component
function renderEmptyState(title, desc) {
  return `
    <div class="empty-state">
      <svg class="empty-state-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="3" y="3" width="18" height="18" rx="2"/><line x1="9" y1="9" x2="15" y2="15"/><line x1="15" y1="9" x2="9" y2="15"/></svg>
      <div class="empty-state-title">${title}</div>
      <div class="empty-state-desc">${desc}</div>
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
      ${renderScenarioRibbon()}
      ${renderTopBar()}
      <main class="content-area">
        ${contentHtml}
      </main>
    </div>
  `;

  // Check if drawer needs rendering
  if (state.activeDrawerSession) {
    renderDrawer();
  }
}

// Initialization on DOM Loaded
document.addEventListener('DOMContentLoaded', () => {
  // Ensure toast container exists
  if (!document.getElementById('toast-container')) {
    const tc = document.createElement('div');
    tc.id = 'toast-container';
    tc.className = 'toast-container';
    document.body.appendChild(tc);
  }

  restoreSession();
});
