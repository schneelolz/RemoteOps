#!/usr/bin/env node
// 仅回环、真实 CLI Agent/Relay/MCP 的上传回执验收；无 GUI、SSH 或外部目标。
import { spawn } from 'node:child_process';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { EventEmitter } from 'node:events';
import fs from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import assert from 'node:assert/strict';

const fixturePrefix = 'remoteops-upload-receipt-e2e-';
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const ps = value => `'${String(value).replaceAll("'", "''")}'`;
const samePath = (left, right) => typeof left === 'string' && typeof right === 'string'
  && path.resolve(left.replace(/^\\\\\?\\/, '')).toLowerCase()
    === path.resolve(right.replace(/^\\\\\?\\/, '')).toLowerCase();
const succeeded = value => value?.isError !== true && value?.status === 'completed' && value.exit_code === 0;

function fail(category) {
  const error = new Error(category);
  error.category = category;
  return error;
}

// 先解析PE子系统；MCP发布版隐藏控制台，另需核验版本资源中的身份。
function consoleSubsystem(bytes) {
  if (!Buffer.isBuffer(bytes) || bytes.length < 64 || bytes.readUInt16LE(0) !== 0x5a4d) return null;
  const pe = bytes.readUInt32LE(0x3c);
  const optional = pe + 24;
  if (optional + 70 > bytes.length || bytes.readUInt32LE(pe) !== 0x4550) return null;
  if (bytes.readUInt16LE(pe + 20) < 70 || ![0x10b, 0x20b].includes(bytes.readUInt16LE(optional))) return null;
  return bytes.readUInt16LE(optional + 68);
}

function verifiedHiddenStdioMcp(subsystem, originalFilename) {
  return [2, 3].includes(subsystem) && originalFilename === 'remoteops-controller-mcp.exe';
}

function receiptMatches(result, roots, relative, bytes) {
  const receipt = result?.details;
  return succeeded(result) && result.sha256 === sha256(bytes) && receipt?.sha256 === sha256(bytes)
    && receipt.size === bytes.length && receipt.remote_path === relative
    && receipt.agent_path_status === 'reported'
    && samePath(receipt.agent_transfer_root, roots.agent)
    && samePath(receipt.local_transfer_root, roots.local)
    && samePath(receipt.remote_absolute_path, path.join(roots.agent, relative))
    && typeof result.summary === 'string' && result.summary.includes(receipt.remote_absolute_path);
}

function parseOptions(argv) {
  const result = { agent: '', relay: '', mcp: '', output: '' };
  const supplied = new Set();
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index]?.replace(/^--/, '');
    const value = argv[index + 1];
    if (!argv[index]?.startsWith('--') || !Object.hasOwn(result, key) || supplied.has(key)
      || !value || value.startsWith('--')) throw fail('invalid_arguments');
    supplied.add(key);
    result[key] = path.resolve(value);
  }
  if (supplied.size !== 4) throw fail('missing_arguments');
  return result;
}

class Mcp {
  constructor(state, timeout = 60000) {
    this.state = state;
    this.timeout = timeout;
    this.pending = new Map();
    this.sequence = 0;
    this.buffer = '';
    this.unexpectedRequests = 0;
    state.child.stdout.on('data', chunk => {
      this.buffer += chunk;
      let newline;
      while ((newline = this.buffer.indexOf('\n')) >= 0) {
        const line = this.buffer.slice(0, newline).trim();
        this.buffer = this.buffer.slice(newline + 1);
        if (!line) continue;
        let message;
        try { message = JSON.parse(line); } catch { continue; }
        if (message.method && message.id !== undefined) {
          // 此 fixture 应由现场缺省授权生效，不自动接受意外弹窗或任意请求。
          this.unexpectedRequests++;
          this.send(message.method === 'elicitation/create'
            ? { jsonrpc: '2.0', id: message.id, result: { action: 'cancel' } }
            : { jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'Unsupported fixture request' } });
        } else this.pending.get(message.id)?.resolve(message);
      }
    });
    state.child.once?.('close', () => {
      for (const pending of this.pending.values()) pending.reject(fail('mcp_transport_closed'));
      this.pending.clear();
    });
  }

  send(value) { this.state.child.stdin.write(`${JSON.stringify(value)}\n`); }

  async request(method, params, timeout = this.timeout) {
    if (this.state.ended) throw fail('mcp_transport_closed');
    const id = ++this.sequence;
    let timer;
    try {
      const message = await new Promise((resolve, reject) => {
        this.pending.set(id, { resolve, reject });
        timer = setTimeout(() => reject(fail('mcp_request_timeout')), timeout);
        this.send({ jsonrpc: '2.0', id, method, params });
      });
      return message.error ? { isError: true, rpc_error_code: message.error.code } : message.result;
    } finally {
      clearTimeout(timer);
      this.pending.delete(id);
    }
  }

  async tool(name, args, timeout = this.timeout) {
    const result = await this.request('tools/call', { name, arguments: args }, timeout);
    if (result?.isError) return result;
    if (result?.structuredContent) return result.structuredContent;
    for (const content of result?.content ?? []) {
      if (content.type === 'text') {
        try { return JSON.parse(content.text); } catch { /* 无结构回执不能按成功处理。 */ }
      }
    }
    return result;
  }
}

async function freePort() {
  const listener = net.createServer();
  await new Promise((resolve, reject) => {
    listener.once('error', reject);
    listener.listen(0, '127.0.0.1', resolve);
  });
  const port = listener.address().port;
  await new Promise((resolve, reject) => listener.close(error => error ? reject(error) : resolve()));
  return port;
}

async function noLinkAncestors(value) {
  for (let current = path.resolve(value); ; current = path.dirname(current)) {
    const stat = await fs.lstat(current).catch(error => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (stat?.isSymbolicLink()) throw fail('linked_fixture_or_report_path');
    if (current === path.dirname(current)) break;
  }
}

async function main(options) {
  const states = [];
  const checks = [];
  const binaries = {};
  const numbers = { file_cases: 0, uploaded_bytes: 0, unexpected_prompt_requests: 0 };
  const tempParent = path.resolve(os.tmpdir());
  const output = path.resolve(options.output);
  const cleanEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^(REMOTEOPS_|JLAI_)/i.test(key)));
  let fixture;
  let mcp;
  let sid;
  let current = 'initialization';
  let failure = null;
  let reportApproved = false;
  let fixtureRemoved = false;

  function check(name, passed) {
    current = name;
    checks.push({ name, passed: Boolean(passed) });
    if (!passed) throw fail('assertion_failed');
    console.log(`PASS ${name}`);
  }

  async function until(name, predicate, timeout = 30000) {
    current = name;
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      const value = await predicate();
      if (value) return value;
      await delay(75);
    }
    throw fail('fixture_wait_timeout');
  }

  function start(file, args, env) {
    const child = spawn(file, args, { cwd: fixture, env, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
    const state = { child, stdout: '', stderr: '', ended: false, code: null };
    state.closed = new Promise(resolve => {
      child.once('error', () => {
        // 已有PID时，kill失败不是退出；继续等待close，无法确认则保留fixture。
        if (!child.pid) { state.ended = true; resolve(); }
      });
      child.once('close', code => { state.ended = true; state.code = code; resolve(); });
    });
    child.stdin.on('error', () => {});
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', value => { state.stdout = (state.stdout + value).slice(-4 * 1024 * 1024); });
    child.stderr.on('data', value => { state.stderr = (state.stderr + value).slice(-1024 * 1024); });
    states.push(state);
    return state;
  }

  async function waitClosed(state, timeout) {
    let timer;
    try {
      await Promise.race([state.closed, new Promise(resolve => { timer = setTimeout(resolve, timeout); })]);
      return state.ended;
    } finally { clearTimeout(timer); }
  }

  try {
    if (process.platform !== 'win32' || path.parse(tempParent).root.toLowerCase() !== 'c:\\') throw fail('windows_c_temp_required');
    await noLinkAncestors(tempParent);
    await noLinkAncestors(path.dirname(output));
    const relativeOutput = path.relative(tempParent, output);
    if (!relativeOutput || relativeOutput.startsWith('..') || path.isAbsolute(relativeOutput)) throw fail('output_must_be_c_temp');
    if (await fs.lstat(output).then(() => true, () => false)) throw fail('report_already_exists');
    reportApproved = true;
    const bytes = {};
    for (const name of ['agent', 'relay', 'mcp']) {
      bytes[name] = await fs.readFile(options[name]);
      const subsystem = consoleSubsystem(bytes[name]);
      if (name !== 'mcp') check(`${name}_console_subsystem_before_start`, subsystem === 3);
      binaries[name] = { sha256: sha256(bytes[name]), subsystem };
    }
    fixture = await fs.mkdtemp(path.join(tempParent, fixturePrefix));
    // 目录内尚无秘密；先锁定并回读ACL，再写入本轮随机认证配置。
    const privacy = start('pwsh.exe', ['-NoProfile', '-Command',
      `$p=${ps(fixture)}; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; `
      + '$acl=[Security.AccessControl.DirectorySecurity]::new(); $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true,$false); '
      + '$rule=[Security.AccessControl.FileSystemAccessRule]::new($sid,"FullControl","ContainerInherit,ObjectInherit","None","Allow"); '
      + '$acl.AddAccessRule($rule); Set-Acl -LiteralPath $p -AclObject $acl; $actual=Get-Acl -LiteralPath $p; '
      + 'if(-not $actual.AreAccessRulesProtected -or @($actual.Access).Count -ne 1 -or '
      + '$actual.Access[0].IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value -cne $sid.Value){exit 7}; '
      + '[Console]::Out.Write("PRIVATE_OK")'], cleanEnv);
    check('private_fixture_acl_before_secrets', await waitClosed(privacy, 15000) && privacy.code === 0 && privacy.stdout === 'PRIVATE_OK');
    const directories = {};
    for (const name of ['package', 'agent-local', 'mcp-local', 'relay-local', 'agent-temp', 'mcp-temp', 'relay-temp', 'agent-transfer 中文', 'mcp-transfer 中文']) {
      directories[name] = path.join(fixture, name);
      await fs.mkdir(directories[name]);
    }
    const envFor = role => ({ ...cleanEnv, APPDATA: directories[`${role}-local`], LOCALAPPDATA: directories[`${role}-local`],
      TEMP: directories[`${role}-temp`], TMP: directories[`${role}-temp`] });
    const executables = {};
    for (const name of ['agent', 'mcp', 'relay']) {
      executables[name] = path.join(directories.package, `fixture-${name}.exe`);
      await fs.writeFile(executables[name], bytes[name], { flag: 'wx' });
    }
    // 只读查询私有快照的版本资源，不执行待测MCP，不按调用文件名猜身份。
    const metadata = start('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command',
      `[Console]::Out.Write([string]([Diagnostics.FileVersionInfo]::GetVersionInfo(${ps(executables.mcp)}).OriginalFilename))`], cleanEnv);
    const metadataRead = await waitClosed(metadata, 15000);
    check('verified_hidden_stdio_mcp_before_start', metadataRead && metadata.code === 0
      && verifiedHiddenStdioMcp(binaries.mcp.subsystem, metadata.stdout));
    binaries.mcp.original_filename = metadata.stdout;
    const token = randomBytes(32).toString('hex');
    const tokenEnv = { REMOTEOPS_AI_CONTROLLER_TOKEN: token, REMOTEOPS_CONTROLLER_TOKEN: token,
      REMOTEOPS_HUMAN_CONTROLLER_TOKEN: randomBytes(32).toString('hex'), REMOTEOPS_CONTROLLER_OWNER_ID: randomUUID() };
    const relayPort = await freePort();
    let healthPort = await freePort();
    while (healthPort === relayPort) healthPort = await freePort();
    const address = `127.0.0.1:${relayPort}`;
    const cert = path.join(fixture, 'relay-cert.pem');
    const relay = start(executables.relay, ['--bind', address, '--health-bind', `127.0.0.1:${healthPort}`,
      '--tls-cert', cert, '--tls-key', path.join(fixture, 'relay-key.pem'), '--tls-sans', '127.0.0.1,localhost',
      '--state-file', path.join(directories['relay-local'], 'relay-state.json'), '--heartbeat-seconds', '1', '--lease-seconds', '60'],
    { ...envFor('relay'), ...tokenEnv });
    await until('private_loopback_relay_ready', async () => {
      if (relay.ended) throw fail('relay_ended');
      return fs.stat(cert).then(() => true, () => false);
    });
    const roots = { agent: directories['agent-transfer 中文'], local: directories['mcp-transfer 中文'] };
    const config = path.join(directories['agent-local'], 'agent-config.json');
    await fs.writeFile(config, JSON.stringify({ relay: address, server_name: 'localhost', ca_cert: cert,
      state_file: path.join(directories['agent-local'], 'agent-state.json'), transfer_root: roots.agent,
      retry_seconds: 1, default_full_control: true }), { flag: 'wx' });
    const agent = start(executables.agent, ['--config', config], envFor('agent'));
    const pairing = await until('fixture_agent_pairing_ready', () => {
      if (agent.ended) throw fail('agent_ended');
      return /控制码[：:]\s*(\d[\d -]{7,14}\d)/u.exec(agent.stdout)?.[1]?.replace(/\D/g, '');
    }, 60000);
    check('fixture_pairing_code_only_in_memory', /^\d{9}$/.test(pairing));
    const mcpConfig = path.join(directories['mcp-local'], 'mcp-config.json');
    await fs.writeFile(mcpConfig, '{}', { flag: 'wx' });
    const transport = start(executables.mcp, ['--config', mcpConfig, '--relay', address, '--server-name', 'localhost',
      '--ca-cert', cert, '--audit-log', path.join(directories['mcp-local'], 'audit.jsonl'), '--transfer-root', roots.local,
      '--reconnect-seconds', '1'], { ...envFor('mcp'), ...tokenEnv });
    mcp = new Mcp(transport);
    await mcp.request('initialize', { protocolVersion: '2025-03-26', capabilities: { elicitation: { form: {} } },
      clientInfo: { name: 'upload-receipt-loopback-fixture', version: '1' } });
    mcp.send({ jsonrpc: '2.0', method: 'notifications/initialized' });
    sid = (await mcp.tool('pair_connection', { pairing_code: pairing }))?.session_id;
    check('paired_only_self_started_fixture', typeof sid === 'string');
    const tool = (name, args = {}) => mcp.tool(name, { session_id: sid, ...args });
    await until('fixture_local_default_full_control_synced', async () => {
      const mode = await tool('get_control_mode');
      return mode.mode === 'full_access' && mode.sync_status === 'synced';
    });
    const target = await tool('get_target_info');
    check('different_controller_and_agent_roots', !samePath(roots.agent, roots.local)
      && samePath(target.transfer_root, roots.local) && samePath(target.local_transfer_root, roots.local)
      && samePath(target.agent_transfer_root, roots.agent) && target.agent_transfer_root_status === 'reported');
    await fs.mkdir(path.join(roots.local, '中文 空格'));
    await fs.mkdir(path.join(roots.local, '回读'));
    const cases = [
      { name: 'empty', relative: '空文件.bin', bytes: Buffer.alloc(0) },
      { name: 'unicode_space_quote', relative: "中文 空格/诊断'文件.txt", bytes: Buffer.from('中文 UTF-8 往返，无凭据。\r\n', 'utf8') },
      { name: 'multi_chunk', relative: '中文 空格/分块工具.bin', bytes: Buffer.alloc(2 * 1024 * 1024 + 17, 0x5a) },
    ];
    for (const item of cases) {
      await fs.writeFile(path.join(roots.local, item.relative), item.bytes, { flag: 'wx' });
      const result = await tool('upload_file', { local_path: item.relative, remote_path: item.relative, overwrite: false });
      check(`${item.name}_direct_upload_absolute_receipt`, receiptMatches(result, roots, item.relative, item.bytes));
      check(`${item.name}_actual_agent_bytes_match`, sha256(await fs.readFile(path.join(roots.agent, item.relative))) === sha256(item.bytes));
      if (item.name === 'multi_chunk') check('multi_chunk_receipt_preserves_chunk_size', result.details.chunk_size_bytes === 1024 * 1024);
      const downloaded = await tool('download_file', { remote_path: item.relative, local_path: `回读/${item.name}.bin`, overwrite_local: false });
      check(`${item.name}_download_and_local_bytes_match`, succeeded(downloaded) && downloaded.sha256 === sha256(item.bytes)
        && sha256(await fs.readFile(path.join(roots.local, '回读', `${item.name}.bin`))) === sha256(item.bytes));
      numbers.file_cases++;
      numbers.uploaded_bytes += item.bytes.length;
    }
    const large = cases[2];
    const replacement = Buffer.from('replacement must require explicit overwrite', 'utf8');
    await fs.writeFile(path.join(roots.local, large.relative), replacement);
    const deniedOverwrite = await tool('upload_file', { local_path: large.relative, remote_path: large.relative, overwrite: false });
    check('no_overwrite_rejected_and_existing_bytes_preserved', !succeeded(deniedOverwrite)
      && sha256(await fs.readFile(path.join(roots.agent, large.relative))) === sha256(large.bytes));
    const overwrite = await tool('upload_file', { local_path: large.relative, remote_path: large.relative, overwrite: true });
    check('explicit_overwrite_returns_fresh_actual_receipt', receiptMatches(overwrite, roots, large.relative, replacement)
      && sha256(await fs.readFile(path.join(roots.agent, large.relative))) === sha256(replacement));
    const missing = await tool('upload_file', { local_path: 'missing-source.bin', remote_path: 'missing-target.bin', overwrite: false });
    check('missing_local_source_fails_without_remote_file', !succeeded(missing)
      && !(await fs.lstat(path.join(roots.agent, 'missing-target.bin')).then(() => true, () => false)));
    await fs.writeFile(path.join(fixture, 'outside.bin'), Buffer.from('owned outside fixture file'), { flag: 'wx' });
    const localEscape = await tool('upload_file', { local_path: '../outside.bin', remote_path: 'local-escape.bin', overwrite: false });
    check('local_parent_escape_rejected_without_publication', !succeeded(localEscape)
      && !(await fs.lstat(path.join(roots.agent, 'local-escape.bin')).then(() => true, () => false)));
    const remoteEscape = await tool('upload_file', { local_path: cases[1].relative, remote_path: '../remote-escape.bin', overwrite: false });
    check('agent_parent_escape_rejected_without_publication', !succeeded(remoteEscape)
      && !(await fs.lstat(path.join(fixture, 'remote-escape.bin')).then(() => true, () => false)));
    numbers.unexpected_prompt_requests = mcp.unexpectedRequests;
    check('no_elicitation_or_gui_prompt_requested', mcp.unexpectedRequests === 0);
    const closed = await tool('close_connection');
    check('fixture_connection_closed', succeeded(closed));
    sid = null;
  } catch (error) {
    failure = { step: current, category: error.category ?? 'fixture_or_product_failure' };
    if (!checks.some(item => item.name === current && !item.passed)) checks.push({ name: current, passed: false });
    process.exitCode = 1;
    console.error(`FAIL ${current} [${failure.category}]`);
  } finally {
    if (mcp && sid) await mcp.tool('close_connection', { session_id: sid }, 5000).catch(() => {});
    for (const state of [...states].reverse()) {
      if (!state.ended) {
        try { state.child.kill(); } catch { /* 有界等待后仍存活时记录清理失败。 */ }
      }
      await waitClosed(state, 5000);
    }
    const processesStopped = states.every(state => state.ended);
    if (!processesStopped) {
      failure ??= { step: 'cleanup', category: 'owned_process_cleanup_failed' };
      process.exitCode = 1;
    }
    if (fixture && processesStopped) {
      try {
        await noLinkAncestors(fixture);
        if (path.dirname(path.resolve(fixture)) !== tempParent || !path.basename(fixture).startsWith(fixturePrefix)
          || (await fs.lstat(fixture)).isSymbolicLink()) throw fail('unsafe_fixture_cleanup_path');
        await fs.rm(fixture, { recursive: true, force: true, maxRetries: 8, retryDelay: 200 });
        fixtureRemoved = true;
      } catch (error) {
        failure ??= { step: 'cleanup', category: error.category ?? 'fixture_cleanup_failed' };
        process.exitCode = 1;
      }
    }
    if (reportApproved) {
      // 不序列化原始RPC、目录、控制码、随机认证值或标准输出。
      await fs.mkdir(path.dirname(output), { recursive: true });
      await fs.writeFile(output, `${JSON.stringify({ schema: 1, binaries, checks, numbers, failure,
        cleanup: { owned_processes_stopped: processesStopped, fixture_removed: fixtureRemoved },
        gui_started: false, external_target_used: false,
        passed: failure === null && fixtureRemoved && checks.every(item => item.passed) }, null, 2)}\n`, { flag: 'wx' });
      console.log(`REPORT ${output}`);
    }
  }
}

async function selfTest() {
  assert.deepEqual(parseOptions(['--agent', 'a', '--relay', 'b', '--mcp', 'c', '--output', 'd']),
    Object.fromEntries([['agent', 'a'], ['relay', 'b'], ['mcp', 'c'], ['output', 'd']].map(([key, value]) => [key, path.resolve(value)])));
  assert.throws(() => parseOptions(['--agent', 'a', '--agent', 'b']), value => value.category === 'invalid_arguments');
  assert.throws(() => parseOptions(['--agent', 'a']), value => value.category === 'missing_arguments');
  assert.equal(ps("中文 '单引号'"), "'中文 ''单引号'''");
  assert.equal(succeeded({ status: 'completed', exit_code: 0 }), true);
  assert.equal(succeeded({ status: 'completed', exit_code: null }), false);
  assert.equal(succeeded({ isError: true, status: 'completed', exit_code: 0 }), false);
  assert.equal(succeeded({ status: 'failed', exit_code: 0 }), false);
  const sample = Buffer.alloc(256);
  sample.writeUInt16LE(0x5a4d, 0); sample.writeUInt32LE(64, 0x3c); sample.writeUInt32LE(0x4550, 64);
  sample.writeUInt16LE(70, 84); sample.writeUInt16LE(0x20b, 88); sample.writeUInt16LE(3, 156);
  assert.equal(consoleSubsystem(sample), 3);
  sample.writeUInt16LE(2, 156); assert.equal(consoleSubsystem(sample), 2);
  sample.writeUInt32LE(0xffffffff, 0x3c); assert.equal(consoleSubsystem(sample), null);
  assert.equal(consoleSubsystem(Buffer.alloc(8)), null);
  assert.equal(verifiedHiddenStdioMcp(2, 'remoteops-controller-mcp.exe'), true);
  assert.equal(verifiedHiddenStdioMcp(3, 'remoteops-controller-mcp.exe'), true);
  assert.equal(verifiedHiddenStdioMcp(2, 'remoteops-agent-native-prototype.exe'), false);
  assert.equal(verifiedHiddenStdioMcp(2, 'remoteops-credential-prompt.exe'), false);
  assert.equal(verifiedHiddenStdioMcp(9, 'remoteops-controller-mcp.exe'), false);
  assert.equal(verifiedHiddenStdioMcp(null, 'remoteops-controller-mcp.exe'), false);
  assert.equal(verifiedHiddenStdioMcp(2, ''), false);
  const roots = { agent: path.resolve('agent-root'), local: path.resolve('local-root') };
  const bytes = Buffer.from('中文');
  const relative = "folder with space/it's.txt";
  const value = { status: 'completed', exit_code: 0, sha256: sha256(bytes), summary: path.join(roots.agent, relative),
    details: { sha256: sha256(bytes), size: bytes.length, remote_path: relative, agent_path_status: 'reported',
      agent_transfer_root: roots.agent, local_transfer_root: roots.local, remote_absolute_path: path.join(roots.agent, relative) } };
  assert.equal(receiptMatches(value, roots, relative, bytes), true);
  assert.equal(receiptMatches({ ...value, summary: 'size only' }, roots, relative, bytes), false);
  assert.equal(receiptMatches({ ...value, details: { ...value.details, agent_path_status: 'unknown' } }, roots, relative, bytes), false);
  assert.equal(receiptMatches({ ...value, details: { ...value.details, remote_absolute_path: path.join(roots.local, relative) } }, roots, relative, bytes), false);
  const stdout = new EventEmitter();
  const child = new EventEmitter();
  const sent = [];
  child.stdout = stdout;
  child.stdin = { write: text => sent.push(JSON.parse(text)) };
  const mcp = new Mcp({ child, ended: false }, 30);
  const result = mcp.tool('fixture', {});
  stdout.emit('data', JSON.stringify({ jsonrpc: '2.0', id: sent[0].id, result: { structuredContent: value } }).slice(0, 22));
  const message = JSON.stringify({ jsonrpc: '2.0', id: sent[0].id, result: { structuredContent: value } });
  stdout.emit('data', `${message.slice(22)}\n`);
  assert.equal(receiptMatches(await result, roots, relative, bytes), true);
  stdout.emit('data', `${JSON.stringify({ jsonrpc: '2.0', id: 'unexpected', method: 'elicitation/create' })}\n`);
  assert.deepEqual(sent.at(-1).result, { action: 'cancel' });
  assert.equal(mcp.unexpectedRequests, 1);
  const rejected = mcp.tool('fixture', {});
  stdout.emit('data', `${JSON.stringify({ jsonrpc: '2.0', id: sent.at(-1).id, error: { code: -1, message: 'DO_NOT_PERSIST' } })}\n`);
  assert.deepEqual(await rejected, { isError: true, rpc_error_code: -1 });
  await assert.rejects(mcp.tool('timeout', {}), value => value.category === 'mcp_request_timeout');
  assert.equal(mcp.pending.size, 0);
  const closed = mcp.tool('closed', {});
  child.emit('close');
  await assert.rejects(closed, value => value.category === 'mcp_transport_closed');
  console.log('PASS upload_receipt_fixture_self_tests');
}

if (process.argv.length === 3 && process.argv[2] === '--self-test') await selfTest();
else {
  try { await main(parseOptions(process.argv.slice(2))); }
  catch (error) { console.error(`FAIL ${error.category ?? 'initialization_failure'}`); process.exitCode = 1; }
}
