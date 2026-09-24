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
  const mediaListeners = [];
  const media = {
    matches: dark,
    addEventListener: (_event, callback) => mediaListeners.push(callback),
    addListener: callback => mediaListeners.push(callback),
  };
  const context = vm.createContext({
    console,
    URL,
    URLSearchParams,
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
    },
    document: {
      documentElement: { setAttribute: (key, value) => attributes.set(key, value), style: {} },
      addEventListener: (event, callback) => events.set(event, callback),
      getElementById: () => null,
      querySelector: () => null,
      querySelectorAll: () => [],
      createElement: () => ({ classList: { add() {}, remove() {} }, remove() {} }),
      body: { appendChild() {} },
    },
  });
  vm.runInContext(source, context, { filename: 'relay-admin-prototype/app.js' });
  vm.runInContext(`
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
