const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const net = require('node:net');
const { spawn } = require('node:child_process');

const repository = path.resolve(__dirname, '..');
const { chromium } = require(process.env.REMOTEOPS_PLAYWRIGHT_MODULE || 'playwright');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

async function freePort() {
  const server = net.createServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
}

async function main() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'remoteops-guide-review-'));
  const artifacts = process.env.REMOTEOPS_UI_ARTIFACTS || path.join(repository, 'output/playwright/tool-center-guide');
  fs.mkdirSync(artifacts, { recursive: true });
  const ports = new Set();
  while (ports.size < 3) ports.add(await freePort());
  const [relayPort, healthPort, adminPort] = [...ports];
  const origin = `http://127.0.0.1:${adminPort}`;
  const username = 'local-review';
  const password = crypto.randomBytes(32).toString('base64url');
  const environment = Object.fromEntries(['SystemRoot', 'WINDIR', 'TEMP', 'TMP', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'PATH'].filter(name => process.env[name]).map(name => [name, process.env[name]]));
  Object.assign(environment, {
    REMOTEOPS_RELAY_BIND: `127.0.0.1:${relayPort}`,
    REMOTEOPS_HEALTH_BIND: `127.0.0.1:${healthPort}`,
    REMOTEOPS_ADMIN_ADDR: `127.0.0.1:${adminPort}`,
    REMOTEOPS_TLS_CERT: path.join(root, 'tls/cert.pem'),
    REMOTEOPS_TLS_KEY: path.join(root, 'tls/key.pem'),
    REMOTEOPS_TLS_SANS: 'localhost,127.0.0.1',
    REMOTEOPS_STATE_FILE: path.join(root, 'relay-state.json'),
    REMOTEOPS_HUMAN_CONTROLLER_TOKEN: crypto.randomBytes(32).toString('base64url'),
    REMOTEOPS_CONTROLLER_OWNER_ID: crypto.randomUUID(),
    REMOTEOPS_ADMIN_USERNAME: username,
    REMOTEOPS_ADMIN_PASSWORD: password,
    REMOTEOPS_ADMIN_COOKIE_SECURE: 'false',
  });
  const executable = process.env.REMOTEOPS_RELAY_EXECUTABLE || path.join(repository, 'target/debug/remoteops-relay.exe');
  const relay = spawn(executable, [], { env: environment, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  relay.stdout.resume();
  relay.stderr.resume();
  let browser;
  const errors = [];
  try {
    let ready = false;
    for (let attempt = 0; attempt < 100; attempt++) {
      if (relay.exitCode !== null) throw new Error('Isolated Relay exited before readiness');
      try { ready = (await fetch(origin)).ok; } catch {}
      if (ready) break;
      await delay(100);
    }
    assert(ready, 'isolated Relay readiness');
    browser = await chromium.launch({ channel: 'chrome', headless: true });
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    page.setDefaultTimeout(15000);
    page.on('pageerror', error => errors.push(error.message));
    page.on('request', request => assert.equal(new URL(request.url()).origin, origin, 'all browser traffic stays local'));
    await page.goto(origin);
    await page.getByRole('textbox', { name: '管理员账号 (Username)' }).fill(username);
    await page.getByRole('textbox', { name: '管理密码 (Password)' }).fill(password);
    await page.getByRole('button', { name: '登录管理控制台', exact: true }).click();
    await page.getByRole('button', { name: '工具中心', exact: true }).click();
    await page.getByRole('button', { name: '编辑说明', exact: true }).click();
    await page.getByRole('button', { name: '填入模板', exact: true }).click();
    assert((await page.getByLabel('全局说明', { exact: true }).inputValue()).includes('结果未知'));
    const guide = '# 工具中心\n\n浏览器任务按实际平台测试选择 A、B、C。\n结果未知时先核实，禁止自动换工具重放。\n<img src="https://invalid.example/leak" onerror="alert(1)">';
    await page.getByLabel('全局说明', { exact: true }).fill(guide);
    await page.getByRole('button', { name: '保存说明', exact: true }).click();
    await page.getByText('全局说明已保存', { exact: true }).waitFor();
    assert.equal(await page.locator('.tools-guide-text').textContent(), guide);
    assert.equal(await page.locator('.tools-guide img').count(), 0);
    await page.reload();
    await page.locator('.tools-guide-text').waitFor();
    assert.equal(await page.locator('.tools-guide-text').textContent(), guide);
    await page.getByRole('button', { name: '编辑说明', exact: true }).click();
    await page.getByLabel('全局说明', { exact: true }).fill('中'.repeat(10923));
    await page.getByRole('button', { name: '保存说明', exact: true }).click();
    await page.locator('#tool-guide-error').waitFor();
    assert.equal((await page.getByLabel('全局说明', { exact: true }).inputValue()).length, 10923);
    await page.getByRole('button', { name: '取消', exact: true }).click();
    await page.getByRole('button', { name: '编辑说明', exact: true }).click();
    await page.getByLabel('全局说明', { exact: true }).fill('# 工具中心\n\n按任务与实际平台测试选择浏览器工具。\n\n## 工具选择\nA：填写常规任务的适用条件。\nB：填写特殊能力与限制。\nC：填写备用场景与前置条件。\n\n## 失败处理\n结果未知时先核实，禁止自动换工具重放。');
    await page.getByRole('button', { name: '保存说明', exact: true }).click();
    await page.getByText('全局说明已保存', { exact: true }).waitFor();
    const cleanToasts = () => page.waitForFunction(() => !document.querySelector('.toast'));
    await cleanToasts();
    for (const [name, viewport] of [['desktop', { width: 1440, height: 900 }], ['mobile', { width: 390, height: 844 }]]) {
      await page.setViewportSize(viewport);
      await page.screenshot({ path: path.join(artifacts, `${name}-guide.png`), fullPage: true });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth), false);
      await page.getByRole('button', { name: '编辑说明', exact: true }).click();
      await page.screenshot({ path: path.join(artifacts, `${name}-guide-editor.png`), fullPage: true });
      if (name === 'mobile') {
        await page.getByRole('button', { name: '保存说明', exact: true }).scrollIntoViewIfNeeded();
        await page.screenshot({ path: path.join(artifacts, 'mobile-guide-editor-actions.png'), fullPage: true });
      }
      await page.getByRole('button', { name: '取消', exact: true }).click();
      if (name === 'mobile') {
        await page.locator('.tools-guide-text').scrollIntoViewIfNeeded();
        await page.screenshot({ path: path.join(artifacts, 'mobile-guide-content.png'), fullPage: true });
      }
    }
    // 旧目录仅注入隔离页面展示；登记约束由真实工具中心测试验证。
    await page.evaluate(() => {
      const versions = packageDefinitionTemplate();
      const pkg = versions[0].packages[0];
      Object.assign(pkg.tool.startup, { interface: 'stdio_mcp', args: ['mcp', '--tools', 'core'], browser_executable: 'browser/chrome-win64/chrome.exe', operations: [] });
      delete pkg.tool.startup.browser_adapter;
      state.toolCenter.snapshot.tools = [{ id: 'my-tool', name: '浏览器适配验收', purpose: '检查实际适配策略', instructions: '明确选择 agent-browser MCP 适配模式', examples: [], dependencies: [], enabled: true, versions }];
      state.toolCenter.selected = 'my-tool';
      renderApp();
    });
    await page.getByText(/旧登记未指定适配模式/).waitFor();
    for (const [name, viewport] of [['desktop', { width: 1440, height: 900 }], ['mobile', { width: 390, height: 844 }]]) {
      await page.setViewportSize(viewport);
      await page.getByText(/旧登记未指定适配模式/).scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(artifacts, `${name}-adapter-warning.png`), fullPage: true });
      await page.evaluate(() => openToolCenterEditor('my-tool'));
      await page.getByText('浏览器启动配置示例', { exact: true }).click();
      for (const kind of ['none', 'playwright', 'agent_browser', 'agent_browser_cli']) {
        await page.getByLabel('接口与适配模式').selectOption(kind);
        assert.equal(JSON.parse(await page.locator('#tool-startup-example').textContent()).startup.interface, kind === 'agent_browser_cli' ? kind : 'stdio_mcp');
      }
      await page.getByLabel('接口与适配模式').selectOption('agent_browser');
      await page.locator('#tool-startup-example').scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(artifacts, `${name}-adapter-editor.png`), fullPage: true });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth), false);
      await page.getByRole('button', { name: '取消', exact: true }).click();
    }
    const put = async (url, data) => {
      const response = await page.request.put(origin + url, { data });
      assert.equal(response.status(), 200);
      return response.json();
    };
    const post = async (url, data) => {
      const response = await page.request.post(origin + url, { data });
      assert.equal(response.status(), 200);
      return response.json();
    };
    await put('/api/admin/mcp/settings', { enrollment_url: 'https://localhost/api/mcp/enroll', relay: `localhost:${relayPort}`, server_name: 'localhost', relay_ca_pem: null, enrollment_ca_pem: null });
    const grant = await post('/api/admin/mcp/setups', { client_name: '本机权限验收 MCP', expires_in_hours: 1 });
    const installation = crypto.randomUUID();
    await post('/api/mcp/enroll', { grant_id: grant.setup.grant_id, grant_secret: grant.setup.grant_secret, installation_id: installation, installation_secret: crypto.randomBytes(32).toString('base64url') });
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.getByRole('button', { name: 'MCP 客户端接入', exact: true }).click();
    const permission = () => page.getByRole('checkbox', { name: '本机权限验收 MCP的工具管理权限', exact: true });
    await permission().waitFor();
    assert.equal(await permission().isChecked(), false);
    await permission().check();
    await page.getByText('已授予工具管理权限', { exact: true }).waitFor();
    await page.getByText('普通使用＋工具管理', { exact: true }).waitFor();
    await cleanToasts();
    await page.locator('[aria-labelledby="mcp-clients-title"]').screenshot({ path: path.join(artifacts, 'desktop-permission.png') });
    await permission().uncheck();
    await page.getByText('已撤销工具管理权限', { exact: true }).waitFor();
    await page.getByText('普通使用', { exact: true }).waitFor();
    await cleanToasts();
    await page.setViewportSize({ width: 390, height: 844 });
    await permission().scrollIntoViewIfNeeded();
    await page.screenshot({ path: path.join(artifacts, 'mobile-permission.png'), fullPage: true });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth), false);
    assert.deepEqual(errors, []);
    fs.writeFileSync(path.join(artifacts, 'verification.json'), JSON.stringify({ passed: true, viewport: ['1440x900', '390x844'], tests: ['guide template/save/reload', 'UTF8 bound and draft retention', 'escaped HTML with no external loads', 'permission default/grant/revoke', 'no horizontal document overflow'], page_errors: errors }, null, 2));
    console.log(`PASS isolated real Relay guide and permission UI; screenshots: ${artifacts}`);
  } finally {
    if (browser) await browser.close();
    if (relay.exitCode === null) {
      relay.kill();
      await new Promise(resolve => relay.once('exit', resolve));
    }
  }
}

main().catch(error => { console.error(error.message); process.exitCode = 1; });
