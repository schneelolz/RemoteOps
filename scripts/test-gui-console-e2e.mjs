#!/usr/bin/env node
// Windows 回环验收：所有原始输出只留内存，持久结果只含检查项与初始化阶段。
import { spawn } from 'node:child_process';
import { randomBytes, randomUUID } from 'node:crypto';
import fs from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const options = {
  agent: path.join(root, 'target/release/remoteops-agent-gui.exe'),
  relay: path.join(root, 'target/release/remoteops-relay.exe'),
  mcp: path.join(root, 'target/release/remoteops-controller-mcp.exe'),
  output: path.join(root, 'artifacts/gui-console-e2e', `${Date.now()}-${randomUUID()}.json`),
};
for (let index = 2; index < process.argv.length; index += 2) {
  const key = process.argv[index].replace(/^--/, '');
  if (!(key in options) || !process.argv[index + 1]) {
    throw new Error('参数：--agent EXE --relay EXE --mcp EXE [--output JSON]');
  }
  options[key] = path.resolve(process.argv[index + 1]);
}
if (process.platform !== 'win32') throw new Error('本脚本仅用于 Windows 原生验收。');

const processes = [];
const results = [];
let current = '初始化';
let fixture;

function check(name, passed) {
  current = name;
  results.push({ test: name, status: passed ? 'passed' : 'failed' });
  if (!passed) throw new Error(name);
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
          this.send({ jsonrpc: '2.0', id: message.id, result: message.method === 'elicitation/create' ? { action: 'decline' } : {} });
        } else if (this.pending.has(message.id)) {
          this.pending.get(message.id)(message);
        }
      }
    });
  }

  send(message) { this.state.child.stdin.write(`${JSON.stringify(message)}\n`); }

  async request(method, params) {
    const id = ++this.sequence;
    let timer;
    try {
      const message = await new Promise((resolve, reject) => {
        this.pending.set(id, resolve);
        timer = setTimeout(() => reject(new Error('MCP 请求超时')), 30000);
        this.send({ jsonrpc: '2.0', id, method, params });
      });
      return message.error ? { isError: true } : message.result;
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

function codeFrom(state) {
  const match = state.stdout.match(/控制码[：:]\s*([0-9][0-9 -]{7,14}[0-9])/);
  const code = match?.[1].replace(/\D/g, '');
  return code?.length === 9 ? code : undefined;
}

const succeeded = result => !result?.isError && result?.status === 'completed'
  && (result.exit_code === 0 || result.exit_code == null);

try {
  for (const key of ['agent', 'relay', 'mcp']) await fs.access(options[key]);
  fixture = await fs.mkdtemp(path.join(os.tmpdir(), 'remoteops-gui-console-e2e-'));
  const agentExe = path.join(fixture, 'agent.exe');
  await fs.copyFile(options.agent, agentExe);
  const cleanEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^(REMOTEOPS_|JLAI_)/i.test(key)));
  const token = randomBytes(32).toString('hex');
  const env = {
    ...cleanEnv, REMOTEOPS_AI_CONTROLLER_TOKEN: token,
    REMOTEOPS_HUMAN_CONTROLLER_TOKEN: randomBytes(32).toString('hex'),
    REMOTEOPS_CONTROLLER_TOKEN: token, REMOTEOPS_CONTROLLER_OWNER_ID: randomUUID(),
  };
  const relayPort = await unusedPort();
  let healthPort = await unusedPort();
  while (healthPort === relayPort) healthPort = await unusedPort();
  const address = `127.0.0.1:${relayPort}`;
  const cert = path.join(fixture, 'cert.pem');
  const relay = start(options.relay, [
    '--bind', address, '--health-bind', `127.0.0.1:${healthPort}`,
    '--tls-cert', cert, '--tls-key', path.join(fixture, 'key.pem'),
    '--tls-sans', '127.0.0.1,localhost', '--state-file', path.join(fixture, 'relay.json'),
    '--heartbeat-seconds', '1', '--lease-seconds', '60',
  ], env);
  await waitFor('临时 Relay 启动', async () => {
    if (relay.ended) throw new Error('临时 Relay 提前退出');
    try { await fs.access(cert); } catch { return false; }
    return new Promise(resolve => {
      const socket = net.connect({ host: '127.0.0.1', port: relayPort });
      socket.setTimeout(200);
      socket.once('connect', () => { socket.destroy(); resolve(true); });
      socket.once('error', () => resolve(false));
      socket.once('timeout', () => { socket.destroy(); resolve(false); });
    });
  });
  const config = path.join(fixture, 'agent-config.json');
  await fs.writeFile(config, JSON.stringify({
    relay: address, server_name: 'localhost', ca_cert: cert,
    state_file: path.join(fixture, 'agent-state.json'), transfer_root: path.join(fixture, 'agent-files'),
    retry_seconds: 1, default_full_control: false,
  }));
  const agentEnv = { ...cleanEnv, REMOTEOPS_VISUAL_PROVIDER_ENABLED: 'true' };
  const agent = start(agentExe, ['--cli', '--config', config], agentEnv);
  const pairingCode = await waitFor('控制台签发控制码', () => {
    if (agent.ended) throw new Error('控制台提前退出');
    return codeFrom(agent);
  });
  check('控制台连接并显示控制码', Boolean(pairingCode));
  await fs.mkdir(path.join(fixture, 'mcp-files'));
  const mcpConfig = path.join(fixture, 'mcp-config.json');
  await fs.writeFile(mcpConfig, '{}');
  const mcp = new Mcp(start(options.mcp, [
    '--config', mcpConfig,
    '--relay', address, '--server-name', 'localhost', '--ca-cert', cert,
    '--audit-log', path.join(fixture, 'audit.jsonl'), '--transfer-root', path.join(fixture, 'mcp-files'),
    '--reconnect-seconds', '1',
  ], env));
  await mcp.request('initialize', {
    protocolVersion: '2025-03-26', capabilities: { elicitation: { form: {} } },
    clientInfo: { name: 'gui-console-e2e', version: '1' },
  });
  mcp.send({ jsonrpc: '2.0', method: 'notifications/initialized' });
  const paired = await mcp.tool('pair_connection', { pairing_code: pairingCode });
  const sid = paired?.session_id;
  check('MCP 成功配对', typeof sid === 'string');
  const tool = (name, args = {}) => mcp.tool(name, { session_id: sid, ...args });
  const info = await tool('get_target_info');
  check('目标报告 Console 模式', info?.target_context?.run_mode === 'console');
  check('环境变量无法开启内建图形能力', !info?.capabilities?.some(value => /visual|screenshot|desktop/i.test(value))
    && info?.target_context?.capabilities?.some(value => value.capability === 'visual' && value.status === 'unavailable'));
  check('CMD 只读命令成功', succeeded(await tool('run_readonly_command', { shell: 'cmd', command: 'ver' })));
  check('Windows PowerShell 只读命令成功', succeeded(await tool('run_readonly_command', { shell: 'windows_power_shell', command: 'Get-Date' })));
  await waitFor('初始逐项确认同步', async () => {
    const mode = await tool('get_control_mode');
    return mode?.sync_status === 'synced' && mode.mode === 'step_by_step';
  });
  agent.child.stdin.write('status\n');
  await waitFor('现场绑定就绪', () => /会话 .* 有效权限[：:]/.test(agent.stdout));
  agent.child.stdin.write('full\n');
  await waitFor('现场完全控制获 Relay 确认', async () => {
    const mode = await tool('get_control_mode');
    return mode?.sync_status === 'synced' && mode.mode === 'full_access' && mode.source === 'local';
  });
  await waitFor('控制台收到授权确认', () => agent.stdout.includes('Relay 已确认当前会话权限：FullAccess'));
  check('full 通过 Relay 授予现场完全控制', true);
  const visual = await tool('observe_window', { include_screenshot: true, include_ui_tree: true });
  check('完全控制仍拒绝图形请求且不初始化 Provider', visual?.status === 'unavailable'
    && visual?.details?.initialized === false && visual?.details?.observation === null);
  agent.child.stdin.write('confirm\n');
  await waitFor('撤销现场完全控制', async () => {
    const mode = await tool('get_control_mode');
    return mode?.sync_status === 'synced' && mode.mode === 'step_by_step';
  });
  check('confirm 恢复逐项确认', true);
  check('临时授权不保存默认偏好', JSON.parse(await fs.readFile(config, 'utf8')).default_full_control === false);
  agent.child.stdin.write('quit\n');
  await waitFor('quit 正常退出', () => agent.ended, 10000);
  check('quit 正常结束进程', agent.code === 0);
  const eofAgent = start(agentExe, ['--cli', '--config', config], agentEnv);
  await waitFor('EOF 用例启动', () => codeFrom(eofAgent));
  eofAgent.child.stdin.end();
  await waitFor('EOF 由控制台处理', () => eofAgent.stdout.includes('控制台输入已关闭'));
  await delay(1000);
  check('stdin EOF 后保持连接和进程', !eofAgent.ended);
  const logsDirectory = path.join(fixture, 'logs');
  const stages = [];
  for (const file of await fs.readdir(logsDirectory)) {
    if (!/^agent-gui-runtime-.*\.log$/.test(file)) continue;
    const text = await fs.readFile(path.join(logsDirectory, file), 'utf8');
    for (const match of text.matchAll(/^stage: ([a-z_]+)\r?$/gm)) stages.push(match[1]);
  }
  check('诊断阶段未进入窗口或图形线程', stages.includes('console_mode_selected')
    && !stages.includes('ui_thread_started') && !stages.includes('starting_native_window'));
  results.push({ test: '初始化阶段', status: 'passed', stages: [...new Set(stages)] });
} catch {
  if (!results.some(item => item.test === current && item.status === 'failed')) {
    results.push({ test: current, status: 'failed' });
  }
  console.error(`FAIL ${current}；原始输出和凭据未持久化。`);
  process.exitCode = 1;
} finally {
  for (const state of processes.reverse()) {
    if (!state.ended) state.child.kill();
    await Promise.race([state.closed, delay(3000)]);
  }
  // fixture 只来自本进程 mkdtemp；不接受外部删除路径，也不保留身份、密钥和审计载荷。
  if (fixture) {
    const expectedParent = path.resolve(os.tmpdir());
    if (path.dirname(path.resolve(fixture)) !== expectedParent
      || !path.basename(fixture).startsWith('remoteops-gui-console-e2e-')) {
      throw new Error('拒绝清理非本次测试目录');
    }
    await fs.rm(fixture, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  }
  await fs.mkdir(path.dirname(options.output), { recursive: true });
  await fs.writeFile(options.output, `${JSON.stringify({ platform: 'windows', tests: results }, null, 2)}\n`);
  console.log(`验收记录：${options.output}`);
}
