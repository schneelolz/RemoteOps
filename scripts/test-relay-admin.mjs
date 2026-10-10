import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../relay-admin-prototype/app.js', import.meta.url), 'utf8');

// 在隔离的浏览器环境运行真实业务函数，不连接实际 Relay 或关闭节点。
function harness({ preference, dark = false } = {}) {
  const stored = new Map(preference === undefined ? [] : [['remoteops-theme', preference]]);
  const attributes = new Map();
  const events = new Map();
  const windowEvents = new Map();
  const nodes = new Map();
  const copied = [];
  const blobs = [];
  const revokedUrls = [];
  const downloads = [];
  class BrowserURL extends URL {
    static createObjectURL(blob) { blobs.push(blob); return 'blob:test-setup'; }
    static revokeObjectURL(url) { revokedUrls.push(url); }
  }
  const mediaListeners = [];
  const media = {
    matches: dark,
    addEventListener: (_event, callback) => mediaListeners.push(callback),
    addListener: callback => mediaListeners.push(callback),
  };
  const context = vm.createContext({
    console,
    URL: BrowserURL,
    URLSearchParams,
    Blob,
    navigator: { clipboard: { writeText: async text => { copied.push(text); } } },
    setTimeout: () => 0,
    clearTimeout: () => {},
    setInterval: () => 0,
    clearInterval: () => {},
    localStorage: {
      getItem: key => stored.get(key) ?? null,
      setItem: (key, value) => stored.set(key, String(value)),
      removeItem: key => stored.delete(key),
    },
    window: {
      location: { protocol: 'https:', search: '', hash: '', origin: 'https://relay.test', hostname: 'relay.test' },
      matchMedia: () => media,
      history: { replaceState() {} },
      addEventListener: (event, callback) => windowEvents.set(event, callback),
    },
    document: {
      documentElement: { setAttribute: (key, value) => attributes.set(key, value), style: {} },
      addEventListener: (event, callback) => events.set(event, callback),
      getElementById: id => nodes.get(id) || null,
      querySelector: () => null,
      querySelectorAll: () => [],
      createElement: () => ({
        classList: { add() {}, remove() {} },
        innerHTML: '',
        querySelector: () => null,
        click() { downloads.push({ href: this.href, download: this.download }); },
        remove() { nodes.delete(this.id); },
      }),
      body: { appendChild(node) { if (node.id) nodes.set(node.id, node); } },
    },
  });
  vm.runInContext(source, context, { filename: 'relay-admin-prototype/app.js' });
  vm.runInContext(`
    globalThis.realRenderModal = renderModal;
    renderApp = () => {};
    renderModal = () => {};
    closeDrawer = () => {};
    restoreSession = () => {};
    restoreVisibleColumns = () => {};
    refreshRealData = async () => {};
    globalThis.toasts = [];
    showToast = (message, type) => toasts.push({ message, type });
    globalThis.requests = [];
    apiFetch = async (url, options) => { requests.push({url, ...options}); return {}; };
  `, context);
  const run = code => vm.runInContext(code, context);
  return {
    run,
    stored,
    attributes,
    nodes,
    copied,
    blobs,
    revokedUrls,
    downloads,
    dispatchWindowEvent: name => windowEvents.get(name)(),
    init: () => events.get('DOMContentLoaded')(),
    changeSystemTheme(value) {
      media.matches = value;
      for (const callback of mediaListeners) callback({ matches: value });
    },
    data: code => JSON.parse(JSON.stringify(run(code))),
  };
}

function seed(h) {
  h.run(`agents.push(
    {id:'a', hostname:'节点01', status:'online', supportsAgentShutdown:true, generation:10},
    {id:'b', hostname:'节点02', status:'online', supportsAgentShutdown:true, generation:20},
    {id:'offline', hostname:'离线', status:'offline', supportsAgentShutdown:true, generation:30},
    {id:'old', hostname:'旧版', status:'online', supportsAgentShutdown:false, generation:40},
    {id:'invalid', hostname:'无代次', status:'online', supportsAgentShutdown:true, generation:0}
  );`);
}

test('首次访问跟随系统，系统切换即时更新主题', () => {
  const h = harness({ dark: true });
  h.init();
  assert.equal(h.run('state.theme'), 'system');
  assert.equal(h.attributes.get('data-theme'), 'dark');
  h.changeSystemTheme(false);
  assert.equal(h.attributes.get('data-theme'), 'light');
});

test('手动主题保存且不受系统变化影响，可恢复跟随系统', () => {
  const h = harness({ dark: false, preference: 'dark' });
  h.init();
  assert.equal(h.attributes.get('data-theme'), 'dark');
  h.run("setTheme('light')");
  assert.equal(h.stored.get('remoteops-theme'), 'light');
  h.changeSystemTheme(true);
  assert.equal(h.attributes.get('data-theme'), 'light');
  h.run("setTheme('system')");
  assert.equal(h.attributes.get('data-theme'), 'dark');
});

test('损坏的主题偏好回退到跟随系统', () => {
  const h = harness({ preference: 'unexpected', dark: false });
  h.init();
  assert.equal(h.run('state.theme'), 'system');
  assert.equal(h.attributes.get('data-theme'), 'light');
});

test('仅允许选择在线、支持关闭且具有有效连接代次的节点', () => {
  const h = harness();
  seed(h);
  h.run('agents.forEach(a => selectAgent(a.id, true))');
  assert.deepEqual(h.data('[...state.selectedAgents.keys()]'), ['a', 'b']);
});

test('连接代次变化或离线后清除选择，避免误关闭新连接', () => {
  const h = harness();
  seed(h);
  h.run("selectAgent('a', true); selectAgent('b', true); agents[0].generation++; agents[1].status='offline'; reconcileAgentSelection();");
  assert.equal(h.run('state.selectedAgents.size'), 0);
});

test('关闭所选与关闭全部的目标语义不同，全部不受搜索条件限制', () => {
  const h = harness();
  seed(h);
  h.run("selectAgent('a', true); state.agentKeyword='节点01'; openAgentShutdown(false)");
  assert.deepEqual(h.data('state.modalTargetData.targets.map(a => a.id)'), ['a']);
  h.run('openAgentShutdown(true)');
  assert.deepEqual(h.data('state.modalTargetData.targets.map(a => a.id)'), ['a', 'b']);
});

test('表头全选只操作筛选后可关闭节点，不清除隐藏的选择', () => {
  const h = harness();
  seed(h);
  h.run("selectAgent('b', true); state.agentKeyword='节点01'; selectVisibleAgents(true);");
  assert.deepEqual(h.data('[...state.selectedAgents.keys()].sort()'), ['a', 'b']);
  h.run('selectVisibleAgents(false)');
  assert.deepEqual(h.data('[...state.selectedAgents.keys()]'), ['b']);
});

test('确认后出现新节点不会扩大已确认的关闭范围', async () => {
  const h = harness();
  seed(h);
  h.run("selectAgent('a', true); openAgentShutdown(false); agents.push({id:'new', hostname:'新节点', status:'online', supportsAgentShutdown:true, generation:50}); selectAgent('b', true);");
  await h.run('confirmShutdownAgent()');
  assert.deepEqual(h.data('requests.map(r => JSON.parse(r.body))'), [{ agent_instance_id: 'a', connection_generation: 10 }]);
});

test('弹窗打开后重连不会将请求改投新连接代次', async () => {
  const h = harness();
  seed(h);
  h.run("selectAgent('a', true); openAgentShutdown(false); agents[0].generation = 99;");
  await h.run('confirmShutdownAgent()');
  assert.equal(h.run('requests.length'), 0);
  assert.equal(h.run('state.modalTargetData.results[0].ok'), false);
  assert.match(h.run('state.modalTargetData.results[0].message'), /连接已变化/);
});

test('关闭进行中重复确认不会再次提交请求', async () => {
  const h = harness();
  seed(h);
  h.run("selectAgent('a', true); openAgentShutdown(false); apiFetch = async (url, options) => { requests.push({url, ...options}); await new Promise(resolve => globalThis.releaseRequest = resolve); return {}; };");
  const first = h.run('confirmShutdownAgent()');
  await h.run('confirmShutdownAgent()');
  assert.equal(h.run('requests.length'), 1);
  h.run('releaseRequest()');
  await first;
  assert.equal(h.run('state.shutdownBusy'), false);
});

test('部分失败保留逐节点结果且不会自动重试已成功目标', async () => {
  const h = harness();
  seed(h);
  h.run("openAgentShutdown(true); apiFetch = async (url, options) => { requests.push({url, ...options}); if (url.includes('/b/')) throw new Error('连接已更换'); return {}; };");
  await h.run('confirmShutdownAgent()');
  const results = h.data('state.modalTargetData.results');
  assert.equal(results.length, 2);
  assert.equal(results.find(r => r.id === 'a').ok, true);
  assert.equal(results.find(r => r.id === 'b').ok, false);
  assert.match(results.find(r => r.id === 'b').message, /连接已更换/);
  await h.run('confirmShutdownAgent()');
  assert.equal(h.run('requests.length'), 2);
});

test('关闭后的列表刷新失败保留操作结果并解除提交锁', async () => {
  const h = harness();
  seed(h);
  h.run("selectAgent('a', true); openAgentShutdown(false); refreshRealData = async () => { throw new Error('网络不可用'); };");
  await h.run('confirmShutdownAgent()');
  assert.equal(h.run('state.modalTargetData.results[0].ok'), true);
  assert.equal(h.run('state.modalTargetData.refreshError'), '网络不可用');
  assert.equal(h.run('state.shutdownBusy'), false);
});

// All onboarding fixtures are synthetic, local-only values with no usable credentials.
const mcpSettings = {
  relay: 'relay.example.test:7443', server_name: 'relay.example.test',
  enrollment_url: 'https://enroll.example.test:18443/api/mcp/enroll',
  relay_ca_pem: null, enrollment_ca_pem: null,
};
const setupFixture = {
  version: 1, grant_id: 'fixture-grant', grant_secret: 'NONFUNCTIONAL-TEST-SECRET',
  ...mcpSettings, expires_at: '2026-10-11T12:00:00Z',
};
const codeFixture = 'remoteops-setup-v1.' + Buffer.from(JSON.stringify(setupFixture)).toString('base64url');

function seedMcp(h) {
  h.run(`
    state.isLoggedIn = true;
    state.currentTab = 'mcp';
    state.mcp.loaded = true;
    state.mcp.settings = ${JSON.stringify(mcpSettings)};
    state.mcp.settingsDraft = mcpSettingsDraft(state.mcp.settings);
    globalThis.setupResponse = ${JSON.stringify({ setup: setupFixture, setup_code: codeFixture })};
    globalThis.listSetups = [];
    globalThis.listClients = [];
    apiFetch = async (url, options = {}) => {
      requests.push({ url, ...options });
      if (url === '/api/admin/mcp/settings') return ${JSON.stringify(mcpSettings)};
      if (url === '/api/admin/mcp/setups' && options.method === 'POST') return setupResponse;
      if (url === '/api/admin/mcp/setups') return listSetups;
      if (url === '/api/admin/mcp/clients') return listClients;
      return {success:true, changed:true};
    };
  `);
}

function seedFreshMcp(h) {
  seedMcp(h);
  h.run('state.mcp.freshSetup = setupResponse; state.activeModal = "mcpFresh";');
}

test('MCP 设置必须明确配置，未配置时不会推导当前管理地址或生成凭据', async () => {
  const h = harness();
  seedMcp(h);
  h.run('state.mcp.settings = null; state.mcp.settingsDraft = null; openMcpSetup();');
  assert.equal(h.run('state.activeModal'), null);
  const html = h.run('renderMcpView()');
  assert.match(html, /尚未配置/);
  assert.match(html, /id="mcp-relay"[^>]*value=""/);
  assert.doesNotMatch(html, /value="https:\/\/relay\.test/);
  assert.equal(h.run('requests.length'), 0);
});

test('MCP 地址校验拒绝监听地址、不明确的端口、非 HTTPS 和私钥', () => {
  const h = harness();
  const validate = values => h.run(`validateMcpSettings(${JSON.stringify({ ...mcpSettings, ...values })})`);
  assert.equal(validate({ relay: 'relay.example.test:443' }).relay, 'relay.example.test:443');
  assert.equal(validate({ relay: '[2001:db8::1]:7443' }).relay, '[2001:db8::1]:7443');
  for (const relay of ['0.0.0.0:7443', '[::]:7443', 'relay.example.test', 'relay.example.test:0', 'relay.example.test:65536', 'https://relay.example.test:7443']) {
    assert.throws(() => validate({ relay }));
  }
  for (const enrollment_url of ['http://relay.example.test/enroll', 'https://u:p@relay.example.test/enroll', 'https://relay.example.test/enroll#secret', 'https://relay.example.test/enroll?token=bad']) {
    assert.throws(() => validate({ enrollment_url }));
  }
  assert.throws(() => validate({ relay_ca_pem: '-----BEGIN PRIVATE KEY-----\nBAD\n-----END PRIVATE KEY-----' }));
  assert.equal(validate({ relay_ca_pem: '   ' }).relay_ca_pem, null);
});

test('MCP 设置保存时包含明确地址、公开证书且阻止重复提交', async () => {
  const h = harness();
  seedMcp(h);
  h.run(`apiFetch = async (url, options) => { requests.push({url, ...options}); await new Promise(resolve => globalThis.releaseSave = resolve); return ${JSON.stringify(mcpSettings)}; };`);
  const first = h.run('saveMcpSettings()');
  await h.run('saveMcpSettings()');
  assert.equal(h.run('requests.length'), 1);
  assert.deepEqual(h.data('JSON.parse(requests[0].body)'), mcpSettings);
  h.run('releaseSave()');
  await first;
  assert.equal(h.run('state.mcp.saving'), false);
  assert.equal(h.run('state.mcp.settingsDirty'), false);
});

test('MCP 设置与客户端列表转义名称、标识和无效时间，不允许事件注入', () => {
  const h = harness();
  seedMcp(h);
  const hostile = `'><img src=x onerror="alert(1)">`;
  h.run(`state.mcp.setups = [{grant_id:${JSON.stringify(hostile)}, client_name:${JSON.stringify(hostile)}, state:'pending', created_at:${JSON.stringify(hostile)}, expires_at:'bad date', installation_id:${JSON.stringify(hostile)}}]; state.mcp.clients = [{installation_id:${JSON.stringify(hostile)}, client_name:${JSON.stringify(hostile)}, revoked:false}];`);
  const html = h.run('renderMcpView()');
  assert.doesNotMatch(html, /<img/);
  assert.match(html, /&lt;img/);
  assert.match(html, /%27%3E%3Cimg/);
  assert.doesNotMatch(h.run(`eventValue(${JSON.stringify(hostile)})`), /decodeURIComponent\(''/);
});

test('MCP 四种设置状态均有文字，已撤销客户端不可再次撤销', () => {
  const h = harness();
  seedMcp(h);
  h.run(`state.mcp.setups = ['pending','redeemed','expired','revoked'].map(state => ({grant_id:state, state})); state.mcp.clients = [{installation_id:'revoked-client', revoked:true}];`);
  const html = h.run('renderMcpView()');
  for (const label of ['Pending', 'Redeemed', 'Expired', 'Revoked', '尚未连接']) assert.match(html, new RegExp(label));
  h.run("openMcpRevoke('clients', 'revoked-client')");
  assert.equal(h.run('state.activeModal'), null);
  assert.match(html, /只限制首次兑换/);
  assert.match(html, /过期后重试或续接/);
  assert.match(html, /不能注册第二台客户端/);
  assert.match(html, /不会配对 Agent/);
});

test('MCP 生成默认 24 小时，支持 1 小时与 7 天，名称可留空', async () => {
  for (const expiry of [1, 24, 168]) {
    const h = harness();
    seedMcp(h);
    h.run('openMcpSetup()');
    assert.equal(h.run('state.mcp.setupExpiry'), 24);
    h.run(`state.mcp.setupExpiry = ${expiry};`);
    await h.run('createMcpSetup()');
    assert.deepEqual(h.data('JSON.parse(requests.find(r => r.method === "POST").body)'), { client_name: null, expires_in_hours: expiry });
    assert.equal(h.run('state.activeModal'), 'mcpFresh');
    assert.equal(h.run('state.mcp.freshSetup.setup_code'), codeFixture);
  }
});

test('MCP 首次生成重复点击只发一个请求，列表不提供重取代码入口', async () => {
  const h = harness();
  seedMcp(h);
  h.run(`globalThis.regularApi = apiFetch; apiFetch = async (url, options) => {
    if (options?.method === 'POST') { requests.push({url, ...options}); await new Promise(resolve => globalThis.releaseCreate = resolve); return setupResponse; }
    return regularApi(url, options);
  }; openMcpSetup(); state.mcp.setupName = '运维客户端';`);
  const first = h.run('createMcpSetup()');
  await h.run('createMcpSetup()');
  assert.equal(h.run('requests.filter(r => r.method === "POST").length'), 1);
  assert.match(h.run('renderMcpModal()'), /type="submit"[^>]*disabled/);
  h.run('releaseCreate()');
  await first;
  await h.run('createMcpSetup()');
  assert.equal(h.run('requests.filter(r => r.method === "POST").length'), 1);
  assert.doesNotMatch(h.run('renderMcpView()'), /NONFUNCTIONAL-TEST-SECRET|remoteops-setup-v1\.|downloadMcpSetup\(/);
  assert.match(h.run('renderMcpView()'), /不可重新查看或下载历史设置代码/);
});

test('MCP 下载 JSON 与复制代码为同一份新生成设置，不写入本地存储', async () => {
  const h = harness();
  seedFreshMcp(h);
  await h.run('copyMcpSetupCode()');
  h.run('downloadMcpSetup()');
  assert.deepEqual(h.copied, [codeFixture]);
  const downloaded = JSON.parse(await h.blobs[0].text());
  const decoded = JSON.parse(Buffer.from(h.copied[0].split('.')[1], 'base64url').toString());
  assert.deepEqual(downloaded, decoded);
  assert.deepEqual(downloaded, setupFixture);
  assert.equal(h.downloads[0].download, 'remoteops.remoteops-setup');
  assert.deepEqual(h.revokedUrls, ['blob:test-setup']);
  assert.equal(h.stored.size, 0);
});

test('MCP 关闭弹窗同时清除内存与隐藏 DOM 中的设置代码', async () => {
  const h = harness();
  seedFreshMcp(h);
  h.run('renderModal = realRenderModal; renderModal()');
  assert.match(h.nodes.get('modal-backdrop').innerHTML, /remoteops-setup-v1\./);
  h.run('closeModal()');
  assert.equal(h.run('state.mcp.freshSetup'), null);
  assert.equal(h.nodes.has('modal-backdrop'), false);
  await h.run('copyMcpSetupCode()');
  h.run('downloadMcpSetup()');
  assert.equal(h.copied.length, 0);
  assert.equal(h.downloads.length, 0);
});

test('MCP 导航、浏览器返回与离开页面都清除新代码', () => {
  for (const action of ['navigateTo("overview")', 'window.location.hash = "#tab=identity"']) {
    const h = harness();
    seedFreshMcp(h);
    h.run(action);
    if (action.includes('hash')) h.dispatchWindowEvent('hashchange');
    assert.equal(h.run('state.mcp.freshSetup'), null);
    assert.equal(h.run('state.activeModal'), null);
  }
  const h = harness();
  seedFreshMcp(h);
  h.dispatchWindowEvent('pagehide');
  assert.equal(h.run('state.mcp.freshSetup'), null);
});

test('MCP 退出登录立即清除凭据，不等待退出网络响应', async () => {
  const h = harness();
  seedFreshMcp(h);
  h.run('apiFetch = async () => new Promise(resolve => globalThis.releaseLogout = resolve);');
  const pending = h.run('logout()');
  assert.equal(h.run('state.mcp.freshSetup'), null);
  assert.equal(h.run('state.mcp.settings'), null);
  assert.equal(h.run('state.isLoggedIn'), false);
  h.run('releaseLogout({})');
  await pending;
});

test('MCP 关闭、导航或退出后的延迟生成响应不可重新显示代码', async () => {
  for (const action of ['closeModal()', 'navigateTo("overview")', 'logout()']) {
    const h = harness();
    seedMcp(h);
    h.run(`apiFetch = async (url, options) => {
      requests.push({url, ...options});
      if (url === '/api/admin/mcp/setups') return new Promise(resolve => globalThis.releaseCreate = resolve);
      return {};
    }; openMcpSetup();`);
    const pending = h.run('createMcpSetup()');
    await h.run(action);
    h.run('releaseCreate(setupResponse)');
    await pending;
    assert.equal(h.run('state.mcp.freshSetup'), null);
    assert.notEqual(h.run('state.activeModal'), 'mcpFresh');
    assert.equal(h.run('state.mcp.creating'), false);
    assert.equal(h.stored.size, 0);
  }
});

test('MCP 旧列表请求不会覆盖导航返回后的新数据', async () => {
  const h = harness();
  seedMcp(h);
  h.run(`globalThis.regularApi = apiFetch; globalThis.oldResolvers = []; apiFetch = (url) => new Promise(resolve => oldResolvers.push(resolve));`);
  const old = h.run('refreshMcpData()');
  h.run(`navigateTo('overview'); apiFetch = regularApi; listSetups = [{grant_id:'new', state:'redeemed', grant_secret:'MUST-NOT-RETAIN'}]; navigateTo('mcp');`);
  await h.run('Promise.resolve()');
  h.run('oldResolvers[0](null); oldResolvers[1]([{grant_id:"old",state:"pending"}]); oldResolvers[2]([]);');
  await old;
  assert.equal(h.run('state.mcp.setups[0].grant_id'), 'new');
  assert.equal(h.run('state.mcp.setups[0].grant_secret'), undefined);
  assert.equal(h.run('state.mcp.settings.relay'), mcpSettings.relay);
});

test('MCP 离开页面后的设置保存响应不修改当前视图或提示成功', async () => {
  const h = harness();
  seedMcp(h);
  h.run('apiFetch = async () => new Promise(resolve => globalThis.releaseSave = resolve);');
  const pending = h.run('saveMcpSettings()');
  h.run('navigateTo("overview"); state.mcp.settings = null; releaseSave({relay:"old-response"});');
  await pending;
  assert.equal(h.run('state.mcp.settings'), null);
  assert.equal(h.run('toasts.length'), 0);
  assert.equal(h.run('state.mcp.saving'), false);
});

test('MCP 撤销进行中阻止重复提交，成功后不能重放', async () => {
  const h = harness();
  seedMcp(h);
  h.run(`state.mcp.clients = [{installation_id:'fixture-client', revoked:false}];
    globalThis.regularApi = apiFetch;
    apiFetch = async (url, options) => { if (url.endsWith('/revoke')) { requests.push({url, ...options}); await new Promise(resolve => globalThis.releaseRevoke = resolve); return {success:true}; } return regularApi(url, options); };
    openMcpRevoke('clients', 'fixture-client');`);
  const first = h.run('confirmMcpRevoke()');
  await h.run('confirmMcpRevoke()');
  assert.equal(h.run('requests.filter(r => r.url.endsWith("/revoke")).length'), 1);
  assert.match(h.run('renderMcpModal()'), /disabled[^>]*onclick="confirmMcpRevoke/);
  h.run('releaseRevoke()');
  await first;
  await h.run('confirmMcpRevoke()');
  assert.equal(h.run('requests.filter(r => r.url.endsWith("/revoke")).length'), 1);
  assert.equal(h.run('state.mcp.revoking.size'), 0);
});

test('MCP 撤销的旧响应不会干扰导航后的新弹窗', async () => {
  const h = harness();
  seedMcp(h);
  h.run(`state.mcp.setups = [{grant_id:'fixture-grant', state:'pending'}]; apiFetch = async () => new Promise(resolve => globalThis.releaseRevoke = resolve); openMcpRevoke('setups', 'fixture-grant');`);
  const pending = h.run('confirmMcpRevoke()');
  h.run('navigateTo("identity"); openModal("terminateSession", {id:"different"}); releaseRevoke({success:true});');
  await pending;
  assert.equal(h.run('state.activeModal'), 'terminateSession');
  assert.equal(h.run('state.mcp.setups[0].state'), 'pending');
  assert.equal(h.run('toasts.length'), 0);
});

test('MCP 请求错误不把潜在凭据回显到页面或通知', async () => {
  const h = harness();
  seedMcp(h);
  h.run(`apiFetch = async () => { throw new Error('NONFUNCTIONAL-TEST-SECRET'); }; openMcpSetup();`);
  await h.run('createMcpSetup()');
  assert.doesNotMatch(h.run('renderMcpModal()'), /NONFUNCTIONAL-TEST-SECRET/);
  assert.match(h.run('renderMcpModal()'), /先刷新设置记录/);
  assert.equal(h.run('state.mcp.freshSetup'), null);
  assert.equal(h.run('state.mcp.creating'), false);
});

test('MCP 演示模式不调用接入 API，不签发代码或下载模拟凭据', async () => {
  const h = harness();
  seedMcp(h);
  h.run('state.demoMode = true;');
  await h.run('refreshMcpData()');
  await h.run('saveMcpSettings()');
  h.run('openMcpSetup(); downloadMcpSetup();');
  await h.run('createMcpSetup()');
  assert.equal(h.run('requests.length'), 0);
  assert.equal(h.run('state.mcp.freshSetup'), null);
  assert.equal(h.downloads.length, 0);
  assert.match(h.run('renderMcpView()'), /不生成代码或凭据/);
});

test('MCP 生成响应不确定时不允许在同一弹窗重复签发', async () => {
  const h = harness();
  seedMcp(h);
  h.run(`apiFetch = async (url, options) => {requests.push({url, ...options}); throw new Error('network');}; openMcpSetup();`);
  await h.run('createMcpSetup()');
  await h.run('createMcpSetup()');
  assert.equal(h.run('requests.length'),1);
  assert.match(h.run('renderMcpModal()'), /请关闭后核对记录/);
});

test('MCP 设置保存过程中不发出可覆盖新配置的旧读取', async () => {
  const h = harness();
  seedMcp(h);
  h.run('state.mcp.saving = true;');
  assert.equal(await h.run('refreshMcpData()'),false);
  assert.equal(h.run('requests.length'),0);
});

test('MCP 非法状态和弹窗设置代码中的 HTML 都作为纯文本处理', () => {
  const h = harness();
  seedFreshMcp(h);
  h.run(`state.mcp.freshSetup = {setup:setupResponse.setup, setup_code:'</textarea><img src=x onerror=alert(1)>'};`);
  assert.match(h.run('mcpStateBadge("constructor")'), /未知状态/);
  const html = h.run('renderMcpModal()');
  assert.doesNotMatch(html, /<img/);
  assert.match(html, /&lt;\/textarea&gt;&lt;img/);
});

test('MCP 保存未完成时离开再返回，完成后解除新页面的锁并重新读取', async () => {
  const h = harness();
  seedMcp(h);
  h.run(`globalThis.regularApi = apiFetch; apiFetch = async (url, options) => {
    if (options?.method === 'PUT') return new Promise(resolve => globalThis.releaseSave = resolve);
    return regularApi(url, options);
  };`);
  const pending = h.run('saveMcpSettings()');
  h.run('navigateTo("identity"); navigateTo("mcp");');
  assert.equal(h.run('state.mcp.saving'),true);
  h.run('releaseSave({relay:"discarded-old-response"})');
  await pending;
  await h.run('Promise.resolve()');
  assert.equal(h.run('state.mcp.saving'),false);
  assert.equal(h.run('state.mcp.loading'),false);
  assert.equal(h.run('state.mcp.settings.relay'),mcpSettings.relay);
  assert.equal(h.run('toasts.length'),0);
});

test('MCP 生成未完成时离开再返回，迟到结果只刷新元数据且解除按钮锁', async () => {
  const h = harness();
  seedMcp(h);
  h.run(`globalThis.regularApi = apiFetch; apiFetch = async (url, options) => {
    if (options?.method === 'POST') return new Promise(resolve => globalThis.releaseCreate = resolve);
    return regularApi(url, options);
  }; openMcpSetup();`);
  const pending = h.run('createMcpSetup()');
  h.run('navigateTo("identity"); navigateTo("mcp");');
  await h.run('Promise.resolve()');
  h.run('releaseCreate(setupResponse)');
  await pending;
  await h.run('Promise.resolve()');
  assert.equal(h.run('state.mcp.creating'),false);
  assert.equal(h.run('state.mcp.freshSetup'),null);
  assert.equal(h.run('state.activeModal'),null);
  assert.equal(h.run('state.mcp.loading'),false);
  assert.match(h.run('renderMcpView()'), /id="mcp-new-setup"/);
});
