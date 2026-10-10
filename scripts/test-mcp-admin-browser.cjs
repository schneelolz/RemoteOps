const fs = require('node:fs');
const assert = require('node:assert/strict');
const path = require('node:path');
const os = require('node:os');
const root = path.resolve(__dirname, '..', 'relay-admin-prototype');
const origin = 'http://127.0.0.1:38181';

function deferredResponse() {
  let started;
  let release;
  const whenStarted = new Promise(resolve => { started = resolve; });
  const ready = new Promise(resolve => { release = resolve; });
  return { started, release, whenStarted, ready };
}

async function waitForFixture(promise, label) {
  let timer;
  try {
    await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`Timed out waiting for mocked ${label}`)), 15000);
    })]);
  } finally { clearTimeout(timer); }
}

// Opt-in only. Every app request is fulfilled from local files or synthetic fixtures;
// no Relay process, listener, real login, enrollment service, or credential is used.
// Clipboard calls are captured in the isolated page, leaving the OS clipboard alone.
async function main() {
  const moduleName = process.env.REMOTEOPS_PLAYWRIGHT_MODULE || 'playwright';
  let chromium;
  try { ({ chromium } = require(moduleName)); }
  catch (_) {
    throw new Error('Playwright is required for this opt-in test. Make playwright available through normal Node resolution, or set REMOTEOPS_PLAYWRIGHT_MODULE to its installed package path.');
  }
  if (!chromium || typeof chromium.launch !== 'function') throw new Error('The configured Playwright module must export chromium.launch.');
  const artifacts = process.env.REMOTEOPS_BROWSER_ARTIFACTS
    ? path.resolve(process.env.REMOTEOPS_BROWSER_ARTIFACTS)
    : fs.mkdtempSync(path.join(os.tmpdir(), 'remoteops-mcp-admin-browser-'));
  fs.mkdirSync(artifacts, { recursive: true });
  const artifact = name => path.join(artifacts, name);
  const launchOptions = { headless: true };
  if (process.env.REMOTEOPS_BROWSER_EXECUTABLE) launchOptions.executablePath = process.env.REMOTEOPS_BROWSER_EXECUTABLE;
  let browser;
  try {
    browser = await chromium.launch(launchOptions);
    const context = await browser.newContext({ viewport: {width:1440,height:1150}, acceptDownloads:true, colorScheme:'light', serviceWorkers:'block' });
    await context.addInitScript(() => {
      Object.defineProperty(navigator, 'clipboard', { configurable: true, value: {
        writeText: async text => { window.__remoteopsTestClipboard = String(text); },
        readText: async () => window.__remoteopsTestClipboard || '',
      } });
    });
    const page = await context.newPage();
    let settings = null;
    let setups = [];
    let clients = [];
    let createCount = 0;
    let revokeCount = 0;
    const issuedExpiryHours = [];
    let holdNextCreate = null;
    let delayNextSetupList = null;
    const unexpectedRequests = [];
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    const json = (route, value, status = 200) => route.fulfill({status, contentType:'application/json', body:JSON.stringify(value)});
    await context.route('**/*', async route => {
      const request = route.request(); const url = new URL(request.url()); const pathname = url.pathname;
      if (url.origin !== origin) { unexpectedRequests.push(`${request.method()} ${url.origin}${pathname}`); return route.abort(); }
      if (!pathname.startsWith('/api/')) {
        const name = pathname === '/' ? 'index.html' : pathname.slice(1);
        if (!['index.html','style.css','app.js','theme.js'].includes(name)) return route.fulfill({status:404});
        return route.fulfill({contentType: name.endsWith('.css') ? 'text/css' : name.endsWith('.js') ? 'application/javascript' : 'text/html', body:fs.readFileSync(path.join(root, name))});
      }
      if (pathname === '/api/admin/session') return json(route,{authenticated:true,username:'local-test-admin'});
      if (pathname === '/api/admin/overview') return json(route,{version:'local-fixture',owner_id:'fixture-owner',uptime_seconds:100});
      if (pathname === '/api/admin/identity') return json(route,{owner_id:'fixture-owner'});
      if (['/api/admin/agents','/api/admin/sessions','/api/admin/audit'].includes(pathname)) return json(route,[]);
      if (pathname === '/api/admin/mcp/settings') { if (request.method() === 'PUT') settings = request.postDataJSON(); return json(route,settings); }
      if (pathname === '/api/admin/mcp/setups') {
        if (request.method() === 'POST') {
          createCount++;
          const input = request.postDataJSON();
          issuedExpiryHours.push(input.expires_in_hours);
          const created = new Date().toISOString();
          const expires = new Date(Date.now() + input.expires_in_hours*3600000).toISOString();
          const setup = {version:1,grant_id:`fixture-${createCount}`,grant_secret:'NONFUNCTIONAL-LOCAL-BROWSER-TEST',...settings,expires_at:expires};
          setups.unshift({grant_id:setup.grant_id,client_name:input.client_name,created_at:created,expires_at:expires,state:'pending',installation_id:null});
          const setupResponse = {setup,setup_code:'remoteops-setup-v1.'+Buffer.from(JSON.stringify(setup)).toString('base64url')};
          if (holdNextCreate) {
            const gate = holdNextCreate;
            holdNextCreate = null;
            gate.started();
            await gate.ready;
          } else {
            await new Promise(resolve => setTimeout(resolve, 120));
          }
          return json(route,setupResponse);
        }
        const snapshot = JSON.parse(JSON.stringify(setups));
        if (delayNextSetupList) {
          const gate = delayNextSetupList;
          delayNextSetupList = null;
          gate.started();
          await gate.ready;
        }
        return json(route,snapshot);
      }
      if (pathname === '/api/admin/mcp/clients') return json(route,clients);
      if (pathname.endsWith('/revoke')) {
        revokeCount++;
        const id = decodeURIComponent(pathname.split('/').at(-2));
        if (pathname.includes('/clients/')) clients.find(x=>x.installation_id===id).revoked = true;
        else setups.find(x=>x.grant_id===id).state = 'revoked';
        await new Promise(resolve=>setTimeout(resolve,120));
        return json(route,{success:true,changed:true});
      }
      if (pathname === '/api/admin/logout') return json(route,{success:true});
      unexpectedRequests.push(`${request.method()} ${pathname}`);
      return json(route,{error:'Unexpected fixture API'},404);
    });
    await page.goto(`${origin}/#tab=mcp`);
    await page.getByRole('heading',{name:/MCP 客户端接入.*One-time Setup/}).waitFor();
    await page.locator('#mcp-relay:not(:disabled)').waitFor();
    assert.equal(await page.locator('#mcp-relay').inputValue(),'');
    assert.equal(await page.locator('#mcp-new-setup').isDisabled(),true);
    await page.locator('#mcp-relay').fill('relay.example.test:7443');
    await page.locator('#mcp-server-name').fill('relay.example.test');
    await page.locator('#mcp-enrollment-url').fill('https://enroll.example.test:18443/api/mcp/enroll');
    await page.getByRole('button',{name:'保存接入配置',exact:true}).click();
    await page.locator('#mcp-new-setup:not(:disabled)').waitFor();
    await page.locator('#mcp-new-setup').click();
    assert.equal(await page.locator('#mcp-setup-expiry').inputValue(),'24');
    assert.equal(await page.locator('#mcp-setup-expiry option').count(),3);
    await page.locator('#mcp-setup-expiry').selectOption('1');
    await page.locator('#mcp-setup-name').fill('运维 MacBook');
    await page.getByRole('button',{name:'生成设置',exact:true}).evaluate(button=>{button.click();button.click();});
    await page.waitForFunction(() => state.activeModal === 'mcpFresh' && !state.mcp.creating && !state.mcp.loading);
    assert.equal(createCount,1);
    await page.screenshot({path:artifact('fresh-light.png')});
    await page.getByRole('button',{name:'复制设置代码',exact:true}).click();
    const copied = await page.evaluate(()=>navigator.clipboard.readText());
    const downloadPromise = page.waitForEvent('download');
    await page.getByRole('button',{name:'下载 .remoteops-setup',exact:true}).click();
    const download = await downloadPromise;
    await download.saveAs(artifact('synthetic.remoteops-setup'));
    const saved = JSON.parse(fs.readFileSync(artifact('synthetic.remoteops-setup'),'utf8'));
    assert.deepEqual(saved,JSON.parse(Buffer.from(copied.split('.')[1],'base64url').toString()));
    await page.getByRole('button',{name:'完成并清除',exact:true}).click();
    assert.equal(await page.locator('#mcp-setup-code').count(),0);
    assert.equal(await page.evaluate(()=>state.mcp.freshSetup),null);
    assert.doesNotMatch(await page.content(),/NONFUNCTIONAL-LOCAL-BROWSER-TEST|remoteops-setup-v1\./);
    assert.equal(await page.evaluate(()=>Object.keys(localStorage).some(k=>/setup|grant|secret/.test(k))),false);
    const now = new Date().toISOString(); const later = new Date(Date.now()+86400000).toISOString();
    setups = ['pending','redeemed','expired','revoked'].map((state,i)=>({grant_id:`metadata-${i}`,client_name:`状态示例 · ${state}`,created_at:now,expires_at:later,state,installation_id:state==='redeemed'?'client-used':null}));
    clients = [{installation_id:'client-used',client_name:'已注册工作站',created_at:now,last_seen:now,revoked:false},{installation_id:'client-revoked',client_name:'已撤销工作站',created_at:now,last_seen:null,revoked:true}];
    await page.evaluate(()=>refreshMcpData());
    for (const state of ['Pending','Redeemed','Expired','Revoked']) assert.ok((await page.locator('main').innerText()).includes(state));
    assert.equal(await page.locator('main img').count(),0);
    await page.screenshot({path:artifact('page-light.png'),fullPage:true});
    await page.locator('.theme-select').selectOption('dark');
    await page.screenshot({path:artifact('page-dark.png'),fullPage:true});
    await page.getByRole('row').filter({hasText:'已注册工作站'}).getByRole('button',{name:'撤销客户端',exact:true}).click();
    await page.getByRole('button',{name:'确认撤销',exact:true}).evaluate(button=>{button.click();button.click();});
    await page.locator('#modal-backdrop').waitFor({state:'detached'});
    assert.equal(revokeCount,1);
    assert.equal(await page.getByRole('row').filter({hasText:'已注册工作站'}).getByRole('button',{name:'撤销客户端',exact:true}).isDisabled(),true);
    await page.waitForFunction(() => !state.mcp.loading && state.mcp.revoking.size === 0);
    await page.getByRole('row').filter({hasText:'metadata-0'}).getByRole('button',{name:'撤销设置',exact:true}).click();
    await page.getByRole('button',{name:'确认撤销',exact:true}).evaluate(button=>{button.click();button.click();});
    await page.locator('#modal-backdrop').waitFor({state:'detached'});
    await page.waitForFunction(() => !state.mcp.loading && state.mcp.revoking.size === 0);
    assert.equal(revokeCount,2);
    assert.equal(await page.getByRole('row').filter({hasText:'metadata-0'}).getByRole('button',{name:'撤销设置',exact:true}).isDisabled(),true);
    // Closing an in-flight creation and leaving the view must never reveal its response later.
    const createGate = deferredResponse();
    holdNextCreate = createGate;
    await page.locator('#mcp-new-setup').click();
    await page.locator('#mcp-setup-expiry').selectOption('168');
    await page.getByRole('button',{name:'生成设置',exact:true}).click();
    await waitForFixture(createGate.whenStarted, 'setup generation');
    await page.getByRole('button',{name:'取消',exact:true}).click();
    await page.getByRole('button',{name:'Relay 身份凭据',exact:true}).click();
    createGate.release();
    await page.waitForFunction(() => !state.mcp.creating);
    assert.equal(await page.locator('#mcp-setup-code').count(),0);
    assert.equal(await page.evaluate(()=>state.mcp.freshSetup),null);
    await page.getByRole('button',{name:'MCP 客户端接入',exact:true}).click();
    await page.locator('#mcp-new-setup:not(:disabled)').waitFor();
    // An earlier list read must not replace fresh data after leaving and returning.
    const listGate = deferredResponse();
    delayNextSetupList = listGate;
    await page.evaluate(() => { window.__remoteopsSlowRefresh = refreshMcpData(); });
    await waitForFixture(listGate.whenStarted, 'setup list refresh');
    setups.unshift({grant_id:'latest-metadata',client_name:'新页面记录',created_at:now,expires_at:later,state:'pending',installation_id:null});
    await page.getByRole('button',{name:'Relay 身份凭据',exact:true}).click();
    await page.getByRole('button',{name:'MCP 客户端接入',exact:true}).click();
    await page.getByRole('cell',{name:/新页面记录.*latest-metadata/}).waitFor();
    listGate.release();
    await page.evaluate(() => window.__remoteopsSlowRefresh);
    await page.waitForFunction(() => !state.mcp.loading);
    assert.ok(await page.getByRole('cell',{name:/新页面记录.*latest-metadata/}).count());
    assert.equal(await page.evaluate(() => state.mcp.freshSetup),null);
    await page.setViewportSize({width:390,height:844});
    await page.screenshot({path:artifact('mobile-dark.png'),fullPage:true});
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth > window.innerWidth),false);
    // A new fresh response clears immediately on logout even if its modal is open.
    await page.evaluate(()=>{openMcpSetup();return createMcpSetup();});
    assert.ok(await page.locator('#mcp-setup-code').count());
    await page.evaluate(()=>logout());
    assert.equal(await page.locator('#mcp-setup-code').count(),0);
    assert.equal(await page.evaluate(()=>state.mcp.freshSetup),null);
    assert.deepEqual(errors,[]);
    assert.deepEqual(unexpectedRequests,[]);
    assert.deepEqual(issuedExpiryHours,[1,168,24]);
    console.log('PASS: isolated Chromium onboarding smoke; endpoint save, 1/24/168-hour controls, copy/download equality, four states, setup/client revoke, duplicate submit/revoke, delayed navigation/list refresh, logout clearing, desktop light/dark and 390px layout. No page errors.');
    console.log(`Synthetic screenshots and download: ${artifacts}`);
  } finally {
    if (browser) await browser.close();
  }
}

if (require.main === module) main().catch(error => { console.error(error.message); process.exitCode = 1; });

