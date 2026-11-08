// 隔离真实 Agent/Relay/MCP 的浏览器准备与加密凭据测试，仅使用合成密码。
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const synthetic = 'Synthetic-RemoteOps-browser-p13-only';

export async function makeBrowserPromptFixture(mcp, fixture, env) {
  const directory = path.join(fixture, 'browser-mcp');
  await fs.mkdir(directory);
  const executable = path.join(directory, 'remoteops-controller-mcp.exe');
  await fs.copyFile(mcp, executable);
  const source = path.join(directory, 'prompt.rs');
  const code = `fn main(){let cancel=std::path::Path::new(${JSON.stringify(path.join(fixture,'cancel-prompt')).replaceAll('\\','\\\\')}).exists();if cancel {println!("{{\\\"action\\\":\\\"cancel\\\",\\\"password\\\":\\\"\\\"}}");}else{println!("{{\\\"action\\\":\\\"use_once\\\",\\\"password\\\":\\\"${synthetic}\\\"}}");}}`;
  // 路径通过Rust原始字面量传入，避免Windows反斜杠转义差异。
  const rust = code.replace(/std::path::Path::new\([^)]*\)/, `std::path::Path::new(r#"${path.join(fixture,'cancel-prompt')}"#)`);
  await fs.writeFile(source, rust);
  const result = spawnSync('rustc', [source, '-o', path.join(directory,'remoteops-credential-prompt.exe')], {env,windowsHide:true,encoding:'utf8',timeout:60000});
  assert.equal(result.status,0,'synthetic prompt fixture build');
  return executable;
}

async function until(name, predicate, milliseconds = 180000) {
  const end=Date.now()+milliseconds;
  while(Date.now()<end){if(await predicate())return;await delay(250);}
  throw new Error(name);
}

export async function runBrowserChecks({agentExe,fixture,transfer,env,relay,cert,client,call,http,check,report}) {
  let agent, server;
  let raw=''; let submitted=false;
  try {
    report.browser_step="agent_start";
    const config=path.join(fixture,'browser-agent.json');
    await fs.writeFile(config,JSON.stringify({relay,server_name:'localhost',ca_cert:cert,state_file:path.join(fixture,'agent-state.json'),transfer_root:path.join(fixture,'agent-transfer'),retry_seconds:1,default_full_control:true}));
    agent=spawn(agentExe,['--config',config],{env,cwd:fixture,windowsHide:true,stdio:['ignore','pipe','pipe']});
    for(const stream of [agent.stdout,agent.stderr])stream.on('data',chunk=>{raw+=chunk.toString();});
    let code;
    report.browser_step='pairing_wait';
    await until('browser_agent_pairing_ready',async()=>{code=/控制码[：:]\s*(\d[\d -]{7,14}\d)/u.exec(raw)?.[1]?.replace(/\D/g,'');return code?.length===9;});
    report.browser_step='pairing';
    const paired=await call('pair_connection',{pairing_code:code});const session=paired.session_id;
    check('browser_fixture_paired',typeof session==='string');
    const tool=(name,args={})=>call(name,{session_id:session,...args});
    report.browser_step='control_sync';
    await until('browser_fixture_full_control',async()=>{const state=await tool('get_control_mode');return state.mode==='full_access'&&state.sync_status==='synced';});
    report.browser_step='package_setup';
    const repository=path.resolve(import.meta.dirname,'../..');
    const site=path.join(repository,'artifacts/resource-candidates/site/remoteops/v1');
    const index=JSON.parse(await fs.readFile(path.join(site,'index.json'),'utf8'));
    const descriptor=structuredClone(index.tools.find(t=>t.id==='agent-browser'));
    await fs.copyFile(path.join(site,descriptor.package.url),path.join(transfer,'browser.zip'));
    let snapshot=await http('/api/admin/tools');let revision=snapshot.revision;
    report.browser_step='package_upload';
    const uploaded=await call('upload_relay_tool_package',{local_path:'browser.zip',expected_revision:revision});
    check('browser_package_uploaded',uploaded.error===null);revision=uploaded.revision;
    descriptor.requires_desktop=false;descriptor.platforms.forEach(p=>p.status='pending');
    descriptor.package.url=`relay://packages/${descriptor.package.sha256}.zip`;
    const definition={id:descriptor.id,name:'isolated browser',purpose:'synthetic login validation',instructions:'test only',examples:[],dependencies:[],enabled:false,versions:[{version:descriptor.version,packages:[{source:{kind:'hosted'},tool:descriptor,file_verified:false,tests:[]}]}]};
    const change=async command=>{const result=await http('/api/admin/tools','POST',{request_id:crypto.randomUUID(),expected_revision:revision,command});assert.equal(result.error,null,'browser catalog mutation');revision=result.revision;};
    report.browser_step='catalog_register';
    await change({action:'put_tool',tool:definition});
    await change({action:'set_policy',enabled:true});await change({action:'set_tool_enabled',tool_id:descriptor.id,enabled:true});
    await until('browser_catalog_synced',async()=>{const tools=await tool('list_resource_tools');return tools.details?.tools?.some(t=>t.tool.id===descriptor.id);});
    report.browser_step='unverified_start';
    const refused=await tool('start_resource_tool',{tool_id:descriptor.id,version:descriptor.version,sha256:descriptor.package.sha256,wait_for_completion:false});
    check('normal_unverified_start_refused',refused.status!=='completed'&&JSON.stringify(refused).includes('platform_unverified'));
    report.browser_step='validation_start';
    const notifications=[];
    const startedRaw=await client.callTool({name:'start_resource_tool',arguments:{session_id:session,tool_id:descriptor.id,version:descriptor.version,sha256:descriptor.package.sha256,validation_mode:true,wait_for_completion:false}},undefined,{timeout:30000,onprogress:p=>notifications.push(p)});
    let started=startedRaw.structuredContent??JSON.parse(startedRaw.content.find(c=>c.type==='text').text);
    const request=started.request_id;const stages=new Set();let updates=0;
    await until('browser_prepare_completed',async()=>{
      if(started.details?.progress){stages.add(started.details.progress.stage);updates++;}
      if(started.status!=='in_progress')return true;
      started=await tool('get_resource_tool_status',{request_id:request});return false;
    });
    check('same_request_completed',started.status==='completed'&&started.request_id===request);
    check('download_status_query_observed',stages.has('download')&&updates>0);
    check('mcp_standard_progress_notified',notifications.length>0);
    report.browser_stages=[...stages];report.browser_progress_notifications=notifications.length;
    report.browser_step='browser_operations';
    const instance=started.details.instance_id;
    const statusAgain=await tool('get_resource_tool_status',{request_id:request});
    check('status_queries_do_not_restart',statusAgain.details.instance_id===instance);
    server=createServer(async(req,res)=>{
      if(req.method==='POST'){let body='';for await(const chunk of req)body+=chunk;submitted=new URLSearchParams(body).get('password')===synthetic;res.end('<title>Fixture signed in</title>');}
      else res.end('<title>Browser credential fixture</title><form method="POST"><label>User<input name="username" id="username"></label><label>Password<input type="password" name="password" id="password"></label><button>Login</button></form>');
    });
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));const origin=`http://127.0.0.1:${server.address().port}`;
    const browser=(tool_name,args)=>tool('call_resource_tool',{instance_id:instance,tool_name,arguments:args});
    await browser('browser_open',{url:origin});
    await browser('browser_eval',{script:"document.querySelector('#username').value='fixture-user'"});
    const passwordArgs={instance_id:instance,expected_origin:origin,selector:'#password',username:'fixture-user'};
    const cancelledFile=path.join(fixture,'cancel-prompt');await fs.writeFile(cancelledFile,'cancel');
    const cancelled=await tool('fill_browser_password',passwordArgs);check('native_prompt_cancel_no_fill',cancelled.status==='cancelled');await fs.unlink(cancelledFile);
    report.browser_step='secret_fill';
    const filled=await tool('fill_browser_password',passwordArgs);
    check('encrypted_password_roundtrip_filled',filled.status==='completed'&&filled.details.status==='filled');
    check('password_fill_does_not_submit',!submitted);
    const value=await browser('browser_eval',{script:"document.querySelector('#password').value"});
    check('subsequent_browser_output_redacted',!JSON.stringify(value).includes(synthetic));
    await browser('browser_eval',{script:"document.querySelector('form').requestSubmit(); true"});
    await until('synthetic_login_submitted',async()=>submitted,10000);check('synthetic_login_received_correct_password',submitted);
    await tool('stop_resource_tool',{instance_id:instance});
    check('agent_output_contains_no_password',!raw.includes(synthetic));
    const audit=await fs.readFile(path.join(fixture,'audit.jsonl'),'utf8');check('audit_contains_no_password',!audit.includes(synthetic));
  } catch(error) { report.browser_failure_name=error?.code??error?.name; report.browser_failure_message=String(error?.message??'').replaceAll(synthetic,'[hidden]').slice(0,200); throw error; } finally {
    if(server)await new Promise(resolve=>server.close(resolve));
    if(agent&&agent.exitCode===null){agent.kill();await new Promise(resolve=>agent.once('close',resolve));}
  }
}
