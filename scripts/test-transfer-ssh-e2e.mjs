#!/usr/bin/env node
// 本机隔离的真实 Agent/Relay/MCP 传输与 SSH 验收；机密与原始输出只留内存。
import { spawn } from 'node:child_process';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { EventEmitter } from 'node:events';
import fs from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import assert from 'node:assert/strict';
import { isDeepStrictEqual } from 'node:util';

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const plinkHash = '50479953865b30775056441b10fdcb984126ba4f98af4f64756902a807b453e7';
const csc = 'C:\\Windows\\Microsoft.NET\\Framework64\\v4.0.30319\\csc.exe';
const ps = value => `'${String(value).replaceAll("'", "''")}'`;
const samePath = (left, right) => typeof left === 'string' && typeof right === 'string'
  && path.resolve(left.replace(/^\\\\\?\\/, '')).toLowerCase()
  === path.resolve(right.replace(/^\\\\\?\\/, '')).toLowerCase();
const succeeded = value => value?.isError !== true && value?.status === 'completed'
  && (value.exit_code === 0 || value.exit_code == null);
const fingerprint = metadata => `${metadata.algorithm} SHA256:${createHash('sha256')
  .update(Buffer.from(metadata.public_key, 'base64')).digest('base64').replace(/=+$/, '')}`;
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

function fail(category) {
  const error = new Error(category);
  error.category = category;
  return error;
}

class Mcp {
  constructor(state, requestTimeout = 60000) {
    this.state = state;
    this.pending = new Map();
    this.sequence = 0;
    this.buffer = '';
    this.requestTimeout = requestTimeout;
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
          // 只确认自己生成的回环 fixture 操作，不能用于现场或任意目标。
          this.send({ jsonrpc: '2.0', id: message.id, result: message.method === 'elicitation/create'
            ? { action: 'accept', content: { confirmed: true } } : {} });
        } else if (this.pending.has(message.id)) {
          this.pending.get(message.id).resolve(message);
        }
      }
    });
    state.child.once?.('close', () => {
      for (const pending of this.pending.values()) pending.reject(fail('mcp_transport_closed'));
      this.pending.clear();
    });
  }

  send(message) { this.state.child.stdin.write(`${JSON.stringify(message)}\n`); }

  async request(method, params) {
    if (this.state.ended) throw fail('mcp_transport_closed');
    const id = ++this.sequence;
    let timer;
    try {
      const message = await new Promise((resolve, reject) => {
        this.pending.set(id, { resolve, reject });
        timer = setTimeout(() => reject(fail('mcp_request_timeout')), this.requestTimeout);
        this.send({ jsonrpc: '2.0', id, method, params });
      });
      // 不把外部错误字符串、命令正文或上下文机密带入报告。
      return message.error ? { isError: true, rpc_error_code: message.error.code } : message.result;
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
        try { return JSON.parse(item.text); } catch { /* 非结构化输出不能作为成功回执。 */ }
      }
    }
    return result;
  }
}

function parseOptions() {
  const options = { agent: '', relay: '', mcp: '', python: '', native: '', askpass: '', scanner: '',
    plink: 'D:\\Tools\\putty\\PLINK.EXE', output: '' };
  for (let index = 2; index < process.argv.length; index += 2) {
    const key = process.argv[index].replace(/^--/, '');
    if (!(key in options) || !process.argv[index + 1]) throw fail('invalid_arguments');
    options[key] = path.resolve(process.argv[index + 1]);
  }
  if (!['agent', 'relay', 'mcp', 'python', 'output'].every(key => options[key])) throw fail('missing_arguments');
  if (!options.askpass) options.askpass = path.join(path.dirname(options.agent), 'remoteops-ssh-askpass.exe');
  return options;
}

async function main(options) {
  const states = [];
  const checks = [];
  const numbers = {};
  const binaries = {};
  let fixture;
  let failure = null;
  let current = 'initialization';
  let mcp;
  let sid;
  let server;
  let cleanEnv;
  let reportApproved = false;
  let secret = randomBytes(32).toString('base64url');
  const syntheticSecrets = [secret];
  const tempParent = path.resolve(os.tmpdir());
  const output = path.resolve(options.output);

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
      const result = await predicate();
      if (result) return result;
      await delay(75);
    }
    throw fail('fixture_wait_timeout');
  }

  function start(executable, args, env, cwd = fixture) {
    const child = spawn(executable, args, { cwd, env, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
    const state = { child, stdout: '', stderr: '', ended: false, code: null };
    state.closed = new Promise(resolve => {
      child.once('error', () => { state.ended = true; resolve(); });
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

  async function command(executable, args, env = cleanEnv, timeout = 30000) {
    const state = start(executable, args, env);
    await Promise.race([state.closed, delay(timeout)]);
    if (!state.ended) throw fail('fixture_process_timeout');
    if (state.code !== 0) throw fail('fixture_process_failed');
    return state;
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

  try {
    if (process.platform !== 'win32' || path.parse(tempParent).root.toLowerCase() !== 'c:\\') throw fail('windows_c_temp_required');
    const relativeOutput = path.relative(tempParent, output);
    if (!relativeOutput || relativeOutput.startsWith('..') || path.isAbsolute(relativeOutput)) throw fail('output_must_be_c_temp');
    if (await fs.stat(output).then(() => true, () => false)) throw fail('report_already_exists');
    reportApproved = true;
    for (const key of ['agent', 'mcp', 'relay', 'python', 'askpass', 'plink']) await fs.access(options[key]);
    if (options.native) await fs.access(options.native);
    for (const key of ['agent', 'mcp', 'relay', 'askpass', 'plink', ...(options.native ? ['native'] : [])]) {
      binaries[key] = sha256(await fs.readFile(options[key]));
    }
    check('fixed_plink_source_hash', sha256(await fs.readFile(options.plink)) === plinkHash);
    fixture = await fs.mkdtemp(path.join(tempParent, 'remoteops-transfer-ssh-e2e-'));
    cleanEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^(REMOTEOPS_|JLAI_)/i.test(key)));
    // 新建目录中还没有秘密：先建立并实读核验当前用户专用继承 ACL，再写配置和密码。
    const privacy = await command('pwsh.exe', ['-NoProfile', '-Command',
      `$p=${ps(fixture)}; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; `
      + '$acl=[Security.AccessControl.DirectorySecurity]::new(); $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true,$false); '
      + '$rule=[Security.AccessControl.FileSystemAccessRule]::new($sid,"FullControl","ContainerInherit,ObjectInherit","None","Allow"); '
      + '$acl.AddAccessRule($rule); Set-Acl -LiteralPath $p -AclObject $acl; $actual=Get-Acl -LiteralPath $p; '
      + 'if(-not $actual.AreAccessRulesProtected -or @($actual.Access).Count -ne 1 -or '
      + '$actual.Access[0].IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value -cne $sid.Value){exit 7}; '
      + '[Console]::Out.Write("PRIVATE_OK")']);
    check('private_fixture_acl_before_secrets', privacy.stdout === 'PRIVATE_OK');
    const directories = {};
    for (const name of ['agent-package 中文', 'mcp-package', 'agent-local', 'mcp-local', 'relay-local', 'server-local',
      'agent-temp', 'mcp-temp', 'relay-temp', 'server-temp', 'agent-transfer 中文', 'mcp-transfer 中文', 'secrets']) {
      directories[name] = path.join(fixture, name);
      await fs.mkdir(directories[name]);
    }
    const envFor = role => ({ ...cleanEnv,
      LOCALAPPDATA: directories[`${role}-local`], APPDATA: directories[`${role}-local`],
      TEMP: directories[`${role}-temp`], TMP: directories[`${role}-temp`] });
    const passwordFile = path.join(directories.secrets, 'synthetic-prompt-value');
    await fs.writeFile(passwordFile, secret, { flag: 'wx' });
    const promptCount = path.join(directories.secrets, 'prompt-count');
    const serverState = start(options.python, [path.join(repository, 'scripts/test-ssh-plink-e2e.py'), 'serve'], envFor('server'));
    const publicReplies = [];
    let serverBuffer = '';
    serverState.child.stdout.on('data', chunk => {
      serverBuffer += chunk;
      let newline;
      while ((newline = serverBuffer.indexOf('\n')) >= 0) {
        const line = serverBuffer.slice(0, newline);
        serverBuffer = serverBuffer.slice(newline + 1);
        try { publicReplies.push(JSON.parse(line)); } catch { /* 由就绪超时报告协议失败。 */ }
      }
    });
    serverState.child.stdin.write(`${JSON.stringify({ password: secret })}\n`);
    server = {
      state: serverState,
      async next(event) {
        return until('ssh_fixture_public_reply', () => {
          if (serverState.ended) throw fail('ssh_fixture_ended');
          const index = publicReplies.findIndex(reply => reply.event === event);
          return index < 0 ? false : publicReplies.splice(index, 1)[0];
        }, 10000);
      },
      async request(action) {
        serverState.child.stdin.write(`${JSON.stringify({ action })}\n`);
        return this.next(action === 'rotate_key' ? 'rotated' : 'stats');
      },
    };
    let ready = await server.next('ready');
    check('ssh_fixture_only_loopback_and_derived_fingerprint', ready.host === '127.0.0.1'
      && Number.isInteger(ready.port) && ready.port > 0 && ready.username === 'remoteops-fixture'
      && ready.paramiko_version === '4.0.0' && fingerprint(ready) === `${ready.algorithm} ${ready.fingerprint}`);
    const agentExe = path.join(directories['agent-package 中文'], 'agent-cli.exe');
    const mcpExe = path.join(directories['mcp-package'], 'remoteops-controller-mcp.exe');
    await fs.copyFile(options.agent, agentExe);
    await fs.copyFile(options.mcp, mcpExe);
    await command('pwsh.exe', ['-NoProfile', '-File', path.join(repository, 'scripts/Copy-BundledPlink.ps1'),
      '-SourcePath', options.plink, '-DestinationDirectory', directories['agent-package 中文'], '-AskpassPath', options.askpass]);
    if (options.scanner) {
      const scannerDestination = path.join(directories['agent-package 中文'], 'tools/ssh-scanner');
      await fs.mkdir(scannerDestination);
      for (const name of ['ssh-keyscan.exe', 'libcrypto.dll', 'LICENSE.txt', 'NOTICE.txt', 'scanner-manifest.json']) {
        await fs.copyFile(path.join(options.scanner, name), path.join(scannerDestination, name));
      }
      check('actual_packaged_scanner_official_manifest', JSON.parse(await fs.readFile(path.join(scannerDestination, 'scanner-manifest.json'), 'utf8')).archive_sha256
        === 'b67ff8becd203502190ec42ac7787c24c2cd3325ed7700485fe7a1c83d2a7957');
    }
    check('actual_packaged_plink_hash', sha256(await fs.readFile(path.join(directories['agent-package 中文'], 'tools/plink.exe'))) === plinkHash);
    const helperSource = path.join(directories['mcp-package'], 'FixturePrompt.cs');
    await fs.writeFile(helperSource, `using System; using System.IO; using System.Text; using System.Collections.Generic; using System.Web.Script.Serialization;
class FixturePrompt { static int Main(string[] a) { try {
 if(a.Length!=10 || a[0]!="--host" || a[1]!="127.0.0.1" || a[2]!="--port" || a[3]!=Environment.GetEnvironmentVariable("REMOTEOPS_TEST_SSH_PORT") || a[4]!="--username" || a[5]!="remoteops-fixture" || a[6]!="--command-sha256" || !System.Text.RegularExpressions.Regex.IsMatch(a[7],"^[0-9a-f]{64}$")||a[8]!="--language"||(a[9]!="zh-CN"&&a[9]!="en-US")) return 3;
 string p=Environment.GetEnvironmentVariable("REMOTEOPS_TEST_PASSWORD_FILE"); string n=Environment.GetEnvironmentVariable("REMOTEOPS_TEST_PROMPT_COUNT_FILE");
 string value=File.ReadAllText(p,new UTF8Encoding(false,true)); int count=File.Exists(n)?Int32.Parse(File.ReadAllText(n)):0; File.WriteAllText(n,(count+1).ToString());
 Console.OutputEncoding=new UTF8Encoding(false); Console.Write(new JavaScriptSerializer().Serialize(new Dictionary<string,object>{{"action","use_once"},{"password",value}})); return 0;
 } catch { return 2; } } }
`, { flag: 'wx' });
    await command(csc, ['/nologo', '/target:exe', '/r:System.Web.Extensions.dll',
      `/out:${path.join(directories['mcp-package'], 'remoteops-credential-prompt.exe')}`, helperSource]);
    check('isolated_fixture_prompt_compiled', true);
    const token = randomBytes(32).toString('hex');
    const owner = randomUUID();
    const tokenEnv = { REMOTEOPS_AI_CONTROLLER_TOKEN: token, REMOTEOPS_CONTROLLER_TOKEN: token,
      REMOTEOPS_HUMAN_CONTROLLER_TOKEN: randomBytes(32).toString('hex'), REMOTEOPS_CONTROLLER_OWNER_ID: owner };
    const relayPort = await freePort();
    let healthPort = await freePort();
    while (healthPort === relayPort) healthPort = await freePort();
    const address = `127.0.0.1:${relayPort}`;
    const cert = path.join(fixture, 'relay-cert.pem');
    const relay = start(options.relay, ['--bind', address, '--health-bind', `127.0.0.1:${healthPort}`,
      '--tls-cert', cert, '--tls-key', path.join(fixture, 'relay-key.pem'), '--tls-sans', '127.0.0.1,localhost',
      '--state-file', path.join(fixture, 'relay-state.json'), '--heartbeat-seconds', '1', '--lease-seconds', '60'],
    { ...envFor('relay'), ...tokenEnv });
    await until('relay_ready', async () => {
      if (relay.ended) throw fail('relay_ended');
      return fs.stat(cert).then(() => true, () => false);
    });
    const agentRoot = directories['agent-transfer 中文'];
    const localRoot = directories['mcp-transfer 中文'];
    const agentConfig = path.join(directories['agent-local'], 'agent-config.json');
    await fs.writeFile(agentConfig, JSON.stringify({ relay: address, server_name: 'localhost', ca_cert: cert,
      state_file: path.join(directories['agent-local'], 'agent-state.json'), transfer_root: agentRoot,
      retry_seconds: 1, default_full_control: true }), { flag: 'wx' });
    const agent = start(agentExe, ['--config', agentConfig], envFor('agent'));
    const pairing = await until('regular_agent_pairing_ready', () => {
      if (agent.ended) throw fail('agent_ended');
      return /控制码[：:]\s*(\d[\d -]{7,14}\d)/u.exec(agent.stdout)?.[1]?.replace(/\D/g, '');
    }, 60000);
    check('pairing_code_only_in_memory', /^\d{9}$/.test(pairing));
    const mcpConfig = path.join(directories['mcp-local'], 'mcp-config.json');
    await fs.writeFile(mcpConfig, '{}', { flag: 'wx' });
    const mcpState = start(mcpExe, ['--config', mcpConfig, '--relay', address, '--server-name', 'localhost',
      '--ca-cert', cert, '--audit-log', path.join(directories['mcp-local'], 'audit.jsonl'),
      '--transfer-root', localRoot, '--reconnect-seconds', '1'], { ...envFor('mcp'), ...tokenEnv,
      REMOTEOPS_TEST_PASSWORD_FILE: passwordFile, REMOTEOPS_TEST_PROMPT_COUNT_FILE: promptCount,
      REMOTEOPS_TEST_SSH_PORT: String(ready.port) });
    mcp = new Mcp(mcpState);
    await mcp.request('initialize', { protocolVersion: '2025-03-26', capabilities: { elicitation: { form: {} } },
      clientInfo: { name: 'transfer-ssh-loopback-fixture', version: '1' } });
    mcp.send({ jsonrpc: '2.0', method: 'notifications/initialized' });
    sid = (await mcp.tool('pair_connection', { pairing_code: pairing }))?.session_id;
    check('paired_only_fixture_agent', typeof sid === 'string');
    const tool = (name, args = {}) => mcp.tool(name, { session_id: sid, ...args });
    await until('fixture_full_control_synced', async () => {
      const mode = await tool('get_control_mode');
      return mode?.sync_status === 'synced' && mode.mode === 'full_access';
    });
    const target = await tool('get_target_info');
    check('distinct_transfer_roots_reported_accurately', !samePath(agentRoot, localRoot)
      && samePath(target.transfer_root, localRoot) && samePath(target.local_transfer_root, localRoot)
      && samePath(target.agent_transfer_root, agentRoot) && target.agent_transfer_root_status === 'reported');
    check('actual_agent_advertises_ssh_trust_cli', samePath(target.ssh_trust_cli, agentExe));
    const run = async commandText => tool('run_command', { shell: 'windows_power_shell', command: commandText });
    const relative = "中文 空格/诊断'脚本.ps1";
    const scriptBytes = Buffer.from("\uFEFF[Console]::Out.WriteLine('TRANSFER_SCRIPT_OK')\r\n", 'utf8');
    await fs.mkdir(path.join(localRoot, '中文 空格'));
    await fs.writeFile(path.join(localRoot, relative), scriptBytes, { flag: 'wx' });
    await fs.writeFile(path.join(fixture, 'outside.ps1'), scriptBytes, { flag: 'wx' });
    const uploaded = await tool('upload_file', { local_path: relative, remote_path: relative, overwrite: false });
    const receipt = uploaded?.details;
    check('unicode_space_quote_upload_complete_receipt', succeeded(uploaded)
      && uploaded.sha256 === sha256(scriptBytes) && receipt?.sha256 === sha256(scriptBytes)
      && receipt.size === scriptBytes.length && receipt.remote_path === relative
      && samePath(receipt.agent_transfer_root, agentRoot) && samePath(receipt.local_transfer_root, localRoot)
      && samePath(receipt.remote_absolute_path, path.join(agentRoot, relative)) && receipt.agent_path_status === 'reported');
    check('uploaded_bytes_match_real_agent_file', sha256(await fs.readFile(path.join(agentRoot, relative))) === sha256(scriptBytes));
    const largeRelative = '中文 空格/分块工具.bin';
    const largeBytes = Buffer.alloc(2 * 1024 * 1024 + 17, 0x5a);
    await fs.writeFile(path.join(localRoot, largeRelative), largeBytes, { flag: 'wx' });
    const largeUpload = await tool('upload_file', { local_path: largeRelative, remote_path: largeRelative, overwrite: false });
    check('multi_chunk_upload_receipt_and_actual_bytes', succeeded(largeUpload)
      && largeUpload.sha256 === sha256(largeBytes) && largeUpload.details?.size === largeBytes.length
      && largeUpload.details?.chunk_size_bytes === 1024 * 1024
      && samePath(largeUpload.details?.remote_absolute_path, path.join(agentRoot, largeRelative))
      && sha256(await fs.readFile(path.join(agentRoot, largeRelative))) === sha256(largeBytes));
    check('upload_without_overwrite_preserves_existing_file', !succeeded(await tool('upload_file', {
      local_path: largeRelative, remote_path: largeRelative, overwrite: false }))
      && sha256(await fs.readFile(path.join(agentRoot, largeRelative))) === sha256(largeBytes));
    check('oneshot_shell_starts_at_agent_transfer_root', samePath((await run('[Console]::Out.WriteLine((Get-Location).Path)'))?.summary?.trim(), agentRoot));
    // 只为本轮已核对内容的 fixture 在当前子进程放行脚本；不改变用户或系统策略。
    const fixtureScriptPolicy = 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File ';
    const relativeRun = await run(`${fixtureScriptPolicy}${ps('.\\' + relative.replaceAll('/', '\\'))}`);
    numbers.relative_script_exit_code = relativeRun?.exit_code ?? null;
    numbers.relative_script_has_marker = relativeRun?.summary?.includes('TRANSFER_SCRIPT_OK') === true;
    numbers.relative_script_policy_error = /PSSecurityException|UnauthorizedAccess|running scripts|禁止运行脚本/i.test(relativeRun?.summary ?? '');
    numbers.relative_script_not_found = /CommandNotFoundException|ObjectNotFound|未能将|无法将/i.test(relativeRun?.summary ?? '');
    numbers.relative_script_summary_length = relativeRun?.summary?.length ?? null;
    check('relative_uploaded_script_executes_from_agent_root', succeeded(relativeRun) && relativeRun.summary.trim() === 'TRANSFER_SCRIPT_OK');
    const absoluteRun = await run(`${fixtureScriptPolicy}${ps(receipt.remote_absolute_path)}`);
    check('absolute_receipt_path_executes', succeeded(absoluteRun) && absoluteRun.summary.trim() === 'TRANSFER_SCRIPT_OK');
    const shell = await tool('open_shell', { shell: 'windows_power_shell' });
    check('persistent_shell_opened', typeof shell?.shell_id === 'string');
    const persistent = commandText => tool('run_command', { shell_id: shell.shell_id, command: commandText });
    const initial = await persistent('[Console]::Out.WriteLine((Get-Location).Path)');
    check('persistent_shell_initial_transfer_root', succeeded(initial) && samePath(initial.summary.trim(), agentRoot));
    await persistent(`Set-Location -LiteralPath ${ps(path.join(agentRoot, '中文 空格'))}`);
    const afterCd = await persistent('[Console]::Out.WriteLine((Get-Location).Path)');
    check('persistent_shell_retains_changed_directory', succeeded(afterCd) && samePath(afterCd.summary.trim(), path.join(agentRoot, '中文 空格')));
    await tool('close_shell', { shell_id: shell.shell_id });
    const localEscape = await tool('upload_file', { local_path: '../outside.ps1', remote_path: 'escape.ps1', overwrite: false });
    const remoteEscape = await tool('upload_file', { local_path: relative, remote_path: '../escape.ps1', overwrite: false });
    check('upload_traversal_rejected_both_sides', !succeeded(localEscape) && !succeeded(remoteEscape)
      && !(await fs.stat(path.join(fixture, 'escape.ps1')).then(() => true, () => false))
      && !(await fs.stat(path.join(agentRoot, 'escape.ps1')).then(() => true, () => false)));
    const sshArgs = commandText => ({ host: '127.0.0.1', port: ready.port, username: ready.username,
      command: commandText, readonly: false, use_password: true });
    const knownHostsPath = path.join(directories['agent-local'], 'RemoteOps', 'ssh-known-hosts');
    const unknown = await tool('run_ssh', sshArgs('fixture-stream'));
    const unknownStats = (await server.request('stats')).stats;
    check('unknown_host_password_fails_before_authentication', !succeeded(unknown)
      && unknownStats.auth_accepted === 0 && unknownStats.auth_rejected === 0
      && !(await fs.stat(knownHostsPath).then(() => true, () => false)));
    const guidance = unknown?.details?.ssh_host_key_management;
    check('failure_exposes_actual_cli_guidance_without_auto_trust', guidance?.automatic_trust === false
      && samePath(guidance.executable, target.ssh_trust_cli));
    const cli = async (action, pins) => {
      const argumentsText = [action, '127.0.0.1', String(ready.port), ...(pins ?? [])]
        .map(ps).join(' ');
      return run(`& ${ps(target.ssh_trust_cli)} ${argumentsText}`);
    };
    async function scanAndVerify(metadata) {
      const response = await cli('--ssh-key-scan');
      let scan;
      try { scan = JSON.parse(response.summary); } catch { throw fail('scan_cli_invalid_json'); }
      check('scan_public_fingerprint_matches_independent_fixture_key', succeeded(response) && scan.status === 'scanned_unverified'
        && scan.trusted === false && scan.host === '127.0.0.1' && scan.port === ready.port
        && scan.fingerprints.length === 1 && scan.fingerprints[0] === fingerprint(metadata));
      return scan.fingerprints;
    }
    let pins = await scanAndVerify(ready);
    // 仅此独立 fixture 的公钥由父 runner 从服务启动管道取得，作为可信带外来源。
    const trust = await cli('--ssh-key-trust', pins);
    const trustReceipt = JSON.parse(trust.summary);
    check('explicit_fixture_verified_key_trusted_via_real_cli', succeeded(trust) && trustReceipt.status === 'trusted'
      && samePath(trustReceipt.known_hosts_file, knownHostsPath));
    const firstPassword = secret;
    let afterSequence = 0;
    let streamedEvents = [];
    async function readEvents() {
      const value = await tool('read_output', { after_sequence: afterSequence, limit: 500 });
      for (const event of value?.events ?? []) {
        afterSequence = Math.max(afterSequence, event.sequence);
        streamedEvents.push(event);
      }
      return streamedEvents;
    }
    await readEvents();
    streamedEvents = [];
    let completed = false;
    const streaming = tool('run_ssh', sshArgs('fixture-stream')).then(value => {
      completed = true;
      return value;
    }, () => { completed = true; return { isError: true }; });
    await until('real_ssh_first_output_event', async () => (await readEvents()).some(event =>
      event.payload?.type === 'output_chunk' && !event.payload.stderr && event.payload.text.includes('FIXTURE-FIRST')));
    check('actual_runssh_first_frame_before_completion', !completed);
    const streamResult = await streaming;
    await readEvents();
    const chunks = streamedEvents.filter(event => event.request_id === streamResult.request_id && event.payload?.type === 'output_chunk');
    const sshPreparation = '[RemoteOps SSH 准备] 后端=Plink；已找到随包组件与精确固定密钥；模式=exec/无PTY，尚未确认连接。\n';
    const actualStdout = chunks.filter(event => !event.payload.stderr).map(event => event.payload.text).join('');
    const actualStderr = chunks.filter(event => event.payload.stderr).map(event => event.payload.text).join('');
    numbers.stream_succeeded = succeeded(streamResult);
    numbers.stream_exit_code = streamResult.exit_code ?? null;
    numbers.stream_stdout_length = actualStdout.length;
    numbers.stream_stderr_length = actualStderr.length;
    const executionMetadata = { ssh_execution: {
      backend: 'plink', error_category: null, execution_mode: 'exec_no_pty',
      selection_reason: 'bundled_plink_exact_pinned_keys',
    } };
    const metadataText = JSON.stringify(executionMetadata);
    // Agent 既有响应契约把 details 另发为一帧 JSON，不能混作远端 stdout 或忽略任意前缀。
    const metadataChunks = chunks.filter(event => !event.payload.stderr && event.payload.text === metadataText);
    const remoteChunks = chunks.filter(event => !metadataChunks.includes(event));
    numbers.stream_metadata_chunks = metadataChunks.length;
    check('actual_runssh_structured_metadata_event_matches_response', metadataChunks.length === 1
      && isDeepStrictEqual(streamResult.details?.ssh_execution, executionMetadata.ssh_execution)
      && chunks.at(-1) === metadataChunks[0]);
    check('actual_runssh_exact_unicode_stdout_and_stderr', succeeded(streamResult)
      && remoteChunks.filter(event => !event.payload.stderr).map(event => event.payload.text).join('') === sshPreparation + 'FIXTURE-FIRST 中文 😀\nFIXTURE-FINAL\n'
      && remoteChunks.filter(event => event.payload.stderr).map(event => event.payload.text).join('') === 'FIXTURE-STDERR\n');
    check('actual_runssh_backend_and_exec_mode_reported', streamResult.details?.ssh_execution?.backend === 'plink'
      && streamResult.details.ssh_execution.selection_reason === 'bundled_plink_exact_pinned_keys'
      && streamResult.details.ssh_execution.execution_mode === 'exec_no_pty'
      && streamResult.details.ssh_execution.error_category === null);
    for (const [query, expected] of [['hostname', 'fixture-linux\n'], ['whoami', 'remoteops-fixture\n'], ['uname -a', 'Linux fixture-linux 6.1 fixture x86_64 GNU/Linux\n']]) {
      const queried = await tool('run_ssh', { ...sshArgs(query), readonly: true });
      await readEvents();
      const queryChunks = streamedEvents.filter(event => event.request_id === queried.request_id && event.payload?.type === 'output_chunk');
      const expectedMetadata = JSON.stringify(executionMetadata);
      const queryMetadata = queryChunks.filter(event => !event.payload.stderr && event.payload.text === expectedMetadata);
      const queryStdout = queryChunks.filter(event => !event.payload.stderr && !queryMetadata.includes(event)).map(event => event.payload.text).join('');
      numbers[`linux_${query.split(' ')[0]}_succeeded`] = succeeded(queried);
      numbers[`linux_${query.split(' ')[0]}_exit_code`] = queried.exit_code ?? null;
      numbers[`linux_${query.split(' ')[0]}_stdout_exact`] = queryStdout === sshPreparation + expected;
      check(`linux_readonly_${query.replaceAll(' ', '_')}_actual_execution`, succeeded(queried)
        && queried.exit_code === 0 && queryStdout === sshPreparation + expected
        && queryMetadata.length === 1
        && queryChunks.every(event => !event.payload.stderr)
        && isDeepStrictEqual(queried.details?.ssh_execution, executionMetadata.ssh_execution));
    }
    const beforeInjection = (await server.request('stats')).stats.exec_accepted;
    for (const query of ['hostname\nwhoami', 'whoami;hostname', 'uname -a >file', '$(whoami)', 'Hostname', 'hostname extra']) {
      const refused = await tool('run_ssh', { ...sshArgs(query), readonly: true });
      check('ssh_readonly_unsafe_or_unlisted_query_refused', !succeeded(refused));
    }
    check('ssh_readonly_rejections_do_not_reach_exec', (await server.request('stats')).stats.exec_accepted === beforeInjection);
    const promptsBeforeWrong = Number(await fs.readFile(promptCount, 'utf8'));
    secret = randomBytes(32).toString('base64url');
    syntheticSecrets.push(secret);
    await fs.writeFile(passwordFile, secret);
    const wrong = await tool('run_ssh', sshArgs('fixture-stream'));
    check('wrong_password_rejected_with_fresh_use_once_prompt', !succeeded(wrong)
      && (await server.request('stats')).stats.auth_rejected > 0
      && Number(await fs.readFile(promptCount, 'utf8')) === promptsBeforeWrong + 1);
    await fs.writeFile(passwordFile, firstPassword);
    const previousPins = pins;
    ready = await server.request('rotate_key');
    const beforeChanged = (await server.request('stats')).stats.auth_accepted;
    const changed = await tool('run_ssh', sshArgs('fixture-stream'));
    check('changed_key_refused_before_authentication', !succeeded(changed)
      && (await server.request('stats')).stats.auth_accepted === beforeChanged);
    const beforeWrongTrust = sha256(await fs.readFile(knownHostsPath));
    const wrongTrust = await cli('--ssh-key-trust', previousPins);
    check('old_fingerprint_cannot_confirm_rotated_key', !succeeded(wrongTrust)
      && sha256(await fs.readFile(knownHostsPath)) === beforeWrongTrust);
    pins = await scanAndVerify(ready);
    check('new_fixture_verified_pin_explicitly_trusted', succeeded(await cli('--ssh-key-trust', pins)));
    const recovered = await tool('run_ssh', sshArgs('fixture-stream'));
    check('actual_ssh_succeeds_after_verified_rotation', succeeded(recovered) && recovered.summary.includes('FIXTURE-FINAL'));
    const beforeReject = (await server.request('stats')).stats;
    const rejected = await tool('run_ssh', sshArgs('fixture-reject'));
    const afterReject = (await server.request('stats')).stats;
    check('exec_rejection_not_retried_as_shell', !succeeded(rejected)
      && afterReject.exec_rejected === beforeReject.exec_rejected + 1 && afterReject.exec_accepted === beforeReject.exec_accepted
      && afterReject.shell_rejected === beforeReject.shell_rejected);
    if (options.native) {
      const nativeExe = path.join(directories['agent-package 中文'], 'native-trust-cli.exe');
      await fs.copyFile(options.native, nativeExe);
      const nativeScan = await command(nativeExe, ['--ssh-key-scan', '127.0.0.1', String(ready.port)], envFor('agent'));
      let value;
      try { value = JSON.parse(nativeScan.stdout); } catch { throw fail('native_cli_stdout_invalid_json'); }
      check('native_gui_executable_cli_exits_with_real_json', nativeScan.code === 0
        && value.status === 'scanned_unverified' && value.fingerprints?.[0] === fingerprint(ready));
      const shellNativeScan = await run(`& ${ps(nativeExe)} ${['--ssh-key-scan', '127.0.0.1', String(ready.port)].map(ps).join(' ')} | Out-String; `
        + "[Console]::Out.WriteLine('NATIVE_CLI_SHELL_DONE')");
      const shellLines = (shellNativeScan?.summary ?? '').trim().split(/\r?\n/);
      let shellValue;
      try { shellValue = JSON.parse(shellLines.slice(0, -1).join('\n')); }
      catch { throw fail('native_cli_remote_shell_stdout_invalid_json'); }
      check('native_cli_real_remote_powershell_waits_for_complete_json', succeeded(shellNativeScan)
        && shellLines.at(-1) === 'NATIVE_CLI_SHELL_DONE' && shellValue.status === 'scanned_unverified'
        && shellValue.host === '127.0.0.1' && shellValue.port === ready.port
        && shellValue.fingerprints?.length === 1 && shellValue.fingerprints[0] === fingerprint(ready));
    }
    await readEvents();
    streamedEvents = [];
    const cancellationStarted = Date.now();
    let cancelCompleted = false;
    let cancelRpcFailed = false;
    const cancellation = tool('run_ssh', sshArgs('fixture-timeout')).then(value => {
      cancelCompleted = true;
      return value;
    }, () => { cancelCompleted = true; cancelRpcFailed = true; return { isError: true }; });
    await until('cancellable_ssh_has_actual_first_frame', async () => (await readEvents()).some(event =>
      event.payload?.type === 'output_chunk' && event.payload.text.includes('FIXTURE-FIRST')));
    const activeCredentials = (await fs.readdir(directories['agent-temp'])).filter(name => /^remoteops-ssh-.*\.pw$/i.test(name));
    check('running_ssh_uses_owned_fixture_temp_credential', !cancelCompleted && activeCredentials.length === 1);
    const credentialAcl = await command('pwsh.exe', ['-NoProfile', '-Command',
      `$acl=Get-Acl -LiteralPath ${ps(path.join(directories['agent-temp'], activeCredentials[0]))}; `
      + '$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; '
      + 'if(-not $acl.AreAccessRulesProtected -or @($acl.Access).Count -ne 1 -or '
      + '$acl.Access[0].IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value -cne $sid.Value){exit 7}; '
      + '[Console]::Out.Write("ACL_OK")']);
    check('actual_agent_password_file_protected_user_only', credentialAcl.stdout === 'ACL_OK');
    const closed = await tool('close_connection');
    check('close_connection_closes_actual_ssh_session', succeeded(closed));
    sid = null;
    const cancelledStats = await until('ssh_server_confirms_cancelled_transport', async () => {
      const stats = (await server.request('stats')).stats;
      return stats.timeout_peer_closed > 0 ? stats : false;
    }, 10000);
    numbers.transport_close_milliseconds = Date.now() - cancellationStarted;
    await until('pending_runssh_settles_after_session_close', () => cancelCompleted, 10000);
    check('pending_runssh_cancel_returns_without_rpc_timeout', !cancelRpcFailed && !succeeded(await cancellation));
    numbers.cancel_milliseconds = Date.now() - cancellationStarted;
    numbers.ssh_auth_accepted = cancelledStats.auth_accepted;
    numbers.ssh_auth_rejected = cancelledStats.auth_rejected;
    numbers.ssh_exec_rejected = cancelledStats.exec_rejected;
    const credentials = await until('fixture_credentials_removed_after_cancel', async () => {
      const names = await fs.readdir(directories['agent-temp']);
      return names.filter(name => /^remoteops-ssh-.*\.pw$/i.test(name)).length === 0;
    }, 10000);
    check('cancelled_runssh_removes_private_credentials', credentials === true);
    check('no_synthetic_password_in_controller_or_agent_outputs', states.every(state =>
      syntheticSecrets.every(value => !state.stdout.includes(value) && !state.stderr.includes(value))));
    check('no_synthetic_password_in_events', syntheticSecrets.every(value => !JSON.stringify(streamedEvents).includes(value)));
  } catch (error) {
    failure = { step: current, category: error.category ?? 'fixture_or_product_failure' };
    if (!checks.some(check => check.name === current && check.passed === false)) checks.push({ name: current, passed: false });
    process.exitCode = 1;
    console.error(`FAIL ${current} [${failure.category}]`);
  } finally {
    if (mcp && sid) {
      await Promise.race([mcp.tool('close_connection', { session_id: sid }).catch(() => {}), delay(5000)]);
    }
    if (server && !server.state.ended) {
      server.state.child.stdin.end(`${JSON.stringify({ action: 'stop' })}\n`);
      await Promise.race([server.state.closed, delay(3000)]);
    }
    for (const state of states.toReversed()) {
      if (!state.ended) state.child.kill();
      await Promise.race([state.closed, delay(3000)]);
    }
    if (fixture) {
      let verified = false;
      try {
        verified = path.dirname(path.resolve(fixture)) === tempParent
          && path.basename(fixture).startsWith('remoteops-transfer-ssh-e2e-') && !(await fs.lstat(fixture)).isSymbolicLink();
      } catch { /* 路径无法确认时不删除，仍写安全失败记录。 */ }
      if (!verified) { failure ??= { step: 'cleanup', category: 'unsafe_fixture_cleanup_path' }; process.exitCode = 1; }
      else {
        try { await fs.rm(fixture, { recursive: true, force: true, maxRetries: 8, retryDelay: 200 }); }
        catch { failure ??= { step: 'cleanup', category: 'fixture_cleanup_failed' }; process.exitCode = 1; }
      }
    }
    // 只写固定检查项、稳定故障分类和数值；永不写原始输出、路径、密码、Token 或控制码。
    if (reportApproved) {
      await fs.mkdir(path.dirname(output), { recursive: true });
      await fs.writeFile(output, `${JSON.stringify({ schema: 1, binaries, checks, numbers, failure,
        passed: failure === null && checks.every(item => item.passed) }, null, 2)}\n`, { flag: 'wx' });
      console.log(`REPORT ${output}`);
    }
  }
}

async function selfTest() {
  assert.equal(ps("中文 空格/it's.ps1"), "'中文 空格/it''s.ps1'");
  assert.equal(ps('[x]$(not executed)`'), "'[x]$(not executed)`'");
  assert.equal(succeeded({ status: 'completed', exit_code: 0 }), true);
  assert.equal(succeeded({ status: 'completed', exit_code: 1 }), false);
  assert.equal(succeeded({ isError: true, status: 'completed', exit_code: 0 }), false);
  assert.equal(succeeded({ status: 'failed' }), false);
  assert.equal(samePath('\\\\?\\C:\\中文 空格\\file.ps1', 'c:\\中文 空格\\file.ps1'), true);
  assert.equal(samePath('C:\\agent-root', 'C:\\mcp-root'), false);
  assert.equal(samePath(null, 'C:\\mcp-root'), false);
  const key = Buffer.from('public-only-fixture-key').toString('base64');
  assert.equal(fingerprint({ algorithm: 'synthetic', public_key: key }),
    `synthetic SHA256:${createHash('sha256').update('public-only-fixture-key').digest('base64').replace(/=+$/, '')}`);
  const out = new EventEmitter();
  const sent = [];
  const client = new Mcp({ child: { stdout: out, stdin: { write: text => sent.push(JSON.parse(text)) } } }, 100);
  const response = client.tool('fixture', { public: true });
  const id = sent[0].id;
  out.emit('data', `{"jsonrpc":"2.0","id":${id},"result":{"structured`);
  out.emit('data', 'Content":{"status":"completed","exit_code":0}}}\n');
  assert.equal(succeeded(await response), true);
  const error = client.request('fixture', {});
  out.emit('data', `${JSON.stringify({ id: sent.at(-1).id, error: { code: -1, message: 'SYNTHETIC_DO_NOT_PERSIST' } })}\n`);
  assert.deepEqual(await error, { isError: true, rpc_error_code: -1 });
  assert.equal(client.pending.size, 0);
  out.emit('data', `${JSON.stringify({ jsonrpc: '2.0', id: 'fixture-elicitation', method: 'elicitation/create' })}\n`);
  assert.deepEqual(sent.at(-1).result, { action: 'accept', content: { confirmed: true } });
  await assert.rejects(client.request('timeout-fixture', {}), value => value.category === 'mcp_request_timeout');
  assert.equal(client.pending.size, 0);
  console.log('PASS fixture_helper_self_tests');
}

if (process.argv.length === 3 && process.argv[2] === '--self-test') await selfTest();
else {
  try { await main(parseOptions()); }
  catch (error) { console.error(`FAIL ${error.category ?? 'initialization_failure'}`); process.exitCode = 1; }
}
