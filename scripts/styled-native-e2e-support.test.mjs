import test from 'node:test';
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {parseDesktopArguments, normalizeTlsFingerprint, sameTlsFingerprint, summarizeApplicationReport, collectApplicationReport, classifyCleanQualityExit, evaluateProcessOutputPrivacy} from './styled-native-e2e-support.mjs';

const defaults = {agent:'', relay:'', mcp:'', python:'', output:'', 'expected-hash':'', scenario:'all', askpass:'', plink:'plink.exe', 'ui-client':'client.ps1'};
const argumentsFor = scenario => ['--agent','candidate.exe','--relay','relay.exe','--mcp','mcp.exe','--python','python.exe','--output','result.json','--expected-hash','a'.repeat(64),'--scenario',scenario];

test('默认拒绝完整且有效的桌面命令，不隐含授权', () => {
  assert.throws(() => parseDesktopArguments(argumentsFor('all'), defaults), /desktop_interaction_not_authorized/);
});

test('空参数也不能触发桌面验收', () => {
  assert.throws(() => parseDesktopArguments([], defaults), /desktop_interaction_not_authorized/);
});

test('仅精确且独立的许可开关能够解锁有效场景', () => {
  for (const scenario of ['all','operations','smoke','shutdown','setup','ui','tls-once','privacy']) {
    const parsed = parseDesktopArguments(['--allow-desktop', ...argumentsFor(scenario)], defaults);
    assert.equal(parsed.scenario, scenario);
    assert.ok(parsed.askpass.endsWith('remoteops-ssh-askpass.exe'));
  }
  assert.throws(() => parseDesktopArguments(['--allow-desktop=false', ...argumentsFor('all')], defaults), /invalid_arguments/);
  assert.throws(() => parseDesktopArguments(['--allow-desktop','false', ...argumentsFor('all')], defaults), /invalid_arguments/);
});

test('拒绝重复许可、重复参数和未知场景', () => {
  assert.throws(() => parseDesktopArguments(['--allow-desktop','--allow-desktop', ...argumentsFor('all')], defaults), /duplicate_desktop_permission/);
  assert.throws(() => parseDesktopArguments(['--allow-desktop', ...argumentsFor('all'), '--agent','other.exe'], defaults), /invalid_arguments/);
  assert.throws(() => parseDesktopArguments(['--allow-desktop', ...argumentsFor('unrecognized')], defaults), /required_arguments_or_scenario/);
});

test('缺值不得吞掉下一个开关，输入默认值保持不变', () => {
  assert.throws(() => parseDesktopArguments(['--allow-desktop','--agent','--relay','relay.exe'], defaults), /invalid_arguments/);
  assert.equal(defaults.agent, '');
  assert.equal(defaults.askpass, '');
});

test('证书显示的前缀、大小写及分隔符不影响独立指纹比较', () => {
  const raw='0123456789abcdef'.repeat(4);
  const formatted=' SHA256: '+raw.match(/../g).join(':').toUpperCase()+' ';
  assert.equal(normalizeTlsFingerprint(formatted), raw);
  assert.equal(sameTlsFingerprint(formatted, raw), true);
  assert.equal(sameTlsFingerprint(formatted, 'f'.repeat(64)), false);
});

test('缺失、短指纹和非十六进制输入不能互相冒充有效指纹', () => {
  for (const value of [undefined, null, '', 'sha256:', 'abc', 'g'.repeat(64), 123]) {
    assert.equal(normalizeTlsFingerprint(value), null);
    assert.equal(sameTlsFingerprint(value, value), false);
  }
});

test('失败报告保留失败检查和数值指标，同时去掉原始内容与非有限值', async () => {
  const reports=[];
  const source={passed:false, detail:'PRIVATE_DETAIL', checks:[{name:'ui_responsive',passed:false,detail:'PRIVATE_TEXT'},null,{name:'unsafe name',passed:true},{name:'truthy_is_not_true',passed:'yes'}], metrics:{max_tick_ms:123, background_stopped:true, output:'PRIVATE_SECRET', infinite:Infinity, nan:NaN, nested:{secret:'PRIVATE_SECRET'}, missing:null, 'unsafe-key':3}};
  const result=await collectApplicationReport(async()=>source,reports);
  assert.equal(result, source);
  assert.deepEqual(reports,[{report_available:true,passed:false,checks:[{name:'ui_responsive',passed:false},{name:'truthy_is_not_true',passed:false}],metrics:{max_tick_ms:123,background_stopped:true}}]);
  assert.equal(JSON.stringify(reports).includes('PRIVATE_'),false);
  // 调用方之后因非零退出抛错，先前已收集的失败指标仍在。
  assert.throws(()=>assert.equal(1,0));
  assert.equal(reports[0].metrics.max_tick_ms,123);
});

test('报告缺失和读取失败也明确留下证据', async () => {
  const reports=[];
  assert.equal(await collectApplicationReport(async()=>{throw new Error('PRIVATE_IO_ERROR');},reports),null);
  assert.deepEqual(reports,[{report_available:false,passed:false}]);
  assert.deepEqual(summarizeApplicationReport([]),{report_available:false,passed:false});
});

test('异常指标容器和真值字符串不能伪造成功', () => {
  assert.deepEqual(summarizeApplicationReport({passed:'true',metrics:['PRIVATE_DATA'],checks:{secret:'PRIVATE_DATA'}}),{report_available:true,passed:false,checks:[],metrics:{}});
});


test('实际主脚本无桌面许可时在 fixture 和进程启动前退出', () => {
  const script=fileURLToPath(new URL('./test-styled-native-e2e.mjs',import.meta.url));
  const result=spawnSync(process.execPath,[script,...argumentsFor('all')],{encoding:'utf8',timeout:5000,windowsHide:true});
  assert.equal(result.error,undefined);
  assert.notEqual(result.status,0);
  assert.match(result.stderr,/desktop_interaction_not_authorized/);
  assert.equal(result.stdout,'');
  assert.doesNotMatch(result.stderr,/exact_candidate_hash|ENOENT|fixture_wait_timeout/);
});


test('最短 setup 场景只要求 Agent、Relay、输出与摘要，其他场景仍要求执行夹具', () => {
  const minimal=['--allow-desktop','--agent','candidate.exe','--relay','relay.exe','--output','result.json','--expected-hash','a'.repeat(64)];
  const setup=parseDesktopArguments([...minimal,'--scenario','setup'],defaults);
  assert.equal(setup.scenario,'setup');
  assert.equal(setup.mcp,'');
  assert.equal(setup.python,'');
  assert.throws(()=>parseDesktopArguments([...minimal,'--scenario','all'],defaults),/required_arguments_or_scenario/);
  assert.throws(()=>parseDesktopArguments([...minimal,'--scenario','shutdown'],defaults),/required_arguments_or_scenario/);
});


test('短 ui 场景需要 MCP，但不要求未使用的 Python SSH 夹具', () => {
  const minimal=['--allow-desktop','--agent','candidate.exe','--relay','relay.exe','--output','result.json','--expected-hash','a'.repeat(64),'--scenario','ui'];
  assert.throws(()=>parseDesktopArguments(minimal,defaults),/required_arguments_or_scenario/);
  const parsed=parseDesktopArguments([...minimal,'--mcp','mcp.exe'],defaults);
  assert.equal(parsed.scenario,'ui');
  assert.equal(parsed.python,'');
});


test('tls-once最短场景保留桌面门禁，不要求MCP或SSH夹具', () => {
  const minimal=['--agent','candidate.exe','--relay','relay.exe','--output','result.json','--expected-hash','a'.repeat(64),'--scenario','tls-once'];
  assert.throws(()=>parseDesktopArguments(minimal,defaults),/desktop_interaction_not_authorized/);
  const parsed=parseDesktopArguments(['--allow-desktop',...minimal],defaults);
  assert.equal(parsed.scenario,'tls-once');
  assert.equal(parsed.mcp,'');
  assert.equal(parsed.python,'');
  assert.throws(()=>parseDesktopArguments(['--allow-desktop',...minimal.filter((_,index)=>![2,3].includes(index))],defaults),/required_arguments_or_scenario/);
});

function cleanApplicationReport() {
  return {
    schema_version: 1,
    passed: true,
    checks: [
      {name:'background_stopped',passed:true},
      {name:'native_message_loop_completed',passed:true},
      {name:'background_cleanup_completed',passed:true},
      {name:'ui_responsive',passed:true},
      {name:'product_page_responsive',passed:true}
    ],
    metrics: {
      background_cleanup_complete:true,native_loop_failed:false,native_callback_failed:false,
      running_count:0,max_tick_ms:35,product_page_max_ms:29,
      observer_text_matches_model_on_close:null,rendered_text_matches_model_on_close:null
    }
  };
}
function qualityApplicationReport() {
  const report=cleanApplicationReport();
  report.passed=false;
  report.checks.find(check=>check.name==='ui_responsive').passed=false;
  report.metrics.max_tick_ms=112;
  return report;
}

test('正常退出和纯性能退出分别记录，性能门限仍为100ms', () => {
  const clean=cleanApplicationReport();
  assert.equal(classifyCleanQualityExit(clean,0),'passed');
  const quality=qualityApplicationReport();
  assert.equal(classifyCleanQualityExit(quality,1),'quality_only');
  assert.equal(quality.passed,false);
  assert.equal(quality.metrics.max_tick_ms,112);
  quality.checks.find(check=>check.name==='product_page_responsive').passed=false;
  quality.metrics.product_page_max_ms=171;
  assert.equal(classifyCleanQualityExit(quality,1),'quality_only');
  const boundary=cleanApplicationReport();
  boundary.metrics.max_tick_ms=100;
  boundary.metrics.product_page_max_ms=100;
  assert.equal(classifyCleanQualityExit(boundary,0),'passed');
  assert.equal(classifyCleanQualityExit({...boundary,passed:false},1),'critical');
});

test('观察窗模型不一致绝不能作为性能失败继续', () => {
  const report=qualityApplicationReport();
  report.checks.push({name:'observer_text_matches_model_on_close',passed:true});
  report.metrics.observer_text_matches_model_on_close=true;
  report.metrics.rendered_text_matches_model_on_close=true;
  assert.equal(classifyCleanQualityExit(report,1),'quality_only');
  report.checks.at(-1).passed=false;
  report.metrics.observer_text_matches_model_on_close=false;
  report.metrics.rendered_text_matches_model_on_close=false;
  assert.equal(classifyCleanQualityExit(report,1),'critical');
});

test('后台未停、循环异常、回调失败和活动残留一律阻断', () => {
  for (const [field,value] of [['background_cleanup_complete',false],['native_loop_failed',true],['native_callback_failed',true],['running_count',1],['native_run_failed',true],['background_stopped',false]]) {
    const report=qualityApplicationReport();
    report.metrics[field]=value;
    assert.equal(classifyCleanQualityExit(report,1),'critical',field);
  }
  for (const name of ['background_stopped','native_message_loop_completed','background_cleanup_completed']) {
    const missing=qualityApplicationReport();
    missing.checks=missing.checks.filter(check=>check.name!==name);
    assert.equal(classifyCleanQualityExit(missing,1),'critical',name);
    const failed=qualityApplicationReport();
    failed.checks.find(check=>check.name===name).passed=false;
    assert.equal(classifyCleanQualityExit(failed,1),'critical',name);
  }
});

test('指标与检查矛盾、总体成功矛盾和观察窗兼容指标矛盾均拒绝', () => {
  const quality=qualityApplicationReport();
  quality.metrics.max_tick_ms=35;
  assert.equal(classifyCleanQualityExit(quality,1),'critical');
  const falseSuccess=cleanApplicationReport();
  falseSuccess.metrics.max_tick_ms=101;
  assert.equal(classifyCleanQualityExit(falseSuccess,0),'critical');
  const noFailure=qualityApplicationReport();
  noFailure.checks.find(check=>check.name==='ui_responsive').passed=true;
  noFailure.metrics.max_tick_ms=35;
  assert.equal(classifyCleanQualityExit(noFailure,1),'critical');
  const wrongSummary=qualityApplicationReport();
  wrongSummary.passed=true;
  assert.equal(classifyCleanQualityExit(wrongSummary,1),'critical');
  const noObserverCheck=qualityApplicationReport();
  noObserverCheck.metrics.observer_text_matches_model_on_close=true;
  noObserverCheck.metrics.rendered_text_matches_model_on_close=true;
  assert.equal(classifyCleanQualityExit(noObserverCheck,1),'critical');
  const wrongAlias=qualityApplicationReport();
  wrongAlias.metrics.rendered_text_matches_model_on_close=true;
  assert.equal(classifyCleanQualityExit(wrongAlias,1),'critical');
});

test('报告缺失、异常类型、重复检查、未知失败及非退出码1不得降级', () => {
  for (const report of [null,undefined,[],{},'report',new Date(),{...qualityApplicationReport(),schema_version:2}]) {
    assert.equal(classifyCleanQualityExit(report,1),'critical');
  }
  for (const value of [null,[],{},'1',Infinity,NaN,1.5,-1]) {
    const report=qualityApplicationReport();
    report.metrics.max_tick_ms=value;
    assert.equal(classifyCleanQualityExit(report,1),'critical');
  }
  for (const value of [null,[],true,'metrics']) {
    assert.equal(classifyCleanQualityExit({...qualityApplicationReport(),metrics:value},1),'critical');
  }
  for (const code of [null,undefined,-1,2,7,137,'1',0]) {
    assert.equal(classifyCleanQualityExit(qualityApplicationReport(),code),'critical');
  }
  const duplicate=qualityApplicationReport();
  duplicate.checks.push({name:'ui_responsive',passed:false});
  assert.equal(classifyCleanQualityExit(duplicate,1),'critical');
  const unknownFailure=qualityApplicationReport();
  unknownFailure.checks.push({name:'unrecognized_functional_failure',passed:false});
  assert.equal(classifyCleanQualityExit(unknownFailure,1),'critical');
  const malformed=qualityApplicationReport();
  malformed.checks[0].passed='true';
  assert.equal(classifyCleanQualityExit(malformed,1),'critical');
});


function privacyFixture() {
  const tokens={credentialSecret:'FIXTURE_CREDENTIAL_BASE64URL_123',outputMarker:'STYLED_PRINTED_OUTPUT_TAG'};
  const agent={stdout:'',stderr:''};
  const relay={stdout:'fixture-ready',stderr:''};
  const protocol={stdout:JSON.stringify({summary:`password=${tokens.outputMarker}`}),stderr:''};
  return {tokens,agent,relay,protocol,states:[agent,relay,protocol]};
}

test('privacy场景仍要求MCP及Python，不能误用最短TLS资源配置', () => {
  const minimal=['--allow-desktop','--agent','candidate.exe','--relay','relay.exe','--output','result.json','--expected-hash','a'.repeat(64),'--scenario','privacy'];
  assert.throws(()=>parseDesktopArguments(minimal,defaults),/required_arguments_or_scenario/);
  assert.throws(()=>parseDesktopArguments([...minimal,'--mcp','mcp.exe'],defaults),/required_arguments_or_scenario/);
  assert.equal(parseDesktopArguments([...minimal,'--mcp','mcp.exe','--python','python.exe'],defaults).scenario,'privacy');
});

test('主动输出tag仅在精确MCP协议stdout允许，且结果只含固定布尔', () => {
  const fixture=privacyFixture();
  const before=JSON.stringify(fixture.states);
  const result=evaluateProcessOutputPrivacy(fixture.states,fixture.protocol,fixture.tokens);
  assert.deepEqual(result,{inputs_valid:true,credential_absent_from_all_outputs:true,output_marker_absent_from_diagnostics:true,output_marker_seen_in_protocol_stdout:true,passed:true});
  assert.equal(JSON.stringify(fixture.states),before);
  assert.equal(JSON.stringify(result).includes(fixture.tokens.credentialSecret),false);
  assert.equal(JSON.stringify(result).includes(fixture.tokens.outputMarker),false);
  assert.ok(Object.values(result).every(value=>typeof value==='boolean'));
});

test('真实SSH凭据在任何stdout或stderr中都失败，MCP原始结果也不例外', () => {
  for(const index of [0,1,2])for(const channel of ['stdout','stderr']) {
    const fixture=privacyFixture();
    fixture.states[index][channel]+=`prefix ${fixture.tokens.credentialSecret} suffix`;
    const result=evaluateProcessOutputPrivacy(fixture.states,fixture.protocol,fixture.tokens);
    assert.equal(result.inputs_valid,true);
    assert.equal(result.credential_absent_from_all_outputs,false,`${index}/${channel}`);
    assert.equal(result.passed,false,`${index}/${channel}`);
  }
});

test('主动tag出现在Agent或其他stdout及任何stderr都失败', () => {
  for(const [index,channel] of [[0,'stdout'],[1,'stdout'],[0,'stderr'],[1,'stderr'],[2,'stderr']]) {
    const fixture=privacyFixture();
    fixture.states[index][channel]+=`tag=${fixture.tokens.outputMarker}`;
    const result=evaluateProcessOutputPrivacy(fixture.states,fixture.protocol,fixture.tokens);
    assert.equal(result.credential_absent_from_all_outputs,true);
    assert.equal(result.output_marker_absent_from_diagnostics,false,`${index}/${channel}`);
    assert.equal(result.passed,false,`${index}/${channel}`);
  }
});

test('没有主动tag时只报告未观察到，不伪造命令确实输出', () => {
  const fixture=privacyFixture();
  fixture.protocol.stdout='{"summary":"ordinary result"}';
  const result=evaluateProcessOutputPrivacy(fixture.states,fixture.protocol,fixture.tokens);
  assert.equal(result.inputs_valid,true);
  assert.equal(result.output_marker_seen_in_protocol_stdout,false);
  assert.equal(result.passed,true);
});

test('空secret、标记缺失或混用、类型错误均fail closed', () => {
  const fixture=privacyFixture();
  for(const field of ['credentialSecret','outputMarker'])for(const value of ['', '  ',null,undefined,123,{},new String('boxed')]) {
    const tokens={...fixture.tokens,[field]:value};
    const result=evaluateProcessOutputPrivacy(fixture.states,fixture.protocol,tokens);
    assert.equal(result.inputs_valid,false);
    assert.ok(Object.values(result).every(value=>value===false));
  }
  for(const tokens of [null,[],{},fixture.tokens.credentialSecret,{...fixture.tokens,outputMarker:fixture.tokens.credentialSecret}]) {
    assert.equal(evaluateProcessOutputPrivacy(fixture.states,fixture.protocol,tokens).passed,false);
  }
});

test('缺少精确协议对象、同形副本及重复协议引用均fail closed', () => {
  const fixture=privacyFixture();
  for(const protocol of [null,undefined,{}, {...fixture.protocol}]) {
    const result=evaluateProcessOutputPrivacy(fixture.states,protocol,fixture.tokens);
    assert.equal(result.inputs_valid,false);
    assert.equal(result.passed,false);
  }
  assert.equal(evaluateProcessOutputPrivacy(fixture.states.slice(0,2),fixture.protocol,fixture.tokens).passed,false);
  assert.equal(evaluateProcessOutputPrivacy([...fixture.states,fixture.protocol],fixture.protocol,fixture.tokens).passed,false);
  assert.equal(evaluateProcessOutputPrivacy([],fixture.protocol,fixture.tokens).passed,false);
});

test('进程输出字段缺失、非字符串或getter均不作为空串放行', () => {
  for(const channel of ['stdout','stderr'])for(const value of [null,undefined,Buffer.from('bytes'),123,{},new String('boxed')]) {
    const fixture=privacyFixture();
    fixture.agent[channel]=value;
    assert.equal(evaluateProcessOutputPrivacy(fixture.states,fixture.protocol,fixture.tokens).inputs_valid,false);
  }
  const fixture=privacyFixture();
  let getterCalled=false;
  Object.defineProperty(fixture.agent,'stdout',{get(){getterCalled=true;return '';}});
  const result=evaluateProcessOutputPrivacy(fixture.states,fixture.protocol,fixture.tokens);
  assert.equal(result.inputs_valid,false);
  assert.equal(getterCalled,false);
});
