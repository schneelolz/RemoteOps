import type { Catalog, ToolVersion } from "./models";

/** API 结果经既有映射转换为界面模型，文本交由 Vue 转义。 */
export interface AgentRow {
  id: string;
  hostname: string;
  macAddress: string;
  os: string;
  status: string;
  sessionId: string;
  codeConfigured: boolean;
  leaseExpiresAt: string;
  lease: string;
  heartbeat: string;
  ready: boolean;
  everPaired: boolean;
  generation: number;
  supportsAgentShutdown: boolean;
  permissionMode: string;
  permissionLabel: string;
  mcpControlMode: string | null;
  mcpControlLabel: string;
}
export interface SessionRow {
  id: string;
  agentName: string;
  agentMacAddress: string;
  agentId: string;
  agentStatus: string;
  hostname: string;
  os: string;
  role: string;
  controllerType: string;
  controllerName: string;
  controllerHostname: string;
  controllerMacAddress: string;
  controllerInstanceId: string;
  humanController: string;
  ownerUuid: string;
  permissionMode: string;
  permissionLabel: string;
  relayPermissionLabel: string;
  mcpControlMode: string | null;
  mcpControlLabel: string;
  supportsAgentShutdown: boolean;
  agentGeneration: number;
  connectTime: string;
  lastHeartbeat: string;
  pendingApprovals: number;
  inflightRequests: number;
  generation: number;
  leaseExpire: string;
  isClosed: boolean;
  status: string;
}
export interface AuditRow {
  id: string;
  time: string;
  operator: string;
  action: string;
  target: string;
  result: string;
  ip: string;
  details: string;
}
export interface Toast {
  id: string;
  message: string;
  type: string;
}
export interface ShutdownTarget {
  id: string;
  hostname: string;
  generation: number;
}
export interface ModalTarget {
  id?: string;
  kind?: string;
  name?: string;
  agentName?: string;
  generation?: number;
  existing?: boolean;
  version?: string;
  sha256?: string;
  targets?: ShutdownTarget[];
  results?: (ShutdownTarget & { ok: boolean; message: string })[] | null;
  all?: boolean;
  refreshError?: string;
}
export type DrawerRow = Partial<AgentRow & SessionRow> & { id: string };
export interface ToolDraft {
  id: string;
  name: string;
  purpose: string;
  instructions: string;
  examplesText: string;
  dependenciesText: string;
  versionsText: string;
  versions: ToolVersion[];
  enabled: boolean;
  source: string;
  url: string;
  expected_sha256: string;
  os: string;
  arch: string;
  environment: string;
  result: string;
  tested_at: string;
  desktop: boolean;
  status: string;
}
export interface ToolCenterState {
  page: string;
  nodeKeyword: string;
  nodeFilter: string;
  nodePage: number;
  editorTab: string;
  snapshot: Catalog | null;
  loading: boolean;
  busy: boolean;
  error: string;
  keyword: string;
  filter: string;
  selected: string | null;
  listPage: number;
  listPageSize: number;
  detailTab: string;
  detailVersion: string;
  returnToDetails: boolean;
  draft: ToolDraft | null;
  receipt: {
    size: number;
    sha256: string;
    source: { kind: string; url?: string };
  } | null;
  progress: string;
  epoch: number;
  sequence: number;
  guideDraft: string | null;
  guideError: string;
}
export interface McpSetup {
  grant_id: string;
  client_name: string | null;
  state: string;
  created_at: string;
  expires_at: string;
  installation_id: string | null;
}
export interface McpClient {
  installation_id: string;
  client_name: string | null;
  revoked: boolean;
  tool_management: boolean;
  created_at: string;
  last_seen: string | null;
}
export interface McpState {
  epoch: number;
  modalEpoch: number;
  refreshSequence: number;
  accessPolicy: { allow_legacy_token: boolean } | null;
  policyLoaded: boolean;
  policyLoading: boolean;
  policySaving: boolean;
  policySequence: number;
  policyError: string | null;
  settings: Record<string, string> | null;
  settingsDraft: Record<string, string> | null;
  settingsDirty: boolean;
  setups: McpSetup[];
  clients: McpClient[];
  loaded: boolean;
  loading: boolean;
  error: string | null;
  saving: boolean;
  creating: boolean;
  createAttempted: boolean;
  revoking: Set<string>;
  renaming: Set<string>;
  freshSetup: {
    setup: { expires_at: string; [key: string]: unknown };
    setup_code: string;
  } | null;
  setupName: string;
  setupExpiry: number;
  modalError: string | null;
}
export interface ConsoleState {
  theme: string;
  selectedAgents: Map<string, number>;
  agentKeyword: string;
  agentStatus: string;
  shutdownBusy: boolean;
  currentTab: string;
  isLoggedIn: boolean;
  demoMode: boolean;
  adminUser: string;
  activeDrawerAgent: DrawerRow | null;
  activeDrawerSession: DrawerRow | null;
  activeModal: string | null;
  modalTargetData: ModalTarget | null;
  stopInputText: string;
  mcp: McpState;
  toolCenter: ToolCenterState;
  filters: {
    keyword: string;
    status: string;
    controllerType: string;
    permissionMode: string;
    auditType: string;
    auditResult: string;
  };
  visibleColumns: { sessions: string[]; agents: string[] };
  api: {
    baseUrl: string;
    token: string;
    connected: boolean;
    loading: boolean;
    error: string | null;
  };
}
