//! 所有远程命令、串口和审批均走原有人工控制器的策略层。
use super::{Events,message};
use anyhow::{Result,bail,anyhow,Context};
use base64::{Engine as _,engine::general_purpose::STANDARD};
use remoteops_application::RelayClient;
use remoteops_domain::{ApprovalId,ApprovalState,EventPayload,EventSource,RemoteEvent,RemoteOperation,RequestId,SerialSettings,SessionId,ShellKind};
use serde_json::{json,Value};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct Tools {
    approvals:HashMap<(SessionId,ApprovalId),Pending>,
    requests:HashMap<(SessionId,RequestId),RemoteOperation>,
}
struct Pending { operation:RemoteOperation, continuation:bool, payload:Option<String> }

impl Tools {
    pub fn event(&mut self,ev:RemoteEvent,out:&Events) {
        let session=ev.session_id;
        let key=ev.request_id.map(|id|(session,id));
        let text=match ev.payload {
            EventPayload::OperationRequested{operation}=>{
                if let Some(key)=key {self.requests.insert(key,operation);}
                return;
            }
            EventPayload::ApprovalRequired{approval_id,reason}=>{
                if let Some(op)=key.and_then(|k|self.requests.get(&k)) {
                    if let std::collections::hash_map::Entry::Vacant(entry)=self.approvals.entry((session,approval_id)){
                        entry.insert(Pending{operation:op.clone(),continuation:false,payload:None});
                        let _=out.try_send(json!({"kind":"approval","session":session,"id":approval_id,"text":format!("{reason}\n{op:?}")}));
                    }
                }
                return;
            }
            EventPayload::OutputChunk{text,..}=>text,
            EventPayload::OperationFailed{message,..}=>{if let Some(k)=key {self.requests.remove(&k);} message}
            EventPayload::OperationCompleted{summary,..}=>{if let Some(k)=key {self.requests.remove(&k);} summary}
            EventPayload::OperationCancelled=>{if let Some(k)=key {self.requests.remove(&k);} "操作已取消".into()}
            EventPayload::SessionDisconnected|EventPayload::SessionClosed=>{
                self.requests.retain(|(s,_),_|*s!=session);
                self.approvals.retain(|(s,_),_|*s!=session);
                "现场连接已断开".into()
            }
            _=>return
        };
        let _=out.try_send(json!({"kind":"output","session":session,"text":text}));
    }

    pub async fn command(&mut self,client:&RelayClient,cmd:&Value,out:&Events)->Result<()> {
        let session:SessionId=serde_json::from_value(cmd["session"].clone())?;
        let target=session.to_string();
        if cmd["op"]=="decide"{
            let id:ApprovalId=serde_json::from_value(cmd["id"].clone())?;
            let approved=cmd["approved"].as_bool().context("缺少审批决定")?;
            let pending=self.approvals.get(&(session,id)).context("审批已结束")?;
            let result=client.decide_approval(session,pending.operation.clone(),id,approved).await?;
            let pending=self.approvals.remove(&(session,id)).context("审批已结束")?;
            let _=out.try_send(json!({"kind":"decided","session":session,"id":id,"text":result.reason}));
            if approved && result.state==ApprovalState::Approved && pending.continuation {
                execute(client,session,pending.operation,Some(id),pending.payload,out).await?;
            }
            return Ok(())
        }
        let required=|key:&str|->Result<String>{cmd[key].as_str().filter(|s|!s.trim().is_empty()).map(str::to_owned).ok_or_else(||anyhow!("缺少操作参数 {key}"))};
        let (op,payload,ask)=match cmd["op"].as_str(){
            Some("shell")=>{
                let readonly=cmd["readonly"].as_bool().unwrap_or(false);
                (RemoteOperation::RunCommand{shell:ShellKind::WindowsPowerShell,command:required("command")?,readonly},None,!readonly)
            }
            Some("ports")=>(RemoteOperation::ListSerial,None,false),
            Some("openPort")=>{
                let settings=SerialSettings{baud_rate:serde_json::from_value(cmd["baud"].clone())?,..Default::default()};
                let writable=cmd["writable"].as_bool().unwrap_or(false);
                (RemoteOperation::OpenSerial{port_name:required("port")?,settings,writable},None,writable)
            }
            Some("writePort")=>{
                let text=required("text")?; let bytes=text.as_bytes();
                (RemoteOperation::WriteSerial{serial_session_id:required("serial")?,byte_count:bytes.len(),sha256:remoteops_audit::sha256_bytes(bytes)},Some(STANDARD.encode(bytes)),true)
            }
            Some("closePort")=>(RemoteOperation::CloseSerial{serial_session_id:required("serial")?},None,false),
            _=>bail!("未知人工操作")
        };
        if ask {
            let approval=client.request_approval(&target,op.clone()).await?;
            match approval.state{
                ApprovalState::Pending=>{
                    let id=approval.approval_id.context("缺少审批编号")?;
                    let text=format!("{}\n{op:?}",approval.reason);
                    self.approvals.insert((session,id),Pending{operation:op,continuation:true,payload});
                    out.try_send(json!({"kind":"approval","session":session,"id":id,"text":text})).map_err(|_|anyhow!("无法显示审批"))?;
                    return Ok(());
                }
                ApprovalState::NotRequired=>{}
                _=>bail!("{}",approval.reason)
            }
        }
        execute(client,session,op,None,payload,out).await
    }
}

async fn execute(client:&RelayClient,session:SessionId,op:RemoteOperation,id:Option<ApprovalId>,payload:Option<String>,out:&Events)->Result<()> {
    let opened=matches!(op,RemoteOperation::OpenSerial{..});
    let closed=matches!(op,RemoteOperation::CloseSerial{..});
    let listed=matches!(op,RemoteOperation::ListSerial);
    let result=client.execute(&session.to_string(),EventSource::Human,op,id,payload,None).await?.response;
    if opened || closed{
        let serial=result.details.as_ref().and_then(|v|v.get("serial_session_id")).and_then(Value::as_str).unwrap_or("");
        if opened && serial.is_empty(){bail!("现场未确认打开串口")}
        let _=out.try_send(json!({"kind":"serial","session":session,"serial":serial,"text":result.summary}));
    }else if listed{
        let _=out.try_send(json!({"kind":"ports","session":session,"items":result.details,"text":result.summary}));
    }else{let _=out.try_send(message("status",&result.summary));}
    Ok(())
}
