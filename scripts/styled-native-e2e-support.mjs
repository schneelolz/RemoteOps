// 不创建进程或窗口的桌面验收参数与证据处理。
import path from 'node:path';

const safeName = /^[a-z][a-z0-9_]{0,63}$/;
const requiredArguments = ['agent', 'relay', 'output', 'expected-hash'];
const scenarios = new Set(['all', 'operations', 'smoke', 'shutdown', 'setup', 'ui', 'tls-once', 'privacy']);

/** 仅显式允许桌面交互时返回参数；调用方必须在创建任何 fixture 或进程前调用。 */
export function parseDesktopArguments(argv, defaults) {
  const result = {...defaults};
  const supplied = new Set();
  let allowDesktop = false;
  for (let i = 0; i < argv.length; i++) {
    const argument = argv[i];
    if (argument === '--allow-desktop') {
      if (allowDesktop) throw new Error('duplicate_desktop_permission');
      allowDesktop = true;
      continue;
    }
    if (typeof argument !== 'string' || !argument.startsWith('--')) throw new Error('invalid_arguments');
    const key = argument.slice(2);
    const value = argv[++i];
    if (!Object.hasOwn(defaults, key) || supplied.has(key) || typeof value !== 'string' || !value || value.startsWith('--')) throw new Error('invalid_arguments');
    supplied.add(key);
    result[key] = ['scenario', 'expected-hash'].includes(key) ? value : path.resolve(value);
  }
  if (!allowDesktop) throw new Error('desktop_interaction_not_authorized');
  const required = ['setup', 'tls-once'].includes(result.scenario) ? requiredArguments : result.scenario === 'ui' ? [...requiredArguments, 'mcp'] : [...requiredArguments, 'mcp', 'python'];
  if (!required.every(key => result[key]) || !/^[0-9a-f]{64}$/i.test(result['expected-hash']) || !scenarios.has(result.scenario)) throw new Error('required_arguments_or_scenario');
  if (!result.askpass) result.askpass = path.join(path.dirname(result.agent), 'remoteops-ssh-askpass.exe');
  return result;
}

/** 将合法的 SHA-256 证书指纹统一为小写十六进制；缺失或格式错误返回 null。 */
export function normalizeTlsFingerprint(value) {
  if (typeof value !== 'string') return null;
  const normalized = value.trim().replace(/^sha256:/i, '').replace(/[:\s-]/g, '').toLowerCase();
  return /^[0-9a-f]{64}$/.test(normalized) ? normalized : null;
}

/** 仅两端都是完整合法 SHA-256 指纹时才允许相等，避免两个空值误通过。 */
export function sameTlsFingerprint(left, right) {
  const normalized = normalizeTlsFingerprint(left);
  return normalized !== null && normalized === normalizeTlsFingerprint(right);
}

/** 只保留固定检查名与有限数值／布尔指标，不保存原始错误、路径、凭据或命令输出。 */
export function summarizeApplicationReport(report) {
  if (!report || typeof report !== 'object' || Array.isArray(report)) return {report_available: false, passed: false};
  const metrics = report.metrics && typeof report.metrics === 'object' && !Array.isArray(report.metrics) ? report.metrics : {};
  return {
    report_available: true,
    passed: report.passed === true,
    checks: (Array.isArray(report.checks) ? report.checks : [])
      .filter(check => check && typeof check.name === 'string' && safeName.test(check.name))
      .map(check => ({name: check.name, passed: check.passed === true})),
    metrics: Object.fromEntries(Object.entries(metrics).filter(([key, value]) => safeName.test(key) &&
      (typeof value === 'boolean' || typeof value === 'number' && Number.isFinite(value))))
  };
}

/** 在退出码断言前收集报告；读取或解析失败也必须留下一条“报告缺失”证据。 */
export async function collectApplicationReport(readReport, reports) {
  let report = null;
  try {
    report = await readReport();
  } catch {}
  reports.push(summarizeApplicationReport(report));
  return report;
}


/** 只有完整且自洽的报告才能把退出1归为纯性能失败；任何生命周期疑点都立即停止。 */
export function classifyCleanQualityExit(report, exitCode) {
  const record = value => value !== null && typeof value === 'object' && !Array.isArray(value) &&
    [Object.prototype, null].includes(Object.getPrototypeOf(value));
  if (!record(report) || report.schema_version !== 1 || typeof report.passed !== 'boolean' ||
      !Array.isArray(report.checks) || report.checks.length === 0 || !record(report.metrics)) return 'critical';
  const checks = new Map();
  for (const check of report.checks) {
    if (!record(check) || typeof check.name !== 'string' || !safeName.test(check.name) ||
        typeof check.passed !== 'boolean' || checks.has(check.name)) return 'critical';
    checks.set(check.name, check.passed);
  }
  const metrics = report.metrics;
  const nullable = new Set(['observer_text_matches_model_on_close', 'rendered_text_matches_model_on_close']);
  for (const [name, value] of Object.entries(metrics)) {
    if (!safeName.test(name) || !(typeof value === 'boolean' ||
        typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ||
        value === null && nullable.has(name))) return 'critical';
  }
  if (['background_stopped', 'native_message_loop_completed', 'background_cleanup_completed'].some(name => checks.get(name) !== true) ||
      metrics.background_cleanup_complete !== true || metrics.native_loop_failed !== false ||
      metrics.native_callback_failed !== false || metrics.running_count !== 0 ||
      metrics.native_run_failed === true ||
      Object.hasOwn(metrics, 'background_stopped') && metrics.background_stopped !== true) return 'critical';
  // 没有观察窗时原始报告会给null；有实际核验时，检查与两个兼容指标必须一致。
  for (const name of nullable) {
    if (Object.hasOwn(metrics, name) && metrics[name] !== null && metrics[name] !== true) return 'critical';
  }
  const observed = metrics.observer_text_matches_model_on_close;
  if (checks.has('observer_text_matches_model_on_close') !== (observed === true) ||
      checks.has('observer_text_matches_model_on_close') && checks.get('observer_text_matches_model_on_close') !== true ||
      Object.hasOwn(metrics, 'rendered_text_matches_model_on_close') &&
      metrics.rendered_text_matches_model_on_close !== observed) return 'critical';
  const quality = new Map([
    ['ui_responsive', 'max_tick_ms'],
    ['product_page_responsive', 'product_page_max_ms']
  ]);
  for (const [checkName, metricName] of quality) {
    const value = metrics[metricName];
    if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0 ||
        checks.has(checkName) && checks.get(checkName) !== (value <= 100) ||
        !checks.has(checkName) && value !== 0) return 'critical';
  }
  const failed = [...checks].filter(([, passed]) => !passed).map(([name]) => name);
  if (report.passed !== (failed.length === 0)) return 'critical';
  if (exitCode === 0 && report.passed) return 'passed';
  if (exitCode === 1 && !report.passed && failed.length > 0 && failed.every(name => quality.has(name))) return 'quality_only';
  return 'critical';
}


/** 检查真实凭据和主动输出标记的不同边界；只返回固定布尔事实，不返回进程原文。 */
export function evaluateProcessOutputPrivacy(states, protocolState, tokens) {
  const invalid = () => ({
    inputs_valid:false,
    credential_absent_from_all_outputs:false,
    output_marker_absent_from_diagnostics:false,
    output_marker_seen_in_protocol_stdout:false,
    passed:false
  });
  const record = value => value !== null && typeof value === 'object' && !Array.isArray(value) &&
    [Object.prototype, null].includes(Object.getPrototypeOf(value));
  // 只接受自身数据字段，不执行调用方 getter，也不把缺失字段当作空输出。
  const ownString = (value, key) => {
    const property = Object.getOwnPropertyDescriptor(value, key);
    return property && Object.hasOwn(property, 'value') && typeof property.value === 'string' ? property.value : null;
  };
  try {
    if (!Array.isArray(states) || states.length === 0 || !record(protocolState) || !record(tokens)) return invalid();
    const credential = ownString(tokens, 'credentialSecret');
    const marker = ownString(tokens, 'outputMarker');
    if (credential === null || marker === null || !credential.trim() || !marker.trim() || credential === marker) return invalid();
    const outputs = [];
    let protocols = 0;
    for (const state of states) {
      if (!record(state)) return invalid();
      const stdout = ownString(state, 'stdout');
      const stderr = ownString(state, 'stderr');
      if (stdout === null || stderr === null) return invalid();
      if (state === protocolState) protocols++;
      outputs.push({state, stdout, stderr});
    }
    if (protocols !== 1) return invalid();
    const credentialAbsent = outputs.every(({stdout, stderr}) => !stdout.includes(credential) && !stderr.includes(credential));
    const markerAbsentFromDiagnostics = outputs.every(({state, stdout, stderr}) =>
      !stderr.includes(marker) && (state === protocolState || !stdout.includes(marker)));
    const protocolOutput = outputs.find(({state}) => state === protocolState);
    return {
      inputs_valid:true,
      credential_absent_from_all_outputs:credentialAbsent,
      output_marker_absent_from_diagnostics:markerAbsentFromDiagnostics,
      output_marker_seen_in_protocol_stdout:protocolOutput.stdout.includes(marker),
      passed:credentialAbsent && markerAbsentFromDiagnostics
    };
  } catch {
    return invalid();
  }
}
