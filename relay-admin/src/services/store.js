import { reactive } from "vue";
/** @type {import("../console-types").ConsoleState} */
const state = reactive({
  theme: readThemePreference(),
  selectedAgents: new Map(),
  agentKeyword: "",
  agentStatus: "ALL",
  shutdownBusy: false,
  currentTab: "overview", // 'overview', 'sessions', 'agents', 'mcp', 'tools', 'identity', 'settings', 'audit'
  isLoggedIn: false,
  demoMode: false,
  adminUser: "",
  activeDrawerSession: null,
  activeDrawerAgent: null,
  activeModal: null, // 'terminateSession', 'emergencyStop', 'shutdownAgent', 'purgeClosedSessions'
  modalTargetData: null,
  stopInputText: "",
  mcp: {
    epoch: 0,
    modalEpoch: 0,
    refreshSequence: 0,
    accessPolicy: null,
    policyLoaded: false,
    policyLoading: false,
    policySaving: false,
    policySequence: 0,
    policyError: null,
    settings: null,
    settingsDraft: null,
    settingsDirty: false,
    setups: [],
    clients: [],
    loaded: false,
    loading: false,
    error: null,
    saving: false,
    creating: false,
    createAttempted: false,
    revoking: new Set(),
    renaming: new Set(),
    freshSetup: null,
    setupName: "",
    setupExpiry: 24,
    modalError: null,
  },
  filters: {
    keyword: "",
    status: "ALL",
    controllerType: "ALL",
    permissionMode: "ALL",
    auditType: "ALL",
    auditResult: "ALL",
  },
  visibleColumns: {
    sessions: [
      "connectionStatus",
      "agentHostname",
      "controller",
      "controlMode",
      "lastHeartbeat",
    ],
    agents: [
      "connectionStatus",
      "hostname",
      "controlCode",
      "session",
      "controlMode",
      "lastHeartbeat",
    ],
  },
  api: {
    baseUrl: "",
    token: "",
    connected: false,
    loading: false,
    error: null,
  },
  toolCenter: {
    page: "settings",
    nodeKeyword: "",
    nodeFilter: "all",
    nodePage: 1,
    editorTab: "basic",
    snapshot: null,
    loading: false,
    busy: false,
    error: "",
    keyword: "",
    filter: "all",
    selected: null,
    listPage: 1,
    listPageSize: 20,
    detailTab: "guide",
    detailVersion: "",
    returnToDetails: false,
    draft: null,
    receipt: null,
    progress: "",
    epoch: 0,
    sequence: 0,
    guideDraft: null,
    guideError: "",
  },
});

/** @type {{address:string;host:string;version:string;uptime:string;uptimeSeconds:number;onlineAgents:number;activeSessions:number;connectedControllers:number;pendingApprovals:number;inFlightRequests:number;ownerUuid:string;allowLegacyToken:boolean|null;aiTokenConfigured:boolean;aiTokenFingerprint:string;humanTokenConfigured:boolean}} */
const relayInfo = reactive({
  address: "-",
  host: "-",
  version: "-",
  uptime: "-",
  uptimeSeconds: 0,
  onlineAgents: 0,
  activeSessions: 0,
  connectedControllers: 0,
  pendingApprovals: 0,
  inFlightRequests: 0,
  ownerUuid: "",
  allowLegacyToken: null,
  aiTokenConfigured: false,
  aiTokenFingerprint: "",
  humanTokenConfigured: false,
});

/** @type {import("../console-types").SessionRow[]} */
const sessions = reactive([]);

/** @type {import("../console-types").AgentRow[]} */
const agents = reactive([]);

/** @type {import("../console-types").AuditRow[]} */
const auditLogs = reactive([]);

/** @type {{time:string;type:string;desc:string;badge:string}[]} */
const recentEvents = reactive([]);

function readThemePreference() {
  try {
    const value = localStorage.getItem("remoteops-theme");
    return ["system", "light", "dark"].includes(value) ? value : "system";
  } catch (_) {
    return "system";
  }
}

export {
  state,
  relayInfo,
  sessions,
  agents,
  auditLogs,
  recentEvents,
  readThemePreference,
};
