#!/usr/bin/env node
// 真实 MCP STDIO → 本轮独立回环 Relay；身份和认证数据仅留内存。
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import fs from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import Ajv2020 from 'ajv/dist/2020.js';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { AjvJsonSchemaValidator } from '@modelcontextprotocol/sdk/validation/ajv';
import { ListToolsResultSchema } from '@modelcontextprotocol/sdk/types.js';
import { sdkVersion, validateToolPages, validateToolResult } from './contract.mjs';

import { makeBrowserPromptFixture, runBrowserChecks } from './browser-live-support.mjs';

const prefix = 'remoteops-mcp-schema-live-';
const sha256 = data => createHash('sha256').update(data).digest('hex');

function options() {
  const values = {};
  for (let index = 2; index < process.argv.length; index += 2) {
    const key = process.argv[index];
    assert(['--mcp', '--relay', '--output', '--agent'].includes(key) && !values[key] && process.argv[index + 1], 'invalid arguments');
    values[key] = path.resolve(process.argv[index + 1]);
  }
  assert(values['--mcp'] && values['--relay'] && values['--output'], 'require --mcp --relay --output');
  return values;
}

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const port = server.address().port;
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return port;
}

async function waitFor(predicate) {
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await delay(100);
  }
  throw new Error('fixture timeout');
}

// 构造一个含固定普通文件的 Stored ZIP，不调用外部归档器或执行上传内容。
function storedZip() {
  const name = Buffer.from('fixture/readme.txt');
  const bytes = Buffer.alloc(300 * 1024, 0x61);
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
  }
  crc = (crc ^ 0xffffffff) >>> 0;
  const local = Buffer.alloc(30);
  local.writeUInt32LE(0x04034b50); local.writeUInt16LE(20, 4);
  local.writeUInt32LE(crc, 14); local.writeUInt32LE(bytes.length, 18); local.writeUInt32LE(bytes.length, 22);
  local.writeUInt16LE(name.length, 26);
  const central = Buffer.alloc(46);
  central.writeUInt32LE(0x02014b50); central.writeUInt16LE(20, 4); central.writeUInt16LE(20, 6);
  central.writeUInt32LE(crc, 16); central.writeUInt32LE(bytes.length, 20); central.writeUInt32LE(bytes.length, 24);
  central.writeUInt16LE(name.length, 28);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50); end.writeUInt16LE(1, 8); end.writeUInt16LE(1, 10);
  end.writeUInt32LE(central.length + name.length, 12); end.writeUInt32LE(local.length + name.length + bytes.length, 16);
  return Buffer.concat([local, name, bytes, central, name, end]);
}

async function main(args) {
  const report = { sdk_version: sdkVersion, ajv_version: '8.17.1', passed: false, checks: [] };
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^(REMOTEOPS_|JLAI_)/i.test(key)));
  const clients = [];
  let child;
  let closed;
  let fixture;
  let stage = 'fixture';
  const check = (name, value) => { assert(value, name); report.checks.push(name); };
  try {
    fixture = await fs.mkdtemp(path.join(os.tmpdir(), prefix));
    if (process.platform === 'win32') {
      const script = '$p=$env:REMOTEOPS_SCHEMA_FIXTURE; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; '
        + '$acl=[Security.AccessControl.DirectorySecurity]::new(); $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true,$false); '
        + '$acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid,"FullControl","ContainerInherit,ObjectInherit","None","Allow")); '
        + 'Set-Acl -LiteralPath $p -AclObject $acl; if(-not (Get-Acl -LiteralPath $p).AreAccessRulesProtected){exit 7}';
      const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', script], {
        env: { ...env, REMOTEOPS_SCHEMA_FIXTURE: fixture }, windowsHide: true, encoding: 'utf8', timeout: 15000,
      });
      assert.equal(result.status, 0, 'private fixture ACL');
    } else await fs.chmod(fixture, 0o700);
    const ports = new Set();
    while (ports.size < 3) ports.add(await freePort());
    const [relayPort, adminPort, healthPort] = [...ports];
    const relay = `127.0.0.1:${relayPort}`;
    const admin = `http://127.0.0.1:${adminPort}`;
    const adminToken = randomBytes(32).toString('hex');
    const owner = randomUUID();
    const cert = path.join(fixture, 'relay-cert.pem');
    const transfer = path.join(fixture, 'transfer');
    await fs.mkdir(transfer);
    const archive = storedZip();
    await fs.writeFile(path.join(transfer, 'fixture.zip'), archive, { flag: 'wx' });
    await fs.writeFile(path.join(transfer, 'invalid.zip'), 'not-a-zip', { flag: 'wx' });
    await fs.writeFile(path.join(fixture, 'mcp.json'), '{}', { flag: 'wx' });
    child = spawn(args['--relay'], ['--bind', relay, '--admin-bind', `127.0.0.1:${adminPort}`,
      '--health-bind', `127.0.0.1:${healthPort}`, '--tls-cert', cert, '--tls-key', path.join(fixture, 'relay-key.pem'),
      '--tls-sans', 'localhost,127.0.0.1', '--state-file', path.join(fixture, 'relay-state.json')], {
      cwd: fixture, env: { ...env, REMOTEOPS_HUMAN_CONTROLLER_TOKEN: randomBytes(32).toString('hex'),
        REMOTEOPS_ADMIN_TOKEN: adminToken, REMOTEOPS_CONTROLLER_OWNER_ID: owner },
      windowsHide: true, stdio: ['ignore', 'ignore', 'ignore'],
    });
    closed = new Promise(resolve => { child.once('close', resolve); child.once('error', resolve); });
    const http = async (route, method = 'GET', body, authenticate = true) => {
      const response = await fetch(admin + route, { method, headers: {
        ...(authenticate ? { Authorization: `Bearer ${adminToken}` } : {}),
        ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      }, ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(5000) });
      assert.equal(response.status, 200, 'loopback admin HTTP status');
      return response.json();
    };
    await waitFor(async () => {
      assert.equal(child.exitCode, null, 'fixture Relay exited');
      return fetch(`http://127.0.0.1:${healthPort}/health`, { signal: AbortSignal.timeout(1000) })
        .then(response => response.ok, () => false);
    });
    stage = 'enrollment';
    await http('/api/admin/mcp/settings', 'PUT', { enrollment_url: `https://localhost:${adminPort}/api/mcp/enroll`,
      relay, server_name: 'localhost', relay_ca_pem: await fs.readFile(cert, 'utf8'), enrollment_ca_pem: null });
    const grant = await http('/api/admin/mcp/setups', 'POST', { client_name: 'isolated schema regression', expires_in_hours: 1 });
    const installation = randomUUID();
    const secret = randomBytes(32).toString('base64url');
    const enrolled = await http('/api/mcp/enroll', 'POST', {
      grant_id: grant.setup.grant_id, grant_secret: grant.setup.grant_secret,
      installation_id: installation, installation_secret: secret,
    }, false);
    check('private_loopback_enrollment', enrolled.installation_id === installation && enrolled.owner_id === owner);

    if (args['--agent']) args['--mcp'] = await makeBrowserPromptFixture(args['--mcp'], fixture, env);
    async function connect(testUi) {
      const client = new Client({ name: 'remoteops-strict-schema-regression', version: '1' }, {
        capabilities: {}, jsonSchemaValidator: new AjvJsonSchemaValidator(
          new Ajv2020({ strict: true, validateFormats: false, allowUnionTypes: true })),
      });
      const inner = new StdioClientTransport({ command: args['--mcp'], args: [
        '--config', path.join(fixture, 'mcp.json'), '--relay', relay, '--server-name', 'localhost', '--ca-cert', cert,
        '--audit-log', path.join(fixture, 'audit.jsonl'), '--transfer-root', transfer,
        ...(testUi ? ['--enable-test-ui'] : []),
      ], cwd: fixture, env: { ...env, REMOTEOPS_CONTROLLER_TOKEN: `roc1.${installation}.${secret}`,
        REMOTEOPS_CONTROLLER_OWNER_ID: owner }, stderr: 'ignore' });
      let initializeId;
      const transport = {
        async start() {
          inner.onmessage = message => {
            if (message.id === initializeId && message.result?.protocolVersion) {
              report.negotiated_protocol_version = message.result.protocolVersion;
            }
            transport.onmessage?.(message);
          };
          inner.onclose = () => transport.onclose?.();
          inner.onerror = error => transport.onerror?.(error);
          await inner.start();
        },
        send(message) { if (message.method === 'initialize') initializeId = message.id; return inner.send(message); },
        close() { return inner.close(); },
      };
      clients.push(client);
      await client.connect(transport, { timeout: 30000 });
      const { tools, pages } = await validateToolPages(cursor => client.request({
        method: 'tools/list', params: cursor === undefined ? {} : { cursor },
      }, ListToolsResultSchema));
      check(testUi ? 'strict_test_ui_discovery' : 'strict_default_discovery', tools.length > 20);
      assert.equal(tools.some(tool => tool.name === 'test_prompt_text'), testUi);
      report[testUi ? 'test_tool_count' : 'default_tool_count'] = tools.length;
      report[testUi ? 'test_page_count' : 'default_page_count'] = pages;
      report.server_version = client.getServerVersion()?.version;
      return { client, tools };
    }

    stage = 'strict_discovery';
    const { client, tools } = await connect(false);
    await connect(true);
    const call = async (name, input) => {
      const tool = tools.find(tool => tool.name === name);
      return validateToolResult(tool, await client.callTool({ name, arguments: input }, undefined, {timeout: 420000}));
    };
    const manage = (revision, command) => call('manage_relay_tools', {
      request_id: randomUUID(), expected_revision: revision, command,
    });
    stage = 'tool_calls';
    const denied = await manage(0, { action: 'snapshot' });
    check('business_permission_error_schema_and_text', denied.error?.code === 'tool_management_denied' && denied.data === null);
    await http(`/api/admin/mcp/clients/${installation}/tool-management`, 'PUT', { enabled: true });
    const snapshot = await manage(0, { action: 'snapshot' });
    check('snapshot_schema_structured_text_agree', snapshot.error === null && snapshot.data.enabled === false);
    const uploaded = await call('upload_relay_tool_package', { local_path: 'fixture.zip', expected_revision: snapshot.revision });
    check('multichunk_upload_receipt_schema_structured_text_agree', uploaded.error === null
      && uploaded.data.file_verified === true && uploaded.data.sha256 === sha256(archive) && uploaded.data.size === archive.length);
    const invalid = await call('upload_relay_tool_package', { local_path: 'invalid.zip', expected_revision: uploaded.revision });
    check('finish_upload_business_error_schema_structured_text_agree', invalid.error?.code === 'invalid_zip' && invalid.data === null);
    const stale = await manage(uploaded.revision + 100, { action: 'set_policy', enabled: false });
    check('stale_revision_business_error_schema_structured_text_agree', stale.error?.code === 'revision_conflict');
    if (args['--agent']) { stage='browser_roundtrip'; await runBrowserChecks({agentExe:args['--agent'],fixture,transfer,env,relay,cert,client,call,http,check,report}); }
    await http(`/api/admin/mcp/clients/${installation}/tool-management`, 'PUT', { enabled: false });
    const early = await call('upload_relay_tool_package', { local_path: 'fixture.zip', expected_revision: uploaded.revision });
    check('begin_upload_early_error_schema_structured_text_agree', early.error?.code === 'tool_management_denied');
    if (!args['--agent']) check('global_execution_remains_disabled', (await http('/api/admin/tools')).data.enabled === false);
    report.passed = true;
  } catch {
    report.failure_stage = stage;
  } finally {
    for (const client of clients.reverse()) await client.close().catch(() => {});
    if (child && child.exitCode === null) child.kill();
    if (closed) await Promise.race([closed, delay(10000)]);
    report.fixture_process_closed = !child || child.exitCode !== null || child.signalCode !== null;
    report.fixture_directory = fixture;
    // 故障现场与受限临时目录保留供复核；报告不包含身份、令牌和业务正文。
    report.fixture_retained = Boolean(fixture);
    await fs.mkdir(path.dirname(args['--output']), { recursive: true });
    await fs.writeFile(args['--output'], JSON.stringify(report, null, 2) + '\n', { flag: 'wx' });
  }
  console.log(JSON.stringify(report));
  if (!report.passed || !report.fixture_process_closed) process.exitCode = 1;
}

await main(options());
