#!/usr/bin/env node
// 本机隔离的真实原生 GUI 端到端验收；凭据、控制码、命令输出只留临时 fixture 和内存。
import { spawn } from 'node:child_process';
import { randomBytes, randomUUID, createHash } from 'node:crypto';
import fs from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const options = {
  scenario: 'all',
  agent: path.join(root, 'target/release-small/remoteops-agent-native-prototype.exe'),
  relay: path.join(root, 'target/release/remoteops-relay.exe'),
  mcp: path.join(root, 'target/release/remoteops-controller-mcp.exe'),
  'ui-client': path.join(root, 'scripts/NativeGui-TestClient.ps1'),
  'clipboard-test': '',
  output: path.join(root, 'artifacts/native-ui-e2e', `${Date.now()}-${randomUUID()}.json`),
};
for (let i = 2; i < process.argv.length; i += 2) {
  const key = process.argv[i].replace(/^--/, '');
  if (!(key in options) || !process.argv[i + 1]) throw new Error('参数：--agent EXE --relay EXE --mcp EXE --ui-client PS1 --output JSON');
  options[key] = key === 'scenario' ? process.argv[i + 1] : path.resolve(process.argv[i + 1]);
}
if (!['all', 'operations'].includes(options.scenario)) throw new Error('--scenario 仅支持 all 或 operations');
if (process.platform !== 'win32') throw new Error('此验收只支持 Windows。');
if (path.parse(os.tmpdir()).root.slice(0, 2).toLowerCase() !== 'c:') throw new Error('本轮 fixture 仅允许在 C 盘系统临时目录创建。');
const processes = [];
const results = [];
let current = '初始化';
let fixture;
let agent;
let cleanEnv;
let processStart;
let expectedHash;
let failure = null;
let relayProcess;
let currentReportPath;
let clipboardEvidence = null;
let clipboardReportDirectory = null;
const uiDiagnostics = [];
const pendingTools = new Set();
function observePendingTool(promise) {
  // 立即观察拒绝，正常路径仍 await 原 Promise 抛出原业务错误；提前失败时只负责有界收尾。
  const observation = promise.then(() => undefined, () => undefined);
  pendingTools.add(observation);
  observation.then(() => pendingTools.delete(observation));
  return promise;
}
function foregroundFacts(snapshot) {
  const handle = snapshot.foreground_handle;
  return { handle, process_id: snapshot.foreground_process_id ?? null,
    owned_modal_dialog_count: snapshot.owned_modal_dialog_count ?? null,
    role: handle === snapshot.main?.handle ? 'main' : handle === snapshot.observer?.handle ? 'observer'
      : !handle ? 'none' : snapshot.foreground_process_id === snapshot.process_id ? 'target_other'
        : snapshot.foreground_process_id ? 'third_party' : 'unknown' };
}
function stableForegroundPair(previous, currentSnapshot) {
  return previous?.owned_modal_dialog_count === 0 && currentSnapshot.owned_modal_dialog_count === 0
    && Number.isInteger(currentSnapshot.foreground_handle) && currentSnapshot.foreground_handle > 0
    && Number.isInteger(currentSnapshot.foreground_process_id) && currentSnapshot.foreground_process_id > 0
    && previous.foreground_handle === currentSnapshot.foreground_handle
    && previous.foreground_process_id === currentSnapshot.foreground_process_id;
}
function check(name, passed, safeDetails = null) {
  current = name;
  results.push({ test: name, status: passed ? 'passed' : 'failed' });
  if (!passed) throw safeDetails ? classifiedError('assertion_failed', safeDetails) : new Error(name);
  console.log(`PASS ${name}`);
}

async function waitFor(name, predicate, timeout = 30000) {
  current = name;
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const value = await predicate();
    if (value) return value;
    await delay(100);
  }
  throw new Error(name);
}

async function unusedPort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const port = server.address().port;
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return port;
}

function start(executable, args, env) {
  const child = spawn(executable, args, {
    cwd: fixture, env, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'],
  });
  const state = { child, stdout: '', stderr: '', ended: false, code: null };
  state.closed = new Promise(resolve => {
    child.once('error', () => { state.ended = true; resolve(); });
    child.once('close', code => { state.code = code; state.ended = true; resolve(); });
  });
  child.stdin.on('error', () => {});
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  child.stdout.on('data', value => { state.stdout = (state.stdout + value).slice(-1024 * 1024); });
  child.stderr.on('data', value => { state.stderr = (state.stderr + value).slice(-1024 * 1024); });
  processes.push(state);
  return state;
}

class Mcp {
  constructor(state) {
    this.state = state;
    this.pending = new Map();
    this.sequence = 0;
    this.buffer = '';
    this.elicitations = 0;
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
          if (message.method === 'elicitation/create') this.elicitations++;
          this.send({ jsonrpc: '2.0', id: message.id, result: message.method === 'elicitation/create' ? { action: 'accept', content: { confirmed: true } } : {} });
        } else if (this.pending.has(message.id)) {
          this.pending.get(message.id).resolve(message);
        }
      }
    });
    state.closed.then(() => {
      for (const pending of this.pending.values()) pending.reject(classifiedError('mcp_transport_closed'));
      this.pending.clear();
    });
  }

  send(message) { this.state.child.stdin.write(`${JSON.stringify(message)}\n`); }

  async request(method, params) {
    if (this.state.ended) throw classifiedError('mcp_transport_closed');
    const id = ++this.sequence;
    let timer;
    try {
      const message = await new Promise((resolve, reject) => {
        this.pending.set(id, { resolve, reject });
        timer = setTimeout(() => reject(new Error('MCP 请求超时')), 30000);
        this.send({ jsonrpc: '2.0', id, method, params });
      });
      return message.error ? { isError: true, rpc_error_code: message.error.code, diagnostic_categories: classifyDiagnosticText(message.error.message) } : message.result;
    } finally {
      clearTimeout(timer);
      this.pending.delete(id);
    }
  }

  async tool(name, args) {
    const result = await this.request('tools/call', { name, arguments: args });
    if (result?.isError) return result;
    if (result?.structuredContent) return result.structuredContent;
    for (const item of result?.content ?? []) {
      if (item.type === 'text') {
        try { return JSON.parse(item.text); } catch { /* 非结构化文字不用于判定成功。 */ }
      }
    }
    return result;
  }
}


const succeeded = result => !result?.isError && result?.status === 'completed'
  && (result.exit_code === 0 || result.exit_code == null);
const controlText = (snapshot, id) => snapshot.main?.controls?.find(row => row.id === id)?.text ?? '';

function classifyDiagnosticText(text) {
  const rules = [
    ['connecting', /正在连接|Connecting/i], ['reconnecting', /正在重连|Reconnecting/i],
    ['waiting', /等待工程师连接|Waiting for engineer/i], ['failed', /连接失败|Connection failed/i],
    ['state_lock', /状态锁|身份状态|state.*lock|WouldBlock|同一状态文件/i],
    ['permission_denied', /拒绝访问|Access.*denied|PermissionDenied/i],
    ['peer_closed', /远端.*关闭|peer.*closed|connection.*closed|EOF/i],
    ['system_probe', /图形探测|系统探测|environment|detect_environment|host_identity|native.*prepare/i],
    ['tls_worker_pending', /TLS.*(?:后台|worker).*(?:尚未|未停止|stop)/i],
    ['trust_not_pending', /没有待确认|没有证书指纹|no.*(?:pending|fingerprint)/i],
    ['certificate', /证书|certificate|fingerprint|指纹|UnknownIssuer/i],
    ['config', /配置.*(?:失败|无效|错误)|(?:invalid|failed|error).*config|config.*(?:invalid|failed|error)/i],
    ['connection_refused', /拒绝连接|连接被拒绝|connection refused|os error 10061/i],
    ['connection', /连接|connection|TLS|Handshake/i],
    ['timeout', /超时|timeout|timed out/i],
    ['worker_stopped', /后台.*(?:退出|停止)|worker.*(?:exit|stop)|Stopped/i],
    ['invalid', /无效|invalid|malformed/i],
    ['panic', /panic|BorrowMutError|already borrowed/i],
  ];
  return rules.filter(([, pattern]) => pattern.test(text ?? '')).map(([name]) => name);
}

function recordUiDiagnostic(snapshot) {
  const has = id => snapshot.main?.controls?.some(row => row.id === id);
  const page = has(335) ? 'trust' : has(314) ? 'setup' : has(331) ? 'permissions' : has(303) ? 'main' : has(337) ? 'details' : has(324) ? 'logs' : has(317) ? 'preferences' : 'other';
  const summary = { step: current, page, copy_enabled: snapshot.main?.controls?.some(row => row.id === 304 && row.enabled) ?? false, code_available: controlText(snapshot, 303).replace(/\D/g, '').length === 9,
    error_categories: classifyDiagnosticText(controlText(snapshot, 312)),
    status_categories: classifyDiagnosticText(controlText(snapshot, 300)),
    observer_visible: Boolean(snapshot.observer?.visible) };
  if (JSON.stringify(summary) !== JSON.stringify(uiDiagnostics.at(-1))) uiDiagnostics.push(summary);
  if (uiDiagnostics.length > 16) uiDiagnostics.shift();
}

function classifiedError(category, details = {}) {
  const error = new Error(category);
  error.category = category;
  error.safeDetails = details;
  return error;
}

async function clipboardDiagnostic() {
  if (!clipboardReportDirectory) return null;
  try {
    const report = JSON.parse(await fs.readFile(path.join(clipboardReportDirectory, 'report.json'), 'utf8'));
    return { executable_sha256: expectedHash, reported_hash_matches: report.executable_sha256 === expectedHash,
      passed: report.passed === true, checks: (report.checks ?? []).map(item => ({
        name: /^[a-z0-9_]+$/.test(item.name) ? item.name : 'invalid_check_name', passed: item.passed === true })),
      metrics: Object.fromEntries(Object.entries(report.metrics ?? {}).filter(([key, value]) =>
        /^[a-z0-9_]+$/.test(key) && typeof value === 'number' && Number.isFinite(value))),
      failure: report.failure ? { step: /^[a-z0-9_]+$/.test(report.failure.step) ? report.failure.step : 'unknown',
        category: /^[a-z0-9_]+$/.test(report.failure.category) ? report.failure.category : 'unknown' } : null };
  } catch { return { executable_sha256: expectedHash, passed: false, report_unavailable: true }; }
}

async function ui(action = 'Snapshot', id = 0, value = undefined) {
  const args = ['-NoProfile', '-STA', '-File', options['ui-client'], '-ProcessId', String(agent.child.pid),
    '-ExpectedHash', expectedHash, '-ProcessStartUtc', processStart, '-Action', action];
  if (id) args.push('-ControlId', String(id));
  if (value !== undefined) args.push('-Value', value);
  const client = start('pwsh.exe', args, cleanEnv);
  await Promise.race([client.closed, delay(15000)]);
  if (!client.ended || client.code !== 0) {
    const rules = [
      ['window_missing', /测试窗口数量不正确|窗口不唯一/], ['control_missing', /目标控件不存在/],
      ['identity', /身份|启动身份|SHA-256|哈希|ProcessStart/], ['confirmation', /确认框|确认按钮/],
      ['uia_pattern', /不支持|模式|Invoke|ValuePattern|TextPattern/], ['access', /拒绝访问|Access.*denied/i],
    ];
    const categories = rules.filter(([, pattern]) => pattern.test(client.stderr)).map(([name]) => name);
    throw classifiedError(categories.includes('window_missing') ? 'ui_window_missing' : 'ui_client_failed',
      { action, control_id: id, exit_code: client.code, timed_out: !client.ended, diagnostics: categories });
  }
  try {
    const result = JSON.parse(client.stdout.replace(/^\uFEFF/, '').trim());
    if (action === 'Snapshot') recordUiDiagnostic(result);
    return result;
  }
  catch { throw classifiedError('ui_client_invalid_json', { action, control_id: id }); }
}

async function startNative(executable, config, report) {
  const relative = path.relative(fixture, report);
  if (relative.startsWith('..') || path.isAbsolute(relative)) throw new Error('拒绝删除fixture以外报告');
  // 同一配置重启时清除前次报告，不能把前一进程结果误作本次证据。
  await fs.rm(report, { force: true });
  currentReportPath = report;
  agent = start(executable, ['--agent-config', config, '--report', report], { ...cleanEnv, NATIVE_E2E_SUFFIX: 'VISIBLE' });
  const identity = start('pwsh.exe', ['-NoProfile', '-Command',
    `[Diagnostics.Process]::GetProcessById(${agent.child.pid}).StartTime.ToUniversalTime().ToString('o')`], cleanEnv);
  await identity.closed;
  processStart = identity.stdout.trim();
  check('原生进程身份可精确绑定', identity.code === 0 && /^\d{4}-.*\.\d{7}Z$/.test(processStart));
}

async function waitUi(name, predicate, timeout = 30000) {
  return waitFor(name, async () => {
    if (agent.ended) throw classifiedError('native_process_ended', { exit_code: agent.code });
    let snapshot;
    try { snapshot = await ui(); }
    catch (error) { if (error.category === 'ui_window_missing') return false; throw error; }
    return predicate(snapshot) ? snapshot : false;
  }, timeout);
}

async function pairingReady(name) {
  try {
    // Agent 首次视觉探测已有35秒预算，且环境探测先于Started；连接等待覆盖该启动预算。
    // 这不是日志的100毫秒批处理或每十万行15秒门槛，后两者保持原值。
    const snapshot = await waitUi(name, value => controlText(value, 303).replace(/\D/g, '').length === 9, 60000);
    return { snapshot, code: controlText(snapshot, 303).replace(/\D/g, '') };
  } catch (error) {
    // 仅在本轮测试主窗超时后进入详情/日志，正文只做固定分类，绝不保存正文或路径。
    const details = {};
    try {
      const snapshot = await ui();
      if (snapshot.main.controls.some(row => row.id === 307)) {
        await ui('Invoke', 307);
        const page = await ui();
        const body = controlText(page, 311);
        details.detail_categories = classifyDiagnosticText(body);
        details.capabilities_present = /(?:Cmd|PowerShell|WindowsPowerShell|Serial|Ssh):/.test(body);
        await ui('Invoke', 310);
        await ui('Invoke', 308);
        const logs = controlText(await ui(), 311);
        details.log_categories = classifyDiagnosticText(logs);
        details.logs_present = logs.trim().length > 0;
      }
    } catch { details.detail_probe_failed = true; }
    const diagnostic = classifiedError(error.category ?? 'pairing_not_ready', { ...(error.safeDetails ?? {}), ...details });
    throw diagnostic;
  }
}

async function applicationDiagnostic() {
  if (!currentReportPath) return null;
  try {
    const report = JSON.parse(await fs.readFile(currentReportPath, 'utf8'));
    const metrics = Object.fromEntries(Object.entries(report.metrics ?? {}).filter(([key, value]) =>
      /^[a-z][a-z0-9_]{0,63}$/.test(key) && (typeof value === 'number' && Number.isFinite(value)
        || typeof value === 'boolean' || Array.isArray(value) && value.length <= 8 && value.every(item => typeof item === 'number' && Number.isFinite(item)))));
    const failed = (report.checks ?? []).filter(item => item.passed !== true).map(item =>
      typeof item.name === 'string' && /^[A-Za-z][A-Za-z0-9_:/.-]{0,127}$/.test(item.name) ? item.name : 'non_identifier_check');
    return { passed: report.passed === true, failed_checks: failed, metrics };
  } catch { return { report_unavailable: true }; }
}

async function stopNative(name) {
  await ui('Close', 0, 'Confirm');
  await waitFor(name, () => agent.ended, 10000);
  const diagnostic = await applicationDiagnostic();
  if (agent.code !== 0) {
    current = name;
    throw classifiedError('native_nonzero_exit', { exit_code: agent.code, application: diagnostic });
  }
  check(name, true);
}

async function confirmPermission(value) {
  // Invoke 可能同步阻塞在仅属于本测试主窗的确认框，须并发启动另一客户端确认。
  const invoking = ui('Invoke', 331);
  const observed = invoking.then(result => ({ result }), error => ({ error }));
  await delay(300);
  await ui('Confirm', 0, value);
  const completed = await observed;
  if (completed.error) throw completed.error;
  if (value === 'Yes') await waitStableForeground('权限确认自有模态框关闭且前台稳定');
}

async function waitStableForeground(name) {
  let previous = null;
  return waitFor(name, async () => {
    const snapshot = await ui();
    if (!Number.isInteger(snapshot.owned_modal_dialog_count)) throw classifiedError('ui_modal_count_missing');
    const stable = stableForegroundPair(previous, snapshot);
    previous = snapshot.owned_modal_dialog_count === 0 ? snapshot : null;
    return stable ? snapshot : false;
  }, 10000);
}

async function observerContains(text, timeout = 15000) {
  return waitFor('实际观察窗内容已追上', async () => {
    const snapshot = await ui();
    return snapshot.observer?.visible && snapshot.observer.text.includes(text) ? snapshot : false;
  }, timeout);
}

try {
  for (const name of ['agent', 'relay', 'mcp', 'ui-client']) await fs.access(options[name]);
  fixture = await fs.mkdtemp(path.join(os.tmpdir(), 'remoteops-native-ui-e2e-'));
  const agentExe = path.join(fixture, 'native-agent.exe');
  await fs.copyFile(options.agent, agentExe);
  // 不复制任何现场配置、身份或凭据；只使用下面生成的回环 fixture。
  expectedHash = createHash('sha256').update(await fs.readFile(agentExe)).digest('hex');
  cleanEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^(REMOTEOPS_|JLAI_)/i.test(key)));
  const token = randomBytes(32).toString('hex');
  const env = { ...cleanEnv, REMOTEOPS_AI_CONTROLLER_TOKEN: token,
    REMOTEOPS_HUMAN_CONTROLLER_TOKEN: randomBytes(32).toString('hex'),
    REMOTEOPS_CONTROLLER_TOKEN: token, REMOTEOPS_CONTROLLER_OWNER_ID: randomUUID() };
  const relayPort = await unusedPort();
  let healthPort = await unusedPort();
  while (healthPort === relayPort) healthPort = await unusedPort();
  const address = `127.0.0.1:${relayPort}`;
  const cert = path.join(fixture, 'cert.pem');
  const relay = start(options.relay, ['--bind', address, '--health-bind', `127.0.0.1:${healthPort}`,
    '--tls-cert', cert, '--tls-key', path.join(fixture, 'key.pem'), '--tls-sans', '127.0.0.1,localhost',
    '--state-file', path.join(fixture, 'relay-state.json'), '--heartbeat-seconds', '1', '--lease-seconds', '60'], env);
  relayProcess = relay;
  await waitFor('临时 Relay 就绪', async () => {
    if (relay.ended) throw new Error('隔离 Relay 提前退出');
    try { await fs.access(cert); } catch { return false; }
    return new Promise(resolve => {
      const socket = net.connect({ host: '127.0.0.1', port: relayPort });
      socket.setTimeout(200);
      socket.once('connect', () => { socket.destroy(); resolve(true); });
      socket.once('error', () => resolve(false));
      socket.once('timeout', () => { socket.destroy(); resolve(false); });
    });
  });
  async function tlsConfig(name, relayAddress) {
    const directory = path.join(fixture, name);
    await fs.mkdir(directory);
    const configPath = path.join(directory, 'agent-config.json');
    await fs.writeFile(configPath, JSON.stringify({ relay: relayAddress, server_name: 'localhost',
      state_file: path.join(directory, 'agent-state.json'), transfer_root: path.join(directory, 'agent-files'),
      retry_seconds: 1, default_full_control: false }));
    return { config: configPath, report: path.join(directory, 'application-report.json') };
  }
  const trustVisible = snapshot => [334, 335, 336].every(id => snapshot.main.controls.some(row => row.id === id && row.enabled));
  const noPairing = snapshot => controlText(snapshot, 303).replace(/\D/g, '').length !== 9
    && !snapshot.main.controls.some(row => row.id === 304 && row.enabled);

  if (options.scenario === 'all') {
  const cancelled = await tlsConfig('tls-setup-cancel', '');
  await startNative(agentExe, cancelled.config, cancelled.report);
  await waitUi('首次设置页真实可用', snapshot => snapshot.main.controls.some(row => row.id === 314 && row.enabled));
  await ui('SetValue', 314, address);
  await ui('Invoke', 316);
  const pendingTrust = await waitUi('首次保存后显示真实未知证书确认', trustVisible);
  check('未知证书确认前未签发可复制控制码', noPairing(pendingTrust));
  await ui('Invoke', 334);
  const afterCancel = await ui();
  check('取消证书信任后保持未连接', noPairing(afterCancel));
  await stopNative('首次设置取消信任后正常退出');

  const once = await tlsConfig('tls-once', address);
  await startNative(agentExe, once.config, once.report);
  await waitUi('仅本次信任场景显示证书确认', trustVisible);
  await ui('Invoke', 335);
  await pairingReady('仅本次信任后实际连接并签发控制码');
  check('仅本次信任不保存证书指纹', !JSON.parse(await fs.readFile(once.config, 'utf8')).tls_fingerprint);
  await stopNative('仅本次信任实例正常退出');

  const saved = await tlsConfig('tls-save', address);
  await startNative(agentExe, saved.config, saved.report);
  await waitUi('保存信任场景显示证书确认', trustVisible);
  await ui('Invoke', 336);
  await pairingReady('保存信任后实际连接并签发控制码');
  const savedConfig = JSON.parse(await fs.readFile(saved.config, 'utf8'));
  check('保存信任后配置包含固定指纹', typeof savedConfig.tls_fingerprint === 'string' && savedConfig.tls_fingerprint.length > 0);
  await stopNative('保存信任实例正常退出');
  await startNative(agentExe, saved.config, saved.report);
  const restarted = await pairingReady('保存信任配置重启后无需再次确认');
  check('重启后直接连接且无证书确认页', !trustVisible(restarted.snapshot));
  await stopNative('保存信任重启实例正常退出');
  }

  const config = path.join(fixture, 'agent-config.json');
  await fs.writeFile(config, JSON.stringify({ relay: address, server_name: 'localhost', ca_cert: cert,
    state_file: path.join(fixture, 'agent-state.json'), transfer_root: path.join(fixture, 'agent-files'),
    retry_seconds: 1, default_full_control: false }));
  const applicationReport = path.join(fixture, 'application-report.json');
  await startNative(agentExe, config, applicationReport);
  const ready = await pairingReady('原生主窗显示真实控制码');
  check('旧版布局核心控件为真实原生按钮', [301, 304, 307, 308, 309].every(id =>
    ready.snapshot.main.controls.some(row => row.id === id && row.invoke && row.control_type === 'ControlType.Button')));
  check('主窗口按旧版紧凑布局启动', ready.snapshot.main.client_width === Math.round(500 * ready.snapshot.main.dpi / 96) && ready.snapshot.main.client_height === Math.round(375 * ready.snapshot.main.dpi / 96));
  await fs.mkdir(path.join(fixture, 'mcp-files'));
  const mcpConfig = path.join(fixture, 'mcp-config.json');
  await fs.writeFile(mcpConfig, '{}');
  const mcp = new Mcp(start(options.mcp, ['--config', mcpConfig, '--relay', address, '--server-name', 'localhost',
    '--ca-cert', cert, '--audit-log', path.join(fixture, 'audit.jsonl'), '--transfer-root', path.join(fixture, 'mcp-files'),
    '--reconnect-seconds', '1'], env));
  await mcp.request('initialize', { protocolVersion: '2025-03-26', capabilities: { elicitation: { form: {} } },
    clientInfo: { name: 'native-ui-fixture-e2e', version: '1' } });
  mcp.send({ jsonrpc: '2.0', method: 'notifications/initialized' });
  const paired = await mcp.tool('pair_connection', { pairing_code: ready.code });
  const sid = paired?.session_id;
  check('仅配对本轮回环原生 Agent', typeof sid === 'string');
  const tool = (name, args = {}) => mcp.tool(name, { session_id: sid, ...args });
  await waitFor('初始逐项确认同步', async () => {
    const mode = await tool('get_control_mode');
    return mode?.sync_status === 'synced' && mode.mode === 'step_by_step';
  });
  check('真实只读 CMD 查询成功', succeeded(await tool('run_readonly_command', { shell: 'cmd', command: 'ver' })));
  check('真实只读 PowerShell 查询成功', succeeded(await tool('run_readonly_command', { shell: 'windows_power_shell', command: 'Get-Date' })));
  await ui('Invoke', 301);
  await ui('Invoke', 322);
  await waitUi('权限页完全控制按钮可用', snapshot => snapshot.main.controls.some(row => row.id === 331 && row.enabled));
  await confirmPermission('No');
  const declined = await tool('get_control_mode');
  check('取消原生完全控制确认不改变权限', declined?.mode === 'step_by_step' && declined?.sync_status === 'synced');
  await confirmPermission('Yes');
  await waitFor('原生授权获Relay确认且来源为现场', async () => {
    const mode = await tool('get_control_mode');
    return mode?.sync_status === 'synced' && mode.mode === 'full_access' && mode.source === 'local';
  });
  check('原生临时授权不保存默认权限', JSON.parse(await fs.readFile(config, 'utf8')).default_full_control === false);
  await ui('Invoke', 310);
  if ((await ui()).observer?.visible) await ui('Invoke', 205);
  const before = await waitStableForeground('观察窗测试前无自有模态框且前台连续稳定');
  check('前台证据接口存在', typeof before.foreground_handle === 'number');
  let done = false;
  const streaming = observePendingTool(tool('run_command', { shell: 'windows_power_shell', command:
    `[Console]::Out.WriteLine(('NATIVE_FIRST_'+'FRAME')); [Console]::Out.WriteLine(('中文正常'+[char]::ConvertFromUtf32(0x1f50e))); [Console]::Out.WriteLine(('password='+'SYNTHETIC_'+'HIDDEN')); Start-Sleep -Seconds 10; 1..300 | ForEach-Object {[Console]::Out.WriteLine(('NATIVE_ROW_'+$_))}; [Console]::Error.WriteLine(('NATIVE_STDERR_'+'FRAME')); [Console]::Out.WriteLine(('NATIVE_LAST_'+'FRAME'))`
  }).finally(() => { done = true; }));
  const first = await observerContains('NATIVE_FIRST_FRAME');
  check('真实输出首帧在任务完成前显示', !done);
  check('观察窗自动出现且只读', first.observer.visible && first.observer.text_pattern && first.observer.read_only === true);
  check('自动出现没有改变前台窗口', first.foreground_handle === before.foreground_handle,
    { before: foregroundFacts(before), after: foregroundFacts(first) });
  check('分片中文与 emoji 正确显示', first.observer.text.includes('中文正常🔎') && !first.observer.text.includes('\ufffd'));
  check('合成密码未进入观察窗', !first.observer.text.includes('SYNTHETIC_HIDDEN') && /REDACTED|已隐藏/i.test(first.observer.text));
  await ui('Invoke', 205);
  const hidden = await ui();
  check('用户关闭观察窗未终止任务', !hidden.observer?.visible && !done);
  check('分批输出真实执行成功', succeeded(await streaming));
  const after = await ui();
  check('同一任务后续输出不会强制重新弹窗', !after.observer?.visible);
  await ui('Invoke', 307);
  await ui('Invoke', 337);
  await ui('Invoke', 310);
  const completed = await observerContains('NATIVE_LAST_FRAME');
  check('超过128行后的末尾仍持续呈现', completed.observer.text.includes('NATIVE_ROW_300'));
  check('stderr仍显示且不冒充失败终态', completed.observer.text.includes('NATIVE_STDERR_FRAME') && /Succeeded|成功|退出码 0/.test(completed.observer.text));
  await ui('Invoke', 205);
  check('真实普通 CMD 命令成功', succeeded(await tool('run_command', { shell: 'cmd', command: 'echo CMD_%NATIVE_E2E_SUFFIX%' })));
  await observerContains('CMD_VISIBLE');
  check('新任务重新自动打开共用观察窗', (await ui()).observer?.visible === true);
  const shell = await tool('open_shell', { shell: 'power_shell' });
  check('持久 Shell 已打开', typeof shell?.shell_id === 'string');
  check('持久 PowerShell 命令成功', succeeded(await tool('run_command', { shell_id: shell.shell_id,
    command: `[Console]::Out.WriteLine(('PERSISTENT_'+'FRAME'))` })));
  const persistent = await observerContains('PERSISTENT_FRAME');
  const requests = new Set([...persistent.observer.text.matchAll(/^\[[^\]]+\]\[[^\]]+\]\[([0-9a-f]{8})\]/gm)].map(match => match[1]));
  check('不同请求保留各自来源与任务编号', requests.size >= 3);
  check('全部观察记录持续隐藏合成密码', !persistent.observer.text.includes('SYNTHETIC_HIDDEN'));
  await tool('close_shell', { shell_id: shell.shell_id });
  await ui('Invoke', 308);
  current = '日志页打开时万行普通命令实际完成';
  const logsCommand = tool('run_command', { shell: 'windows_power_shell', command:
    `1..10000 | ForEach-Object {[Console]::Out.WriteLine(('MAIN_LOG_ROW_'+$_))}; [Console]::Out.WriteLine(('MAIN_LOG_LAST_'+'FRAME'))` });
  const logsResult = await logsCommand;
  if (!succeeded(logsResult)) throw classifiedError('logs_command_failed', {
    status: ['completed', 'running', 'pending', 'failed', 'rejected', 'cancelled'].includes(logsResult?.status) ? logsResult.status : 'other',
    exit_code: typeof logsResult?.exit_code === 'number' ? logsResult.exit_code : null,
    is_error: logsResult?.isError === true,
    rpc_error_code: typeof logsResult?.rpc_error_code === 'number' ? logsResult.rpc_error_code : null,
    categories: logsResult?.diagnostic_categories ?? classifyDiagnosticText(JSON.stringify(logsResult)),
  });
  check('日志页打开时万行普通命令实际完成', true);
  const logsDone = await waitUi('主窗口日志页显示万行末尾', value => controlText(value, 311).includes('MAIN_LOG_LAST_FRAME'), 20000);
  const body = logsDone.main.controls.find(row => row.id === 311);
  check('主窗口日志正文真实只读', body?.value_read_only === true);
  check('主窗口日志页持续更新到最后一行', controlText(logsDone, 311).includes('MAIN_LOG_ROW_10000'));
  await delay(300);
  await ui();
  await ui('Invoke', 310);
  const returned = await ui();
  check('日志实际显示后返回主窗未读清零', returned.main.controls.some(row => row.id === 308)
    && !/[·•]\s*\d+/.test(controlText(returned, 308)));
  await observerContains('MAIN_LOG_LAST_FRAME', 20000);
  check('万行突发后观察窗实际追到末尾', (await ui()).observer?.text?.includes('MAIN_LOG_ROW_10000') === true);

  if (options['clipboard-test']) {
    await fs.access(options['clipboard-test']);
    const clipboardDirectory = path.join(fixture, 'clipboard-attached');
    clipboardReportDirectory = clipboardDirectory;
    const firstMarker = 'CLIPBOARD_PARALLEL_FIRST';
    const lastMarker = 'CLIPBOARD_PARALLEL_LAST';
    check('复制并行真实输出首帧成功', succeeded(await tool('run_command', { shell: 'windows_power_shell',
      command: "[Console]::Out.WriteLine(('CLIPBOARD_PARALLEL_'+'FIRST'))" })));
    await observerContains(firstMarker);
    const clipboard = start('pwsh.exe', ['-NoProfile', '-STA', '-File', options['clipboard-test'],
      '-ProcessId', String(agent.child.pid), '-ProcessStartUtc', processStart, '-ExpectedHash', expectedHash,
      '-OutputDirectory', clipboardDirectory, '-FirstMarker', firstMarker, '-LastMarker', lastMarker], cleanEnv);
    let stopOutput = false;
    // 每个真实命令有界完成，避免为测试延长产品操作超时；整个复制压力期间持续产生真实输出。
    const producer = (async () => {
      let sequence = 0;
      while (!stopOutput && !clipboard.ended && sequence < 180) {
        const result = await tool('run_command', { shell: 'windows_power_shell', command:
          `1..10 | ForEach-Object {[Console]::Out.WriteLine(('CLIPBOARD_PARALLEL_ROW_' + ${sequence} + '_' + $_)); Start-Sleep -Milliseconds 50}` });
        if (!succeeded(result)) throw classifiedError('clipboard_parallel_command_failed');
        sequence++;
      }
    })().then(() => ({ passed: true }), () => ({ passed: false }));
    try {
      await waitFor('复制占用用例完成且输出仍在推进', async () => {
        if (clipboard.ended) throw classifiedError('clipboard_fixture_ended_early');
        return fs.access(path.join(clipboardDirectory, 'copies-complete')).then(() => true, () => false);
      }, 90000);
    } finally { stopOutput = true; }
    check('复制压力期间真实命令未中断', (await producer).passed);
    check('复制并行真实输出末帧成功', succeeded(await tool('run_command', { shell: 'windows_power_shell',
      command: "[Console]::Out.WriteLine(('CLIPBOARD_PARALLEL_'+'LAST'))" })));
    await Promise.race([clipboard.closed, delay(30000)]);
    let clipboardReport;
    try { clipboardReport = JSON.parse(await fs.readFile(path.join(clipboardDirectory, 'report.json'), 'utf8')); }
    catch { throw classifiedError('clipboard_report_invalid'); }
    clipboardEvidence = await clipboardDiagnostic();
    check('真实复制占用与输出并行全部通过', clipboard.ended && clipboard.code === 0
      && clipboardReport.executable_sha256 === expectedHash && clipboardReport.passed === true && clipboardReport.checks.every(item => item.passed === true));
    await observerContains(lastMarker);
  }

  check('默认权限未被测试临时授权保存', JSON.parse(await fs.readFile(config, 'utf8')).default_full_control === false);
  await tool('close_connection');
  await delay(1500);
  await ui('Close', 0, 'Confirm');
  await waitFor('断开测试连接后正常退出', () => agent.ended, 8000);
  check('原生退出码正常且后台已关闭', agent.code === 0);
  const report = JSON.parse(await fs.readFile(applicationReport, 'utf8'));
  check('真实链路单批刷新不超过100毫秒', report.metrics.max_tick_ms <= 100);
  check('真实观察文本在退出前已完整追赶模型', report.metrics.pending_units === 0 && report.metrics.view_caught_up === true
    && report.metrics.rendered_text_matches_model_on_close === true
    && report.metrics.rendered_total_lines === report.metrics.received_lines);
  check('三类普通命令复用同一观察窗口', persistent.observer.handle === first.observer.handle);
} catch (error) {
  failure = { step: current, category: error.category ?? 'test_or_fixture_failure',
    details: error.safeDetails ?? { code: typeof error.code === 'string' && /^E[A-Z]+$/.test(error.code) ? error.code : null } };
  failure.application = await applicationDiagnostic();
  failure.ui = uiDiagnostics;
  failure.native = { ended: agent?.ended ?? null, exit_code: agent?.code ?? null, error_categories: classifyDiagnosticText(agent?.stderr) };
  failure.relay = { ended: relayProcess?.ended ?? null, exit_code: relayProcess?.code ?? null, error_categories: classifyDiagnosticText(`${relayProcess?.stdout ?? ''}\n${relayProcess?.stderr ?? ''}`) };
  if (!results.some(item => item.test === current && item.status === 'failed')) results.push({ test: current, status: 'failed' });
  console.error(`FAIL ${current} [${failure.category}]；原始输出、控制码和凭据不写入结果。`);
  process.exitCode = 1;
} finally {
  const application = await applicationDiagnostic();
  for (const state of processes.reverse()) {
    if (!state.ended) state.child.kill();
    await Promise.race([state.closed, delay(3000)]);
  }
  await Promise.allSettled([...pendingTools]);
  clipboardEvidence = await clipboardDiagnostic();
  if (fixture) {
    if (path.dirname(path.resolve(fixture)) !== path.resolve(os.tmpdir()) || !path.basename(fixture).startsWith('remoteops-native-ui-e2e-')) throw new Error('拒绝清理非本轮临时目录');
    await fs.rm(fixture, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  }
  await fs.mkdir(path.dirname(options.output), { recursive: true });
  await fs.writeFile(options.output, `${JSON.stringify({ platform: 'windows', scenario: options.scenario, tests: results, failure, application, clipboard: clipboardEvidence }, null, 2)}\n`);
  console.log(`验收记录：${options.output}`);
}
