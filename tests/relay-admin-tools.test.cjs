const test = require('node:test');
const assert = require('node:assert/strict');
const { createHash, webcrypto } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

test('浏览器适配提示与四种示例明确区分旧默认和显式模式', () => {
  const context = fixture();
  context.escapeHtml = value => String(value).replaceAll('<', '&lt;');
  const legacy = context.browserAdapterFacts({ interface: 'stdio_mcp', browser_executable: 'browser.exe' });
  assert.equal(legacy.effective, 'playwright');
  assert.equal(legacy.legacy, true);
  assert.match(context.renderBrowserAdapter({ interface: 'stdio_mcp', browser_executable: 'browser.exe' }), /--browser/);
  for (const kind of ['none', 'playwright', 'agent_browser', 'agent_browser_cli']) {
    const example = context.browserStartupExample(kind);
    assert.equal(example.startup.browser_adapter, kind === 'agent_browser_cli' ? undefined : kind);
    assert.equal(example.startup.interface, kind === 'agent_browser_cli' ? kind : 'stdio_mcp');
    assert.equal(context.browserAdapterFacts(example.startup).legacy, false);
  }
  assert.equal(context.sameStartup({ interface: 'stdio_mcp', executable: 'x', args: [] }, { args: [], executable: 'x', interface: 'stdio_mcp', browser_adapter: null, operations: [], browser_executable: '' }), true);
});

test('遗漏浏览器适配字段时阻止新配置提交并保留草稿', async () => {
  const context = fixture();
  const pkg = { tool: { startup: { interface: 'stdio_mcp', executable: 'tool.exe', args: [], browser_executable: 'browser.exe' }, package: { sha256: 'a'.repeat(64) }, platforms: [{ os: 'windows', arch: 'x86_64' }] } };
  context.renderModal = () => {};
  let calls = 0;
  context.toolCenterCommand = async () => { calls++; };
  const draft = { id: 'agent', name: 'agent', purpose: 'browser', instructions: 'use', enabled: true, examplesText: '', dependenciesText: '', versionsText: JSON.stringify([{ version: '1', packages: [pkg] }]) };
  context.state.toolCenter.draft = draft;
  await context.saveManagedTool({ preventDefault() {} });
  assert.match(context.state.toolCenter.error, /browser_adapter/);
  assert.equal(context.state.toolCenter.draft, draft);
  assert.equal(calls, 0);
});

function fixture() {
  const context = vm.createContext({
    state: { isLoggedIn: true, currentTab: 'tools', demoMode: false },
    crypto: webcrypto,
    renderApp() {}, renderModal() {}, showToast() {}, TextEncoder,
    document: { addEventListener() {}, getElementById() { return null; } }, setTimeout
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, '../relay-admin-prototype/tools.js'), 'utf8'), context);
  context.state.toolCenter.snapshot = { revision: 7, tools: [] };
  return context;
}

test('增量摘要匹配标准 SHA-256，覆盖填充边界及不整齐分块', () => {
  const context = fixture();
  for (const size of [0, 1, 55, 56, 63, 64, 65, 257, 1024 * 1024 + 17]) {
    const bytes = Buffer.alloc(size);
    for (let i = 0; i < size; i++) bytes[i] = (i * 37) % 256;
    const digest = vm.runInContext('new ToolPackageSha256()', context);
    for (let offset = 0; offset < size; offset += 997) digest.update(bytes.subarray(offset, offset + 997));
    assert.equal(digest.digest(), createHash('sha256').update(bytes).digest('hex'), `大小 ${size}`);
    assert.equal(digest.buffer.length, 64);
  }
});

test('并发冲突保留旧修订号，要求重新读取后合并资料', async () => {
  const context = fixture();
  context.apiFetch = async (_, options) => {
    const request = JSON.parse(options.body);
    assert.equal(request.expected_revision, 7);
    return { request_id: request.request_id, revision: 8, error: { code: 'revision_conflict', message: '目录已变化' } };
  };
  await assert.rejects(context.toolCenterCommand({ action: 'set_policy', enabled: true }), /刷新后合并/);
  assert.equal(context.state.toolCenter.snapshot.revision, 7);
});

test('成功结果确认请求标识及修订号后才更新本地状态', async () => {
  const context = fixture();
  context.apiFetch = async (_, options) => ({ request_id: JSON.parse(options.body).request_id, revision: 8, error: null, data: { enabled: true } });
  assert.equal((await context.toolCenterCommand({ action: 'set_policy', enabled: true })).enabled, true);
  assert.equal(context.state.toolCenter.snapshot.revision, 8);
  context.apiFetch = async () => ({ request_id: 'other-request', revision: 99, error: null, data: {} });
  await assert.rejects(context.toolCenterCommand({ action: 'set_policy', enabled: false }), /结果无法确认/);
  assert.equal(context.state.toolCenter.snapshot.revision, 8);
});

test('响应丢失返回结果未确认且不自动重放', async () => {
  const context = fixture();
  let calls = 0;
  context.apiFetch = async () => { calls++; throw new Error('connection closed'); };
  await assert.rejects(context.toolCenterCommand({ action: 'put_tool', tool: {} }), /提交结果尚未确认/);
  assert.equal(calls, 1);
  assert.equal(context.state.toolCenter.snapshot.revision, 7);
});

test('导航失效后的旧目录响应不能覆盖新页面状态', async () => {
  const context = fixture();
  let finish;
  context.apiFetch = () => new Promise(resolve => { finish = resolve; });
  const pending = context.refreshToolsData();
  context.invalidateToolsView();
  finish({ data: { revision: 99, enabled: true, tools: [] } });
  assert.equal(await pending, false);
  assert.equal(context.state.toolCenter.snapshot.revision, 7);
});

test('全局说明按 UTF-8 限额校验，失败保留草稿，成功才更新正文', async () => {
  const context = fixture();
  context.state.toolCenter.guideDraft = '中'.repeat(10923);
  context.apiFetch = async () => { throw new Error('不应提交超限内容'); };
  await context.saveToolGuide({ preventDefault() {} });
  assert.match(context.state.toolCenter.guideError, /32 KiB/);
  const draft = '# guide\n<script>not executed</script>';
  context.state.toolCenter.guideDraft = draft;
  await context.saveToolGuide({ preventDefault() {} });
  assert.equal(context.state.toolCenter.guideDraft, draft);
  assert.match(context.state.toolCenter.guideError, /提交结果尚未确认/);
  context.apiFetch = async (_, options) => {
    const request = JSON.parse(options.body);
    assert.deepEqual(request.command, { action: 'set_guide', guide_markdown: draft });
    return { request_id: request.request_id, revision: 8, error: null, data: { guide_markdown: draft } };
  };
  await context.saveToolGuide({ preventDefault() {} });
  assert.equal(context.state.toolCenter.guideDraft, null);
  assert.equal(context.state.toolCenter.snapshot.guide_markdown, draft);
});

test('全局说明展示转义文本，模板不覆盖非空草稿', () => {
  const context = fixture();
  context.escapeHtml = value => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
  context.state.toolCenter.snapshot.guide_markdown = '<img src=x onerror=alert(1)>';
  const html = context.renderToolGuide(false);
  assert(!html.includes('<img'));
  assert(html.includes('&lt;img'));
  context.state.toolCenter.guideDraft = 'keep';
  context.useToolGuideTemplate();
  assert.equal(context.state.toolCenter.guideDraft, 'keep');
});
