#!/usr/bin/env node
// 回环真实业务验收；机密和原始输出仅在内存或当前用户私有C盘fixture中。
import {spawn} from 'node:child_process';

import {randomBytes,randomUUID,createHash,X509Certificate} from 'node:crypto';

import fs from 'node:fs/promises';

import net from 'node:net';

import os from 'node:os';

import path from 'node:path';

import {fileURLToPath} from 'node:url';

import {setTimeout as delay} from 'node:timers/promises';

import {parseDesktopArguments,sameTlsFingerprint,collectApplicationReport,classifyCleanQualityExit,evaluateProcessOutputPrivacy} from './styled-native-e2e-support.mjs';

const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');

const plinkHash='50479953865b30775056441b10fdcb984126ba4f98af4f64756902a807b453e7';

const hash=x=>createHash('sha256').update(x).digest('hex');

const samePath=(left,right)=>typeof left==='string'&&typeof right==='string'&&path.resolve(left.replace(/^\\\\\?\\/, '')).toLowerCase()===path.resolve(right.replace(/^\\\\\?\\/, '')).toLowerCase();

const ps=x=>`'${String(x).replaceAll("'","''")}'`;

const succeeded=x=>x?.isError!==true&&x?.status==='completed'&&(x.exit_code===0||x.exit_code==null);

const defaults={
  agent:'',
  relay:'',
  mcp:'',
  python:'',
  askpass:'',
  plink:'D:\\Tools\\putty\\PLINK.EXE',
  output:'',
  scenario:'all',
  'expected-hash':'',
  'ui-client':path.join(root,'scripts/Test-NativeStylePreview.ps1')
};

// No fixture, process, window or UIA action may precede explicit desktop permission.
const options=parseDesktopArguments(process.argv.slice(2),defaults);

function fail(category,details={}){
  const e=new Error(category);
  e.category=category;
  e.details=details;
  return e;
}

class Mcp{
  constructor(state){
    this.state=state;
    this.pending=new Map();
    this.sequence=0;
    this.buffer='';
    state.child.stdout.on(
      'data',
      s=>{
        this.buffer+=s;
        let i;
        while((i=this.buffer.indexOf('\n'))>=0){
          const line=this.buffer.slice(0,i);
          this.buffer=this.buffer.slice(i+1);
          let m;
          try{
            m=JSON.parse(line);
          }catch{
            continue;
          }
          if(m.method&&m.id!==undefined){
            this.send({
                jsonrpc:'2.0',
                id:m.id,
                result:m.method==='elicitation/create'?{action:'accept',content:{confirmed:true}}:{}
              });
          }else if(this.pending.has(m.id))this.pending.get(m.id).resolve(m);
        }
      }
    );
    state.closed.then(()=>{
        for(const p of this.pending.values())p.reject(fail('mcp_transport_closed'));
        this.pending.clear();
      });
  }
  send(x){
    this.state.child.stdin.write(`${JSON.stringify(x)}\n`);
  }
  async request(method,params){
    if(this.state.ended)throw fail('mcp_transport_closed');
    const id=++this.sequence;
    let timer;
    try{
      const m=await new Promise((resolve,reject)=>{
          this.pending.set(id,{resolve,reject});
          timer=setTimeout(()=>reject(fail('mcp_request_timeout')),90000);
          this.send({
              jsonrpc:'2.0',
              id,
              method,
              params
            });
        });
      return m.error?{isError:true,rpc_error_code:m.error.code}:m.result;
    }finally{
      clearTimeout(timer);
      this.pending.delete(id);
    }
  }
  async tool(name,args){
    const r=await this.request('tools/call',{name,arguments:args});
    if(r?.isError)return r;
    if(r?.structuredContent)return r.structuredContent;
    for(const c of r?.content??[])if(c.type==='text'){
      try{
        return JSON.parse(c.text);
      }catch{}
    }
    return r;
  }
}

async function main(){

  const checks=[],states=[],pending=new Set(),uiFacts=[],reports=[],qualityFailures=[],hashes={};

  let current='startup',fixture,outputApproved=false,failure=null,agent,stamp,agentReport,expectedHash,envFor,mcp,sid,relay,nativeExe,dirs;

  const secret=randomBytes(32).toString('base64url'),redactionSecret=`STYLED_${randomBytes(12).toString('hex')}`,secrets=[secret,redactionSecret];

  const temp=path.resolve(os.tmpdir()),output=path.resolve(options.output);

  function check(name,ok){
    current=name;
    checks.push({name,passed:Boolean(ok)});
    if(!ok)throw fail('assertion_failed');
    console.log(`PASS ${name}`);
  }

  function performanceCheck(name,value){
    current=name;
    if(!Number.isSafeInteger(value)||value<0)throw fail('invalid_performance_metric');
    if(value<=100){check(name,true);return;}
    if(qualityFailures.length===0)throw fail('unrecorded_performance_failure');
    checks.push({name,passed:false,category:'performance'});
    console.log(`QUALITY FAIL ${name}；${value}ms，门限100ms，最终仍记失败。`);
  }

  function track(p){
    pending.add(p);
    p.then(()=>pending.delete(p),()=>pending.delete(p));
    p.catch(()=>{});
    return p;
  }

  async function until(name,predicate,ms=30000){
    current=name;
    const end=Date.now()+ms;
    do{
      const r=await predicate();
      if(r)return r;
      await delay(100);
    }while(Date.now()<end);
    throw fail('fixture_wait_timeout');
  }

  function start(exe,args,env,cwd=fixture){
    const child=spawn(exe,args,{
        cwd,
        env,
        // Native UI fixtures must be visible; SW_HIDE in STARTUPINFO overrides the first ShowWindow.
        windowsHide:exe!==nativeExe,
        stdio:['pipe','pipe','pipe']
      });
    const s={
      child,
      stdout:'',
      stderr:'',
      ended:false,
      code:null
    };
    s.closed=new Promise(resolve=>{
        child.once('error',()=>{
            s.ended=true;
            resolve();
          });
        child.once('close',code=>{
            s.ended=true;
            s.code=code;
            resolve();
          });
      });
    child.stdin.on('error',()=>{});
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data',x=>{
        s.stdout=(s.stdout+x).slice(-4*1024*1024);
      });
    child.stderr.on('data',x=>{
        s.stderr=(s.stderr+x).slice(-1024*1024);
      });
    states.push(s);
    return s;
  }

  async function command(exe,args,env,ms=30000){
    const s=start(exe,args,env);
    s.child.stdin.end();
    await Promise.race([s.closed,delay(ms)]);
    if(!s.ended){
      s.child.kill();
      throw fail('fixture_process_timeout');
    }
    if(s.code!==0)throw fail('fixture_process_failed',{exit_code:s.code});
    return s;
  }

  async function port(){
    const n=net.createServer();
    await new Promise((resolve,reject)=>{
        n.once('error',reject);
        n.listen(0,'127.0.0.1',resolve);
      });
    const p=n.address().port;
    await new Promise((resolve,reject)=>n.close(e=>e?reject(e):resolve()));
    return p;
  }

  const rows=s=>(s?.windows??[]).flatMap(w=>w.controls??[]),row=(s,id)=>rows(s).find(x=>x.id===id),text=(s,id)=>row(s,id)?.text??'',observer=s=>s?.windows?.find(w=>w.class==='RemoteOpsStylePreviewObserver');

  const running=(s,id,n)=>new RegExp(`·\\s*${n}(?:\\D|$)`).test(text(s,id));

  const idle=s=>/执行中\s*[：:]\s*0(?:\D|$)|Running:\s*0(?:\D|$)/.test(text(s,222));

  const selected=(s,n)=>new RegExp(`查看\\s*[：:]\\s*${n}(?:\\s|[·|]|$)`).test(text(s,222));

  async function nativeDiagnostic() {
    if (!agent || agent.ended) return { process_alive: false, exit_code: agent?.code ?? null };
    const probeScript = `$ErrorActionPreference='Stop';$targetProcess=Get-Process -Id ${agent.child.pid};
$verified=$targetProcess.StartTime.ToUniversalTime().ToString('o') -ceq ${ps(stamp)} -and (Get-FileHash -LiteralPath $targetProcess.MainModule.FileName).Hash -ieq ${ps(expectedHash)};
if(-not $verified){@{identity_verified=$false}|ConvertTo-Json -Compress;exit 0}
Add-Type -TypeDefinition @"
using System;using System.Collections.Generic;using System.Runtime.InteropServices;using System.Text;
public static class StyledFixtureProbe{
 public delegate bool Callback(IntPtr w,IntPtr p);
 [StructLayout(LayoutKind.Sequential)]public struct Rect{public int Left,Top,Right,Bottom;}
 [DllImport("user32.dll")]static extern bool EnumWindows(Callback c,IntPtr p);
 [DllImport("user32.dll")]static extern uint GetWindowThreadProcessId(IntPtr w,out uint p);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)]static extern int GetClassName(IntPtr w,StringBuilder s,int n);
 [DllImport("user32.dll")]static extern bool IsWindowVisible(IntPtr w);
 [DllImport("user32.dll")]static extern bool GetClientRect(IntPtr w,out Rect r);
 [DllImport("user32.dll",SetLastError=true)]static extern IntPtr SendMessageTimeout(IntPtr w,uint m,IntPtr a,IntPtr b,uint f,uint t,out IntPtr r);
 public static object[] Read(uint expected){var rows=new List<object>();EnumWindows((w,p)=>{uint pid;GetWindowThreadProcessId(w,out pid);if(pid!=expected)return true;var name=new StringBuilder(256);GetClassName(w,name,256);if(!name.ToString().StartsWith("RemoteOpsStylePreview",StringComparison.Ordinal))return true;IntPtr result;bool responsive=SendMessageTimeout(w,0,IntPtr.Zero,IntPtr.Zero,2,500,out result)!=IntPtr.Zero;int error=responsive?0:Marshal.GetLastWin32Error();Rect rect;GetClientRect(w,out rect);rows.Add(new{handle=w.ToInt64(),class_name=name.ToString(),visible=IsWindowVisible(w),wm_null_responded=responsive,wm_null_error=error,width=rect.Right-rect.Left,height=rect.Bottom-rect.Top});return true;},IntPtr.Zero);return rows.ToArray();}
}
"@
@{identity_verified=$true;process_alive=$true;windows=@([StyledFixtureProbe]::Read(${agent.child.pid}))}|ConvertTo-Json -Depth 5 -Compress`;
    const diagnostic = start('pwsh.exe', ['-NoProfile', '-STA', '-EncodedCommand', Buffer.from(probeScript, 'utf16le').toString('base64')], envFor('agent'));
    diagnostic.child.stdin.end();
    await Promise.race([diagnostic.closed, delay(8000)]);
    if (!diagnostic.ended) { diagnostic.child.kill(); return { diagnostic_timeout: true }; }
    if (diagnostic.code !== 0) return { diagnostic_failed: true, exit_code: diagnostic.code };
    try { return JSON.parse(diagnostic.stdout.replace(/^\uFEFF/, '').trim()); }
    catch { return { diagnostic_invalid_json: true }; }
  }

  async function ui(action='Snapshot',id=0,value){
    if(!agent||agent.ended)throw fail('native_process_ended',{exit_code:agent?.code??null});
    const args=[
      '-NoProfile',
      '-STA',
      '-File',
      options['ui-client'],
      '-ProcessId',
      String(agent.child.pid),
      '-ProcessStartUtc',
      stamp,
      '-ExpectedHash',
      expectedHash,
      '-Action',
      action
    ];
    if(id)args.push('-ControlId',String(id));
    if(action==='Invoke'||action==='Toggle')args.push('-FocusBeforeInvoke');
    if(value!==undefined)args.push('-Value',value);
    const s=start('pwsh.exe',args,envFor('agent'));
    // PowerShell -File can wait for redirected stdin before entering the script.
    s.child.stdin.end();
    await Promise.race([s.closed,delay(15000)]);
    if(!s.ended){
      s.child.kill();
      throw fail('uia_worker_timeout',{action,control_id:id,native:await nativeDiagnostic()});
    }
    if(s.code!==0)throw fail(/desktop_locked_or_unavailable/.test(s.stderr)?'desktop_unavailable':'uia_worker_failed',{
        action,
        control_id:id,
        exit_code:s.code,
        native:await nativeDiagnostic()
      });
    let r;
    try{
      r=JSON.parse(s.stdout.replace(/^\uFEFF/,'').trim());
    }catch{
      throw fail('uia_invalid_json');
    }
    if(action==='Snapshot'){
      // 失败证据只保存固定分类和自有窗口结构，不写状态原文、路径、控制码或指纹。
      const stateText=text(r,300),detailText=text(r,312);
      const f={
        step:current,
        status_connecting:/^(正在连接|Connecting)$/.test(stateText),
        status_failed:/^(连接失败，请查看详情|Connection failed)$/.test(stateText),
        status_stopped:/^(远程协助已停止|Remote assistance stopped)$/.test(stateText),
        tls_preflight_timeout:/TLS 预检超时|TLS preflight timed out/i.test(detailText),
        connect_timeout:/连接.*超时|connection timed out|connect.*timeout/i.test(detailText),
        tls_handshake_failed:/握手|handshake/i.test(detailText),
        native_roots_failed:/系统可信根证书|系统证书存储|native certificate/i.test(detailText),
        server_name_invalid:/服务器名称|invalid.*server.*name|server.*name.*invalid/i.test(detailText),
        windows:(r.windows??[]).map(w=>({class:/^RemoteOpsStylePreview(?:Main|Observer|Dialog)$/.test(w.class)?w.class:'other_own_class',enabled:w.enabled===true,control_ids:(w.controls??[]).map(c=>c.id).filter(id=>Number.isInteger(id)&&id>=200&&id<1000)})),
        setup:Boolean(row(r,502)),
        tls:Boolean(row(r,524)),
        code_available:/^\d{9}$/.test(text(r,303).replace(/\D/g,'')),
        observer:Boolean(observer(r)),
        readonly:row(r,202)?.read_only===true,
        running:Object.fromEntries([211,212,213,214,215].map(x=>[x,/·\s*(\d+)/.exec(text(r,x))?.[1]??'0']))
      };
      if(JSON.stringify(f)!==JSON.stringify(uiFacts.at(-1)))uiFacts.push(f);
      if(uiFacts.length>16)uiFacts.shift();
    }
    return r;
  }

  async function waitUi(name,p,ms=30000){
    return until(name,async()=>{
        const s=await ui();
        return p(s)?s:false;
      },ms);
  }

  async function pairReady(name){
    const s=await waitUi(name,x=>/^\d{9}$/.test(text(x,303).replace(/\D/g,'')),65000);
    return{snapshot:s,code:text(s,303).replace(/\D/g,'')};
  }

  async function startNative(config,report,demo=false){
    agentReport=report;
    agent=start(nativeExe,demo?['--product-demo','--report',report]:['--agent-config',config,'--report',report],envFor('agent'));
    const r=await command(
      'pwsh.exe',
      ['-NoProfile','-Command',`[Diagnostics.Process]::GetProcessById(${agent.child.pid}).StartTime.ToUniversalTime().ToString('o')`],
      envFor('agent')
    );
    stamp=r.stdout.trim();
    check('native_process_identity_bound',/^\d{4}-.*\.\d{7}Z$/.test(stamp));
  }

  async function closeNative(name){
    let s=await ui();
    for(const[id,cancel]of[[524,521],[502,505],[443,442],[425,425]])if(row(s,id)){
      await ui('Invoke',cancel);
      s=await ui();
    }
    await ui('Close');
    await waitUi('native_exit_confirmation',x=>Boolean(row(x,463)));
    await ui('Invoke',463);
    await until(name,()=>agent.ended,10000);
    // 先收集允许公开的数值/布尔证据，再断言退出码；失败退出也不能丢失原因。
    const r=await collectApplicationReport(async()=>JSON.parse(await fs.readFile(agentReport,'utf8')),reports);
    const disposition=classifyCleanQualityExit(r,agent.code);
    if(disposition==='critical'){
      check(name+'_critical_exit_validation',false);
    }
    if(disposition==='quality_only'){
      // 不把退出1或性能失败改成通过；只在已证实正常收尾时继续剩余功能断言。
      const failedChecks=r.checks.filter(item=>!item.passed).map(item=>item.name);
      qualityFailures.push({step:name,exit_code:agent.code,failed_checks:failedChecks,
        max_tick_ms:r.metrics.max_tick_ms,product_page_max_ms:r.metrics.product_page_max_ms,limit_ms:100});
      checks.push({name:name+'_performance',passed:false,category:'performance'});
      console.log(`QUALITY FAIL ${name}；已保留性能失败，正常收尾后继续功能检查。`);
      check(name+'_lifecycle_clean',true);
      return r;
    }
    check(name,agent.code===0);
    check(name+'_application_report_passed',r.passed===true);
    return r;
  }

  const tool=(name,args={})=>mcp.tool(name,{session_id:sid,...args});

  async function pair(code){
    sid=(await mcp.tool('pair_connection',{pairing_code:code}))?.session_id;
    check('only_fixture_agent_paired',typeof sid==='string');
  }

  async function grant(){
    const m=await tool('get_control_mode');
    if(m?.sync_status==='synced'&&m.mode==='full_access')return;
    await ui('Command',5020);
    await waitUi('permissions_ready',s=>row(s,423)?.enabled);
    await ui('Invoke',423);
    await waitUi('grant_confirmation',s=>row(s,443)?.enabled);
    await ui('Invoke',442);
    check('declined_permission_remains_step_by_step',(await tool('get_control_mode')).mode==='step_by_step');
    await ui('Invoke',423);
    await waitUi('grant_confirmation',s=>row(s,443)?.enabled);
    await ui('Invoke',443);
    await until(
      'local_full_control_synced',
      async()=>{
        const x=await tool('get_control_mode');
        return x?.sync_status==='synced'&&x.mode==='full_access'&&x.source==='local';
      }
    );
    if(row(await ui(),425))await ui('Invoke',425);
    check('real_permission_change_synced',true);
  }

  const contains=(marker,ms=20000)=>waitUi('observer_reaches_actual_marker',s=>observer(s)&&text(s,202).includes(marker),ms);

  const noSecrets=s=>check('synthetic_credentials_not_displayed',!secrets.some(v=>rows(s).some(x=>x.text?.includes(v))));
  function checkProcessPrivacy(){
    const result=evaluateProcessOutputPrivacy(states,mcp?.state,{credentialSecret:secret,outputMarker:redactionSecret});
    check('process_privacy_inputs_valid',result.inputs_valid);
    check('ssh_credential_absent_from_all_process_outputs',result.credential_absent_from_all_outputs);
    check('synthetic_output_absent_from_diagnostic_channels',result.output_marker_absent_from_diagnostics);
    if(['all','operations','privacy'].includes(options.scenario)){
      check('controlled_command_value_observed_only_on_mcp_protocol',result.output_marker_seen_in_protocol_stdout);
    }
  }

  try{

    const rel=path.relative(temp,output);
    if(process.platform!=='win32'||path.parse(temp).root.toLowerCase()!=='c:\\'||!rel||rel.startsWith('..')||path.isAbsolute(rel))throw fail('new_c_temp_output_required');

    if(await fs.stat(output).then(()=>true,()=>false))throw fail('output_exists');
    outputApproved=true;

    const setupOnly=options.scenario==='setup',tlsOnceOnly=options.scenario==='tls-once',noMcp=setupOnly||tlsOnceOnly,uiOnly=options.scenario==='ui',privacyOnly=options.scenario==='privacy',skipSsh=noMcp||uiOnly;
    for(const k of(noMcp?['agent','relay','ui-client']:uiOnly?['agent','relay','mcp','ui-client']:['agent','relay','mcp','python','plink','askpass','ui-client']))await fs.access(options[k]);
    for(const k of(noMcp?['agent','relay']:uiOnly?['agent','relay','mcp']:['agent','relay','mcp','plink','askpass']))hashes[k]=hash(await fs.readFile(options[k]));

    check('exact_candidate_hash',hashes.agent===options['expected-hash'].toLowerCase());
    if(!skipSsh)check('official_fixed_plink_hash',hashes.plink===plinkHash);

    fixture=await fs.mkdtemp(path.join(temp,'remoteops-styled-product-e2e-'));

    const clean=Object.fromEntries(Object.entries(process.env).filter(([k])=>!/^(REMOTEOPS_|JLAI_)/i.test(k)));

    const privacy=await command(
      'pwsh.exe',
      [
        '-NoProfile',
        '-Command',
        `$p=${ps(fixture)};$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User;`+'$a=[Security.AccessControl.DirectorySecurity]::new();$a.SetAccessRuleProtection($true,$false);$r=[Security.AccessControl.FileSystemAccessRule]::new($sid,"FullControl","ContainerInherit,ObjectInherit","None","Allow");$a.AddAccessRule($r);Set-Acl -LiteralPath $p -AclObject $a;$b=Get-Acl -LiteralPath $p;if(-not $b.AreAccessRulesProtected -or @($b.Access).Count -ne 1 -or $b.Access[0].IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value -cne $sid.Value){exit 7};[Console]::Out.Write("PRIVATE_OK")'
      ],
      clean
    );

    check('private_acl_before_credentials',privacy.stdout==='PRIVATE_OK');
    dirs={};

    for(const n of(noMcp?['agent-package 中文','agent-local','relay-local','agent-temp','relay-temp','agent-transfer 中文']:[
        'agent-package 中文',
        'mcp-package',
        'agent-local',
        'mcp-local',
        'server-local',
        'relay-local',
        'agent-temp',
        'mcp-temp',
        'server-temp',
        'relay-temp',
        'agent-transfer 中文',
        'mcp-transfer 中文',
        'secrets'
      ])){
      dirs[n]=path.join(fixture,n);
      await fs.mkdir(dirs[n]);
    }

    envFor=role=>({
        ...clean,
        APPDATA:dirs[`${role}-local`],
        LOCALAPPDATA:dirs[`${role}-local`],
        TEMP:dirs[`${role}-temp`],
        TMP:dirs[`${role}-temp`]
      });

    nativeExe=path.join(dirs['agent-package 中文'],'styled-agent.exe');
    const mcpExe=noMcp?null:path.join(dirs['mcp-package'],'remoteops-controller-mcp.exe');
    await fs.copyFile(options.agent,nativeExe);
    if(!noMcp)await fs.copyFile(options.mcp,mcpExe);
    expectedHash=hashes.agent;
    if (options.scenario === 'smoke') {
      await startNative(null, path.join(fixture, 'smoke-report.json'), true);
      const first = await waitUi('smoke_product_demo_snapshot', snapshot => Boolean(row(snapshot, 303)));
      check('smoke_ui_omits_business_demo_controls', !row(first, 220) && !row(first, 221));
      const native = await nativeDiagnostic();
      check('smoke_native_message_pump_responds', native.identity_verified === true && native.windows?.length > 0 && native.windows.every(window => window.wm_null_responded));
      await delay(300);
      const report = await closeNative('smoke_product_demo_exits');
      check('smoke_has_actual_ui_ticks', typeof report.metrics?.ui_ticks === 'number' && report.metrics.ui_ticks > 0);
      performanceCheck('smoke_tick_within_100ms',report.metrics?.max_tick_ms);
      return;
    }

    let passwordFile,ssh,sshReady,fingerprint;
    if(!skipSsh){
    await command(
      'pwsh.exe',
      [
        '-NoProfile',
        '-File',
        path.join(root,'scripts/Copy-BundledPlink.ps1'),
        '-SourcePath',
        options.plink,
        '-DestinationDirectory',
        dirs['agent-package 中文'],
        '-AskpassPath',
        options.askpass
      ],
      envFor('agent')
    );

    check('packaged_plink_exact_hash',hash(await fs.readFile(path.join(dirs['agent-package 中文'],'tools/plink.exe')))===plinkHash);

    passwordFile=path.join(dirs.secrets,'synthetic-value');
    await fs.writeFile(passwordFile,secret,{flag:'wx'});

    const sshState=start(options.python,[path.join(root,'scripts/test-ssh-plink-e2e.py'),'serve'],envFor('server')),replies=[];
    let buffer='';

    sshState.child.stdout.on(
      'data',
      s=>{
        buffer+=s;
        let i;
        while((i=buffer.indexOf('\n'))>=0){
          const line=buffer.slice(0,i);
          buffer=buffer.slice(i+1);
          try{
            replies.push(JSON.parse(line));
          }catch{}
        }
      }
    );
    sshState.child.stdin.write(`${JSON.stringify({password:secret})}\n`);

    ssh={async next(event){
        return until(
          'ssh_fixture_reply',
          ()=>{
            if(sshState.ended)throw fail('ssh_fixture_ended');
            const i=replies.findIndex(x=>x.event===event);
            return i<0?false:replies.splice(i,1)[0];
          },
          10000
        );
      },async stats(){
        sshState.child.stdin.write('{"action":"stats"}\n');
        return(await this.next('stats')).stats;
      }};

    sshReady=await ssh.next('ready');
    fingerprint=`${sshReady.algorithm} SHA256:${createHash('sha256').update(Buffer.from(sshReady.public_key,'base64')).digest('base64').replace(/=+$/,'')}`;

    check(
      'ssh_loopback_key_independently_verified',
      sshReady.host==='127.0.0.1'&&sshReady.username==='remoteops-fixture'&&sshReady.paramiko_version==='4.0.0'&&fingerprint===`${sshReady.algorithm} ${sshReady.fingerprint}`
    );

    const promptSource=path.join(dirs['mcp-package'],'FixturePrompt.cs');
    await fs.writeFile(
      promptSource,
      `using System;using System.IO;using System.Text;using System.Collections.Generic;using System.Web.Script.Serialization;
class FixturePrompt{static int Main(string[] a){try{if(a.Length!=8||a[0]!="--host"||a[1]!="127.0.0.1"||a[2]!="--port"||a[3]!=Environment.GetEnvironmentVariable("REMOTEOPS_TEST_SSH_PORT")||a[4]!="--username"||a[5]!="remoteops-fixture"||a[6]!="--command-sha256"||!System.Text.RegularExpressions.Regex.IsMatch(a[7],"^[0-9a-f]{64}$"))return 3;Console.OutputEncoding=new UTF8Encoding(false);Console.Write(new JavaScriptSerializer().Serialize(new Dictionary<string,object>{{"action","use_once"},{"password",File.ReadAllText(Environment.GetEnvironmentVariable("REMOTEOPS_TEST_PASSWORD_FILE"),new UTF8Encoding(false,true))}}));return 0;}catch{return 2;}}}`
    );

    await command(
      'C:\\Windows\\Microsoft.NET\\Framework64\\v4.0.30319\\csc.exe',
      [
        '/nologo',
        '/target:exe',
        '/r:System.Web.Extensions.dll',
        `/out:${path.join(dirs['mcp-package'],'remoteops-credential-prompt.exe')}`,
        promptSource
      ],
      envFor('mcp')
    );

    }

    const tokens={
      REMOTEOPS_AI_CONTROLLER_TOKEN:randomBytes(32).toString('hex'),
      REMOTEOPS_HUMAN_CONTROLLER_TOKEN:randomBytes(32).toString('hex'),
      REMOTEOPS_CONTROLLER_OWNER_ID:randomUUID()
    };
    tokens.REMOTEOPS_CONTROLLER_TOKEN=tokens.REMOTEOPS_AI_CONTROLLER_TOKEN;

    const relayPort=await port();
    let health=await port();
    while(health===relayPort)health=await port();
    const address=`127.0.0.1:${relayPort}`,cert=path.join(fixture,'relay-cert.pem');

    relay=start(
      options.relay,
      [
        '--bind',
        address,
        '--health-bind',
        `127.0.0.1:${health}`,
        '--tls-cert',
        cert,
        '--tls-key',
        path.join(fixture,'relay-key.pem'),
        '--tls-sans',
        '127.0.0.1,localhost',
        '--state-file',
        path.join(fixture,'relay-state.json'),
        '--heartbeat-seconds',
        '1',
        '--lease-seconds',
        '60'
      ],
      {...envFor('relay'),...tokens}
    );

    await until(
      'local_relay_ready',
      async()=>{
        if(relay.ended)throw fail('relay_ended');
        try{
          await fs.access(cert);
        }catch{
          return false;
        }
        return new Promise(resolve=>{
            const n=net.connect({host:'127.0.0.1',port:relayPort});
            n.setTimeout(200);
            n.once('connect',()=>{
                n.destroy();
                resolve(true);
              });
            n.once('error',()=>resolve(false));
            n.once('timeout',()=>{
                n.destroy();
                resolve(false);
              });
          });
      }
    );

    async function config(name,relayAddress,ca=false){
      const d=path.join(fixture,name);
      await fs.mkdir(d);
      const file=path.join(d,'agent-config.json'),report=path.join(d,'application-report.json');
      await fs.writeFile(
        file,
        JSON.stringify({
            relay:relayAddress,
            server_name:'localhost',
            ...(ca?{ca_cert:cert}:{}),
            state_file:path.join(d,'agent-state.json'),
            transfer_root:dirs['agent-transfer 中文'],
            retry_seconds:1,
            default_full_control:privacyOnly
          }),
        {flag:'wx'}
      );
      return{file,report};
    }

    // 指纹从本次回环 Relay 的实际 PEM 独立计算，不从 UI 或 Agent 回执反推。
    const relayFingerprint=new X509Certificate(await fs.readFile(cert)).fingerprint256;

    async function saveSetupRelay(configuration,prefix,keepExisting=false){
      const readySetup=await waitUi(`${prefix}_ready`,s=>row(s,502)?.enabled);
      if(keepExisting){
        check(`${prefix}_keeps_saved_relay`,text(readySetup,502)===address);
      }else{
        const input=await ui('SetValue',502,address);
        check(`${prefix}_value_pattern_readback_matches`,input.value_matches===true);
      }
      await waitUi(`${prefix}_field_contains_actual_input`,s=>text(s,502)===address);
      await ui('Invoke',506);
      await waitUi(`${prefix}_save_closes_dialog`,s=>!row(s,502));
      const saved=JSON.parse(await fs.readFile(configuration.file,'utf8'));
      check(`${prefix}_saves_private_loopback_relay`,saved.relay===address);
      check(`${prefix}_infers_loopback_tls_server_name`,saved.server_name==='127.0.0.1');
      check(`${prefix}_does_not_persist_unconfirmed_trust`,!saved.ca_cert&&!saved.tls_fingerprint);
    }

    async function verifiedTlsPrompt(prefix){
      const prompt=await waitUi(`${prefix}_tls_prompt`,s=>row(s,524)?.read_only===true&&row(s,523)?.enabled);
      check(`${prefix}_shows_independently_verified_fingerprint`,sameTlsFingerprint(text(prompt,524).split(/\r?\n/).find(line=>line.startsWith('SHA256:'))?.slice(7),relayFingerprint));
      check(`${prefix}_unconfirmed_has_no_pairing_code`,!/^\d{9}$/.test(text(prompt,303).replace(/\D/g,'')));
      return prompt;
    }

    async function cancelTlsPrompt(prefix){
      await ui('Invoke',521);
      const cancelled=await waitUi(`${prefix}_cancel_prompt_closed`,s=>!row(s,524));
      check(`${prefix}_cancel_does_not_connect`,!/^\d{9}$/.test(text(cancelled,303).replace(/\D/g,'')));
    }

    if(tlsOnceOnly){
      // 仅本次信任不能跨进程保留：首轮真实连接，同配置仅重启一次再核验证书提示。
      const configuration=await config('tls-once',address);
      await startNative(configuration.file,configuration.report);
      const prompt=await verifiedTlsPrompt('tls_once');
      check('tls_once_trust_action_enabled',row(prompt,522)?.enabled===true);
      await ui('Invoke',522);
      const connected=await pairReady('tls_once_actual_connection');
      check('tls_once_connected_without_tls_prompt',!row(connected.snapshot,524));
      let stored=JSON.parse(await fs.readFile(configuration.file,'utf8'));
      check('tls_once_does_not_persist_pin_or_ca',!stored.tls_fingerprint&&!stored.ca_cert);
      await closeNative('tls_once_connection_exits');
      stored=JSON.parse(await fs.readFile(configuration.file,'utf8'));
      check('tls_once_exit_keeps_trust_unpersisted',!stored.tls_fingerprint&&!stored.ca_cert);

      await startNative(configuration.file,path.join(path.dirname(configuration.file),'restart-report.json'));
      await verifiedTlsPrompt('tls_once_restart');
      check('tls_once_restart_requires_confirmation',!JSON.parse(await fs.readFile(configuration.file,'utf8')).tls_fingerprint);
      await cancelTlsPrompt('tls_once_restart');
      await closeNative('tls_once_restart_cancel_exits');
      return;
    }

    if(setupOnly){
      // 单个首次设置实例完成取消／重试保存；仅同一配置重启一次，不进入其他业务场景。
      const configuration=await config('setup','');
      await startNative(configuration.file,configuration.report);
      await saveSetupRelay(configuration,'setup');
      await verifiedTlsPrompt('setup');
      await cancelTlsPrompt('setup');
      check('setup_cancel_keeps_trust_unconfirmed',!JSON.parse(await fs.readFile(configuration.file,'utf8')).tls_fingerprint);
      await ui('Command',5022);
      await saveSetupRelay(configuration,'setup_retry',true);
      await verifiedTlsPrompt('setup_retry');
      await ui('Invoke',523);
      const connected=await pairReady('setup_saved_trust_connects');
      check('setup_saved_connection_has_no_tls_prompt',!row(connected.snapshot,524));
      check('setup_saved_pin_matches_relay_certificate',sameTlsFingerprint(JSON.parse(await fs.readFile(configuration.file,'utf8')).tls_fingerprint,relayFingerprint));
      await closeNative('setup_saved_connection_exits');

      await startNative(configuration.file,path.join(path.dirname(configuration.file),'restart-report.json'));
      const restarted=await pairReady('setup_saved_configuration_restarts');
      check('setup_restart_skips_setup_and_tls_prompts',!row(restarted.snapshot,502)&&!row(restarted.snapshot,524));
      check('setup_restart_keeps_independently_verified_pin',sameTlsFingerprint(JSON.parse(await fs.readFile(configuration.file,'utf8')).tls_fingerprint,relayFingerprint));
      await closeNative('setup_saved_configuration_restart_exits');
      return;
    }

    if(options.scenario==='all'){

      const c=await config('tls-cancel','');
      await startNative(c.file,c.report);
      await saveSetupRelay(c,'setup');
      await verifiedTlsPrompt('setup');
      await cancelTlsPrompt('setup');
      await closeNative('tls_cancel_exits');

      const once=await config('tls-once',address);
      await startNative(once.file,once.report);
      await waitUi('tls_once_prompt',s=>row(s,522)?.enabled);
      await ui('Invoke',522);
      await pairReady('tls_once_connects');
      check('tls_once_not_persisted',!JSON.parse(await fs.readFile(once.file,'utf8')).tls_fingerprint);
      await closeNative('tls_once_exits');

      const saved=await config('tls-save',address);
      await startNative(saved.file,saved.report);
      await waitUi('tls_save_prompt',s=>row(s,523)?.enabled);
      await ui('Invoke',523);
      await pairReady('tls_saved_connects');
      check('tls_save_persists_real_pin',sameTlsFingerprint(JSON.parse(await fs.readFile(saved.file,'utf8')).tls_fingerprint,relayFingerprint));
      await closeNative('tls_save_exits');

      await startNative(saved.file,path.join(path.dirname(saved.file),'restart-report.json'));
      const restarted=await pairReady('tls_saved_restarts');
      check('saved_tls_skips_prompt',!row(restarted.snapshot,524));
      await closeNative('tls_restart_exits');

    }

    const operational=await config('operations',address,true);
    await startNative(operational.file,operational.report);
    const ready=await pairReady('real_pairing_ready');

    check(
      'business_has_no_synthetic_controls',
      !row(ready.snapshot,220)&&!row(ready.snapshot,221)&&!/界面预览|Preview/.test(ready.snapshot.windows.find(w=>w.class==='RemoteOpsStylePreviewMain')?.title??'')
    );

    check(
      'compact_business_main_layout',
      ready.snapshot.windows.some(w=>w.class==='RemoteOpsStylePreviewMain'&&w.width===Math.round(500*w.dpi/96)&&w.height===Math.round(375*w.dpi/96))
    );

    const mcpConfig=path.join(dirs['mcp-local'],'mcp-config.json');
    await fs.writeFile(mcpConfig,'{}',{flag:'wx'});

    mcp=new Mcp(start(
        mcpExe,
        [
          '--config',
          mcpConfig,
          '--relay',
          address,
          '--server-name',
          'localhost',
          '--ca-cert',
          cert,
          '--audit-log',
          path.join(dirs['mcp-local'],'audit.jsonl'),
          '--transfer-root',
          dirs['mcp-transfer 中文'],
          '--reconnect-seconds',
          '1'
        ],
        {
          ...envFor('mcp'),
          ...tokens,
          ...(!skipSsh?{REMOTEOPS_TEST_PASSWORD_FILE:passwordFile,REMOTEOPS_TEST_SSH_PORT:String(sshReady.port)}:{})
        }
      ));

    await mcp.request(
      'initialize',
      {
        protocolVersion:'2025-03-26',
        capabilities:{elicitation:{form:{}}},
        clientInfo:{name:'styled-native-loopback-fixture',version:'1'}
      }
    );
    mcp.send({jsonrpc:'2.0',method:'notifications/initialized'});
    await pair(ready.code);

    await until('initial_mode_synced',async()=>{
        const m=await tool('get_control_mode');
        return m?.sync_status==='synced'&&m.mode===(privacyOnly?'full_access':'step_by_step');
      });

    if(options.scenario!=='shutdown'&&!uiOnly&&!privacyOnly){
      check('actual_readonly_cmd_success',succeeded(await tool('run_readonly_command',{shell:'cmd',command:'ver'})));
      check('actual_readonly_ps_success',succeeded(await tool('run_readonly_command',{shell:'windows_power_shell',command:'Get-Date'})));
    }
    if(privacyOnly){
      check('isolated_privacy_fixture_authorized',JSON.parse(await fs.readFile(operational.file,'utf8')).default_full_control===true);
    }else{
      await grant();
      check('temporary_control_not_saved_as_default',JSON.parse(await fs.readFile(operational.file,'utf8')).default_full_control===false);
    }

    if(options.scenario==='all'||uiOnly){
      // 完整业务 UI 验收只使用本次私有配置，不打开资源管理器或写个人剪贴板。
      const targetInfo=await tool('get_target_info');
      check('target_confirms_private_agent_transfer_root',targetInfo.agent_transfer_root_status==='reported'&&samePath(targetInfo.agent_transfer_root,dirs['agent-transfer 中文']));
      await ui('Invoke',307);
      const details=await waitUi('actual_details_ready',s=>Boolean(row(s,406)));
      check('details_shows_exact_confirmed_receive_path',text(details,406)===targetInfo.agent_transfer_root);
      check('details_receive_path_uses_readonly_native_edit',row(details,406)?.read_only===true&&row(details,406)?.type==='ControlType.Edit');
      check('details_folder_action_available',row(details,323)?.enabled===true);
      await ui('Invoke',409);
      await waitUi('actual_details_closed',s=>!row(s,406));

      await ui('Command',5020);
      let permissions=await waitUi('default_permission_setting_ready',s=>row(s,424)?.toggle_state==='Off');
      check('default_control_initially_off',row(permissions,424)?.toggle_state==='Off');
      await ui('Toggle',424);
      await waitUi('default_control_cancel_prompt',s=>row(s,443)?.enabled);
      await ui('Invoke',442);
      permissions=await waitUi('default_control_cancelled',s=>row(s,424)?.toggle_state==='Off'&&!row(s,443));
      check('declined_default_control_not_saved',row(permissions,424)?.toggle_state==='Off'&&JSON.parse(await fs.readFile(operational.file,'utf8')).default_full_control===false);
      await ui('Toggle',424);
      await waitUi('default_control_save_prompt',s=>row(s,443)?.enabled);
      await ui('Invoke',443);
      await waitUi('default_control_saved_on',s=>row(s,424)?.toggle_state==='On'&&!row(s,443));
      check('default_control_saved_to_private_config',JSON.parse(await fs.readFile(operational.file,'utf8')).default_full_control===true);
      await ui('Toggle',424);
      await waitUi('default_control_saved_off',s=>row(s,424)?.toggle_state==='Off');
      check('default_control_revocation_saved',JSON.parse(await fs.readFile(operational.file,'utf8')).default_full_control===false);
      await ui('Invoke',425);
      await waitUi('default_permissions_closed',s=>!row(s,424));
      const currentMode=await tool('get_control_mode');
      check('default_setting_does_not_replace_current_grant',currentMode?.sync_status==='synced'&&currentMode.mode==='full_access');

      check('activity_fixture_command_succeeds',succeeded(await tool('run_command',{shell:'windows_power_shell',command:"[Console]::Out.WriteLine(('STYLED_ACTIVITY_'+'FRAME'))"})));
      await ui('Invoke',308);
      const activity=await waitUi('actual_activity_log_output',s=>row(s,311)?.read_only===true&&text(s,311).includes('STYLED_ACTIVITY_FRAME'));
      check('activity_page_shows_actual_output',text(activity,311).includes('STYLED_ACTIVITY_FRAME'));
      await ui('Invoke',328);
      let filtered=await waitUi('actual_info_filter_selected',s=>text(s,328).startsWith('● ')&&text(s,311).includes('STYLED_ACTIVITY_FRAME'));
      check('activity_info_filter_preserves_actual_output',text(filtered,311).includes('STYLED_ACTIVITY_FRAME'));
      await ui('Invoke',326);
      filtered=await waitUi('actual_success_filter_selected',s=>text(s,326).startsWith('● ')&&text(s,311).includes('Succeeded'));
      check('activity_success_filter_excludes_info_output',!text(filtered,311).includes('STYLED_ACTIVITY_FRAME'));
      await ui('Invoke',324);
      await waitUi('actual_all_log_filter_selected',s=>text(s,324).startsWith('● ')&&text(s,311).includes('STYLED_ACTIVITY_FRAME'));
      await ui('Invoke',329);
      const cleared=await waitUi('actual_activity_logs_cleared',s=>row(s,311)?.read_only===true&&text(s,311)==='');
      check('actual_activity_clear_empties_native_log',text(cleared,311)==='');
      await ui('Invoke',310);
      await waitUi('activity_returns_to_business_main',s=>Boolean(row(s,307))&&!row(s,311));

      const preferencesFile=path.join(path.dirname(operational.file),'native-gui-preferences.json');
      await ui('Command',5002);
      const english=await waitUi('actual_english_main',s=>text(s,307)==='Details');
      let preferences=JSON.parse(await fs.readFile(preferencesFile,'utf8'));
      check('language_menu_updates_ui_and_private_preferences',text(english,307)==='Details'&&preferences.language==='English');
      const lightHandle=row(english,307)?.hwnd;
      await ui('Command',5012);
      const dark=await waitUi('actual_dark_page_rebuilt',s=>Boolean(row(s,307))&&row(s,307)?.hwnd!==lightHandle);
      preferences=JSON.parse(await fs.readFile(preferencesFile,'utf8'));
      check('dark_menu_rebuilds_ui_and_saves_preferences',row(dark,307)?.hwnd!==lightHandle&&preferences.appearance==='Dark');
      const darkHandle=row(dark,307)?.hwnd;
      await ui('Command',5011);
      const light=await waitUi('actual_light_page_rebuilt',s=>Boolean(row(s,307))&&row(s,307)?.hwnd!==darkHandle);
      preferences=JSON.parse(await fs.readFile(preferencesFile,'utf8'));
      check('light_menu_rebuilds_ui_and_saves_preferences',row(light,307)?.hwnd!==darkHandle&&preferences.appearance==='Light');
      await ui('Command',5001);
      await waitUi('actual_chinese_restored',s=>text(s,307)==='连接详情');
      preferences=JSON.parse(await fs.readFile(preferencesFile,'utf8'));
      check('chinese_language_saved_and_restored',preferences.language==='Chinese'&&preferences.appearance==='Light');
    }

    if(uiOnly){
      await closeNative('ui_business_actions_exit');
      return;
    }

    if(privacyOnly){
      const output=await tool('run_command',{shell:'windows_power_shell',command:`[Console]::Out.WriteLine(('password='+'${redactionSecret.slice(0,10)}'+'${redactionSecret.slice(10)}'));[Console]::Out.WriteLine(('PRIVACY_OUTPUT_'+'READY'))`});
      check('controlled_privacy_command_succeeds',succeeded(output));
      check('authorized_command_result_retains_deliberate_value',output.summary?.includes('password='+redactionSecret));
      noSecrets(await contains('PRIVACY_OUTPUT_READY'));
      const target=await tool('get_target_info');
      check('privacy_trust_cli_advertised',typeof target.ssh_trust_cli==='string');
      const trusted=await tool('run_command',{shell:'windows_power_shell',command:`& ${ps(target.ssh_trust_cli)} '--ssh-key-trust' '127.0.0.1' '${sshReady.port}' ${ps(fingerprint)} | Out-String`});
      let receipt;
      try{receipt=JSON.parse(trusted.summary);}catch{throw fail('privacy_trust_receipt_invalid');}
      check('privacy_fixture_key_trusted',succeeded(trusted)&&receipt.status==='trusted');
      const sshResult=await tool('run_ssh',{host:'127.0.0.1',port:sshReady.port,username:sshReady.username,command:'fixture-stream',readonly:false,use_password:true});
      check('privacy_real_password_ssh_succeeds',succeeded(sshResult));
      noSecrets(await contains('FIXTURE-FINAL'));
      await waitUi('privacy_tasks_finished',idle);
      await closeNative('privacy_fixture_exits');
      checkProcessPrivacy();
      return;
    }

    let snapshot;
    if(options.scenario!=='shutdown'){
    snapshot=await ui();
    if(observer(snapshot)){
      await ui('Invoke',205);
      await waitUi('observer_initially_hidden',s=>!observer(s));
    }

    // 只读采样既有有效前台（可为Codex等第三方），不通过聚焦或菜单制造基线。
    const beforeFirst=await ui();
    const before=await ui();
    check('valid_stable_foreground_baseline',before.foreground>0&&beforeFirst.foreground===before.foreground&&before.foreground_metadata?.process_id>0&&before.foreground_metadata.process_id===beforeFirst.foreground_metadata?.process_id&&before.menu_count===0&&!observer(before));

    let firstDone=false;
    const first=track(tool(
        'run_command',
        {shell:'windows_power_shell',command:`[Console]::Out.WriteLine(('STYLED_REAL_FIRST_'+'FRAME'));[Console]::Out.WriteLine(('中文正常'+[char]::ConvertFromUtf32(0x1f50e)));[Console]::Out.WriteLine(('password='+'${redactionSecret.slice(0,10)}'+'${redactionSecret.slice(10)}'));Start-Sleep -Seconds 8;[Console]::Out.WriteLine(('STYLED_REAL_LAST_'+'FRAME'))`}
      ).finally(()=>{
          firstDone=true;
        }));

    const visible=await contains('STYLED_REAL_FIRST_FRAME');
    check('real_first_output_before_completion',!firstDone);
    check('real_observer_auto_readonly',row(visible,202)?.read_only===true);
    check(
      'auto_show_keeps_valid_foreground',
      before.foreground>0&&before.foreground===visible.foreground&&visible.foreground_metadata?.process_id===before.foreground_metadata.process_id&&visible.foreground_metadata?.class_name!=='RemoteOpsStylePreviewObserver'
    );
    check('real_unicode_emoji_preserved',text(visible,202).includes('中文正常🔎')&&!text(visible,202).includes('\ufffd'));
    noSecrets(visible);
    const firstResult=await first;
    check('actual_stream_succeeds',succeeded(firstResult));
    check('authorized_command_result_retains_deliberate_value',firstResult.summary?.includes('password='+redactionSecret));
    await contains('STYLED_REAL_LAST_FRAME');
    await waitUi('first_job_idle',idle);

    // 由本夹具释放并发屏障，确保两项完成确实发生在暂停期间；超时也会自行结束。
    const concurrentRelease=path.join(fixture,'concurrent-shells.release');
    const concurrentCmd=path.join(fixture,'concurrent-shells.cmd');
    await fs.writeFile(concurrentCmd,[
      '@echo off',
      'set "marker=STYLED_CMD_"',
      'echo %marker%FIRST',
      'for /l %%i in (1,1,60) do (',
      `  if not exist "${concurrentRelease}" ping -n 2 127.0.0.1 >nul`,
      ')',
      `if not exist "${concurrentRelease}" exit /b 9`,
      'echo %marker%LAST',
      ''
    ].join('\r\n'),{flag:'wx'});
    const cmd=track(tool('run_command',{shell:'cmd',command:`call "${concurrentCmd}"`}));

    const powershell=track(tool(
        'run_command',
        {shell:'windows_power_shell',command:`[Console]::Out.WriteLine(('STYLED_PS_'+'FIRST'));$deadline=[DateTime]::UtcNow.AddSeconds(60);while(-not [IO.File]::Exists(${ps(concurrentRelease)})){if([DateTime]::UtcNow -ge $deadline){exit 9};Start-Sleep -Milliseconds 100};[Console]::Out.WriteLine(('STYLED_PS_'+'LAST'))`}
      ));

    snapshot=await waitUi('cmd_ps_actual_concurrency',s=>running(s,211,1)&&running(s,212,1)&&text(s,202).includes('STYLED_CMD_FIRST')&&text(s,202).includes('STYLED_PS_FIRST'));
    check('independent_cmd_ps_running_badges',running(snapshot,211,1)&&running(snapshot,212,1));

    const sharedEditHandle=row(snapshot,202)?.hwnd;
    await ui('Invoke',212);
    snapshot=await waitUi('ps_filter_selected',s=>selected(s,'PowerShell'));
    check('ps_filter_excludes_cmd_records',text(snapshot,202).includes('STYLED_PS_FIRST')&&!text(snapshot,202).includes('STYLED_CMD_FIRST'));
    check('filter_keeps_same_native_readonly_edit',row(snapshot,202)?.hwnd===sharedEditHandle&&row(snapshot,202)?.read_only===true);

    await ui('Invoke',204);
    snapshot=await waitUi('actual_view_paused',s=>/继续显示/.test(text(s,204))&&running(s,211,1)&&running(s,212,1));
    check('real_jobs_running_when_view_paused',running(snapshot,211,1)&&running(snapshot,212,1));
    const frozen=text(snapshot,202);
    await fs.writeFile(concurrentRelease,'release',{flag:'wx'});

    check('both_real_shell_commands_succeed',(await Promise.all([cmd,powershell])).every(succeeded));
    snapshot=await waitUi('real_jobs_idle_while_view_paused',idle);
    check('pause_does_not_freeze_actual_job_state',text(snapshot,202)===frozen&&selected(snapshot,'PowerShell'));
    await ui('Invoke',206);
    await contains('STYLED_PS_LAST');

    const persistent=await tool('open_shell',{shell:'windows_power_shell'});
    check('real_persistent_shell_opened',typeof persistent?.shell_id==='string');
    check(
      'persistent_command_succeeds',
      succeeded(await tool('run_command',{shell_id:persistent.shell_id,command:"[Console]::Out.WriteLine('STYLED_PERSISTENT_PS')"}))
    );
    snapshot=await contains('STYLED_PERSISTENT_PS');
    check('persistent_inherits_real_ps_category',selected(snapshot,'PowerShell'));
    await tool('close_shell',{shell_id:persistent.shell_id});

    const version=await command('pwsh.exe',['-NoProfile','-Command','$PSVersionTable.PSVersion.Major'],envFor('agent'));
    if(/^[7-9]/.test(version.stdout.trim())){
      check('actual_ps7_command_succeeds',succeeded(await tool('run_command',{shell:'power_shell',command:"[Console]::Out.WriteLine('STYLED_PS7_FRAME')"})));
      await contains('STYLED_PS7_FRAME');
    }

    await ui('Invoke',210);
    await waitUi('all_selected',s=>selected(s,'全部'));

    const stderr=track(tool(
        'run_command',
        {shell:'windows_power_shell',command:"[Console]::Error.WriteLine(('STYLED_EARLY_'+'STDERR'));Start-Sleep -Seconds 8;[Console]::Out.WriteLine(('STYLED_AFTER_'+'STDERR'))"}
      ));
    snapshot=await contains('STYLED_EARLY_STDERR');
    check('stderr_does_not_end_running_state',running(snapshot,212,1));
    check('stderr_job_actual_success',succeeded(await stderr));
    await waitUi('stderr_job_true_terminal_state',idle);

    check('nonzero_exit_is_real_failure',!succeeded(await tool('run_command',{shell:'cmd',command:'exit /b 7'})));
    await waitUi('failed_job_clears_running',idle);

    // 指纹来自私有SSH服务管道并独立计算，不自动信任任意扫描目标。

    const target=await tool('get_target_info');
    check('actual_trust_cli_advertised',typeof target.ssh_trust_cli==='string');
    const trust=await tool(
      'run_command',
      {shell:'windows_power_shell',command:`& ${ps(target.ssh_trust_cli)} '--ssh-key-trust' '127.0.0.1' '${sshReady.port}' ${ps(fingerprint)} | Out-String`}
    );
    let receipt;
    try{
      receipt=JSON.parse(trust.summary);
    }catch{
      throw fail('trust_receipt_invalid');
    }
    check('fixture_key_trusted_via_real_cli',succeeded(trust)&&receipt.status==='trusted');

    const sshArgs=commandText=>({
        host:'127.0.0.1',
        port:sshReady.port,
        username:sshReady.username,
        command:commandText,
        readonly:false,
        use_password:true
      });

    await ui('Invoke',214);
    await waitUi('ssh_filter_selected',s=>selected(s,'SSH'));
    check('actual_plink_stream_success',succeeded(await tool('run_ssh',sshArgs('fixture-stream'))));
    snapshot=await contains('FIXTURE-FINAL');
    check('actual_ssh_unicode_stderr_visible',text(snapshot,202).includes('FIXTURE-FIRST 中文 😀')&&text(snapshot,202).includes('FIXTURE-STDERR'));
    noSecrets(snapshot);
    check('actual_ssh_rejection_reported',!succeeded(await tool('run_ssh',sshArgs('fixture-reject'))));
    await waitUi('rejected_ssh_idle',idle);

    const sshA=track(tool('run_ssh',sshArgs('fixture-timeout'))),sshB=track(tool('run_ssh',sshArgs('fixture-timeout')));
    snapshot=await waitUi('two_actual_ssh_running',s=>running(s,214,2),20000);
    check('real_ssh_two_running_badge',running(snapshot,214,2));
    await ui('Invoke',212);
    snapshot=await waitUi('ps_selected_during_real_ssh_two',s=>selected(s,'PowerShell')&&running(s,214,2));
    check('selection_independent_of_real_execution',selected(snapshot,'PowerShell')&&running(snapshot,214,2));

    check('actual_connection_close_succeeds',succeeded(await tool('close_connection')));
    sid=null;
    await waitUi('session_close_clears_activity',idle,20000);
    const interrupted=await Promise.race([Promise.allSettled([sshA,sshB]),delay(20000).then(()=>null)]);
    check('two_ssh_settle_after_session_close',interrupted!==null&&interrupted.every(x=>x.status==='fulfilled'&&!succeeded(x.value)));
    const stats=await until('ssh_closed_transports',async()=>{
        const x=await ssh.stats();
        return x.timeout_peer_closed>=2?x:false;
      },15000);
    check('ssh_server_confirms_real_transport_close',stats.timeout_peer_closed>=2);

    const again=await pairReady('pairing_after_session_close');
    await pair(again.code);
    await grant();
    }else{
      // 定向退出复现只测压力/断线/退出，不把省略的核心验收记作通过。
      await ui('Command',5043);
      await waitUi('shutdown_observer_ready',s=>observer(s)&&row(s,202)?.read_only===true);
    }
    await ui('Invoke',210);
    await waitUi('all_for_pressure',s=>selected(s,'全部'));
    check(
      'actual_5000_line_command_succeeds',
      succeeded(await tool(
          'run_command',
          {shell:'windows_power_shell',command:"1..5000 | ForEach-Object {[Console]::Out.WriteLine(('STYLED_BATCH_'+$_))};[Console]::Out.WriteLine(('STYLED_BATCH_LAST_'+'FRAME'))"}
        ))
    );
    snapshot=await contains('STYLED_BATCH_LAST_FRAME');
    check('actual_pressure_tail_and_readonly',row(snapshot,202)?.read_only===true&&text(snapshot,202).includes('STYLED_BATCH_5000'));
    const pressureView=row(snapshot,202);
    check('actual_pressure_has_vertical_scroll_range',(pressureView.native_style&0x200000)!==0&&pressureView.vertical_scroll?.nMax>pressureView.vertical_scroll?.nPage);
    noSecrets(snapshot);

    const disconnected=track(tool(
        'run_command',
        {shell:'windows_power_shell',command:"[Console]::Out.WriteLine('STYLED_DISCONNECT_FIRST');Start-Sleep -Seconds 30;[Console]::Out.WriteLine('STYLED_DISCONNECT_LAST')"}
      ));
    await contains('STYLED_DISCONNECT_FIRST');
    relay.child.kill();
    await relay.closed;
    await waitUi('relay_disconnect_clears_activity',idle,30000);
    const ended=await Promise.race([disconnected.then(value=>({value}),()=>({rejected:true})),delay(20000).then(()=>null)]);
    check('relay_disconnect_settles_pending_request',ended!==null&&(ended.rejected||!succeeded(ended.value)));

    const final=await closeNative('business_exits_after_disconnect');
    performanceCheck('real_ui_tick_within_100ms',final.metrics?.max_tick_ms);
    performanceCheck('real_page_transition_within_100ms',final.metrics?.product_page_max_ms);
    check('actual_observer_text_matches_model',final.metrics?.observer_text_matches_model_on_close===true);
    checkProcessPrivacy();

  }catch(e){
    failure={
      step:current,
      category:e.category??'test_or_fixture_failure',
      details:e.details??{},
      ui:uiFacts,
      native_ended:agent?.ended??null,
      native_exit_code:agent?.code??null,
      diagnostics:{
        agent_panic_seen:/panicked at/.test(agent?.stderr??''),
        tls_worker_panic_seen:/thread ['"]remoteops-native-tls['"] panicked/.test(agent?.stderr??''),
        native_certs_panic_seen:/panicked at/.test(agent?.stderr??'')&&/rustls-native-certs|schannel/.test(agent?.stderr??''),
        uia_worker_failure_seen:e.category==='uia_worker_failed',
        uia_worker_timeout_seen:e.category==='uia_worker_timeout'
      }
    };
    if(!checks.some(x=>x.name===current&&x.passed===false))checks.push({name:current,passed:false});
    console.error(`FAIL ${current} [${failure.category}]；原始输出与机密不写报告。`);
    process.exitCode=1;

  }finally{

    for(const s of [...states].reverse()){
      if(!s.ended)s.child.kill();
      await Promise.race([s.closed,delay(3000)]);
    }
    await Promise.allSettled([...pending]);

    if(fixture){
      if(path.dirname(path.resolve(fixture))!==temp||!path.basename(fixture).startsWith('remoteops-styled-product-e2e-'))throw fail('unsafe_fixture_cleanup_refused');
      await fs.rm(fixture,{
          recursive:true,
          force:true,
          maxRetries:10,
          retryDelay:200
        });
    }

    if(qualityFailures.length>0)process.exitCode=1;
    if(outputApproved){
      await fs.mkdir(path.dirname(output),{recursive:true});
      await fs.writeFile(
        output,
        `${JSON.stringify(
            {
              passed:failure===null&&qualityFailures.length===0,
              functional_checks_passed:failure===null,
              performance_checks_passed:failure===null?qualityFailures.length===0:null,
              quality_failures:qualityFailures,
              executable_sha256:expectedHash??hashes.agent??null,
              scenario:options.scenario,
              checks,
              failure,
              application_reports:reports,
              component_hashes:hashes,
              fixture_loopback_only:true,
              real_business_test:options.scenario!=='smoke',
              personal_clipboard_touched:false,
              real_switch_verified:false,
              physical_serial_verified:false,
              rdp_dpi_verified:false,
              individual_cancellation_verified:false,
              cancellation_note:options.scenario==='privacy'?'仅验证授权命令原始结果、观察窗/诊断脱敏及SSH凭据隔离；只在私有测试配置预设完全控制，不触碰用户配置。':options.scenario==='tls-once'?'本场景仅验证仅本次TLS信任、实际连接、未持久化、同配置单次重启再次确认及取消退出；未执行MCP、Shell、SSH、串口或压力日志。':options.scenario==='setup'?'本场景仅验证首次输入、TLS取消与保存、同配置单次重启和退出；未执行MCP、Shell、SSH、串口或压力日志。':options.scenario==='ui'?'本场景仅验证详情、权限、实际日志页和偏好保存；未执行TLS重启、SSH、串口或压力日志。':options.scenario==='shutdown'?'本场景仅验证压力日志、Relay断线与退出；未执行会话关闭、SSH或单任务取消。':'单任务取消由Agent/Host单测覆盖；此夹具验证关闭连接与断线造成的中断，不冒充单任务取消。'
            },
            null,
            2
          )}\n`
      );
      console.log(`验收记录：${output}`);
    }

  }

}

await main();
