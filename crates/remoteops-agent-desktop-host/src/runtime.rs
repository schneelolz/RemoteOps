//! 复用公共 Agent 的配置、TLS、权限与生命周期，不在原生控件中执行网络操作。

use super::lifecycle::LifecycleKind;
use super::{
    ActiveOperation, LogLevel, NativeAppearance, NativeLanguage, Observation, ObservationCategory,
    ObservationRole, ProductAction, ProductLog, ProductPhase, ProductSnapshot, TrustPrompt,
};
use remoteops_agent::{
    AgentConfig, AgentControllerBinding, AgentEvent, AgentLogLevel, AgentOperationState,
    AgentPermissionControl, AgentRuntimeOptions, BoundedAgentEventReceiver, bounded_agent_events,
    run_agent_with_bounded_scoped_options,
};
use remoteops_domain::{Capability, RequestId, ShellKind};
use remoteops_product::ProductProfile;
use remoteops_protocol::{
    ControllerControlMode, ControllerKind, SessionControlState, connect_tls,
    load_native_client_config, load_pinned_client_config, probe_server_certificate,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write as _,
    path::PathBuf,
    sync::mpsc,
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Preferences {
    language: NativeLanguage,
    appearance: NativeAppearance,
}

struct Worker {
    stop: watch::Sender<bool>,
    thread: JoinHandle<()>,
}

enum TrustResult {
    Ready(AgentConfig),
    Confirmation(AgentConfig, String, String),
    Failed(String),
}

struct OperationContext {
    // 保留既有来源标签，旧表现层继续显示同一格式。
    label: String,
    // 分类只由 Agent 的结构化操作类型和实际 Shell 类型确认。
    category: ObservationCategory,
    // 内部诊断仅保留到实时日志，不使操作观察窗弹出或显示正在执行。
    observer_visible: bool,
    // 显式终态展示已到达时，关键状态事件不再重复添加结果行。
    terminal_presented: bool,
}

fn observation_category(
    kind: remoteops_agent::AgentOperationKind,
    shell: Option<ShellKind>,
) -> ObservationCategory {
    match kind {
        remoteops_agent::AgentOperationKind::Ssh => ObservationCategory::Ssh,
        remoteops_agent::AgentOperationKind::Serial => ObservationCategory::Serial,
        remoteops_agent::AgentOperationKind::Other => ObservationCategory::Other,
        remoteops_agent::AgentOperationKind::Shell => match shell {
            Some(ShellKind::Cmd) => ObservationCategory::Cmd,
            Some(ShellKind::WindowsPowerShell | ShellKind::PowerShell) => {
                ObservationCategory::PowerShell
            }
            Some(ShellKind::System) | None => ObservationCategory::Shell,
        },
    }
}

pub(super) struct RuntimeHost {
    // 新旧产品的配置及身份目录隔离，正式入口切换前不改旧 Agent 数据。
    profile: ProductProfile,
    bundled_tools: Option<remoteops_agent::BundledTools>,
    config: AgentConfig,
    config_path: PathBuf,
    preferences_path: PathBuf,
    explicit_config: bool,
    permission: AgentPermissionControl,
    // 宿主切换语言后，后台目标上下文立即读取当前值。
    ui_language: remoteops_agent::AgentUiLanguage,
    worker: Option<Worker>,
    events: Option<BoundedAgentEventReceiver>,
    trust_events: Option<mpsc::Receiver<TrustResult>>,
    pending_trust: Option<AgentConfig>,
    epoch: Option<u64>,
    bindings: Vec<AgentControllerBinding>,
    running: BTreeSet<RequestId>,
    contexts: BTreeMap<RequestId, OperationContext>,
    pending_control: Option<(RequestId, Instant)>,
    last_control: Option<RequestId>,
    reported_dropped: u64,
}

fn target_ui_language(language: NativeLanguage) -> remoteops_domain::TargetUiLanguage {
    match language {
        NativeLanguage::Chinese => remoteops_domain::TargetUiLanguage::ZhCn,
        NativeLanguage::English => remoteops_domain::TargetUiLanguage::EnUs,
    }
}

fn safe(text: &str) -> String {
    remoteops_agent::redact_diagnostic(text)
}

impl RuntimeHost {
    pub(super) fn load(
        options: &crate::HostOptions,
        state: &mut ProductSnapshot,
    ) -> Result<Self, String> {
        let profile = options
            .product_profile
            .clone()
            .unwrap_or_else(|| options.ui_flavor.profile(&options.host_version));
        state.version.clone_from(&profile.version);
        state.product_name.clone_from(&profile.name);
        profile
            .validate()
            .map_err(|error| safe(&error.to_string()))?;
        let config_path = options
            .agent_config
            .clone()
            .unwrap_or_else(|| AgentConfig::product_config_path(&profile));
        let preferences_path = if options.agent_config.is_some() {
            config_path.with_file_name(options.ui_flavor.preferences_filename())
        } else {
            profile
                .paths()
                .gui_preferences
                .with_file_name(options.ui_flavor.preferences_filename())
        };
        if let Ok(bytes) = std::fs::read(&preferences_path) {
            match serde_json::from_slice::<Preferences>(&bytes) {
                Ok(preferences) => {
                    state.language = preferences.language;
                    state.appearance = preferences.appearance;
                }
                Err(error) => state.detail = safe(&format!("界面偏好读取失败：{error}")),
            }
        }
        let config =
            match AgentConfig::load_file_with_profile(options.agent_config.as_deref(), &profile)
                .and_then(|c| c.resolve_product(&profile, None))
            {
                Ok(config) => config,
                Err(error) => {
                    state.detail = safe(&format!("配置读取失败：{error}"));
                    AgentConfig::for_product(&profile)
                }
            };
        state.config_path = config_path.display().to_string();
        state.relay.clone_from(&config.relay);
        state.server_name.clone_from(&config.server_name);
        state.transfer_root = config.transfer_root.display().to_string();
        state.resource_tools_index_url =
            config.resource_tools.index_url.clone().unwrap_or_default();
        state
            .resource_tools_allowed_origins
            .clone_from(&config.resource_tools.allowed_origins);
        state.default_full = config.default_full_control;
        state.endpoint_editable = !profile.endpoint.is_fixed();
        state.setup_needed = config.relay.trim().is_empty() || !state.detail.is_empty();
        let permission = AgentPermissionControl::from_config(&config);
        let ui_language = remoteops_agent::AgentUiLanguage::default();
        ui_language.set(target_ui_language(state.language));
        let mut host = Self {
            profile,
            bundled_tools: options.bundled_tools.clone(),
            config,
            config_path,
            preferences_path,
            explicit_config: options.agent_config.is_some(),
            permission,
            ui_language,
            worker: None,
            events: None,
            trust_events: None,
            pending_trust: None,
            epoch: None,
            bindings: Vec::new(),
            running: BTreeSet::new(),
            contexts: BTreeMap::new(),
            pending_control: None,
            last_control: None,
            reported_dropped: 0,
        };
        if !state.setup_needed
            && let Err(error) = host.begin_connect(state)
        {
            state.setup_needed = true;
            state.phase = ProductPhase::Setup;
            state.detail = error;
            // 保留独立路径，恢复 GUI 无法编辑的无效重试/指纹字段；磁盘只在用户保存时更改。
            host.config.retry_seconds = host.config.retry_seconds.max(1);
            if host.config.clone().normalize_and_validate().is_err() {
                host.config.tls_fingerprint = None;
            }
            Self::update_status(state);
        }
        Self::update_status(state);
        Ok(host)
    }

    fn save_preferences(&self, state: &ProductSnapshot) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(&Preferences {
            language: state.language,
            appearance: state.appearance,
        })
        .map_err(|e| e.to_string())?;
        let parent = self.preferences_path.parent().ok_or("偏好目录无效")?;
        std::fs::create_dir_all(parent).map_err(|e| safe(&e.to_string()))?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|e| safe(&e.to_string()))?;
        temporary
            .write_all(&data)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|e| safe(&e.to_string()))?;
        temporary
            .persist(&self.preferences_path)
            .map_err(|e| safe(&e.error.to_string()))?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // 配置保存与信任确认保持同一事务分派，避免乐观更新权限。
    pub(super) fn action(
        &mut self,
        action: ProductAction,
        state: &mut ProductSnapshot,
    ) -> Result<(), String> {
        match action {
            ProductAction::SaveSetup { relay } => {
                if self
                    .worker
                    .as_ref()
                    .is_some_and(|w| !w.thread.is_finished())
                {
                    return Err(state
                        .t("连接正在运行，请先停止", "Stop the active connection first")
                        .into());
                }
                let previous = self.config.clone();
                let mut updated = previous.clone();
                relay.trim().clone_into(&mut updated.relay);
                if updated.relay != previous.relay {
                    updated.server_name.clear();
                }
                updated = updated
                    .resolve_product(
                        &self.profile,
                        Some((&previous.relay, &previous.server_name)),
                    )
                    .and_then(AgentConfig::normalize_and_validate)
                    .map_err(|e| safe(&e.to_string()))?;
                let saved = updated
                    .save_for_product(&self.profile, &self.config_path, !self.explicit_config)
                    .map_err(|e| safe(&e.to_string()))?;
                self.config = updated;
                self.config_path = saved;
                state.config_path = self.config_path.display().to_string();
                state.relay.clone_from(&self.config.relay);
                state.server_name.clone_from(&self.config.server_name);
                state.setup_needed = false;
                state.detail.clear();
                self.begin_connect(state)
            }
            ProductAction::SaveResourceTools { .. } => Err(state
                .t(
                    "集中管理模式不支持保存本地资源设置；工具由已连接中继统一管理。",
                    "Centralized mode does not support saving local resource settings; tools are managed by the connected relay.",
                )
                .into()),
            ProductAction::SetLanguage(_) | ProductAction::SetAppearance(_) => {
                self.ui_language.set(target_ui_language(state.language));
                self.save_preferences(state)?;
                Self::update_status(state);
                self.update_control(state);
                Ok(())
            }
            ProductAction::RequestControl { full } => {
                if self.pending_control.is_some() {
                    return Err(state
                        .t("正在等待权限确认", "Waiting for permission confirmation")
                        .into());
                }
                let current = self.control_state().ok_or_else(|| {
                    state
                        .t("尚未获得有效会话权限", "No confirmed session permission")
                        .to_owned()
                })?;
                if matches!(
                    current.mode,
                    ControllerControlMode::ReadOnly | ControllerControlMode::ExternalApproval
                ) {
                    return Err(state
                        .t(
                            "当前会话不允许修改控制模式",
                            "This session does not allow changing control mode",
                        )
                        .into());
                }
                let id = self
                    .permission
                    .request_control(
                        current,
                        if full {
                            ControllerControlMode::FullAccess
                        } else {
                            ControllerControlMode::StepByStep
                        },
                    )
                    .map_err(|e| safe(&e.to_string()))?;
                self.pending_control = Some((id, Instant::now()));
                self.last_control = Some(id);
                state.control_pending = true;
                state.control_error = None;
                self.update_control(state);
                Ok(())
            }
            ProductAction::SaveDefaultControl { full } => {
                let mut updated = self.config.clone();
                updated.default_full_control = full;
                let saved = updated
                    .save_for_product(&self.profile, &self.config_path, false)
                    .map_err(|e| safe(&e.to_string()))?;
                self.config = updated;
                self.config_path = saved;
                self.permission.set_default_full_control(full);
                state.default_full = full;
                state.control_error = None;
                Ok(())
            }
            ProductAction::TrustCancel => {
                self.pending_trust = None;
                state.tls = None;
                state.phase = ProductPhase::Stopped;
                state.detail = state
                    .t(
                        "已取消证书信任，未连接",
                        "Certificate trust cancelled; not connected",
                    )
                    .into();
                Self::update_status(state);
                Ok(())
            }
            ProductAction::TrustOnce | ProductAction::TrustSave => {
                let config =
                    self.accept_trust(matches!(action, ProductAction::TrustSave), state)?;
                self.start_agent(config, state)
            }
            ProductAction::Stop => {
                let clean = self.shutdown();
                self.pending_trust = None;
                self.clear_connection(state);
                state.phase = ProductPhase::Stopped;
                state.tls = None;
                Self::update_status(state);
                if clean {
                    Ok(())
                } else {
                    Err("后台仍在停止，请稍后重试".into())
                }
            }
            ProductAction::ClearLogs | ProductAction::MarkLogsRead => Ok(()),
        }
    }

    // 一次信任只交给本次运行时，后续保存权限偏好不能把临时指纹写入配置。
    fn accept_trust(
        &mut self,
        persist: bool,
        state: &mut ProductSnapshot,
    ) -> Result<AgentConfig, String> {
        let mut config = self.pending_trust.clone().ok_or("没有待确认的证书")?;
        config.tls_fingerprint = Some(
            state
                .tls
                .as_ref()
                .ok_or("没有证书指纹")?
                .fingerprint
                .clone(),
        );
        if persist {
            match config.save_for_product(&self.profile, &self.config_path, !self.explicit_config) {
                Ok(path) => {
                    self.config_path = path;
                    state.config_path = self.config_path.display().to_string();
                    self.config = config.clone();
                }
                Err(error) => {
                    let error = safe(&error.to_string());
                    if let Some(prompt) = state.tls.as_mut() {
                        prompt.error = Some(error.clone());
                    }
                    return Err(error);
                }
            }
        }
        self.pending_trust = None;
        state.tls = None;
        Ok(config)
    }

    fn begin_connect(&mut self, state: &mut ProductSnapshot) -> Result<(), String> {
        if !self.shutdown() {
            return Err("上次后台尚未退出".into());
        }
        let config = self
            .config
            .clone()
            .normalize_and_validate()
            .map_err(|e| safe(&e.to_string()))?;
        self.clear_connection(state);
        state.phase = ProductPhase::Connecting;
        state.setup_needed = false;
        state.tls = None;
        state.detail.clear();
        Self::update_status(state);
        let (sender, receiver) = mpsc::channel();
        let (stop, mut shutdown) = watch::channel(false);
        let thread = std::thread::Builder::new().name("remoteops-native-tls".into()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
            let result = match runtime {
                Ok(runtime) => runtime.block_on(async move {
                    tokio::select! {
                        _ = shutdown.changed() => None,
                        result = tokio::time::timeout(Duration::from_secs(12), verify_trust(config)) => Some(match result { Ok(result) => result, Err(_) => TrustResult::Failed("TLS 预检超时".into()) }),
                    }
                }),
                Err(error) => Some(TrustResult::Failed(safe(&error.to_string()))),
            };
            if let Some(result) = result { let _ = sender.send(result); }
        }).map_err(|e| safe(&e.to_string()))?;
        self.worker = Some(Worker { stop, thread });
        self.trust_events = Some(receiver);
        Ok(())
    }

    fn start_agent(
        &mut self,
        config: AgentConfig,
        state: &mut ProductSnapshot,
    ) -> Result<(), String> {
        if !self.shutdown() {
            return Err("TLS 后台尚未停止".into());
        }
        let (sender, receiver) = bounded_agent_events(1024);
        let (stop, shutdown) = watch::channel(false);
        self.permission = AgentPermissionControl::from_config(&config);
        let permission = self.permission.clone();
        let profile = self.profile.clone();
        let bundled_tools = self.bundled_tools.clone();
        let ui_language = self.ui_language.clone();
        let (error_sender, error_receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("remoteops-native-agent".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build();
                if let Ok(runtime) = runtime {
                    let result = runtime.block_on(run_agent_with_bounded_scoped_options(
                        config,
                        sender,
                        shutdown,
                        permission,
                        AgentRuntimeOptions::interactive(profile)
                            .with_ssh_trust_cli()
                            .with_ui_language(ui_language)
                            .with_bundled_tools(bundled_tools),
                    ));
                    if let Err(error) = result {
                        let _ = error_sender.send(TrustResult::Failed(safe(&error.to_string())));
                    }
                    runtime.shutdown_timeout(Duration::from_millis(200));
                } else {
                    let _ = error_sender.send(TrustResult::Failed("无法创建 Agent 运行时".into()));
                }
            })
            .map_err(|e| safe(&e.to_string()))?;
        self.worker = Some(Worker { stop, thread });
        self.events = Some(receiver);
        self.trust_events = Some(error_receiver);
        self.reported_dropped = 0;
        state.phase = ProductPhase::Connecting;
        state.detail.clear();
        Self::update_status(state);
        Ok(())
    }

    pub(super) fn tick(&mut self, state: &mut ProductSnapshot) -> Vec<Observation> {
        let mut output = Vec::new();
        let trust = match self.trust_events.as_ref().map(mpsc::Receiver::try_recv) {
            Some(Ok(result)) => Some(result),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.trust_events = None;
                // 同一结果通道也承载运行中 Agent 的异常；正常关闭由有序 Agent 事件判定。
                self.events.is_none().then(|| {
                    TrustResult::Failed(
                        state
                            .t(
                                "TLS 预检后台未返回结果，请重新连接",
                                "TLS preflight ended without a result; reconnect",
                            )
                            .into(),
                    )
                })
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => None,
        };
        if let Some(result) = trust {
            self.trust_events = None;
            match result {
                TrustResult::Ready(config) => {
                    if let Err(error) = self.start_agent(config, state) {
                        state.phase = ProductPhase::Failed;
                        state.detail = error;
                    }
                }
                TrustResult::Confirmation(config, fingerprint, reason) => {
                    state.phase = ProductPhase::Failed;
                    state.tls = Some(TrustPrompt {
                        relay: config.relay.clone(),
                        fingerprint,
                        reason: safe(&reason),
                        error: None,
                    });
                    self.pending_trust = Some(config);
                }
                TrustResult::Failed(error) => {
                    if let Some(events) = self.events.take() {
                        events.close();
                    }
                    if let Some(worker) = &self.worker {
                        let _ = worker.stop.send(true);
                    }
                    self.clear_connection(state);
                    state.phase = ProductPhase::Failed;
                    state.detail = error;
                }
            }
            Self::update_status(state);
            state.revision = state.revision.wrapping_add(1);
        }
        let started = Instant::now();
        let mut events_exhausted = false;
        for _ in 0..512 {
            if started.elapsed() > Duration::from_millis(4) {
                break;
            }
            let event = self
                .events
                .as_ref()
                .and_then(|events| events.try_recv().ok());
            let Some(event) = event else {
                events_exhausted = true;
                break;
            };
            self.apply(event, state, &mut output);
        }
        // 只有队列确已排空才判断异常退出，不能把尚未消费到的 Stopped 误判为失败。
        if events_exhausted
            && self
                .worker
                .as_ref()
                .is_some_and(|worker| worker.thread.is_finished())
            && matches!(
                state.phase,
                ProductPhase::Connecting
                    | ProductPhase::Waiting
                    | ProductPhase::Controlled
                    | ProductPhase::Reconnecting
            )
        {
            if let Some(events) = self.events.take() {
                events.close();
            }
            self.clear_connection(state);
            state.phase = ProductPhase::Failed;
            state.detail = state
                .t(
                    "Agent 后台意外退出，请重新启动连接",
                    "Agent worker exited unexpectedly; restart the connection",
                )
                .to_owned();
            Self::update_status(state);
            state.revision = state.revision.wrapping_add(1);
        }
        self.report_omissions(state, &mut output);
        self.refresh_control_timeout(state);
        output
    }

    fn report_omissions(&mut self, state: &mut ProductSnapshot, output: &mut Vec<Observation>) {
        let Some(events) = self.events.as_ref() else {
            return;
        };
        let dropped = events.dropped_log_lines();
        state.dropped_observations = dropped;
        if dropped > self.reported_dropped {
            let count = dropped - self.reported_dropped;
            self.reported_dropped = dropped;
            let message = format!("[界面处理落后，{count} 条中间显示记录已省略；任务状态继续保留]");
            output.push(Observation {
                repetition: None,
                text: message.clone(),
                role: ObservationRole::Info,
                category: ObservationCategory::Other,
                request: None,
                new_task: false,
            });
            state.record(ProductLog {
                record_id: None,
                occurred: chrono::Local::now().format("%H:%M:%S").to_string(),
                request: String::new(),
                source: "观察窗".into(),
                level: LogLevel::Info,
                message,
            });
        }
    }

    fn refresh_control_timeout(&mut self, state: &mut ProductSnapshot) {
        if self
            .pending_control
            .as_ref()
            .is_some_and(|(_, start)| start.elapsed() >= Duration::from_secs(5))
        {
            self.pending_control = None;
            state.control_pending = false;
            state.control_error = Some(state.t("权限确认超时，可重试；迟到确认仍会处理", "Permission confirmation timed out; late confirmation will still be handled").into());
            self.update_control(state);
            state.revision = state.revision.wrapping_add(1);
        }
        self.update_control(state);
    }

    // 仅结构化展示事件进入终端；详细日志始终独立保留。
    fn present(
        &mut self,
        output: &mut Vec<Observation>,
        request_id: RequestId,
        role: remoteops_agent::AgentPresentationRole,
        text: &str,
        terminal: bool,
    ) {
        let context = self
            .contexts
            .entry(request_id)
            .or_insert_with(|| OperationContext {
                label: "操作".into(),
                category: ObservationCategory::Other,
                observer_visible: true,
                terminal_presented: false,
            });
        context.observer_visible = true;
        context.terminal_presented |= terminal;
        let role = match role {
            remoteops_agent::AgentPresentationRole::Command => ObservationRole::Command,
            remoteops_agent::AgentPresentationRole::Output => ObservationRole::Output,
            remoteops_agent::AgentPresentationRole::Success => ObservationRole::Success,
            remoteops_agent::AgentPresentationRole::Error => ObservationRole::Error,
            remoteops_agent::AgentPresentationRole::Info => ObservationRole::Info,
            remoteops_agent::AgentPresentationRole::Progress => ObservationRole::Progress,
        };
        let short = request_id.to_string().chars().take(8).collect::<String>();
        let text = safe(text);
        let text = if role == ObservationRole::Command {
            let source = match context.category {
                ObservationCategory::Cmd => "CMD",
                ObservationCategory::PowerShell => "PowerShell",
                ObservationCategory::Shell => "Shell",
                ObservationCategory::Ssh => "SSH",
                ObservationCategory::Serial => "串口",
                ObservationCategory::Other => "操作",
            };
            format!(
                "[{}] {source} #{short}\r\n> {text}",
                chrono::Local::now().format("%H:%M:%S")
            )
        } else if terminal || self.running.len() > 1 {
            format!("[{short}] {text}")
        } else {
            text
        };
        if role == ObservationRole::Progress
            && let Some(row) = output.iter_mut().rev().find(|row| {
                row.request.as_deref() == Some(&request_id.to_string())
                    && row.role == ObservationRole::Progress
            })
        {
            row.text = text;
            return;
        }
        output.push(Observation {
            repetition: None,
            text,
            role,
            new_task: false,
            category: context.category,
            request: Some(request_id.to_string()),
        });
    }

    #[allow(clippy::too_many_lines)] // 连接代次、清理与真实终态集中归并，便于审计旧事件隔离。
    fn apply(
        &mut self,
        event: AgentEvent,
        state: &mut ProductSnapshot,
        output: &mut Vec<Observation>,
    ) {
        if let AgentEvent::ScopedOperation { epoch, event } = event {
            if self.epoch == Some(epoch) {
                self.apply(*event, state, output);
            }
            return;
        }
        match event {
            AgentEvent::ConnectionEpoch(epoch) => {
                self.epoch = Some(epoch);
                self.contexts.clear();
                self.running.clear();
            }
            AgentEvent::Started {
                relay,
                transfer_root,
                capabilities,
                ..
            } => {
                state.relay = relay;
                state.transfer_root = transfer_root.display().to_string();
                state.capabilities = [
                    Capability::Cmd,
                    Capability::WindowsPowerShell,
                    Capability::PowerShell,
                    Capability::Ssh,
                    Capability::Serial,
                    Capability::FileTransfer,
                    Capability::PortProbe,
                    Capability::SystemOperations,
                    Capability::Visual,
                ]
                .map(|c| (format!("{c:?}"), capabilities.contains(c)))
                .to_vec();
            }
            AgentEvent::Connecting => {
                self.clear_connection(state);
                state.phase = ProductPhase::Connecting;
            }
            AgentEvent::Connected {
                pairing_code,
                lease_expires_at,
            } => {
                state.code = Some(format_code(&pairing_code));
                state.code_expires_unix = Some(lease_expires_at.timestamp());
                state.phase = if state.controllers > 0 {
                    ProductPhase::Controlled
                } else {
                    ProductPhase::Waiting
                };
                state.detail.clear();
            }
            AgentEvent::Registered { .. } => {
                state.code = None;
                state.phase = ProductPhase::Waiting;
            }
            AgentEvent::LeaseRenewed { lease_expires_at } => {
                state.code_expires_unix = Some(lease_expires_at.timestamp());
            }
            AgentEvent::ControllerCountChanged { active_connections } => {
                if active_connections > state.controllers {
                    state.record_lifecycle(LifecycleKind::Connected, None);
                } else if active_connections < state.controllers {
                    state.record_lifecycle(LifecycleKind::Disconnected, None);
                }
                state.controllers = active_connections;
                if active_connections == 0 {
                    self.bindings.clear();
                    self.running.clear();
                    self.contexts.clear();
                    self.pending_control = None;
                    self.last_control = None;
                    state.control_pending = false;
                    state.control_error = None;
                    state.last_result.clear();
                    state.running_count = 0;
                }
                if matches!(
                    state.phase,
                    ProductPhase::Waiting | ProductPhase::Controlled
                ) {
                    state.phase = if active_connections > 0 {
                        ProductPhase::Controlled
                    } else {
                        ProductPhase::Waiting
                    };
                }
                self.update_control(state);
            }
            AgentEvent::ControllerBindingsChanged { bindings } => {
                let previous = self.control_state().map(|s| {
                    (
                        s.session_id,
                        s.agent_generation,
                        s.controller_id,
                        s.controller_generation,
                    )
                });
                self.bindings = bindings;
                let current = self.control_state().map(|s| {
                    (
                        s.session_id,
                        s.agent_generation,
                        s.controller_id,
                        s.controller_generation,
                    )
                });
                if previous.is_some() && previous != current {
                    self.pending_control = None;
                    self.last_control = None;
                    state.control_pending = false;
                    state.control_error = None;
                }
                self.update_control(state);
            }
            AgentEvent::ControlStatePending(id) => {
                self.pending_control = Some((id, Instant::now()));
                self.last_control = Some(id);
                state.control_pending = true;
                self.update_control(state);
            }
            AgentEvent::ControlStateResult(result) => {
                if self.last_control == Some(result.request_id) {
                    self.pending_control = None;
                    state.control_pending = false;
                    state.control_error = result.error.map(|error| safe(&error));
                }
                self.update_control(state);
            }
            AgentEvent::OperationContext {
                request_id,
                kind,
                resource_id,
                source,
                shell,
            } => {
                if self.contexts.len() >= 256
                    && let Some(key) = self
                        .contexts
                        .keys()
                        .find(|key| !self.running.contains(key))
                        .copied()
                {
                    self.contexts.remove(&key);
                }
                let category = observation_category(kind, shell);
                let kind = match kind {
                    remoteops_agent::AgentOperationKind::Shell => "Shell",
                    remoteops_agent::AgentOperationKind::Ssh => "SSH",
                    remoteops_agent::AgentOperationKind::Serial => "Console",
                    remoteops_agent::AgentOperationKind::Other => state.t("操作", "Operation"),
                };
                let origin = match source {
                    remoteops_domain::EventSource::Ai => "AI",
                    remoteops_domain::EventSource::Human => state.t("人工", "Human"),
                    remoteops_domain::EventSource::System => state.t("系统", "System"),
                };
                let label = format!("{kind}/{origin}");
                let previously_visible = self
                    .contexts
                    .get(&request_id)
                    .is_some_and(|context| context.observer_visible);
                let visible = category != ObservationCategory::Other
                    || self
                        .contexts
                        .get(&request_id)
                        .is_some_and(|context| context.observer_visible);
                let terminal_presented = self
                    .contexts
                    .get(&request_id)
                    .is_some_and(|context| context.terminal_presented);
                self.contexts.insert(
                    request_id,
                    OperationContext {
                        label: resource_id.map_or_else(
                            || label.clone(),
                            |resource| {
                                format!("{label}/{}", resource.chars().take(8).collect::<String>())
                            },
                        ),
                        category,
                        observer_visible: visible,
                        terminal_presented,
                    },
                );
                if !previously_visible && visible && self.running.contains(&request_id) {
                    state.record_lifecycle(LifecycleKind::Running, Some(category));
                    output.push(Observation {
                        repetition: None,
                        text: String::new(),
                        role: ObservationRole::Info,
                        new_task: true,
                        category,
                        request: Some(request_id.to_string()),
                    });
                }
            }
            AgentEvent::OperationPresentation {
                request_id,
                role,
                text,
                terminal,
            } => {
                if role == remoteops_agent::AgentPresentationRole::Progress {
                    if state
                        .resource_progress
                        .as_ref()
                        .is_none_or(|(id, _)| *id != request_id)
                    {
                        state.record_lifecycle(
                            LifecycleKind::Running,
                            Some(ObservationCategory::Other),
                        );
                    }
                    state.resource_progress = Some((request_id, safe(&text)));
                }
                self.present(output, request_id, role, &text, terminal);
            }
            AgentEvent::OperationOutputUpdate {
                request_id,
                text,
                repetition,
            } => {
                let text = if repetition.total > 1 {
                    format!("{text} × {}", repetition.total)
                } else {
                    text
                };
                self.present(
                    output,
                    request_id,
                    remoteops_agent::AgentPresentationRole::Output,
                    &text,
                    false,
                );
                if let Some(observation) = output.last_mut() {
                    observation.repetition = Some(super::ObservationRepetition {
                        record_id: repetition.record_id,
                        total: repetition.total,
                    });
                }
            }
            AgentEvent::OperationLog(log) => {
                let source = log
                    .request_id
                    .and_then(|id| self.contexts.get(&id))
                    .map_or_else(|| "系统".into(), |context| context.label.clone());
                let request = log
                    .request_id
                    .map_or_else(String::new, |id| id.to_string().chars().take(8).collect());
                let entry = ProductLog {
                    record_id: log.repetition.map(|value| value.record_id),
                    occurred: log
                        .occurred_at
                        .with_timezone(&chrono::Local)
                        .format("%H:%M:%S")
                        .to_string(),
                    request,
                    source,
                    level: match log.level {
                        AgentLogLevel::Running => LogLevel::Running,
                        AgentLogLevel::Success => LogLevel::Success,
                        AgentLogLevel::Error => LogLevel::Error,
                        AgentLogLevel::Info => LogLevel::Info,
                    },
                    message: safe(&log.message),
                };
                state.record(entry);
            }
            AgentEvent::OperationStateChanged {
                request_id,
                state: operation_state,
            } => {
                if operation_state != AgentOperationState::Running
                    && state
                        .resource_progress
                        .as_ref()
                        .is_some_and(|(id, _)| *id == request_id)
                {
                    state.resource_progress = None;
                }
                let visible = self
                    .contexts
                    .get(&request_id)
                    .is_some_and(|context| context.observer_visible);
                let was_running = self.running.contains(&request_id);
                let new_task = if operation_state == AgentOperationState::Running {
                    self.running.insert(request_id)
                } else {
                    self.running.remove(&request_id);
                    if visible {
                        state.last_result = format!("{operation_state:?}");
                    }
                    false
                };
                if visible
                    && ((new_task && !was_running)
                        || (was_running && operation_state != AgentOperationState::Running))
                {
                    let kind = match operation_state {
                        AgentOperationState::Running => LifecycleKind::Running,
                        AgentOperationState::Succeeded => LifecycleKind::Succeeded,
                        _ => LifecycleKind::Failed,
                    };
                    state.record_lifecycle(
                        kind,
                        self.contexts
                            .get(&request_id)
                            .map(|context| context.category),
                    );
                }
                state.record(ProductLog {
                    record_id: None,
                    occurred: chrono::Local::now().format("%H:%M:%S").to_string(),
                    request: request_id.to_string().chars().take(8).collect(),
                    source: self
                        .contexts
                        .get(&request_id)
                        .map_or_else(|| "操作".into(), |context| context.label.clone()),
                    level: if operation_state == AgentOperationState::Running {
                        LogLevel::Running
                    } else if operation_state == AgentOperationState::Succeeded {
                        LogLevel::Success
                    } else {
                        LogLevel::Error
                    },
                    message: format!(
                        "{}：{operation_state:?}",
                        state.t("执行状态", "Execution state")
                    ),
                });
                if visible {
                    if operation_state == AgentOperationState::Running {
                        output.push(Observation {
                            repetition: None,
                            text: String::new(),
                            role: ObservationRole::Info,
                            new_task,
                            category: self
                                .contexts
                                .get(&request_id)
                                .map_or(ObservationCategory::Other, |context| context.category),
                            request: Some(request_id.to_string()),
                        });
                    } else if !self
                        .contexts
                        .get(&request_id)
                        .is_some_and(|context| context.terminal_presented)
                    {
                        let (role, text) = match operation_state {
                            AgentOperationState::Succeeded => (
                                remoteops_agent::AgentPresentationRole::Success,
                                state.t("执行完成", "Completed"),
                            ),
                            AgentOperationState::Cancelled | AgentOperationState::Interrupted => (
                                remoteops_agent::AgentPresentationRole::Error,
                                state.t("操作已中断", "Interrupted"),
                            ),
                            _ => (
                                remoteops_agent::AgentPresentationRole::Error,
                                state.t("执行失败", "Failed"),
                            ),
                        };
                        self.present(output, request_id, role, text, true);
                    }
                }
            }
            AgentEvent::Reconnecting { message, .. } => {
                self.clear_connection(state);
                state.phase = ProductPhase::Reconnecting;
                state.detail = safe(&message);
            }
            AgentEvent::Failed { message } => {
                self.clear_connection(state);
                state.phase = ProductPhase::Failed;
                state.detail = safe(&message);
            }
            AgentEvent::Stopped => {
                self.clear_connection(state);
                state.phase = ProductPhase::Stopped;
            }
            AgentEvent::ScopedOperation { .. } => unreachable!(),
        }
        self.refresh_active_operations(state);
        Self::update_status(state);
        state.revision = state.revision.wrapping_add(1);
    }

    fn refresh_active_operations(&self, state: &mut ProductSnapshot) {
        state.active_operations = self
            .running
            .iter()
            .filter(|request| {
                self.contexts
                    .get(request)
                    .is_none_or(|context| context.observer_visible)
            })
            .map(|request| ActiveOperation {
                request: request.to_string(),
                category: self
                    .contexts
                    .get(request)
                    .map_or(ObservationCategory::Other, |context| context.category),
            })
            .collect();
        state.running_count = state.active_operations.len();
    }

    fn clear_connection(&mut self, state: &mut ProductSnapshot) {
        state.resource_progress = None;
        if state.controllers > 0 {
            state.record_lifecycle(LifecycleKind::Disconnected, None);
        }
        self.epoch = None;
        self.bindings.clear();
        self.running.clear();
        self.contexts.clear();
        self.pending_control = None;
        self.last_control = None;
        state.controllers = 0;
        state.running_count = 0;
        state.active_operations.clear();
        state.code = None;
        state.code_expires_unix = None;
        state.control_pending = false;
        state.control_error = None;
        self.update_control(state);
    }

    fn control_state(&self) -> Option<SessionControlState> {
        self.bindings
            .iter()
            .find(|binding| binding.controller_kind == ControllerKind::Ai)
            .and_then(|binding| binding.control_state.clone())
    }

    fn update_control(&self, state: &mut ProductSnapshot) {
        let current = self.control_state();
        state.control_enabled = current.as_ref().is_some_and(|s| {
            !matches!(
                s.mode,
                ControllerControlMode::ReadOnly | ControllerControlMode::ExternalApproval
            )
        }) && !state.control_pending;
        let expired = current.as_ref().is_some_and(|s| {
            s.mode == ControllerControlMode::FullAccess
                && s.expires_at.is_some_and(|end| end <= chrono::Utc::now())
        });
        state.control_summary = current.as_ref().map_or_else(
            || {
                state
                    .t("等待控制端连接", "Waiting for controller")
                    .to_owned()
            },
            |s| {
                match if expired {
                    ControllerControlMode::Expired
                } else {
                    s.mode
                } {
                    ControllerControlMode::StepByStep => state.t("逐项确认", "Step by step"),
                    ControllerControlMode::FullAccess => state.t("完全控制", "Full access"),
                    ControllerControlMode::ReadOnly => state.t("只读", "Read only"),
                    ControllerControlMode::ExternalApproval => {
                        state.t("外部审批", "External approval")
                    }
                    ControllerControlMode::Expired => state.t("授权已过期", "Permission expired"),
                }
                .to_owned()
            },
        );
        state.control_source = current
            .and_then(|s| s.source)
            .map_or_else(String::new, |source| format!("{source:?}"));
    }

    fn update_status(state: &mut ProductSnapshot) {
        state.title = format!(
            "{} · {} v{}",
            state.product_name,
            state.t("远程协助", "Remote Assistance"),
            state.version
        );
        state.status = match state.phase {
            ProductPhase::Setup => state.t("首次使用，请配置连接", "Configure the connection"),
            ProductPhase::Connecting => state.t("正在连接", "Connecting"),
            ProductPhase::Waiting => state.t("等待工程师连接", "Waiting for engineer"),
            ProductPhase::Controlled => state.t("AI工程师已连接", "AI engineer connected"),
            ProductPhase::Reconnecting => state.t("连接中断，正在重连", "Reconnecting"),
            ProductPhase::Failed => state.t("连接失败，请查看详情", "Connection failed"),
            ProductPhase::Stopped => state.t("远程协助已停止", "Remote assistance stopped"),
        }
        .into();
        if state.tls.is_some() {
            state.status = state
                .t("等待证书确认", "Awaiting certificate confirmation")
                .into();
        }
        state.activity = if state.tls.is_some() {
            state
                .t(
                    "请通过可信渠道核对证书指纹",
                    "Verify the certificate fingerprint using a trusted channel",
                )
                .into()
        } else if state.running_count > 0 {
            format!(
                "{} {}",
                state.t("正在执行操作：", "Running operations:"),
                state.running_count
            )
        } else if matches!(
            state.phase,
            ProductPhase::Connecting | ProductPhase::Reconnecting
        ) {
            state
                .t(
                    "正在准备连接并获取控制码",
                    "Preparing connection and pairing code",
                )
                .into()
        } else if state.phase == ProductPhase::Failed {
            state
                .t(
                    "连接未完成，请查看连接详情",
                    "Connection incomplete; see details",
                )
                .into()
        } else if state.phase == ProductPhase::Stopped {
            state
                .t("远程协助已停止", "Remote assistance stopped")
                .into()
        } else if state.phase == ProductPhase::Controlled {
            state
                .t("等待下一步操作", "Waiting for the next operation")
                .into()
        } else {
            state
                .t("系统就绪 · 等待操作", "Ready · waiting for operations")
                .into()
        };
    }

    pub(super) fn shutdown(&mut self) -> bool {
        if let Some(receiver) = self.events.take() {
            receiver.close();
        }
        self.trust_events = None;
        if let Some(worker) = self.worker.as_ref() {
            let _ = worker.stop.send(true);
        }
        let started = Instant::now();
        while self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.thread.is_finished())
        {
            if started.elapsed() > Duration::from_millis(1800) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.worker
            .take()
            .is_none_or(|worker| worker.thread.join().is_ok())
    }
}

impl Drop for RuntimeHost {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn format_code(code: &str) -> String {
    if code.len() == 9 && code.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}-{}", &code[..3], &code[3..6], &code[6..])
    } else {
        code.to_owned()
    }
}

async fn verify_trust(config: AgentConfig) -> TrustResult {
    if config.ca_cert.is_some() {
        return TrustResult::Ready(config);
    }
    let verified = if let Some(fingerprint) = config.tls_fingerprint.as_deref() {
        load_pinned_client_config(&config.relay, &config.server_name, fingerprint)
            .await
            .map(|_| ())
            .map_err(|e| safe(&e.to_string()))
    } else {
        match load_native_client_config() {
            Ok(client) => connect_tls(&config.relay, &config.server_name, client)
                .await
                .map(|_| ())
                .map_err(|e| safe(&e.to_string())),
            Err(error) => Err(safe(&error.to_string())),
        }
    };
    match verified {
        Ok(()) => TrustResult::Ready(config),
        Err(reason) => match probe_server_certificate(&config.relay, &config.server_name).await {
            Ok(probe) => TrustResult::Confirmation(config, probe.sha256_fingerprint, reason),
            Err(error) => TrustResult::Failed(safe(&format!("{reason}; {error}"))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(
        request_id: RequestId,
        kind: remoteops_agent::AgentOperationKind,
        shell: Option<ShellKind>,
    ) -> AgentEvent {
        AgentEvent::OperationContext {
            request_id,
            kind,
            shell,
            resource_id: None,
            source: remoteops_domain::EventSource::Human,
        }
    }

    fn operation_state(request_id: RequestId, state: AgentOperationState) -> AgentEvent {
        AgentEvent::OperationStateChanged { request_id, state }
    }

    #[test]
    fn lifecycle_preserves_all_batched_transitions_and_parallel_execution_without_output() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        host.apply(
            AgentEvent::ControllerCountChanged {
                active_connections: 1,
            },
            &mut state,
            &mut output,
        );
        let first = RequestId::new();
        let second = RequestId::new();
        for id in [first, second] {
            host.apply(
                context(id, remoteops_agent::AgentOperationKind::Ssh, None),
                &mut state,
                &mut output,
            );
            host.apply(
                operation_state(id, AgentOperationState::Running),
                &mut state,
                &mut output,
            );
            host.apply(
                operation_state(id, AgentOperationState::Running),
                &mut state,
                &mut output,
            );
        }
        assert_eq!(state.running_count, 2);
        host.apply(
            operation_state(first, AgentOperationState::Succeeded),
            &mut state,
            &mut output,
        );
        assert_eq!(state.running_count, 1);
        host.apply(
            operation_state(second, AgentOperationState::Failed),
            &mut state,
            &mut output,
        );
        assert_eq!(state.running_count, 0);
        host.apply(
            AgentEvent::ControllerCountChanged {
                active_connections: 0,
            },
            &mut state,
            &mut output,
        );
        assert_eq!(
            state
                .lifecycle_events
                .iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            vec![
                LifecycleKind::Connected,
                LifecycleKind::Running,
                LifecycleKind::Running,
                LifecycleKind::Succeeded,
                LifecycleKind::Failed,
                LifecycleKind::Disconnected
            ]
        );
        let before = state.lifecycle_events.clone();
        state.logs.clear();
        assert_eq!(state.lifecycle_events, before);
        assert!(
            state
                .lifecycle_events
                .iter()
                .all(|event| event.text(NativeLanguage::Chinese).len() < 128)
        );
        state.language = NativeLanguage::English;
        assert_eq!(
            state.lifecycle_events[0].text(state.language),
            "AI engineer connected"
        );
    }

    #[test]
    fn resource_progress_updates_one_observation_and_main_activity() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        let id = RequestId::new();
        host.apply(
            operation_state(id, AgentOperationState::Running),
            &mut state,
            &mut output,
        );
        for percent in [10, 50, 100] {
            host.apply(
                AgentEvent::OperationPresentation {
                    request_id: id,
                    role: remoteops_agent::AgentPresentationRole::Progress,
                    text: format!("browser：下载中 {percent}%"),
                    terminal: false,
                },
                &mut state,
                &mut output,
            );
        }
        assert_eq!(
            output
                .iter()
                .filter(|row| row.role == ObservationRole::Progress)
                .count(),
            1
        );
        assert!(state.resource_progress.as_ref().unwrap().1.contains("100%"));
        host.apply(
            operation_state(id, AgentOperationState::Succeeded),
            &mut state,
            &mut output,
        );
        assert!(state.resource_progress.is_none());
    }

    #[test]
    fn internal_diagnostics_keep_realtime_details_without_observer_or_activity() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        let id = RequestId::new();
        host.apply(
            context(id, remoteops_agent::AgentOperationKind::Other, None),
            &mut state,
            &mut output,
        );
        host.apply(
            operation_state(id, AgentOperationState::Running),
            &mut state,
            &mut output,
        );
        host.apply(
            AgentEvent::OperationLog(remoteops_agent::AgentOperationLog {
                repetition: None,
                request_id: Some(id),
                occurred_at: chrono::Utc::now(),
                level: AgentLogLevel::Info,
                message: r#"visual_probe_ready; {"capabilities":["cmd"]}"#.into(),
            }),
            &mut state,
            &mut output,
        );
        assert!(output.is_empty());
        assert_eq!(state.running_count, 0);
        assert!(
            state
                .logs
                .iter()
                .any(|log| log.message.contains("visual_probe_ready"))
        );
        host.apply(
            operation_state(id, AgentOperationState::Succeeded),
            &mut state,
            &mut output,
        );
        assert!(output.is_empty());
        assert_eq!(state.active_operations, Vec::<ActiveOperation>::new());
    }

    #[test]
    fn semantic_command_json_stderr_and_terminal_keep_independent_roles() {
        use remoteops_agent::AgentPresentationRole as Role;
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        let id = RequestId::new();
        host.apply(
            context(
                id,
                remoteops_agent::AgentOperationKind::Shell,
                Some(ShellKind::Cmd),
            ),
            &mut state,
            &mut output,
        );
        host.apply(
            AgentEvent::OperationPresentation {
                request_id: id,
                role: Role::Command,
                text: "echo command".into(),
                terminal: false,
            },
            &mut state,
            &mut output,
        );
        host.apply(
            operation_state(id, AgentOperationState::Running),
            &mut state,
            &mut output,
        );
        host.apply(
            AgentEvent::OperationPresentation {
                request_id: id,
                role: Role::Output,
                text: r#"{"error":"ordinary data"}"#.into(),
                terminal: false,
            },
            &mut state,
            &mut output,
        );
        host.apply(
            AgentEvent::OperationPresentation {
                request_id: id,
                role: Role::Error,
                text: "nonterminal stderr".into(),
                terminal: false,
            },
            &mut state,
            &mut output,
        );
        assert_eq!(state.running_count, 1);
        assert_eq!(output[0].role, ObservationRole::Command);
        assert_eq!(output[1].text, "");
        assert!(output[1].new_task);
        assert_eq!(output[2].role, ObservationRole::Output);
        assert_eq!(output[2].text, r#"{"error":"ordinary data"}"#);
        assert_eq!(output[3].role, ObservationRole::Error);
        host.apply(
            AgentEvent::OperationPresentation {
                request_id: id,
                role: Role::Success,
                text: "执行完成 · 退出码 0".into(),
                terminal: true,
            },
            &mut state,
            &mut output,
        );
        let rows = output.len();
        host.apply(
            operation_state(id, AgentOperationState::Succeeded),
            &mut state,
            &mut output,
        );
        assert_eq!(output.len(), rows);
        assert_eq!(output.last().unwrap().role, ObservationRole::Success);
        assert!(output.last().unwrap().text.contains("退出码 0"));
        assert_eq!(state.running_count, 0);
    }

    #[test]
    fn language_switch_updates_live_runtime_before_saving_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        for language in [NativeLanguage::English, NativeLanguage::Chinese] {
            state.language = language;
            host.action(ProductAction::SetLanguage(language), &mut state)
                .unwrap();
            assert_eq!(host.ui_language.get(), Some(target_ui_language(language)));
        }
    }

    #[test]
    fn connecting_summary_does_not_claim_the_system_is_idle() {
        let mut state = ProductSnapshot {
            phase: ProductPhase::Connecting,
            ..ProductSnapshot::default()
        };
        RuntimeHost::update_status(&mut state);
        assert!(state.activity.contains("正在准备连接"));
        state.language = NativeLanguage::English;
        state.phase = ProductPhase::Reconnecting;
        RuntimeHost::update_status(&mut state);
        assert!(state.activity.contains("Preparing connection"));
    }

    #[test]
    fn categories_use_structured_kind_and_shell_with_transport_priority() {
        use remoteops_agent::AgentOperationKind as Kind;
        for (kind, shell, expected) in [
            (Kind::Shell, Some(ShellKind::Cmd), ObservationCategory::Cmd),
            (
                Kind::Shell,
                Some(ShellKind::WindowsPowerShell),
                ObservationCategory::PowerShell,
            ),
            (
                Kind::Shell,
                Some(ShellKind::PowerShell),
                ObservationCategory::PowerShell,
            ),
            (
                Kind::Shell,
                Some(ShellKind::System),
                ObservationCategory::Shell,
            ),
            (Kind::Shell, None, ObservationCategory::Shell),
            (Kind::Ssh, Some(ShellKind::Cmd), ObservationCategory::Ssh),
            (
                Kind::Serial,
                Some(ShellKind::PowerShell),
                ObservationCategory::Serial,
            ),
            (
                Kind::Other,
                Some(ShellKind::Cmd),
                ObservationCategory::Other,
            ),
        ] {
            assert_eq!(observation_category(kind, shell), expected);
        }
    }

    #[test]
    fn concurrent_requests_keep_full_identity_and_independent_categories() {
        use remoteops_agent::AgentOperationKind as Kind;
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        let inputs = [
            (
                RequestId::new(),
                Kind::Shell,
                Some(ShellKind::Cmd),
                ObservationCategory::Cmd,
            ),
            (
                RequestId::new(),
                Kind::Shell,
                Some(ShellKind::PowerShell),
                ObservationCategory::PowerShell,
            ),
            (RequestId::new(), Kind::Ssh, None, ObservationCategory::Ssh),
            (RequestId::new(), Kind::Ssh, None, ObservationCategory::Ssh),
        ];
        for (id, kind, shell, category) in inputs {
            host.apply(context(id, kind, shell), &mut state, &mut output);
            host.apply(
                operation_state(id, AgentOperationState::Running),
                &mut state,
                &mut output,
            );
            let observation = output.last().unwrap();
            assert_eq!(
                observation.request.as_deref(),
                Some(id.to_string().as_str())
            );
            assert_eq!(observation.category, category);
            assert!(observation.new_task);
        }
        assert_eq!(state.running_count, 4);
        assert_eq!(state.active_operations.len(), 4);
        assert_eq!(
            state
                .active_operations
                .iter()
                .filter(|operation| operation.category == ObservationCategory::Ssh)
                .count(),
            2
        );
        for (id, _, _, category) in inputs {
            assert!(state.active_operations.contains(&ActiveOperation {
                request: id.to_string(),
                category
            }));
        }
    }

    #[test]
    fn all_terminal_states_clear_activity_and_duplicate_running_does_not_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        for terminal in [
            AgentOperationState::Succeeded,
            AgentOperationState::Failed,
            AgentOperationState::Cancelled,
            AgentOperationState::Interrupted,
        ] {
            let id = RequestId::new();
            host.apply(
                context(
                    id,
                    remoteops_agent::AgentOperationKind::Shell,
                    Some(ShellKind::Cmd),
                ),
                &mut state,
                &mut output,
            );
            host.apply(
                operation_state(id, AgentOperationState::Running),
                &mut state,
                &mut output,
            );
            assert!(output.last().unwrap().new_task);
            host.apply(
                operation_state(id, AgentOperationState::Running),
                &mut state,
                &mut output,
            );
            assert!(!output.last().unwrap().new_task);
            assert_eq!(state.running_count, 1);
            host.apply(operation_state(id, terminal), &mut state, &mut output);
            assert!(!output.last().unwrap().new_task);
            assert_eq!(state.running_count, 0);
            assert_eq!(state.active_operations, Vec::<ActiveOperation>::new());
        }
    }

    #[test]
    fn output_level_and_text_cannot_invent_activity_or_change_category() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        let id = RequestId::new();
        host.apply(
            context(
                id,
                remoteops_agent::AgentOperationKind::Shell,
                Some(ShellKind::Cmd),
            ),
            &mut state,
            &mut output,
        );
        host.apply(
            AgentEvent::OperationLog(remoteops_agent::AgentOperationLog {
                repetition: None,
                request_id: Some(id),
                occurred_at: chrono::Utc::now(),
                level: AgentLogLevel::Running,
                message: "PowerShell SSH bash 正在执行".into(),
            }),
            &mut state,
            &mut output,
        );
        assert_eq!(state.running_count, 0);
        assert_eq!(state.active_operations, Vec::<ActiveOperation>::new());
        assert!(output.is_empty());
        assert!(
            state
                .logs
                .back()
                .unwrap()
                .message
                .contains("PowerShell SSH bash")
        );
        host.apply(
            AgentEvent::OperationPresentation {
                request_id: id,
                role: remoteops_agent::AgentPresentationRole::Output,
                text: "PowerShell SSH bash 正在执行".into(),
                terminal: false,
            },
            &mut state,
            &mut output,
        );
        assert_eq!(output[0].category, ObservationCategory::Cmd);
        assert_eq!(output[0].role, ObservationRole::Output);
        assert!(!output[0].new_task);
    }

    #[test]
    fn unknown_context_is_other_and_late_structured_context_updates_activity() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        let id = RequestId::new();
        host.apply(
            operation_state(id, AgentOperationState::Running),
            &mut state,
            &mut output,
        );
        assert!(output.is_empty());
        assert_eq!(
            state.active_operations[0].category,
            ObservationCategory::Other
        );
        host.apply(
            context(
                id,
                remoteops_agent::AgentOperationKind::Shell,
                Some(ShellKind::WindowsPowerShell),
            ),
            &mut state,
            &mut output,
        );
        assert_eq!(
            state.active_operations[0].category,
            ObservationCategory::PowerShell
        );
        assert_eq!(state.active_operations[0].request, id.to_string());
    }

    #[test]
    fn epoch_change_and_disconnect_clear_activity_and_ignore_old_scoped_events() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        host.apply(AgentEvent::ConnectionEpoch(1), &mut state, &mut output);
        let id = RequestId::new();
        for event in [
            context(id, remoteops_agent::AgentOperationKind::Ssh, None),
            operation_state(id, AgentOperationState::Running),
        ] {
            host.apply(
                AgentEvent::ScopedOperation {
                    epoch: 1,
                    event: Box::new(event),
                },
                &mut state,
                &mut output,
            );
        }
        assert_eq!(state.running_count, 1);
        host.apply(AgentEvent::ConnectionEpoch(2), &mut state, &mut output);
        assert_eq!(state.running_count, 0);
        assert_eq!(state.active_operations, Vec::<ActiveOperation>::new());
        output.clear();
        host.apply(
            AgentEvent::ScopedOperation {
                epoch: 1,
                event: Box::new(operation_state(id, AgentOperationState::Running)),
            },
            &mut state,
            &mut output,
        );
        assert!(output.is_empty());
        host.apply(
            operation_state(RequestId::new(), AgentOperationState::Running),
            &mut state,
            &mut output,
        );
        host.apply(
            AgentEvent::Reconnecting {
                message: "fixture".into(),
                retry_seconds: 1,
            },
            &mut state,
            &mut output,
        );
        assert_eq!(state.active_operations, Vec::<ActiveOperation>::new());
        assert_eq!(state.running_count, 0);
        output.clear();
        host.apply(
            AgentEvent::ScopedOperation {
                epoch: 2,
                event: Box::new(operation_state(id, AgentOperationState::Running)),
            },
            &mut state,
            &mut output,
        );
        assert!(output.is_empty());
        assert_eq!(state.active_operations, Vec::<ActiveOperation>::new());
    }

    #[test]
    fn context_eviction_and_history_trimming_do_not_change_active_requests() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        let id = RequestId::new();
        host.apply(
            context(
                id,
                remoteops_agent::AgentOperationKind::Shell,
                Some(ShellKind::Cmd),
            ),
            &mut state,
            &mut output,
        );
        host.apply(
            operation_state(id, AgentOperationState::Running),
            &mut state,
            &mut output,
        );
        for index in 0..1200 {
            host.apply(
                context(
                    RequestId::new(),
                    remoteops_agent::AgentOperationKind::Other,
                    None,
                ),
                &mut state,
                &mut output,
            );
            state.record(ProductLog {
                record_id: None,
                occurred: String::new(),
                request: String::new(),
                source: "fixture".into(),
                level: LogLevel::Success,
                message: index.to_string(),
            });
        }
        assert_eq!(state.logs.len(), 1000);
        assert_eq!(state.running_count, 1);
        assert_eq!(
            state.active_operations,
            vec![ActiveOperation {
                request: id.to_string(),
                category: ObservationCategory::Cmd
            }]
        );
        assert_eq!(
            host.contexts.get(&id).unwrap().category,
            ObservationCategory::Cmd
        );
        host.apply(
            AgentEvent::ControllerCountChanged {
                active_connections: 0,
            },
            &mut state,
            &mut output,
        );
        assert_eq!(state.active_operations, Vec::<ActiveOperation>::new());
        assert_eq!(state.running_count, 0);
    }

    #[test]
    fn custom_product_identity_reaches_native_host_without_connecting_or_writing_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("example.json");
        let original = r#"{"relay":"other.invalid:7443"}"#;
        std::fs::write(&path, original).unwrap();
        let profile = ProductProfile {
            id: "example-agent".into(),
            name: "示例产品 · Example".into(),
            version: "9.8.7-example.1".into(),
            directory_name: "RemoteOps-Example".into(),
            config_file_name: "example-agent-config.json".into(),
            state_file_name: "agent-state.json".into(),
            env_prefix: "EXAMPLE".into(),
            default_full_control: true,
            legacy_remoteops_paths: false,
            endpoint: remoteops_product::EndpointPolicy::Fixed {
                relay: "relay.example.invalid:7443".into(),
                server_name: "relay.example.invalid".into(),
            },
        };
        let mut state = ProductSnapshot::default();
        let host = RuntimeHost::load(
            &crate::HostOptions {
                agent_config: Some(path.clone()),
                product_profile: Some(profile),
                ..crate::HostOptions::default()
            },
            &mut state,
        )
        .unwrap();
        assert!(host.worker.is_none());
        assert!(state.setup_needed);
        assert!(!state.endpoint_editable);
        assert_eq!(state.product_name, "示例产品 · Example");
        assert_eq!(state.version, "9.8.7-example.1");
        assert!(state.title.starts_with("示例产品 · Example"));
        assert_eq!(state.relay, "relay.example.invalid:7443");
        assert!(state.default_full);
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
        assert_eq!(
            host.preferences_path,
            directory.path().join("native-gui-preferences.json")
        );
        state.language = NativeLanguage::English;
        RuntimeHost::update_status(&mut state);
        assert!(state.title.contains("示例产品 · Example"));
        assert!(state.title.ends_with("v9.8.7-example.1"));
    }

    #[test]
    fn runtime_status_preserves_the_frontend_host_version() {
        let mut state = ProductSnapshot {
            version: "frontend-version-fixture".into(),
            ..ProductSnapshot::default()
        };
        RuntimeHost::update_status(&mut state);
        assert!(state.title.ends_with("vfrontend-version-fixture"));
        state.language = NativeLanguage::English;
        RuntimeHost::update_status(&mut state);
        assert!(state.title.ends_with("vfrontend-version-fixture"));
    }

    fn isolated_host(directory: &std::path::Path) -> (RuntimeHost, ProductSnapshot) {
        let path = directory.join("agent-config.json");
        std::fs::write(&path, b"{}").unwrap();
        let mut state = ProductSnapshot::default();
        let host = RuntimeHost::load(
            &crate::HostOptions {
                agent_config: Some(path),
                ..crate::HostOptions::default()
            },
            &mut state,
        )
        .unwrap();
        (host, state)
    }

    fn binding() -> AgentControllerBinding {
        AgentControllerBinding {
            control_state: Some(SessionControlState {
                initialization_pending: false,
                session_id: remoteops_domain::SessionId::new(),
                agent_generation: 1,
                controller_id: remoteops_domain::ControllerInstanceId::new(),
                controller_generation: 1,
                revision: 1,
                mode: ControllerControlMode::StepByStep,
                source: None,
                expires_at: None,
            }),
            owner_id: remoteops_domain::ControllerOwnerId::new(),
            controller_kind: ControllerKind::Ai,
            permission_mode: remoteops_domain::PermissionMode::ApprovalRequired,
            controller_control_mode: Some(ControllerControlMode::StepByStep),
            full_access_authorized_locally: false,
        }
    }

    #[test]
    fn one_time_trust_is_not_persisted_by_later_permission_preference_save() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        host.config.relay = "127.0.0.1:7443".into();
        host.config.server_name = "localhost".into();
        host.pending_trust = Some(host.config.clone());
        state.tls = Some(TrustPrompt {
            relay: host.config.relay.clone(),
            fingerprint: "a".repeat(64),
            reason: "test".into(),
            error: None,
        });
        let active = host.accept_trust(false, &mut state).unwrap();
        assert!(active.tls_fingerprint.is_some());
        assert!(host.config.tls_fingerprint.is_none());
        host.action(ProductAction::SaveDefaultControl { full: true }, &mut state)
            .unwrap();
        let disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&host.config_path).unwrap()).unwrap();
        assert!(
            disk.get("tls_fingerprint")
                .and_then(serde_json::Value::as_str)
                .is_none()
        );
        assert_eq!(disk["default_full_control"], true);
    }

    #[test]
    fn failed_trust_save_keeps_confirmation_and_persistent_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        host.config_path = directory.path().to_path_buf();
        host.pending_trust = Some(host.config.clone());
        state.tls = Some(TrustPrompt {
            relay: String::new(),
            fingerprint: "b".repeat(64),
            reason: "test".into(),
            error: None,
        });
        assert!(host.accept_trust(true, &mut state).is_err());
        assert!(host.config.tls_fingerprint.is_none());
        assert!(state.tls.unwrap().error.is_some());
        assert!(host.pending_trust.is_some());
    }

    #[test]
    fn binding_then_permission_result_and_timeout_reenable_controls() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut binding = binding();
        host.bindings = vec![binding.clone()];
        let request = RequestId::new();
        host.pending_control = Some((request, Instant::now()));
        host.last_control = Some(request);
        state.control_pending = true;
        binding.control_state.as_mut().unwrap().revision = 2;
        binding.control_state.as_mut().unwrap().mode = ControllerControlMode::FullAccess;
        let mut output = Vec::new();
        host.apply(
            AgentEvent::ControllerBindingsChanged {
                bindings: vec![binding.clone()],
            },
            &mut state,
            &mut output,
        );
        assert!(!state.control_enabled);
        host.apply(
            AgentEvent::ControlStateResult(remoteops_protocol::ControlStateResult {
                request_id: request,
                state: binding.control_state,
                error: None,
            }),
            &mut state,
            &mut output,
        );
        assert!(state.control_enabled);
        assert!(!state.control_pending);
        host.pending_control = Some((
            request,
            Instant::now().checked_sub(Duration::from_secs(6)).unwrap(),
        ));
        state.control_pending = true;
        host.update_control(&mut state);
        assert!(!state.control_enabled);
        host.tick(&mut state);
        assert!(state.control_enabled);
        assert!(!state.control_pending);
        assert!(state.control_error.is_some());
    }

    #[test]
    fn expired_authority_is_displayed_without_waiting_for_another_event() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut binding = binding();
        let authority = binding.control_state.as_mut().unwrap();
        authority.mode = ControllerControlMode::FullAccess;
        authority.expires_at = Some(chrono::Utc::now() - chrono::Duration::seconds(1));
        host.bindings = vec![binding];
        host.tick(&mut state);
        assert_eq!(state.control_summary, "授权已过期");
    }

    #[test]
    fn unbinding_clears_pending_and_does_not_apply_old_confirmation() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        host.bindings = vec![binding()];
        let request = RequestId::new();
        host.pending_control = Some((request, Instant::now()));
        host.last_control = Some(request);
        state.control_pending = true;
        state.code = Some("123-456-789".into());
        let mut output = Vec::new();
        host.apply(
            AgentEvent::ControllerCountChanged {
                active_connections: 0,
            },
            &mut state,
            &mut output,
        );
        host.apply(
            AgentEvent::ControlStateResult(remoteops_protocol::ControlStateResult {
                request_id: request,
                state: None,
                error: Some("old result".into()),
            }),
            &mut state,
            &mut output,
        );
        assert!(!state.control_pending);
        assert!(state.control_error.is_none());
        assert!(state.code.is_some());
    }

    #[test]
    fn unexpected_worker_exit_clears_code_authority_and_receiver() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let (sender, receiver) = bounded_agent_events(8);
        drop(sender);
        let (stop, _) = watch::channel(false);
        let thread = std::thread::spawn(|| {});
        while !thread.is_finished() {
            std::thread::yield_now();
        }
        host.worker = Some(Worker { stop, thread });
        host.events = Some(receiver);
        state.phase = ProductPhase::Controlled;
        state.code = Some("123-456-789".into());
        host.bindings = vec![binding()];
        host.tick(&mut state);
        assert_eq!(state.phase, ProductPhase::Failed);
        assert!(state.code.is_none());
        assert!(!state.control_enabled);
        assert!(host.events.is_none());
    }

    #[test]
    fn invalid_existing_configuration_stays_repairable_without_a_worker() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        std::fs::write(
            &path,
            br#"{"relay":"127.0.0.1:7443","server_name":"localhost","retry_seconds":0}"#,
        )
        .unwrap();
        let mut state = ProductSnapshot::default();
        let host = RuntimeHost::load(
            &crate::HostOptions {
                agent_config: Some(path),
                ..crate::HostOptions::default()
            },
            &mut state,
        )
        .unwrap();
        assert!(state.setup_needed);
        assert_eq!(state.phase, ProductPhase::Setup);
        assert!(host.worker.is_none());
        assert_ne!(state.detail, "");
        assert!(host.config.clone().normalize_and_validate().is_ok());
    }

    #[test]
    fn startup_diagnostics_stay_in_history_without_opening_command_observer() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        host.apply(
            AgentEvent::OperationLog(remoteops_agent::AgentOperationLog {
                repetition: None,
                request_id: None,
                occurred_at: chrono::Utc::now(),
                level: AgentLogLevel::Info,
                message: "宿主初始化".into(),
            }),
            &mut state,
            &mut output,
        );
        assert!(output.is_empty());
        assert_eq!(state.logs.len(), 1);
    }

    #[test]
    fn pairing_code_format_does_not_slice_non_ascii() {
        assert_eq!(format_code("123456789"), "123-456-789");
        assert_eq!(format_code("中文码"), "中文码");
    }

    #[test]
    fn native_profile_isolated_and_bad_configuration_remains_repairable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        std::fs::write(&path, b"not json").unwrap();
        let mut state = ProductSnapshot::default();
        let mut host = RuntimeHost::load(
            &crate::HostOptions {
                agent_config: Some(path.clone()),
                ..crate::HostOptions::default()
            },
            &mut state,
        )
        .unwrap();
        assert!(state.setup_needed);
        assert!(!host.profile.legacy_remoteops_paths);
        assert!(host.worker.is_none());
        assert!(
            host.action(
                ProductAction::SaveSetup {
                    relay: "invalid".into()
                },
                &mut state
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
        assert!(host.shutdown());
    }

    #[test]
    fn centralized_resources_preserve_legacy_config_and_reject_local_saves() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent-config.json");
        std::fs::write(
            &path,
            br#"{"resource_tools":{"index_url":"https://legacy.example.test/remoteops/v1/index.json","allowed_origins":["https://legacy.example.test"]},"default_full_control":false}"#,
        )
        .unwrap();
        let mut state = ProductSnapshot::default();
        let mut host = RuntimeHost::load(
            &crate::HostOptions {
                agent_config: Some(path.clone()),
                ..crate::HostOptions::default()
            },
            &mut state,
        )
        .unwrap();
        assert_eq!(
            state.resource_tools_index_url,
            "https://legacy.example.test/remoteops/v1/index.json"
        );
        assert_eq!(
            state.resource_tools_allowed_origins,
            ["https://legacy.example.test"]
        );
        host.action(
            ProductAction::SaveDefaultControl { full: false },
            &mut state,
        )
        .unwrap();
        let original = std::fs::read(&path).unwrap();
        let saved = AgentConfig::load_file(Some(&path)).unwrap();
        assert_eq!(
            saved.resource_tools.index_url,
            host.config.resource_tools.index_url
        );
        assert_eq!(
            saved.resource_tools.allowed_origins,
            host.config.resource_tools.allowed_origins
        );
        for language in [NativeLanguage::Chinese, NativeLanguage::English] {
            state.language = language;
            for index_url in [
                "https://replacement.example.test/index.json",
                "http://invalid.test",
                "",
            ] {
                let before = format!("{state:?}");
                let error = host
                    .action(
                        ProductAction::SaveResourceTools {
                            index_url: index_url.into(),
                            allowed_origins: vec!["https://replacement.example.test".into()],
                        },
                        &mut state,
                    )
                    .unwrap_err();
                assert!(error.contains(if language == NativeLanguage::Chinese {
                    "集中管理模式不支持"
                } else {
                    "Centralized mode does not support"
                }));
                assert_eq!(std::fs::read(&path).unwrap(), original);
                assert_eq!(format!("{state:?}"), before);
                assert!(!host.config.default_full_control);
                assert!(host.worker.is_none());
            }
        }
    }

    #[test]
    fn serial_output_updates_preserve_stable_id_total_and_single_log_record() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let mut output = Vec::new();
        let id = RequestId::new();
        host.apply(
            context(id, remoteops_agent::AgentOperationKind::Serial, None),
            &mut state,
            &mut output,
        );
        for total in [1, 128, 10_000] {
            let repetition = remoteops_agent::AgentOutputRepetition {
                record_id: 3,
                total,
            };
            host.apply(
                AgentEvent::OperationLog(remoteops_agent::AgentOperationLog {
                    repetition: Some(repetition),
                    request_id: Some(id),
                    occurred_at: chrono::Utc::now(),
                    level: AgentLogLevel::Info,
                    message: format!("same × {total}"),
                }),
                &mut state,
                &mut output,
            );
            host.apply(
                AgentEvent::OperationOutputUpdate {
                    request_id: id,
                    text: "same".into(),
                    repetition,
                },
                &mut state,
                &mut output,
            );
        }
        assert_eq!(state.logs.len(), 1);
        assert_eq!(state.unread, 1);
        assert_eq!(output.last().unwrap().text, "same × 10000");
        assert_eq!(
            output.last().unwrap().repetition,
            Some(super::super::ObservationRepetition {
                record_id: 3,
                total: 10_000
            })
        );
    }

    #[test]
    fn portable_preview_does_not_discover_the_legacy_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let (host, _) = isolated_host(directory.path());
        let legacy = directory
            .path()
            .join(ProductProfile::default().config_file_name);
        std::fs::write(&legacy, b"legacy configuration must stay untouched").unwrap();
        let mut paths = host.profile.paths();
        paths.portable_config = directory.path().join(&host.profile.config_file_name);
        paths.user_config = directory
            .path()
            .join("preview-user")
            .join(&host.profile.config_file_name);
        assert_ne!(paths.active_config(), legacy);
        assert!(!paths.active_config().exists());
        assert_ne!(
            host.profile.state_file_name,
            ProductProfile::default().state_file_name
        );
        assert_eq!(
            std::fs::read(&legacy).unwrap(),
            b"legacy configuration must stay untouched"
        );
    }

    #[test]
    fn stale_epoch_is_ignored_and_reconnect_clears_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        std::fs::write(&path, b"{}").unwrap();
        let mut state = ProductSnapshot::default();
        let mut host = RuntimeHost::load(
            &crate::HostOptions {
                agent_config: Some(path),
                ..crate::HostOptions::default()
            },
            &mut state,
        )
        .unwrap();
        let mut output = Vec::new();
        host.apply(AgentEvent::ConnectionEpoch(2), &mut state, &mut output);
        host.apply(
            AgentEvent::ScopedOperation {
                epoch: 1,
                event: Box::new(AgentEvent::OperationStateChanged {
                    request_id: RequestId::new(),
                    state: AgentOperationState::Running,
                }),
            },
            &mut state,
            &mut output,
        );
        assert!(output.is_empty());
        host.apply(
            AgentEvent::Reconnecting {
                message: "test".into(),
                retry_seconds: 1,
            },
            &mut state,
            &mut output,
        );
        assert!(host.epoch.is_none());
        assert!(!state.control_enabled);
    }

    #[test]
    fn closed_tls_result_channel_reports_failure_before_worker_exit() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let (stop, shutdown) = watch::channel(false);
        let signal = stop.clone();
        let thread = std::thread::spawn(move || {
            let started = Instant::now();
            while !*shutdown.borrow() && started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        host.worker = Some(Worker { stop, thread });
        host.trust_events = Some(receiver);
        state.phase = ProductPhase::Connecting;
        host.tick(&mut state);
        assert_eq!(state.phase, ProductPhase::Failed);
        assert!(host.trust_events.is_none());
        assert!(*signal.borrow());
        assert!(state.detail.contains("TLS"));
        assert!(host.shutdown());
    }

    #[test]
    fn empty_tls_result_channel_remains_pending() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let (_sender, receiver) = mpsc::channel();
        host.trust_events = Some(receiver);
        state.phase = ProductPhase::Connecting;
        host.tick(&mut state);
        assert_eq!(state.phase, ProductPhase::Connecting);
        assert!(host.trust_events.is_some());
        assert_eq!(state.detail, "");
    }

    #[test]
    fn closed_agent_error_channel_does_not_impersonate_tls_failure() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let (_events_sender, events) = bounded_agent_events(8);
        let (errors_sender, errors) = mpsc::channel();
        drop(errors_sender);
        host.events = Some(events);
        host.trust_events = Some(errors);
        state.phase = ProductPhase::Controlled;
        host.tick(&mut state);
        assert_eq!(state.phase, ProductPhase::Controlled);
        assert!(host.trust_events.is_none());
        assert_eq!(state.detail, "");
        host.apply(AgentEvent::Stopped, &mut state, &mut Vec::new());
        assert_eq!(state.phase, ProductPhase::Stopped);
        assert_eq!(state.detail, "");
    }

    #[test]
    fn cancelling_tls_confirmation_clears_pending_without_persisting_pin() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        let original = std::fs::read(&host.config_path).unwrap();
        host.pending_trust = Some(host.config.clone());
        state.tls = Some(TrustPrompt {
            relay: "127.0.0.1:7443".into(),
            fingerprint: "c".repeat(64),
            reason: "fixture".into(),
            error: None,
        });
        host.action(ProductAction::TrustCancel, &mut state).unwrap();
        assert_eq!(state.phase, ProductPhase::Stopped);
        assert!(host.pending_trust.is_none());
        assert!(state.tls.is_none());
        assert!(state.code.is_none());
        assert!(host.accept_trust(true, &mut state).is_err());
        assert_eq!(std::fs::read(&host.config_path).unwrap(), original);
    }

    #[test]
    fn saved_tls_confirmation_roundtrips_only_the_confirmed_pin() {
        let directory = tempfile::tempdir().unwrap();
        let (mut host, mut state) = isolated_host(directory.path());
        host.config.relay = "127.0.0.1:7443".into();
        host.config.server_name = "localhost".into();
        host.pending_trust = Some(host.config.clone());
        let fingerprint = "d".repeat(64);
        state.tls = Some(TrustPrompt {
            relay: host.config.relay.clone(),
            fingerprint: fingerprint.clone(),
            reason: "fixture".into(),
            error: None,
        });
        let accepted = host.accept_trust(true, &mut state).unwrap();
        let restored =
            AgentConfig::load_file_with_profile(Some(&host.config_path), &host.profile).unwrap();
        assert_eq!(
            accepted.tls_fingerprint.as_deref(),
            Some(fingerprint.as_str())
        );
        assert_eq!(restored.tls_fingerprint, accepted.tls_fingerprint);
        assert_eq!(restored.relay, accepted.relay);
        assert_eq!(restored.server_name, accepted.server_name);
        assert!(host.pending_trust.is_none());
        assert!(state.tls.is_none());
    }
}
