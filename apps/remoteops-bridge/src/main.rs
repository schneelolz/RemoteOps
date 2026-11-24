//! C# 宿主的隔离运行进程；标准输出只写本机协议。
#[cfg(test)]
mod tests;
use anyhow::{Context, bail};
use async_trait::async_trait;
use remoteops_agent::{
    AgentConfig, AgentEvent, AgentPermissionControl, AgentRuntimeOptions, HostBusinessProvider,
    HostQuery, HostServiceDescription,
};
use remoteops_bridge_protocol::{MAX_CONCURRENT, MAX_MESSAGE_BYTES, Message, VERSION, read_line};
use remoteops_domain::PermissionMode;
use remoteops_product::{AgentCapabilityPolicy, ProductProfile};
use remoteops_protocol::PairingMode;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

type Pending = Arc<Mutex<BTreeMap<String, oneshot::Sender<anyhow::Result<Value>>>>>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Initialize {
    protocol_version: u16,
    role: String,
    instance_id: String,
    relay: String,
    server_name: String,
    data_root: Option<PathBuf>,
    ca_cert: Option<PathBuf>,
    tls_fingerprint: Option<String>,
    allowed_capabilities: Vec<String>,
    #[serde(default = "default_read_only")]
    read_only: bool,
    #[serde(default)]
    visual_enabled: bool,
    host: Option<HostServiceDescription>,
}

fn default_read_only() -> bool {
    true
}

fn error_code(message: &str) -> &'static str {
    let lower = message.to_lowercase();
    if lower.contains("certificate") || lower.contains("证书") || lower.contains("指纹") {
        "tls_trust_failed"
    } else if lower.contains("锁") || lower.contains("lock") {
        "instance_in_use"
    } else if lower.contains("协议") || lower.contains("降级") {
        "relay_protocol_not_supported"
    } else {
        "agent_start_or_transport_failed"
    }
}

struct Provider {
    description: HostServiceDescription,
    output: mpsc::Sender<Message>,
    pending: Pending,
    sequence: AtomicU64,
}

// 即使远程取消导致 future 被丢弃，也释放关联请求并通知宿主。
struct PendingGuard {
    id: String,
    pending: Pending,
    output: mpsc::Sender<Message>,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id)
            .is_some()
        {
            let _ = self.output.try_send(Message::request(
                None,
                "$/cancelRequest",
                json!({"id":self.id}),
            ));
        }
    }
}

#[async_trait]
impl HostBusinessProvider for Provider {
    fn describe(&self) -> HostServiceDescription {
        self.description.clone()
    }
    async fn query(&self, request: HostQuery) -> anyhow::Result<Value> {
        let id = format!("bridge:{}", self.sequence.fetch_add(1, Ordering::Relaxed));
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if pending.len() >= MAX_CONCURRENT {
                bail!("host_busy");
            }
            pending.insert(id.clone(), sender);
        }
        let _guard = PendingGuard {
            id: id.clone(),
            pending: self.pending.clone(),
            output: self.output.clone(),
        };
        self.output
            .send(Message::request(
                Some(Value::String(id)),
                "host.invoke",
                serde_json::to_value(request)?,
            ))
            .await
            .context("host_closed")?;
        tokio::time::timeout(Duration::from_secs(30), receiver)
            .await
            .context("host_timeout: result_unknown")?
            .context("host_closed: result_unknown")?
    }
}

struct Running {
    shutdown: watch::Sender<bool>,
    task: JoinHandle<anyhow::Result<()>>,
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn configure(input: &Initialize) -> anyhow::Result<(AgentConfig, AgentRuntimeOptions)> {
    if input.protocol_version != VERSION {
        bail!("unsupported_local_protocol");
    }
    if !matches!(input.role.as_str(), "grclient" | "udserver")
        || !safe_component(&input.instance_id)
    {
        bail!("invalid_role_or_instance");
    }
    if input.relay.is_empty() || input.server_name.is_empty() {
        bail!("missing_relay_endpoint");
    }
    if input.role == "udserver" && input.visual_enabled {
        bail!("service_visual_not_supported");
    }
    if input
        .host
        .as_ref()
        .is_some_and(|host| host.instance_id != input.instance_id || host.service_id != input.role)
    {
        bail!("host_identity_mismatch");
    }
    let root = if let Some(root) = &input.data_root {
        if !root.is_absolute() {
            bail!("data_root_must_be_absolute");
        }
        root.clone()
    } else {
        let key = if input.role == "udserver" {
            "PROGRAMDATA"
        } else {
            "LOCALAPPDATA"
        };
        PathBuf::from(std::env::var_os(key).context("missing_default_data_root")?)
            .join("RemoteOpsBridge")
    };
    let endpoint = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", input.relay, input.server_name))
    );
    let directory = root
        .join(&input.role)
        .join(&input.instance_id)
        .join(&endpoint[..24]);
    remoteops_device::protect_instance_directory(&directory)?;
    for child in ["transfer", "cache", "tmp", "logs", "diagnostics"] {
        remoteops_device::protect_instance_directory(&directory.join(child))?;
    }
    let product = ProductProfile {
        id: "remoteops-bridge".into(),
        name: "RemoteOpsBridge".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        directory_name: "RemoteOpsBridge".into(),
        env_prefix: "REMOTEOPS_BRIDGE".into(),
        legacy_remoteops_paths: false,
        ..ProductProfile::default()
    };
    let mut config = AgentConfig::for_product(&product);
    config.relay.clone_from(&input.relay);
    config.server_name.clone_from(&input.server_name);
    config.ca_cert.clone_from(&input.ca_cert);
    config.tls_fingerprint.clone_from(&input.tls_fingerprint);
    config.state_file = directory.join("identity.json");
    config.transfer_root = directory.join("transfer");
    let mut options = if input.visual_enabled {
        AgentRuntimeOptions::interactive(product)
    } else {
        AgentRuntimeOptions::console(product)
    };
    let policy = if input.visual_enabled {
        AgentCapabilityPolicy::default()
    } else {
        AgentCapabilityPolicy::headless()
    };
    options.capability_policy = if input.read_only {
        policy.with_read_only()
    } else {
        policy
    }
    .with_allowed_capabilities(&input.allowed_capabilities)?;
    options.pairing_mode = PairingMode::LongPerBoot;
    Ok((config, options))
}

async fn write_messages(mut receiver: mpsc::Receiver<Message>) -> anyhow::Result<()> {
    let mut output = tokio::io::stdout();
    while let Some(message) = receiver.recv().await {
        let mut bytes = serde_json::to_vec(&message)?;
        if bytes.len() >= MAX_MESSAGE_BYTES {
            bail!("outbound_message_too_large");
        }
        bytes.push(b'\n');
        output.write_all(&bytes).await?;
        output.flush().await?;
    }
    Ok(())
}

async fn stop(running: &mut Option<Running>) {
    if let Some(mut child) = running.take() {
        let _ = child.shutdown.send(true);
        if tokio::time::timeout(Duration::from_secs(10), &mut child.task)
            .await
            .is_err()
        {
            child.task.abort();
            let _ = child.task.await;
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (output, receiver) = mpsc::channel(64);
    let writer = tokio::spawn(write_messages(receiver));
    let pending: Pending = Arc::default();
    let status = Arc::new(Mutex::new(json!({"state":"uninitialized"})));
    let mut running = None;
    let mut initialized = false;
    let mut input = BufReader::new(tokio::io::stdin());
    let result = async {
        while let Some(line) = read_line(&mut input).await? {
            let message = match serde_json::from_slice::<Message>(&line) {
                Ok(message) if message.valid() => message,
                _ => { output.send(Message::failure(None, -32600, "invalid_message")).await?; continue; }
            };
            let Some(method) = message.method.as_deref() else {
                if let Some(id) = message.id.as_ref().and_then(Value::as_str) {
                    let sender = pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(id);
                    if let Some(sender) = sender { let _ = sender.send(if message.error.is_some() { Err(anyhow::anyhow!("host_rejected")) } else { Ok(message.result.unwrap_or(Value::Null)) }); }
                }
                continue;
            };
            let Some(id) = message.id.clone() else { continue; };
            let response = match method {
                "initialize" if !initialized => {
                    let configured = serde_json::from_value::<Initialize>(message.params.unwrap_or(Value::Null)).map_err(anyhow::Error::from).and_then(|input| { let (config, options) = configure(&input)?; Ok((input, config, options)) });
                    match configured {
                        Err(_) => Message::failure(Some(id), -32602, "invalid_initialize_configuration"),
                        Ok((input, config, mut options)) => {
                            if let Some(description) = input.host { options.host_business = Some(Arc::new(Provider { description, output: output.clone(), pending: pending.clone(), sequence: AtomicU64::new(1) })); }
                            let (shutdown, shutdown_receiver) = watch::channel(false);
                            let (events, event_receiver) = remoteops_agent::bounded_agent_events(128);
                            let event_output = output.clone();
                            let event_status = status.clone();
                            let mut event_shutdown = shutdown_receiver.clone();
                            let ready_started = std::time::Instant::now();
                            tokio::spawn(async move {
                                let mut timer = tokio::time::interval(Duration::from_millis(20));
                                loop {
                                    tokio::select! { _ = event_shutdown.changed() => break, _ = timer.tick() => {} }
                                    while let Ok(event) = event_receiver.try_recv() {
                                        let update = match event {
                                            AgentEvent::Connected { pairing_code, lease_expires_at } => Some(json!({"state":"ready","accessCode":pairing_code,"leaseExpiresAt":lease_expires_at,"readyElapsedMs":ready_started.elapsed().as_millis()})),
                                            AgentEvent::Connecting => Some(json!({"state":"connecting"})),
                                            AgentEvent::Reconnecting { message, .. } => Some(json!({"state":"offline","code":error_code(&message)})),
                                            AgentEvent::OperationLog(log) if log.message.starts_with("startup_phase=") => { let _ = event_output.send(Message::request(None, "bridge.timing", json!({"measurement":log.message}))).await; None },
                                            AgentEvent::LeaseRenewed { lease_expires_at } => { let mut current = event_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner); current["leaseExpiresAt"] = json!(lease_expires_at); None },
                                            AgentEvent::Stopped => Some(json!({"state":"stopped"})),
                                            _ => None,
                                        };
                                        if let Some(update) = update { *event_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = update.clone(); if event_output.send(Message::request(None, "bridge.status", update)).await.is_err() { return; } }
                                    }
                                }
                            });
                            let failures = output.clone();
                            let failure_status = status.clone();
                            let task = tokio::spawn(async move {
                                let result = remoteops_agent::run_agent_with_bounded_scoped_options(config, events, shutdown_receiver, AgentPermissionControl::new(PermissionMode::ReadOnly), options).await;
                                if let Err(error) = &result { let code = error_code(&format!("{error:#}")); *failure_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = json!({"state":"failed","code":code}); let _ = failures.send(Message::request(None, "bridge.error", json!({"code":code}))).await; }
                                result
                            });
                            running = Some(Running { shutdown, task });
                            initialized = true;
                            Message::success(id, json!({"protocolVersion":VERSION,"bridgeVersion":env!("CARGO_PKG_VERSION"),"accepted":true}))
                        }
                    }
                }
                "status" => Message::success(id, status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()),
                "shutdown" => { stop(&mut running).await; output.send(Message::success(id, json!({"stopped":true}))).await?; break; }
                "initialize" => Message::failure(Some(id), -32600, "already_initialized"),
                _ => Message::failure(Some(id), -32601, "method_not_found"),
            };
            output.send(response).await?;
        }
        Ok::<(), anyhow::Error>(())
    }.await;
    stop(&mut running).await;
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    drop(output);
    if tokio::time::timeout(Duration::from_secs(2), writer)
        .await
        .is_err()
    {
        eprintln!("bridge_output_closed");
    }
    result
}
