//! 工具库管理、目录同步和有界托管包传输。
use super::{
    AgentInstanceId, Arc, BTreeMap, ControllerInstanceId, Mutex, Relay, RelayState,
    RemoteOperation, RemoteRequest, RequestId, Sender, SessionId, StoreError, Uuid, WireMessage,
    anyhow, append_audit, bail, time,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use remoteops_tool_center::{
    ToolCatalog, ToolCenter, ToolCenterError, ToolCenterRequest, ToolCenterResponse,
};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

pub(super) type ManagementLeases =
    Arc<std::sync::Mutex<BTreeMap<Uuid, Vec<std::sync::Weak<std::sync::RwLock<bool>>>>>>;
#[derive(Default)]
pub(super) struct ToolRelayState {
    applied: BTreeMap<AgentInstanceId, (u64, u64, Option<String>)>,
    starts: BTreeMap<RequestId, (SessionId, String, String, String)>,
    instances: BTreeMap<(SessionId, String), (String, String, String)>,
}
fn failure(request_id: String, code: &str, message: &str) -> ToolCenterResponse {
    ToolCenterResponse {
        request_id,
        revision: 0,
        error: Some(ToolCenterError {
            code: code.into(),
            message: message.into(),
        }),
        data: serde_json::Value::Null,
    }
}
fn send_response(sender: &Sender, response: ToolCenterResponse) {
    let request_id = response.request_id.clone();
    if let Ok(response) = serde_json::to_value(response) {
        let _ = sender.send(WireMessage::ToolCenterResponse {
            request_id,
            response,
        });
    }
}
impl Relay {
    pub(crate) fn tool_center(&self) -> anyhow::Result<&Arc<ToolCenter>> {
        self.tool_center
            .as_ref()
            .ok_or_else(|| anyhow!("tool_center_unavailable: 工具中心未配置"))
    }
    pub(crate) async fn set_mcp_tool_management(
        &self,
        id: Uuid,
        enabled: bool,
    ) -> Result<bool, StoreError> {
        let _gate = self.enrollment_gate.lock().await;
        if !enabled {
            self.cancel_tool_management(id).await;
        }
        let store = self
            .enrollment_store
            .clone()
            .ok_or(StoreError::NotConfigured)?;
        let changed = tokio::task::spawn_blocking(move || store.set_tool_management(id, enabled))
            .await
            .map_err(|_| StoreError::NotConfigured)??;
        append_audit(
            &mut *self.state.lock().await,
            "tool_management_permission",
            Some(id.to_string()),
            true,
            "admin",
            if enabled {
                "已授予工具管理权限"
            } else {
                "已撤销工具管理权限"
            },
        );
        Ok(changed)
    }
    pub(super) async fn cancel_tool_management(&self, id: Uuid) {
        let leases = self
            .tool_management_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
            .unwrap_or_default();
        let _ = tokio::task::spawn_blocking(move || {
            for lease in leases.into_iter().filter_map(|lease| lease.upgrade()) {
                *lease
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
            }
        })
        .await;
    }
    pub(crate) async fn admin_tools_request(
        &self,
        actor: &str,
        request: ToolCenterRequest,
    ) -> ToolCenterResponse {
        let Ok(center) = self.tool_center() else {
            return failure(request.request_id, "unavailable", "工具中心未配置");
        };
        self.ensure_catalog_push();
        let mut response = center.execute(actor, request).await;
        if response.error.is_none() && response.data.get("tools").is_some() {
            let sync = self.tool_sync_status().await;
            response.data["nodes"] = sync["nodes"].clone();
        }
        response
    }
    fn ensure_catalog_push(&self) {
        let Some(center) = &self.tool_center else {
            return;
        };
        if self
            .tool_catalog_started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let mut changes = center.subscribe();
        let state = self.state.clone();
        tokio::spawn(async move {
            loop {
                match changes.recv().await {
                    Ok(catalog) => broadcast_catalog(&state, catalog).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // 丢失过渡开关时断开旧连接，强制停止实例后重新取得权威目录。
                        let state = state.lock().await;
                        for agent in state
                            .agents
                            .values()
                            .filter(|a| a.hello.protocol_version >= 19)
                        {
                            if let Some(sender) = &agent.sender {
                                sender.slow_consumer.notify_one();
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
    pub(super) fn send_tool_catalog(&self, sender: &Sender) {
        self.ensure_catalog_push();
        if let Some(center) = &self.tool_center {
            let _ = sender.send(WireMessage::ToolCenterCapabilities { schema_version: 1 });
            if let Ok(catalog) = serde_json::to_value(center.cached_catalog()) {
                let _ = sender.send(WireMessage::ToolCatalog { catalog });
            }
        }
    }
    pub(super) async fn record_catalog_applied(
        &self,
        id: AgentInstanceId,
        generation: u64,
        revision: u64,
        error: Option<String>,
    ) {
        let mut state = self.state.lock().await;
        if state
            .agents
            .get(&id)
            .is_some_and(|agent| agent.connection_generation == generation)
        {
            // 不记录对端自由文本，避免把包地址或输出带入后台。
            state.tool_state.applied.insert(
                id,
                (
                    generation,
                    revision,
                    error.map(|_| "catalog_apply_failed".into()),
                ),
            );
        }
    }
    pub(crate) async fn tool_sync_status(&self) -> serde_json::Value {
        let state = self.state.lock().await;
        let revision = self
            .tool_center
            .as_ref()
            .map_or(0, |center| center.cached_catalog().revision);
        let nodes = state.agents.iter().map(|(id, agent)| {
            let applied = state.tool_state.applied.get(id).filter(|(generation, _, _)| *generation == agent.connection_generation);
            let supported = agent.hello.protocol_version >= 19;
            // 按本次连接的应用回执判断，修订号相等不能掩盖应用失败。
            let sync_status = if !supported {
                "upgrade_required"
            } else if !agent.ready {
                "offline"
            } else if applied.is_some_and(|(_, _, error)| error.is_some()) {
                "apply_failed"
            } else if applied.is_some_and(|(_, rev, _)| *rev == revision) {
                "synced"
            } else if applied.is_some() {
                "update_pending"
            } else {
                "pending"
            };
            serde_json::json!({"agent_instance_id":id,"hostname":agent.hello.hostname,"supported":supported,"online":agent.ready,"applied_revision":applied.map(|(_,rev,_)|rev),"synced":sync_status == "synced", "sync_status":sync_status})
        }).collect::<Vec<_>>();
        serde_json::json!({"revision":revision,"nodes":nodes})
    }
    pub(super) async fn dispatch_tool_management(
        &self,
        id: ControllerInstanceId,
        generation: u64,
        request_id: String,
        request: serde_json::Value,
        sender: Sender,
        busy: Arc<tokio::sync::Semaphore>,
    ) {
        let registration_gate = self.enrollment_gate.lock().await;
        let identity = {
            let state = self.state.lock().await;
            state
                .controllers
                .get(&id)
                .filter(|c| c.connection_generation == generation && c.protocol_version >= 19)
                .and_then(|c| c.installation_id)
        };
        let authorized = if let (Some(id), Some(store)) = (identity, self.enrollment_store.clone())
        {
            tokio::task::spawn_blocking(move || store.has_tool_management(id).unwrap_or(false))
                .await
                .unwrap_or(false)
        } else {
            false
        };
        let Some(installation) = identity.filter(|_| authorized) else {
            send_response(
                &sender,
                failure(
                    request_id,
                    "tool_management_denied",
                    "此安装客户端未获工具管理权限",
                ),
            );
            return;
        };
        let Ok(request) = serde_json::from_value::<ToolCenterRequest>(request) else {
            send_response(
                &sender,
                failure(request_id, "invalid_request", "工具管理请求字段无效"),
            );
            return;
        };
        if request.request_id != request_id {
            send_response(
                &sender,
                failure(request_id, "invalid_request", "请求标识不一致"),
            );
            return;
        }
        let Ok(permit) = busy.try_acquire_owned() else {
            send_response(
                &sender,
                failure(request_id, "management_busy", "请等待上一条管理操作确认"),
            );
            return;
        };
        let Some(center) = self.tool_center.clone() else {
            send_response(
                &sender,
                failure(request_id, "unavailable", "工具中心未配置"),
            );
            return;
        };
        let lease = Arc::new(std::sync::RwLock::new(true));
        {
            let mut leases = self
                .tool_management_leases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entries = leases.entry(installation).or_default();
            entries.retain(|entry| entry.strong_count() > 0);
            entries.push(Arc::downgrade(&lease));
        }
        drop(registration_gate);
        self.ensure_catalog_push();
        let state = self.state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let actor = format!("mcp:{installation}");
            let execution = center.execute_authorized(&actor, request, lease.clone());
            tokio::pin!(execution);
            let mut monitor = time::interval(time::Duration::from_millis(100));
            let response = loop {
                tokio::select! {
                    biased;
                    _ = monitor.tick() => {
                        let connected = state.lock().await.controllers.get(&id).is_some_and(|c|c.connection_generation==generation);
                        if !connected || !*lease.read().unwrap_or_else(std::sync::PoisonError::into_inner) {
                            break failure(request_id.clone(), "tool_management_revoked", "工具管理权限或连接已撤销，请查询当前修订号核实结果");
                        }
                    }
                    response = &mut execution => break response,
                }
            };
            send_response(&sender, response);
        });
    }
    pub(super) fn check_tool_request(
        &self,
        state: &RelayState,
        request: &RemoteRequest,
    ) -> Result<(), (&'static str, &'static str)> {
        if !request.operation.is_resource_tool()
            || matches!(request.operation, RemoteOperation::StopResourceTool { .. })
        {
            return Ok(());
        }
        let catalog = self
            .tool_center
            .as_ref()
            .map(|center| center.cached_catalog())
            .ok_or(("tool_center_unavailable", "中继工具中心未配置"))?;
        if !catalog.enabled {
            return Err(("tool_center_disabled", "中继工具中心已关闭"));
        }
        if !state.agents.values().any(|agent| {
            agent.session_id == request.session_id && agent.hello.protocol_version >= 19
        }) {
            return Err((
                "tool_center_unsupported",
                "目标缺少集中目录能力，请升级 Agent",
            ));
        }
        let required = match &request.operation {
            RemoteOperation::ValidateResourceTool { .. } => Some("resource_tools_validation_v1"),
            RemoteOperation::PrepareBrowserPassword { .. }
            | RemoteOperation::FillBrowserPassword { .. } => Some("browser_password_v1"),
            _ => None,
        };
        if let Some(required) = required
            && !state.agents.values().any(|agent| {
                agent.session_id == request.session_id
                    && agent.hello.target_context.as_ref().is_some_and(|context| {
                        context.capabilities.iter().any(|capability| {
                            capability.capability == required
                                && capability.status
                                    == remoteops_domain::CapabilityAvailability::Available
                        })
                    })
            })
        {
            return Err((
                "resource_capability_unsupported",
                "目标缺少本次操作所需能力，请升级Agent",
            ));
        }
        let identity = match &request.operation {
            RemoteOperation::StartResourceTool {
                tool_id,
                version,
                sha256,
            }
            | RemoteOperation::ValidateResourceTool {
                tool_id,
                version,
                sha256,
            } => Some((tool_id, version, sha256)),
            RemoteOperation::CallResourceTool { instance_id, .. }
            | RemoteOperation::PrepareBrowserPassword { instance_id, .. }
            | RemoteOperation::FillBrowserPassword { instance_id, .. } => {
                let (id, version, sha) = state
                    .tool_state
                    .instances
                    .get(&(request.session_id, instance_id.clone()))
                    .ok_or(("tool_instance_unknown", "工具实例未知或目录已撤销"))?;
                Some((id, version, sha))
            }
            _ => None,
        };
        if let Some((id, version, sha)) = identity {
            let tool = catalog
                .tools
                .iter()
                .find(|tool| {
                    &tool.id == id && &tool.version == version && &tool.package.sha256 == sha
                })
                .ok_or(("tool_disabled", "该工具或固定版本未启用"))?;
            if tool.startup.browser_adapter.is_some()
                && !state
                    .agents
                    .values()
                    .find(|agent| agent.session_id == request.session_id)
                    .and_then(|agent| agent.hello.target_context.as_ref())
                    .is_some_and(|context| {
                        context.capabilities.iter().any(|status| {
                            status.capability == "resource_tools_browser_adapters_v1"
                                && status.status
                                    == remoteops_domain::CapabilityAvailability::Available
                        })
                    })
            {
                return Err((
                    "browser_adapter_unsupported",
                    "目标不支持显式浏览器适配，请升级 Agent",
                ));
            }
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn send_tool_package(
        &self,
        id: AgentInstanceId,
        generation: u64,
        request_id: String,
        sha: String,
        offset: u64,
        sender: Sender,
        busy: Arc<tokio::sync::Semaphore>,
    ) {
        let Some(center) = self.tool_center.clone() else {
            return;
        };
        let Ok(permit) = busy.try_acquire_owned() else {
            let _ = sender.send(WireMessage::ToolPackageChunk {
                request_id,
                offset,
                data_base64: String::new(),
                eof: true,
                error: Some("package_transfer_busy".into()),
            });
            return;
        };
        let state = self.state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result = async {
                let state = state.lock().await;
                if !state
                    .agents
                    .get(&id)
                    .is_some_and(|a| a.ready && a.connection_generation == generation)
                {
                    bail!("connection_revoked");
                }
                let catalog = center.cached_catalog();
                if !catalog.enabled
                    || !catalog.tools.iter().any(|tool| {
                        tool.package.sha256 == sha
                            && tool.package.url.starts_with("relay://packages/")
                    })
                {
                    bail!("tool_disabled");
                }
                drop(state);
                let path = tokio::task::spawn_blocking(move || center.package_path(&sha)).await??;
                let mut file = tokio::fs::File::open(path).await?;
                let size = file.metadata().await?.len();
                if offset > size || !offset.is_multiple_of(256 * 1024) {
                    bail!("invalid_offset");
                }
                file.seek(std::io::SeekFrom::Start(offset)).await?;
                let mut bytes = Vec::with_capacity(256 * 1024);
                let count = file.take(256 * 1024).read_to_end(&mut bytes).await?;
                Ok::<_, anyhow::Error>((BASE64.encode(bytes), offset + count as u64 == size))
            }
            .await;
            let (data_base64, eof, error) = match result {
                Ok((data, eof)) => (data, eof, None),
                Err(_) => (
                    String::new(),
                    true,
                    Some("package_unavailable_or_disabled".into()),
                ),
            };
            let _ = sender.send(WireMessage::ToolPackageChunk {
                request_id,
                offset,
                data_base64,
                eof,
                error,
            });
        });
    }
}
async fn broadcast_catalog(state: &Arc<Mutex<RelayState>>, catalog: ToolCatalog) {
    let Ok(catalog) = serde_json::to_value(catalog) else {
        return;
    };
    let state = state.lock().await;
    for agent in state
        .agents
        .values()
        .filter(|a| a.ready && a.hello.protocol_version >= 19)
    {
        if let Some(sender) = &agent.sender {
            let _ = sender.send(WireMessage::ToolCatalog {
                catalog: catalog.clone(),
            });
        }
    }
}
pub(super) fn track_tool_start(state: &mut ToolRelayState, request: &RemoteRequest) {
    if let RemoteOperation::StopResourceTool { instance_id } = &request.operation {
        state
            .instances
            .remove(&(request.session_id, instance_id.clone()));
    }
    if let RemoteOperation::StartResourceTool {
        tool_id,
        version,
        sha256,
    }
    | RemoteOperation::ValidateResourceTool {
        tool_id,
        version,
        sha256,
    } = &request.operation
    {
        state.starts.insert(
            request.request_id,
            (
                request.session_id,
                tool_id.clone(),
                version.clone(),
                sha256.clone(),
            ),
        );
    }
}
pub(super) fn track_tool_response(state: &mut ToolRelayState, message: &WireMessage) {
    if let WireMessage::Error {
        request_id: Some(id),
        ..
    } = message
    {
        state.starts.remove(id);
    }
    if let WireMessage::RemoteResponse(response) = message
        && let Some((session, id, version, sha)) = state.starts.remove(&response.request_id)
        && response.error_code.is_none()
        && let Some(instance) = response
            .details
            .as_ref()
            .and_then(|v| v.get("instance_id"))
            .and_then(serde_json::Value::as_str)
    {
        state
            .instances
            .insert((session, instance.into()), (id, version, sha));
    }
}

pub(super) fn clear_session(state: &mut ToolRelayState, session_id: SessionId) {
    state
        .instances
        .retain(|(session, _), _| *session != session_id);
    state
        .starts
        .retain(|_, (session, _, _, _)| *session != session_id);
}
