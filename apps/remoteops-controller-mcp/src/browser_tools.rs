//! 浏览器密码的本机输入及可查询的资源准备任务。
use super::*;
use remoteops_domain::EventPayload;
use serde_json::json;

pub(super) struct ResourceTask {
    session: SessionId,
    created: Instant,
    state: tokio::sync::watch::Receiver<ActionOutput>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ResourceStatusInput {
    /// 原启动请求的目标会话。
    pub session_id: String,
    /// 原启动请求标识；查询不会重新启动。
    pub request_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowserPasswordInput {
    /// 精确远端会话。
    pub session_id: String,
    /// 当前会话拥有的浏览器实例。
    pub instance_id: String,
    /// 预期来源，格式为协议、主机及可选端口，不含路径。
    pub expected_origin: String,
    /// 主文档中的唯一密码输入框 CSS 选择器。
    pub selector: String,
    /// AI 已填写的用户名，仅用于本机弹窗核对。
    pub username: String,
}

impl RemoteOpsMcp {
    pub(super) async fn tracked_resource_start(
        &self,
        session: SessionId,
        operation: RemoteOperation,
        authorization: WriteAuthorization,
        wait: bool,
        context: &RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let mut tasks = self.resource_tasks.lock().await;
        tasks.retain(|_, t| {
            t.created.elapsed() < Duration::from_mins(30)
                || t.state.borrow().status == "in_progress"
        });
        if tasks.len() >= 64 {
            return Err("资源任务达到上限，请先核实现有任务".into());
        }
        let events = self.client.subscribe_events();
        let record = serde_json::to_value(&operation).map_err(|_| "资源请求无效".to_owned())?;
        let pending = self
            .client
            .start_execute_with_control(
                &session.to_string(),
                EventSource::Ai,
                operation,
                authorization.approval_id,
                None,
                None,
                authorization.proof,
            )
            .await
            .map_err(application_error)?;
        let id = pending.request_id;
        let initial = ActionOutput {
            status: "in_progress".into(),
            session_id: session.to_string(),
            request_id: Some(id.to_string()),
            exit_code: None,
            summary: "工具准备已开始；请查询同一request_id，禁止再次启动".into(),
            approval_id: None,
            sha256: None,
            details: Some(
                json!({"stage":"waiting","retry_safe":false,"operation_replay":"never_automatic"}),
            ),
        };
        let (sender, mut state) = tokio::sync::watch::channel(initial);
        tasks.insert(
            id,
            ResourceTask {
                session,
                created: Instant::now(),
                state: state.clone(),
            },
        );
        drop(tasks);
        tokio::spawn(run_resource_task(pending, events, sender, session, record));
        let deadline = tokio::time::Instant::now()
            + if wait {
                Duration::from_secs(915)
            } else {
                Duration::from_secs(2)
            };
        let token = context.meta.get_progress_token();
        let mut notification_sequence = 0.0;
        loop {
            let current = state.borrow().clone();
            if current.status != "in_progress" {
                return Ok(Json(current));
            }
            if let Some(token) = &token {
                notification_sequence += 1.0;
                let mut notification = rmcp::model::ProgressNotificationParam::new(
                    token.clone(),
                    notification_sequence,
                );
                notification.message = Some(
                    current
                        .details
                        .as_ref()
                        .map_or_else(|| current.summary.clone(), ToString::to_string),
                );
                let _ = context.peer.notify_progress(notification).await;
            }
            tokio::select! {
                ()=tokio::time::sleep_until(deadline)=>return Ok(Json(state.borrow().clone())),
                ()=context.ct.cancelled()=>return Ok(Json(state.borrow().clone())),
                result=state.changed()=>{ if result.is_err(){return Ok(Json(state.borrow().clone()));} }
            }
        }
    }

    pub(super) async fn resource_status(
        &self,
        input: ResourceStatusInput,
    ) -> Result<Json<ActionOutput>, String> {
        let session = parse_session_id(&input.session_id)?;
        self.ensure_resource_tools(session).await?;
        let _ = self
            .client
            .query_control_state(session)
            .await
            .map_err(application_error)?;
        let id = input
            .request_id
            .parse::<RequestId>()
            .map_err(|_| "request_id无效".to_owned())?;
        let tasks = self.resource_tasks.lock().await;
        let task = tasks
            .get(&id)
            .filter(|t| t.session == session)
            .ok_or_else(|| "未找到本MCP进程中的同会话任务；请核实远端，禁止重新启动".to_owned())?;
        Ok(Json(task.state.borrow().clone()))
    }

    pub(super) async fn browser_password(
        &self,
        input: BrowserPasswordInput,
        context: &RequestContext<RoleServer>,
    ) -> Result<Json<ActionOutput>, String> {
        let session = parse_session_id(&input.session_id)?;
        self.ensure_resource_tools(session).await?;
        let connection = self
            .client
            .list_connections()
            .await
            .into_iter()
            .find(|c| c.session_id == session)
            .ok_or_else(|| "目标会话不存在".to_owned())?;
        if connection.environment.protocol_version < 20
            || connection.credential_encryption_public_key.is_empty()
        {
            return Err("网页登录凭据需要协议20及Agent加密能力".into());
        }
        if self.command_mode == CommandMode::Approval {
            return Err("独立Human审批模式尚不支持网页凭据组合操作；操作未执行".into());
        }
        let authorization = self
            .authorize_mutation(
                context,
                session,
                "核验指定网页密码框并通过本机弹窗填写一次",
                None,
            )
            .await?;
        let approval_id = authorization.approval_id;
        let proof = authorization.proof.clone();
        let prepared = self
            .client
            .execute_with_control(
                &session.to_string(),
                EventSource::Ai,
                RemoteOperation::PrepareBrowserPassword {
                    instance_id: input.instance_id.clone(),
                    expected_origin: input.expected_origin,
                    selector: input.selector,
                    username: input.username,
                },
                approval_id,
                None,
                None,
                proof.clone(),
            )
            .await
            .map_err(application_error)?;
        if prepared.response.error_code.is_some() {
            return Ok(Json(action_output(session, prepared)));
        }
        self.fill_prepared_password(
            session,
            connection,
            input.instance_id,
            prepared,
            WriteAuthorization { approval_id, proof },
        )
        .await
    }

    async fn fill_prepared_password(
        &self,
        session: SessionId,
        connection: remoteops_domain::ConnectionDescriptor,
        instance_id: String,
        prepared: OperationResult,
        authorization: WriteAuthorization,
    ) -> Result<Json<ActionOutput>, String> {
        let approval_id = authorization.approval_id;
        let proof = authorization.proof;
        let target = prepared
            .response
            .details
            .as_ref()
            .ok_or_else(|| "Agent未返回密码目标".to_owned())?;
        let binding = target["binding_id"]
            .as_str()
            .ok_or_else(|| "Agent未返回单次绑定".to_owned())?
            .to_owned();
        let origin = target["origin"]
            .as_str()
            .ok_or_else(|| "Agent未返回实际站点".to_owned())?;
        let username = target["username"].as_str().unwrap_or_default();
        let description = format!(
            "{} | {}",
            connection.alias.as_deref().unwrap_or("远端Agent"),
            target["selector"].as_str().unwrap_or("密码框")
        );
        let digest = sha256_bytes(binding.as_bytes());
        let _prompt_guard = self.credential_prompt_lock.lock().await;
        let prompted = self
            .credential_prompt
            .prompt(CredentialPromptRequest {
                host: origin,
                port: 0,
                username,
                command_sha256: &digest,
                language: "zh-CN",
                browser_target: Some(&description),
            })
            .await?;
        let Some(mut prompted) = prompted else {
            return Ok(Json(ActionOutput {
                status: "cancelled".into(),
                session_id: session.to_string(),
                request_id: None,
                exit_code: None,
                summary: "已取消网页密码输入；未填写、未提交登录".into(),
                approval_id: None,
                sha256: None,
                details: Some(json!({"status":"cancelled"})),
            }));
        };
        let encryption = remoteops_protocol::BrowserCredentialContext {
            protocol_version: PROTOCOL_VERSION,
            agent_instance_id: connection.agent_instance_id,
            session_id: session,
            envelope_id: RequestId::new(),
            instance_id: instance_id.clone(),
            binding_id: binding.clone(),
        };
        let payload = remoteops_protocol::seal_browser_credential(
            &connection.credential_encryption_public_key,
            &connection.credential_encryption_key_id,
            &encryption,
            prompted.password.as_bytes(),
        )
        .map_err(|_| "网页凭据加密失败".to_owned())?;
        prompted.password.zeroize();
        let payload =
            BASE64.encode(serde_json::to_vec(&payload).map_err(|_| "网页凭据编码失败".to_owned())?);
        let operation = RemoteOperation::FillBrowserPassword {
            instance_id,
            binding_id: binding,
        };
        let record = json!({"kind":"fill_browser_password","operation_replay":"never_automatic"});
        let pending = self
            .client
            .start_execute_with_control(
                &session.to_string(),
                EventSource::Ai,
                operation,
                approval_id,
                Some(payload),
                None,
                proof,
            )
            .await;
        let pending = match pending {
            Ok(pending) => pending,
            Err(error) => {
                return Ok(Json(resource_transport_output(
                    session, None, error, record,
                )));
            }
        };
        let request_id = pending.request_id;
        Ok(Json(match pending.wait().await {
            Ok(result) => resource_response_output(session, result, record),
            Err(error) => resource_transport_output(session, Some(request_id), error, record),
        }))
    }
}

async fn run_resource_task(
    pending: remoteops_application::PendingOperation,
    mut events: tokio::sync::broadcast::Receiver<remoteops_domain::RemoteEvent>,
    sender: tokio::sync::watch::Sender<ActionOutput>,
    session: SessionId,
    record: serde_json::Value,
) {
    let id = pending.request_id;
    let result = pending.wait_for(Duration::from_secs(915));
    tokio::pin!(result);
    loop {
        tokio::select! {
            result=&mut result => {
                let output=match result {Ok(result)=>resource_response_output(session,result,record),Err(error)=>resource_transport_output(session,Some(id),error,record)};
                sender.send_replace(output); break;
            }
            event=events.recv()=> {
                if let Ok(event)=event {
                    if event.session_id==session && event.request_id==Some(id) && matches!(event.payload,EventPayload::ResourceToolProgress {..}) {
                        let mut output=sender.borrow().clone();
                        output.details=Some(json!({"progress":event.payload,"updated_at":event.occurred_at,"retry_safe":false,"operation_replay":"never_automatic"}));
                        sender.send_replace(output);
                    }
                } else if matches!(event,Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                sender.send_replace(resource_transport_output(session,Some(id),ApplicationError::Transport("进度连接已关闭，操作结果未知".into()),record)); break;
            }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_password_schema_has_no_secret_input_and_rejects_inline_password() {
        let schema = serde_json::to_value(schemars::schema_for!(BrowserPasswordInput)).unwrap();
        assert!(schema["properties"].get("password").is_none());
        let input = json!({"session_id":"session","instance_id":"instance","expected_origin":"http://router.example","selector":"#password","username":"admin","password":"must-not-enter-tool-arguments"});
        assert!(serde_json::from_value::<BrowserPasswordInput>(input).is_err());
    }
}
