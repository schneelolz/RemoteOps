//! 产品控制台宿主；权限由 Relay 确认，始终禁止内建图形能力。
use std::io::{BufRead, Write};

use super::{
    AgentConfig, AgentEvent, AgentPermissionControl, AgentRuntimeOptions, Context,
    ControllerControlMode, DateTime, ProductProfile, RequestId, SessionControlState, Utc,
    initialize_tracing, print_agent_event, redact_diagnostic, run_agent_with_options,
    shutdown_signal,
};
use std::{collections::BTreeMap, sync::mpsc as std_mpsc};
use tokio::{
    sync::{mpsc, watch},
    time::{Duration, interval, timeout},
};

/// 使用既有产品配置运行无图形 Agent，并接受现场控制台命令。
///
/// # Errors
///
/// 产品配置、Agent 生命周期或信号监听失败时返回错误。
pub async fn run_product_console(
    profile: ProductProfile,
    config: AgentConfig,
) -> anyhow::Result<()> {
    profile.validate()?;
    initialize_tracing();
    println!("{} {} · CLI 无图形模式", profile.name, profile.version);
    println!("已禁用截图、桌面输入等内建图形能力；权限沿用现有配置。");
    print_help();
    let permission = AgentPermissionControl::from_config(&config);
    let (events, receiver) = std_mpsc::channel();
    let (shutdown, shutdown_receiver) = watch::channel(false);
    let options = AgentRuntimeOptions::console(profile.clone());
    let mut task = tokio::spawn(run_agent_with_options(
        config,
        Some(events),
        shutdown_receiver,
        permission.clone(),
        options,
    ));
    let mut commands = read_commands();
    let mut input_open = true;
    let mut state = ConsoleState::default();
    let mut poll = interval(Duration::from_millis(50));
    let signal = shutdown_signal();
    tokio::pin!(signal);
    let mut completed = None;
    let mut signal_error = None;
    loop {
        // 先应用断线和绑定快照，再处理现场命令，避免对旧会话授权。
        drain_events(&receiver, &mut state, &profile);
        tokio::select! {
            result = &mut task => {
                completed = Some(result);
                break;
            }
            result = &mut signal => {
                signal_error = result.err();
                break;
            }
            command = commands.recv(), if input_open => {
                drain_events(&receiver, &mut state, &profile);
                if let Some(command) = command {
                    if state.command(&command, &permission) {
                        break;
                    }
                } else {
                    input_open = false;
                    println!("控制台输入已关闭，Agent 继续运行；按 Ctrl+C 停止。");
                }
            }
            _ = poll.tick() => {}
        }
    }
    let _ = shutdown.send(true);
    let result = if let Some(result) = completed {
        result.context("Agent 运行任务异常结束")?
    } else if let Ok(result) = timeout(Duration::from_secs(5), &mut task).await {
        result.context("Agent 停止任务异常结束")?
    } else {
        task.abort();
        let _ = task.await;
        println!("Agent 停止等待超时，已取消后台任务。");
        Ok(())
    };
    drain_events(&receiver, &mut state, &profile);
    if let Some(error) = signal_error {
        return Err(error).context("无法监听 Ctrl+C");
    }
    result
}

fn read_commands() -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel(16);
    // 独立线程不属于 Tokio 阻塞线程池；退出时不等待可能阻塞的 stdin。
    std::thread::spawn(move || {
        let input = std::io::stdin();
        for line in input.lock().lines() {
            match line {
                Ok(line) => {
                    if sender.blocking_send(line).is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
    });
    receiver
}

fn drain_events(
    receiver: &std_mpsc::Receiver<AgentEvent>,
    state: &mut ConsoleState,
    profile: &ProductProfile,
) {
    while let Ok(event) = receiver.try_recv() {
        state.observe(&event);
        match &event {
            AgentEvent::Started {
                agent_instance_id,
                relay,
                transfer_root,
                ..
            } => {
                println!("{} Agent 已启动", profile.name);
                println!("Agent GUID：{agent_instance_id}");
                println!("目标 Relay：{relay}");
                println!("文件交换目录：{}", transfer_root.display());
            }
            AgentEvent::Stopped => println!("{} Agent 已停止", profile.name),
            AgentEvent::Failed { message } => {
                eprintln!(
                    "{} Agent 运行失败：{}",
                    profile.name,
                    redact_diagnostic(message)
                );
            }
            // 只有与当前绑定匹配的待处理请求才可报告授权成功。
            AgentEvent::ControlStateResult(_) => {}
            _ => print_agent_event(&event),
        }
    }
    let _ = std::io::stdout().flush();
}

fn print_help() {
    println!("命令：help 帮助 | status 状态 | full 临时授权当前会话完全控制");
    println!("      confirm 撤销完全控制并恢复逐项确认 | quit 停止（或 Ctrl+C）");
    println!("full/confirm 需要工程师已连接；只作用于当前会话，不保存默认偏好。");
}

#[derive(Default)]
struct ConsoleState {
    // 仅保存当前连接公开的控制码与完整绑定快照。
    connected: bool,
    pairing_code: Option<String>,
    bindings: Vec<SessionControlState>,
    // 请求成功必须与发起时的绑定及请求模式一致。
    pending: BTreeMap<RequestId, (SessionControlState, ControllerControlMode)>,
}

impl ConsoleState {
    fn clear_connection(&mut self) {
        self.connected = false;
        self.pairing_code = None;
        self.bindings.clear();
        self.pending.clear();
    }

    fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Connected { pairing_code, .. } => {
                self.clear_connection();
                self.connected = true;
                self.pairing_code = Some(pairing_code.clone());
            }
            AgentEvent::Registered { .. } => {
                self.clear_connection();
                self.connected = true;
            }
            AgentEvent::ConnectionEpoch(_)
            | AgentEvent::ControllerCountChanged {
                active_connections: 0,
            } => {
                self.bindings.clear();
                self.pending.clear();
            }
            AgentEvent::Connecting
            | AgentEvent::Reconnecting { .. }
            | AgentEvent::Stopped
            | AgentEvent::Failed { .. } => {
                self.clear_connection();
            }
            AgentEvent::ControllerBindingsChanged { bindings } if self.connected => {
                self.bindings = bindings
                    .iter()
                    .filter_map(|b| b.control_state.clone())
                    .collect();
                self.bindings.sort_by_key(|state| state.session_id);
                self.bindings.dedup_by(|a, b| a == b);
                self.pending.retain(|_, (expected, _)| {
                    self.bindings
                        .iter()
                        .any(|state| same_binding(expected, state))
                });
            }
            AgentEvent::ControlStateResult(result) => {
                if let Some((expected, mode)) = self.pending.remove(&result.request_id) {
                    let confirmed = result.state.as_ref().is_some_and(|state| {
                        same_binding(&expected, state)
                            && state.mode == mode
                            && self.bindings.iter().any(|current| current == state)
                    });
                    if result.error.is_none() && confirmed {
                        println!("Relay 已确认当前会话权限：{mode:?}");
                    } else {
                        println!(
                            "权限请求未确认：{}",
                            redact_diagnostic(
                                result.error.as_deref().unwrap_or("绑定或权限状态已变化")
                            )
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn command(&mut self, command: &str, permission: &AgentPermissionControl) -> bool {
        match command.trim().to_ascii_lowercase().as_str() {
            "" => {}
            "help" => print_help(),
            "status" => {
                println!(
                    "Relay：{}",
                    if self.connected {
                        "已连接"
                    } else {
                        "未连接"
                    }
                );
                if let Some(code) = &self.pairing_code {
                    println!("控制码：{code}");
                }
                if self.bindings.is_empty() {
                    println!("当前无已确认的控制会话");
                }
                for binding in &self.bindings {
                    println!(
                        "会话 {} 有效权限：{}",
                        binding.session_id,
                        effective_permission_label(binding, Utc::now())
                    );
                    if let Some(expires_at) = binding.expires_at {
                        println!("授权到期时间：{}", expires_at.to_rfc3339());
                    }
                }
                println!("待确认权限请求：{}", self.pending.len());
            }
            "full" => self.request(permission, ControllerControlMode::FullAccess),
            "confirm" => self.request(permission, ControllerControlMode::StepByStep),
            "quit" => return true,
            _ => println!("未知命令；输入 help 查看帮助。"),
        }
        let _ = std::io::stdout().flush();
        false
    }

    fn request(&mut self, permission: &AgentPermissionControl, mode: ControllerControlMode) {
        if !self.pending.is_empty() {
            println!("已有权限请求等待 Relay 确认，请收到结果后重试。");
            return;
        }
        if !self.connected || self.bindings.len() != 1 {
            println!("需要唯一且已确认的控制会话后才能变更权限；请先输入 status。");
            return;
        }
        let state = self.bindings[0].clone();
        match permission.request_control(state.clone(), mode) {
            Ok(id) => {
                self.pending.insert(id, (state, mode));
                println!("权限请求已提交，等待 Relay 确认。");
            }
            Err(error) => println!("权限请求未提交：{}", redact_diagnostic(&error.to_string())),
        }
    }
}

fn same_binding(a: &SessionControlState, b: &SessionControlState) -> bool {
    a.session_id == b.session_id
        && a.agent_generation == b.agent_generation
        && a.controller_id == b.controller_id
        && a.controller_generation == b.controller_generation
}

fn effective_permission_label(state: &SessionControlState, now: DateTime<Utc>) -> &'static str {
    if state.expires_at.is_some_and(|expires_at| expires_at <= now) {
        return "已过期，需重新授权";
    }
    match state.mode {
        ControllerControlMode::FullAccess => "完全控制",
        ControllerControlMode::StepByStep => "逐项确认",
        ControllerControlMode::ReadOnly => "只读",
        ControllerControlMode::ExternalApproval => "外部审批",
        ControllerControlMode::Expired => "已过期，需重新授权",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentControllerBinding, AgentRunMode, ControlSource, ControlStateResult,
        ControllerInstanceId, ControllerOwnerId, PermissionMode, SessionId,
    };

    fn binding() -> SessionControlState {
        SessionControlState {
            session_id: SessionId::new(),
            agent_generation: 2,
            controller_id: ControllerInstanceId::new(),
            controller_generation: 3,
            revision: 1,
            mode: ControllerControlMode::StepByStep,
            source: None,
            expires_at: None,
        }
    }

    #[test]
    fn console_preserves_product_and_forbids_visual() {
        let profile = ProductProfile {
            name: "定制产品".into(),
            version: "1.2.3".into(),
            directory_name: "Custom".into(),
            ..ProductProfile::default()
        };
        let config = AgentConfig::for_product(&profile);
        let options = AgentRuntimeOptions::console(profile.clone());
        assert_eq!(options.product.name, profile.name);
        assert_eq!(options.product.version, profile.version);
        assert_eq!(
            config.state_file,
            AgentConfig::for_product(&options.product).state_file
        );
        assert_eq!(options.run_mode, AgentRunMode::Console);
        assert!(!options.visual_enabled);
        assert!(!options.capability_policy.visual_allowed());
        assert!(options.visual_provider_factory.is_none());
    }

    #[test]
    fn effective_permission_expires_at_deadline() {
        let now = Utc::now();
        let mut state = binding();
        state.mode = ControllerControlMode::FullAccess;
        state.expires_at = Some(now + chrono::Duration::seconds(1));
        assert_eq!(effective_permission_label(&state, now), "完全控制");
        state.expires_at = Some(now);
        assert_eq!(
            effective_permission_label(&state, now),
            "已过期，需重新授权"
        );
        state.expires_at = Some(now - chrono::Duration::seconds(1));
        assert_eq!(
            effective_permission_label(&state, now),
            "已过期，需重新授权"
        );
        state.expires_at = None;
        assert_eq!(effective_permission_label(&state, now), "完全控制");
        state.mode = ControllerControlMode::Expired;
        assert_eq!(
            effective_permission_label(&state, now),
            "已过期，需重新授权"
        );
    }

    #[test]
    fn commands_require_current_binding_and_never_change_defaults() {
        let control = AgentPermissionControl::default();
        let mut requests = control.control_requests.subscribe();
        let mut state = ConsoleState::default();
        assert!(!state.command("full", &control));
        assert!(requests.try_recv().is_err());
        state.connected = true;
        state.bindings.push(binding());
        state.command("full", &control);
        let request = requests.try_recv().unwrap();
        assert_eq!(request.mode, Some(ControllerControlMode::FullAccess));
        assert_eq!(request.source, Some(ControlSource::Local));
        assert_eq!(request.expected, Some(state.bindings[0].clone()));
        assert_eq!(state.bindings[0].mode, ControllerControlMode::StepByStep);
        assert!(!control.default_full_control());
        state.command("confirm", &control);
        assert!(requests.try_recv().is_err());
        assert_eq!(state.pending.len(), 1);
        let mut confirmed = state.bindings[0].clone();
        confirmed.revision += 1;
        confirmed.mode = ControllerControlMode::FullAccess;
        confirmed.source = Some(ControlSource::Local);
        state.observe(&AgentEvent::ControllerBindingsChanged {
            bindings: vec![AgentControllerBinding {
                control_state: Some(confirmed.clone()),
                owner_id: ControllerOwnerId::new(),
                controller_kind: remoteops_protocol::ControllerKind::Ai,
                permission_mode: PermissionMode::FullAccess,
                controller_control_mode: Some(ControllerControlMode::FullAccess),
                full_access_authorized_locally: true,
            }],
        });
        state.observe(&AgentEvent::ControlStateResult(ControlStateResult {
            request_id: request.request_id,
            state: Some(confirmed.clone()),
            error: None,
        }));
        assert!(state.pending.is_empty());
        state.command("confirm", &control);
        let request = requests.try_recv().unwrap();
        assert_eq!(request.mode, Some(ControllerControlMode::StepByStep));
        assert_eq!(request.expected, Some(confirmed));
        assert!(state.command("quit", &control));
    }

    #[test]
    fn rejected_permission_request_allows_retry() {
        let control = AgentPermissionControl::default();
        let mut requests = control.control_requests.subscribe();
        let mut state = ConsoleState {
            connected: true,
            bindings: vec![binding()],
            ..ConsoleState::default()
        };
        state.command("full", &control);
        let request = requests.try_recv().unwrap();
        state.observe(&AgentEvent::ControlStateResult(ControlStateResult {
            request_id: request.request_id,
            state: Some(state.bindings[0].clone()),
            error: Some("控制快照已过期".into()),
        }));
        assert!(state.pending.is_empty());
        state.command("full", &control);
        let retry = requests.try_recv().unwrap();
        assert_ne!(retry.request_id, request.request_id);
        assert_eq!(retry.expected, Some(state.bindings[0].clone()));
        assert_eq!(state.pending.len(), 1);
    }

    #[test]
    fn disconnect_discards_pending_authorization_and_old_results() {
        let control = AgentPermissionControl::default();
        let _requests = control.control_requests.subscribe();
        let mut state = ConsoleState {
            connected: true,
            bindings: vec![binding()],
            ..ConsoleState::default()
        };
        state.command("full", &control);
        let (id, (mut old, _)) = state
            .pending
            .iter()
            .next()
            .map(|(id, item)| (*id, item.clone()))
            .unwrap();
        state.observe(&AgentEvent::Reconnecting {
            message: "断线".into(),
            retry_seconds: 3,
        });
        old.mode = ControllerControlMode::FullAccess;
        state.observe(&AgentEvent::ControlStateResult(ControlStateResult {
            request_id: id,
            state: Some(old),
            error: None,
        }));
        assert!(!state.connected);
        assert!(state.bindings.is_empty());
        assert!(state.pending.is_empty());
        state.command("full", &control);
        assert!(state.pending.is_empty());
    }

    #[test]
    fn replaced_binding_discards_pending_request() {
        let control = AgentPermissionControl::default();
        let _requests = control.control_requests.subscribe();
        let mut state = ConsoleState {
            connected: true,
            bindings: vec![binding()],
            ..ConsoleState::default()
        };
        state.command("full", &control);
        let replacement = AgentControllerBinding {
            control_state: Some(binding()),
            owner_id: ControllerOwnerId::new(),
            controller_kind: remoteops_protocol::ControllerKind::Ai,
            permission_mode: PermissionMode::ApprovalRequired,
            controller_control_mode: None,
            full_access_authorized_locally: false,
        };
        state.observe(&AgentEvent::ControllerBindingsChanged {
            bindings: vec![replacement],
        });
        assert!(state.pending.is_empty());
    }
}
