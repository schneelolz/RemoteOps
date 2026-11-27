import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import vm from "node:vm";
import ts from "typescript";
import { createHash, webcrypto } from "node:crypto";
import { reactive, toRaw } from "vue";

function fixture() {
  const source = ["store", "core", "mcp", "tools"]
    .map((name) =>
      fs.readFileSync(
        new URL("../src/services/" + name + ".js", import.meta.url),
        "utf8",
      ),
    )
    .join("\n");
  const names = [...source.matchAll(/export \{([^}]+)\};/g)].flatMap((m) =>
    m[1]
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean),
  );
  const context = vm.createContext({
    reactive,
    toRaw,
    crypto: webcrypto,
    structuredClone,
    TextEncoder,
    URL,
    URLSearchParams,
    Uint8Array,
    Uint32Array,
    DataView,
    Map,
    Set,
    Date,
    Blob,
    console,
    setTimeout() {},
    localStorage: {
      getItem() {
        return null;
      },
      setItem() {},
      removeItem() {},
    },
    document: {
      getElementById() {
        return null;
      },
      activeElement: null,
    },
    window: {
      location: {
        origin: "http://localhost",
        hostname: "localhost",
        host: "localhost",
      },
      matchMedia() {
        return { matches: false };
      },
    },
    router: { push() {}, replace() {} },
    navigator: { clipboard: { writeText: async () => {} } },
  });
  const syntax = ts.createSourceFile(
    "actions.js",
    source,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.JS,
  );
  const body = syntax.statements
    .filter(
      (node) => !ts.isImportDeclaration(node) && !ts.isExportDeclaration(node),
    )
    .map((node) => node.getText(syntax))
    .join("\n");
  vm.runInContext(
    body.replace(
      "export function configureNavigation",
      "function configureNavigation",
    ) +
      `\nglobalThis.actions={${names.join(",")}};globalThis.Sha=ToolPackageSha256;`,
    context,
  );
  const c = context.actions;
  c.state.isLoggedIn = true;
  c.state.currentTab = "tools";
  c.state.toolCenter.snapshot = { revision: 7, tools: [], nodes: [] };
  return { c, context };
}
test("Vue 响应式目录克隆为编辑草稿，不改变原记录", () => {
  const { c } = fixture();
  const tool = {
    id: "tool",
    name: "工具",
    purpose: "测试",
    instructions: "使用",
    enabled: true,
    examples: ["one"],
    dependencies: [],
    versions: [],
  };
  c.state.toolCenter.snapshot.tools.push(tool);
  c.openToolCenterEditor("tool");
  c.state.toolCenter.draft.name = "新名称";
  assert.equal(c.state.toolCenter.snapshot.tools[0].name, "工具");
  assert.equal(c.state.toolCenter.editorTab, "basic");
});
test("同步状态优先处理旧端和离线，修订号相等不能代替成功回执", () => {
  const { c } = fixture();
  assert.equal(
    c.toolNodeStatus({ supported: false, online: false }, 7),
    "upgrade_required",
  );
  assert.equal(c.toolNodeStatus({ online: false, synced: true }, 7), "offline");
  assert.equal(
    c.toolNodeStatus(
      { applied_revision: 7, synced: false, sync_status: "apply_failed" },
      7,
    ),
    "apply_failed",
  );
  assert.notEqual(
    c.toolNodeStatus({ applied_revision: 7, synced: false }, 7),
    "synced",
  );
});
test("旧目录响应在离开页面后不能覆盖状态", async () => {
  const { c, context } = fixture();
  let finish;
  context.apiFetch = () =>
    new Promise((r) => {
      finish = r;
    });
  const pending = c.refreshToolsData();
  c.invalidateToolsView();
  finish({ data: { revision: 99, tools: [] } });
  assert.equal(await pending, false);
  assert.equal(c.state.toolCenter.snapshot.revision, 7);
});
test("目录并发冲突不采纳新修订，要求重新合并", async () => {
  const { c, context } = fixture();
  context.apiFetch = async (_, options) => ({
    request_id: JSON.parse(options.body).request_id,
    revision: 8,
    error: { code: "revision_conflict", message: "目录已变化" },
  });
  await assert.rejects(
    c.toolCenterCommand({ action: "set_policy", enabled: true }),
    /刷新后合并/,
  );
  assert.equal(c.state.toolCenter.snapshot.revision, 7);
});
test("结果未知不自动重放写请求", async () => {
  const { c, context } = fixture();
  let calls = 0;
  context.apiFetch = async () => {
    calls++;
    throw Error("network");
  };
  await assert.rejects(
    c.toolCenterCommand({ action: "set_policy", enabled: true }),
    /提交结果尚未确认/,
  );
  assert.equal(calls, 1);
});
test("全局说明 UTF-8 超限保留草稿且不发送请求", async () => {
  const { c, context } = fixture();
  let calls = 0;
  context.apiFetch = async () => {
    calls++;
  };
  c.state.toolCenter.guideDraft = "中".repeat(10923);
  await c.saveToolGuide({ preventDefault() {} });
  assert.match(c.state.toolCenter.guideError, /32 KiB/);
  assert.equal(c.state.toolCenter.guideDraft.length, 10923);
  assert.equal(calls, 0);
});
test("跨页签校验定位 JSON 与基本资料，保留全部草稿", async () => {
  const { c } = fixture();
  c.openToolCenterEditor();
  Object.assign(c.state.toolCenter.draft, {
    id: "test",
    name: "名称",
    purpose: "用途",
    instructions: "说明",
    versionsText: "{",
  });
  await c.saveManagedTool({ preventDefault() {} });
  assert.equal(c.state.toolCenter.editorTab, "advanced");
  assert.equal(c.state.toolCenter.draft.name, "名称");
  c.state.toolCenter.draft.name = "";
  await c.saveManagedTool({ preventDefault() {} });
  assert.equal(c.state.toolCenter.editorTab, "basic");
});
test("节点关闭请求只接受确认时的连接代次，不改投重连节点", async () => {
  const { c, context } = fixture();
  c.state.currentTab = "agents";
  c.agents.push({
    id: "a",
    hostname: "A",
    generation: 1,
    status: "online",
    supportsAgentShutdown: true,
  });
  c.state.selectedAgents.set("a", 1);
  c.openAgentShutdown(false);
  c.agents[0].generation = 2;
  let calls = 0;
  context.apiFetch = async () => {
    calls++;
  };
  context.refreshRealData = async () => {};
  await c.confirmShutdownAgent();
  assert.equal(calls, 0);
  assert.equal(c.state.modalTargetData.results[0].ok, false);
});
test("一次性设置关闭立即清除 DOM 与内存", () => {
  const { c, context } = fixture();
  const field = { value: "secret", textContent: "secret" };
  context.document.getElementById = () => field;
  c.state.mcp.freshSetup = { setup: {}, setup_code: "secret" };
  c.state.activeModal = "mcpFresh";
  c.closeModal();
  assert.equal(c.state.mcp.freshSetup, null);
  assert.equal(field.value, "");
  assert.equal(field.textContent, "");
});
test("关闭后的迟到设置响应不能重新显示秘密", async () => {
  const { c, context } = fixture();
  c.state.currentTab = "mcp";
  c.state.mcp.settings = { relay: "localhost:7443" };
  c.openMcpSetup();
  let finish;
  context.apiFetch = () =>
    new Promise((r) => {
      finish = r;
    });
  const pending = c.createMcpSetup();
  c.closeModal();
  finish({
    setup: { version: 1, grant_id: "id", grant_secret: "private" },
    setup_code: "private",
  });
  await pending;
  assert.equal(c.state.mcp.freshSetup, null);
  assert.equal(c.state.activeModal, null);
});
test("普通客户端默认权限不被缺失字段提升", async () => {
  const { c, context } = fixture();
  c.state.currentTab = "mcp";
  context.apiFetch = async (url) =>
    url.endsWith("/clients")
      ? [
          {
            installation_id: "a",
            client_name: "A",
            revoked: false,
            secret: "must not retain",
          },
        ]
      : url.endsWith("/setups")
        ? []
        : url.endsWith("/access-policy")
          ? { allow_legacy_token: false }
          : {};
  await c.refreshMcpData();
  assert.equal(c.state.mcp.clients[0].tool_management, false);
  assert.equal(c.state.mcp.clients[0].secret, undefined);
  assert.equal(c.state.mcp.accessPolicy.allow_legacy_token, false);
});
test("增量摘要覆盖分块与填充边界", () => {
  const { context } = fixture();
  for (const size of [0, 1, 55, 56, 63, 64, 65, 100001]) {
    const bytes = Buffer.alloc(size, 73);
    const hash = new context.Sha();
    for (let i = 0; i < size; i += 997) hash.update(bytes.subarray(i, i + 997));
    assert.equal(
      hash.digest(),
      createHash("sha256").update(bytes).digest("hex"),
    );
  }
});
test("启动适配显式模式与旧默认规则保持一致", () => {
  const { c } = fixture();
  assert.equal(
    c.browserAdapterFacts({
      interface: "stdio_mcp",
      browser_executable: "chrome.exe",
    }).legacy,
    true,
  );
  for (const kind of [
    "none",
    "playwright",
    "agent_browser",
    "agent_browser_cli",
  ])
    assert(c.browserStartupExample(kind).startup);
});

test("详情进入编辑后取消恢复详情页签和版本，关闭详情才退出弹窗", () => {
  const { c } = fixture();
  c.state.toolCenter.snapshot.tools.push({
    id: "test",
    name: "工具",
    purpose: "用途",
    instructions: "说明",
    examples: [],
    dependencies: [],
    enabled: false,
    versions: [{ version: "1.0", packages: [] }],
  });
  c.selectManagedTool("test");
  assert.equal(c.state.activeModal, "toolDetails");
  c.state.toolCenter.detailTab = "packages";
  c.openToolCenterEditor("test");
  assert.equal(c.state.activeModal, "toolCenterEdit");
  assert.equal(c.state.toolCenter.returnToDetails, true);
  c.closeModal();
  assert.equal(c.state.activeModal, "toolDetails");
  assert.equal(c.state.toolCenter.detailTab, "packages");
  assert.equal(c.state.toolCenter.detailVersion, "1.0");
  c.closeModal();
  assert.equal(c.state.activeModal, null);
});

test("测试登记取消返回详情，离开页面清除详情返回目标", () => {
  const { c } = fixture();
  c.state.toolCenter.snapshot.tools.push({
    id: "test",
    versions: [{ version: "1", packages: [] }],
  });
  c.selectManagedTool("test");
  c.openToolPlatformTest("test", "1", "a".repeat(64));
  c.closeModal();
  assert.equal(c.state.activeModal, "toolDetails");
  c.openToolPlatformTest("test", "1", "a".repeat(64));
  c.invalidateToolsView();
  assert.equal(c.state.activeModal, null);
  assert.equal(c.state.toolCenter.selected, null);
  assert.equal(c.state.toolCenter.returnToDetails, false);
});

test("详情转编辑保存后重新读取工具，并回到原详情", async () => {
  const { c, context } = fixture();
  const tool = {
    id: "test",
    name: "名称",
    purpose: "用途",
    instructions: "说明",
    enabled: false,
    examples: [],
    dependencies: [],
    versions: [{ version: "1", packages: [] }],
  };
  c.state.toolCenter.snapshot.tools.push(tool);
  c.selectManagedTool("test");
  c.openToolCenterEditor("test");
  c.state.toolCenter.draft.name = "保存后名称";
  let writes = 0;
  context.apiFetch = async (_url, options) => {
    if (options) {
      writes++;
      const request = JSON.parse(options.body);
      return {
        request_id: request.request_id,
        revision: 8,
        data: {},
        error: null,
      };
    }
    return {
      data: {
        revision: 8,
        tools: [{ ...tool, name: "保存后名称" }],
        nodes: [],
      },
    };
  };
  await c.saveManagedTool({ preventDefault() {} });
  assert.equal(writes, 1);
  assert.equal(c.state.activeModal, "toolDetails");
  assert.equal(c.state.toolCenter.snapshot.tools[0].name, "保存后名称");
});

test("会话列表控制端表头统一为“控制端”", () => {
  const { c } = fixture();
  const col = c.sessionColumnDefinitions.find(
    (item) => item.key === "controller",
  );
  assert.equal(col?.label, "控制端");
});

test("双绑定会话可按人工与 MCP 完整 UUID、短 ID 和主机名搜索，无关 UUID 命中 0 条", () => {
  const { c } = fixture();
  const humanUuid = "11111111-2222-3333-4444-aaaaaaaaaaaa";
  const mcpUuid = "55555555-6666-7777-8888-bbbbbbbbbbbb";
  const unrelatedUuid = "99999999-9999-9999-9999-999999999999";

  c.applyApiData(
    {
      version: "0.2.0-preview.5",
      uptime_seconds: 120,
      online_agents: 1,
      active_sessions: 1,
      connected_controllers: 2,
      pending_approvals: 0,
      in_flight_requests: 0,
      owner_id: "00000000-0000-0000-0000-000000000001",
    },
    {},
    [
      {
        agent_instance_id: "agent-dual-1",
        hostname: "FIELD-DUAL-01",
        state: "online",
        session_id: "sess-dual-1",
      },
    ],
    [
      {
        session_id: "sess-dual-1",
        agent_instance_id: "agent-dual-1",
        hostname: "FIELD-DUAL-01",
        state: "online",
        permission_mode: "approval_required",
        controller_bindings: [
          {
            kind: "ai",
            controller_instance_id: mcpUuid,
            hostname: "MCP-HOST-DEV",
          },
          {
            kind: "human",
            controller_instance_id: humanUuid,
            hostname: "OPS-LAPTOP-01",
          },
        ],
      },
    ],
    [],
  );

  for (const query of [
    humanUuid,
    `  ${humanUuid.toUpperCase()}  `,
    mcpUuid,
    "11111111",
    "55555555",
    "MCP-HOST-DEV",
    "OPS-LAPTOP-01",
  ]) {
    c.state.filters.keyword = query;
    const matched = c.filteredSessions();
    assert.equal(matched.length, 1, `查询 ${query} 应命中双绑定会话`);
    assert.equal(matched[0].id, "sess-dual-1");
  }

  c.state.filters.keyword = unrelatedUuid;
  assert.equal(c.filteredSessions().length, 0);
});

test("会话空状态区分控制端待配对与筛选排除，清除筛选恢复列表", () => {
  const { c } = fixture();

  // 场景 1：已有控制端连接、尚无设备绑定会话且未设置筛选
  c.clearSessionFilters();
  c.applyApiData(
    {
      version: "0.2.0-preview.5",
      uptime_seconds: 60,
      online_agents: 0,
      active_sessions: 0,
      connected_controllers: 1,
      pending_approvals: 0,
      in_flight_requests: 0,
    },
    {},
    [],
    [],
    [],
  );
  const waiting = c.sessionEmptyState(true);
  assert.equal(waiting.title, "控制端已连接，等待配对设备");
  assert.equal(waiting.showClearFilters, false);

  // 场景 2：存在会话但被搜索或筛选排除，提供清除筛选并可恢复
  c.applyApiData(
    {
      version: "0.2.0-preview.5",
      uptime_seconds: 60,
      online_agents: 1,
      active_sessions: 1,
      connected_controllers: 1,
      pending_approvals: 0,
      in_flight_requests: 0,
    },
    {},
    [],
    [
      {
        session_id: "sess-only-1",
        agent_instance_id: "agent-1",
        hostname: "FIELD-01",
        state: "online",
        controller_bindings: [
          {
            kind: "human",
            controller_instance_id: "aaaaaaaa-1111-2222-3333-444444444444",
          },
        ],
      },
    ],
    [],
  );
  c.state.filters.keyword = "no-match-uuid";
  assert.equal(c.filteredSessions().length, 0);
  const filteredEmpty = c.sessionEmptyState(true);
  assert.equal(filteredEmpty.title, "暂无匹配会话");
  assert.equal(filteredEmpty.showClearFilters, true);

  c.clearSessionFilters();
  assert.equal(c.filteredSessions().length, 1);
});

test("在线无绑定 Agent 显示为待配对，离线无绑定保留为已关闭，且待配对状态同步反映控制端等待配对", () => {
  const { c } = fixture();
  c.clearSessionFilters();
  c.applyApiData(
    {
      version: "0.2.0-preview.5",
      uptime_seconds: 90,
      online_agents: 1,
      active_sessions: 0,
      connected_controllers: 1,
      pending_approvals: 0,
      in_flight_requests: 0,
    },
    {},
    [
      {
        agent_instance_id: "agent-online-unbound",
        hostname: "FIELD-ONLINE-UNBOUND",
        state: "online",
        session_id: "sess-online-unbound",
      },
      {
        agent_instance_id: "agent-offline-closed",
        hostname: "FIELD-OFFLINE-CLOSED",
        state: "offline",
        session_id: "sess-offline-closed",
      },
    ],
    [
      {
        session_id: "sess-online-unbound",
        agent_instance_id: "agent-online-unbound",
        hostname: "FIELD-ONLINE-UNBOUND",
        state: "online",
        role: "unbound",
        controller_bindings: [],
      },
      {
        session_id: "sess-offline-closed",
        agent_instance_id: "agent-offline-closed",
        hostname: "FIELD-OFFLINE-CLOSED",
        state: "offline",
        role: "unbound",
        controller_bindings: [],
      },
    ],
    [],
  );

  const onlineUnbound = c.sessions.find(
    (item) => item.id === "sess-online-unbound",
  );
  const offlineClosed = c.sessions.find(
    (item) => item.id === "sess-offline-closed",
  );
  assert.equal(onlineUnbound?.status, "WAITING");
  assert.equal(onlineUnbound?.isClosed, false);
  assert.equal(offlineClosed?.status, "CLOSED");
  assert.equal(offlineClosed?.isClosed, true);

  assert.equal(c.hasBoundSessions(), false);
  assert.equal(c.isControllerWaitingForPairing(), true);
  const empty = c.sessionEmptyState(true);
  assert.equal(empty.title, "控制端已连接，等待配对设备");
  assert.equal(empty.showClearFilters, false);
});
