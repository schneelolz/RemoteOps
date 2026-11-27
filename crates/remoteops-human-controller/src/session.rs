use super::{Events,Video,ReleaseSignal,desktop::Desktop,message,tools::Tools};
use anyhow::{Result,Context,bail};
use remoteops_application::{ControllerKind,RelayClient,RelayClientConfig};
use remoteops_domain::{ConnectionState,ControllerInstanceId,PairingCode,PermissionMode,SessionId};
use serde_json::{Value,json};
use std::{env,path::PathBuf,time::Duration};
use tokio::sync::{mpsc,oneshot};

fn config(v:&Value)->Result<RelayClientConfig>{
    let field=|key:&str,env_name:&str| v[key].as_str().filter(|s|!s.is_empty()).map(str::to_owned)
        .or_else(||env::var(env_name).ok().filter(|s|!s.is_empty()));
    Ok(RelayClientConfig{
        relay_address:field("relay","REMOTEOPS_RELAY").context("请设置 Relay 地址")?,
        server_name:field("server","REMOTEOPS_SERVER_NAME").context("请设置 TLS 服务名")?,
        ca_certificate:field("ca","REMOTEOPS_CA_CERT").map(PathBuf::from),
        tls_fingerprint:None,
        audit_log:Some(remoteops_audit::default_audit_log_path()),
        controller_kind:ControllerKind::Human,
        owner_id:field("owner","REMOTEOPS_CONTROLLER_OWNER_ID").context("请配置人工身份所属 Owner")?.parse().context("Owner 格式不正确")?,
        permission_mode:PermissionMode::ApprovalRequired,
        authentication_token:field("token","REMOTEOPS_HUMAN_CONTROLLER_TOKEN").context("请配置人工控制端凭据")?,
        reconnect_delay:Duration::from_secs(2),
    })
}
fn device(c:&remoteops_domain::ConnectionDescriptor, connected:bool, desktop:bool)->Value {
    json!({"session":c.session_id,"device_id":c.agent_instance_id,"name":c.display_name(),"hostname":c.hostname,"online":connected && c.state==ConnectionState::Online,
        "desktop":desktop && c.environment.tools.iter().any(|t|t.name=="remoteops-desktop-v1" && t.available),
        "writable":c.permission_mode!=PermissionMode::ReadOnly})
}
async fn devices(client:&RelayClient)->Value {
    let connected=client.is_connected();
    let items=client.list_connections().await.iter().map(|c|device(c,connected,client.supports_desktop())).collect::<Vec<_>>();
    json!({"kind":"devices","items":items})
}
pub(super) async fn run(options:Value,mut commands:mpsc::Receiver<Value>,out:Events,video:Video,mut stop:oneshot::Receiver<()>,release:ReleaseSignal)->Result<()> {
    let config=config(&options)?;
    let connect=RelayClient::connect(config,ControllerInstanceId::new());
    let client=tokio::select!{_= &mut stop=>return Ok(()),r=connect=>r?};
    let (tools_tx,mut tools_rx)=mpsc::channel::<Value>(16);
    let tool_client=client.clone();let tool_out=out.clone();
    let mut events=client.subscribe_events();
    let tools=tokio::spawn(async move{
        let mut state=Tools::default();
        loop{
            tokio::select!{
                cmd=tools_rx.recv()=>{
                    let Some(cmd)=cmd else {break};
                    let action=async {
                        if !tool_client.is_connected(){bail!("Relay 已离线，请重新操作")}
                        match cmd["op"].as_str(){
                            Some("pair")=>{
                                let code:PairingCode=cmd["code"].as_str().context("请输入控制码")?.parse()?;
                                let grouped=code.display_grouped();
                                let paired=tool_client.pair(code).await?;
                                let _=tool_out.try_send(json!({"kind":"paired","pair_code":grouped,"session":paired.session_id,"device":device(&paired,tool_client.is_connected(),tool_client.supports_desktop()),"text":"已连接"}));
                            }
                            Some("disconnect")=>{
                                let session:SessionId=serde_json::from_value(cmd["session"].clone())?;
                                tool_client.release_binding(session).await?;
                                let _=tool_out.try_send(message("status","本次人工运维已断开"));
                            }
                            _=>state.command(&tool_client,&cmd,&tool_out).await?
                        }
                        Ok::<_,anyhow::Error>(())
                    }.await;
                    if let Err(err)=action{let _=tool_out.try_send(json!({"kind":"error","session":cmd["session"],"op":cmd["op"],"text":err.to_string()}));}
                }
                ev=events.recv()=>match ev{
                    Ok(ev)=>state.event(ev,&tool_out),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{
                        state=Tools::default();
                        let _=tool_out.try_send(message("error","操作记录更新中断，请重新发起未完成的审批"));
                    }
                    Err(_)=>break
                }
            }
        }
    });
    let mut desktop:Option<Desktop>=None;
    let mut timer=tokio::time::interval(Duration::from_secs(1));
    let mut prev=Value::Null;
    let mut online=None;
    loop{
        if desktop.as_ref().is_some_and(Desktop::finished){desktop=None}
        tokio::select!{
            _=&mut stop=>break,
            _=timer.tick()=>{
                let current=client.is_connected();
                if online!=Some(current){
                    let text=if current{"Relay 已连接"}else{"Relay 已断开，正在重新连接"};
                    let release = options["version"].as_str().and_then(|v| client.published_version(
                        if options["managed"].as_bool() == Some(true) { "jlai-human-desktop" } else { "remoteops-human-desktop" }, v));
                    let update = release.filter(|(_,status)| *status == "update_available").map(|(v,_)| v);
                    if out.try_send(json!({"kind":"connection","online":current,"text":text,"update":update})).is_ok(){
                        online=Some(current);
                    }
                }
                let list=devices(&client).await;
                if list!=prev && out.try_send(list.clone()).is_ok(){prev=list}
            }
            cmd=commands.recv()=>{
                let Some(cmd)=cmd else{break};
                let action=async{
                    match cmd["op"].as_str(){
                        Some("refresh")=>{prev=Value::Null;}
                        Some("open")=>{
                            let session:SessionId=serde_json::from_value(cmd["session"].clone())?;
                            if desktop.is_some(){bail!("请先关闭当前桌面窗口")}
                            let mode=cmd.get("quality").map(|v| serde_json::from_value(v.clone())).transpose()?.unwrap_or_default();
                            desktop=Some(Desktop::start(client.clone(),session,cmd["view"].as_u64().unwrap_or(0),mode,cmd["backend"].as_str().unwrap_or("auto"),out.clone(),video.clone(),release.subscribe()));
                        }
                        Some("end"|"disconnect")=>{
                            let session:SessionId=serde_json::from_value(cmd["session"].clone())?;
                            if desktop.as_ref().is_some_and(|d|d.session==session && (cmd["op"]=="disconnect" || cmd["view"].as_u64().unwrap_or(0)==d.view)) {
                                if let Some(d)=desktop.take(){d.end().await;}
                            }
                            if cmd["op"]=="disconnect"{tools_tx.try_send(cmd.clone()).context("人工工具忙")?;}
                        }
                        Some("quality")=>{
                            let session:SessionId=serde_json::from_value(cmd["session"].clone())?;
                            if let Some(d)=desktop.as_ref().filter(|d|d.session==session && cmd["view"].as_u64()==Some(d.view)) {
                                d.picture.send_replace(serde_json::from_value(cmd["quality"].clone())?);
                            }
                        }
                        Some("input"|"control"|"release"|"clipboard")=>{
                            let session:SessionId=serde_json::from_value(cmd["session"].clone())?;
                            if let Some(d)=desktop.as_ref().filter(|d|d.session==session && cmd["view"].as_u64().unwrap_or(0)==d.view) {
                                if d.commands.try_send(cmd.clone()).is_err(){
                                    release.send_replace(Some((d.session,d.view)));
                                }
                            }
                        }
                        _=>{tools_tx.try_send(cmd.clone()).context("人工工具忙")?;}
                    }
                    Ok::<_,anyhow::Error>(())
                }.await;
                if let Err(e)=action{let _=out.try_send(json!({"kind":"error","session":cmd["session"],"view":cmd["view"],"op":cmd["op"],"text":e.to_string()}));}
            }
        }
    }
    if let Some(d)=desktop{d.end().await;}
    tools.abort();let _=tools.await;
    Ok(())
}
