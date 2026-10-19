//! 产品能力上限、宿主事实和延迟图形 Provider 的统一运行边界。
use super::{ProductProfile, capabilities_from_environment};
use anyhow::Context;
use remoteops_domain::{
    AgentRunMode, Capability, CapabilityAvailability, CapabilitySet, EnvironmentProfile,
    PermissionMode, RemoteOperation, RequestId, SessionId, TargetCapabilityStatus, TargetContext,
    TargetGuidance, TargetRuntimeFacts,
};
use remoteops_policy::{DefaultPolicy, PolicyDecision};
pub use remoteops_product::AgentCapabilityPolicy;
use remoteops_protocol::PairingMode;
use remoteops_visual::{VisualProvider, VisualProviderError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const VISUAL_PROBE_BUDGET: Duration = Duration::from_secs(35);
const VISUAL_PROBE_COOLDOWN: Duration = Duration::from_secs(3);

/// 延迟创建真实图形 Provider 的可信宿主工厂。
pub type VisualProviderFactory = Arc<dyn Fn() -> Arc<dyn VisualProvider> + Send + Sync>;

/// 只由产品宿主构造的运行约束；普通配置、环境变量和远程上下文不能扩张能力。
#[derive(Clone)]
pub struct AgentRuntimeOptions {
    /// 产品显示信息与端点规则；保持原 `ProductProfile` 契约。
    pub product: ProductProfile,
    /// 真实宿主入口，不能根据图形环境变量推断。
    pub run_mode: AgentRunMode,
    /// 本次运行不可被 `FullAccess` 扩张的产品能力上限。
    pub capability_policy: AgentCapabilityPolicy,
    /// 宿主请求图形能力；false 进一步收紧产品上限。
    pub visual_enabled: bool,
    /// 仅允许在产品和原生宿主事实允许时调用的工厂。
    pub visual_provider_factory: Option<VisualProviderFactory>,
    /// 普通临时控制码或门户受管按需邀请。
    pub pairing_mode: PairingMode,
    /// 产品说明与案例引用，只提供给当前目标，不授予权限。
    pub guidance: Vec<TargetGuidance>,
}
impl AgentRuntimeOptions {
    /// 创建兼容原 GUI 行为的交互宿主配置。
    #[must_use]
    pub fn interactive(product: ProductProfile) -> Self {
        Self {
            product,
            run_mode: AgentRunMode::Interactive,
            capability_policy: AgentCapabilityPolicy::default(),
            visual_enabled: cfg!(windows),
            visual_provider_factory: None,
            pairing_mode: PairingMode::AutoPerBoot,
            guidance: Vec::new(),
        }
    }
    /// 创建无图形服务产品配置；控制台诊断也保留该产品上限。
    #[must_use]
    pub fn service(product: ProductProfile) -> Self {
        Self {
            run_mode: AgentRunMode::WindowsService,
            capability_policy: AgentCapabilityPolicy::headless(),
            visual_enabled: false,
            ..Self::interactive(product)
        }
    }
}

/// 产品或宿主能力拒绝，稳定错误码透传至协议响应。
#[derive(Debug)]
pub(crate) struct RuntimeDenied {
    pub code: String,
}
impl std::fmt::Display for RuntimeDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "目标运行约束拒绝操作：{}", self.code)
    }
}
impl std::error::Error for RuntimeDenied {}

// 每个运行边界独立持有图形探测状态，不能跨目标共享。
struct VisualState {
    context: TargetContext,
    diagnostic: Option<String>,
    last_probe: Option<Instant>,
}

#[derive(Clone)]
pub(crate) struct RuntimeBoundary {
    pub context: Arc<TargetContext>,
    pub policy: AgentCapabilityPolicy,
    // 同步快照不跨异步等待持锁。
    visual_state: Arc<Mutex<Option<VisualState>>>,
    // 同一目标的主动查询合并为串行且有冷却的探测。
    visual_probe: Arc<tokio::sync::Mutex<()>>,
}
impl Default for RuntimeBoundary {
    fn default() -> Self {
        Self {
            context: Arc::new(TargetContext::default()),
            policy: AgentCapabilityPolicy::default(),
            visual_state: Arc::new(Mutex::new(None)),
            visual_probe: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}
impl RuntimeBoundary {
    fn latest_context(&self) -> TargetContext {
        self.visual_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map_or_else(|| (*self.context).clone(), |state| state.context.clone())
    }

    fn hard_visual_reason(&self) -> Option<&'static str> {
        if !self.policy.visual_allowed() || self.context.run_mode == AgentRunMode::WindowsService {
            Some("product_policy_disabled")
        } else if self.context.run_mode == AgentRunMode::Unknown {
            Some("host_context_unknown")
        } else {
            None
        }
    }

    pub fn snapshot(&self) -> TargetContext {
        let mut context = self.latest_context();
        context.facts = native_facts();
        if let Some(reason) = self.hard_visual_reason() {
            set_visual_status(&mut context, Some(reason));
        } else if context.facts.interactive_desktop != CapabilityAvailability::Available {
            set_visual_status(&mut context, Some("no_interactive_desktop"));
        }
        context
    }

    /// 返回稳定错误码、耗时和经过脱敏截断的诊断，不包含窗口观察结果。
    pub fn visual_diagnostic(&self) -> Option<String> {
        self.visual_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|state| state.diagnostic.clone())
    }

    /// 在硬策略和最新原生桌面事实允许时重新进行轻量图形探测。
    pub async fn refresh_visual(&self, provider: &dyn VisualProvider) -> TargetContext {
        let _probe_guard = self.visual_probe.lock().await;
        self.refresh_visual_unlocked(
            provider,
            native_facts(),
            VISUAL_PROBE_COOLDOWN,
            VISUAL_PROBE_BUDGET,
        )
        .await
    }

    async fn refresh_visual_with_facts(
        &self,
        provider: &dyn VisualProvider,
        facts: TargetRuntimeFacts,
        cooldown: Duration,
        budget: Duration,
    ) -> TargetContext {
        let _probe_guard = self.visual_probe.lock().await;
        self.refresh_visual_unlocked(provider, facts, cooldown, budget)
            .await
    }

    async fn refresh_visual_unlocked(
        &self,
        provider: &dyn VisualProvider,
        facts: TargetRuntimeFacts,
        cooldown: Duration,
        budget: Duration,
    ) -> TargetContext {
        let mut context = self.latest_context();
        let desktop_changed = context.facts.interactive_desktop != facts.interactive_desktop;
        context.facts = facts;
        let reason = self.hard_visual_reason().or_else(|| {
            (context.facts.interactive_desktop != CapabilityAvailability::Available)
                .then_some("no_interactive_desktop")
        });
        {
            let mut state = self
                .visual_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if reason.is_none()
                && !desktop_changed
                && state
                    .as_ref()
                    .and_then(|state| state.last_probe)
                    .is_some_and(|last| last.elapsed() < cooldown)
                && let Some(state) = state.as_mut()
            {
                state.context.facts = context.facts;
                return state.context.clone();
            }
            // 先发布最新宿主事实，延迟 Provider 只在此硬边界允许后创建。
            let mut pending = context.clone();
            set_visual_status(&mut pending, reason.or(Some("visual_probe_pending")));
            *state = Some(VisualState {
                context: pending,
                diagnostic: None,
                last_probe: None,
            });
        }
        let started = Instant::now();
        let attempted = reason.is_none();
        let (reason, detail) = if let Some(reason) = reason {
            (Some(reason), None)
        } else {
            match tokio::time::timeout(
                budget,
                provider.observe(RequestId::new(), SessionId::new(), false, false),
            )
            .await
            {
                Err(_) => (
                    Some("visual_probe_timeout"),
                    Some("desktop observation exceeded startup budget".to_owned()),
                ),
                Ok(Err(error)) => {
                    let code = match &error {
                        VisualProviderError::NoInteractiveDesktop => "no_interactive_desktop",
                        VisualProviderError::Unavailable => "visual_provider_unavailable",
                        VisualProviderError::Rejected(_) => "visual_probe_rejected",
                        VisualProviderError::Protocol(_) => "visual_probe_error",
                    };
                    (
                        Some(code),
                        Some(
                            super::redact_diagnostic(&error.to_string())
                                .chars()
                                .take(512)
                                .collect(),
                        ),
                    )
                }
                Ok(Ok(observation))
                    if observation.state != remoteops_domain::VisualSessionState::Ready =>
                {
                    (Some("visual_desktop_not_ready"), None)
                }
                Ok(Ok(observation)) if observation.windows.is_empty() => {
                    (Some("visual_windows_unavailable"), None)
                }
                Ok(Ok(observation)) if observation.active_window_fingerprint.is_none() => {
                    (Some("visual_foreground_unavailable"), None)
                }
                Ok(Ok(_)) => (None, None),
            }
        };
        set_visual_status(&mut context, reason);
        let diagnostic = format!(
            "{}; elapsed_ms={}; {}",
            reason.unwrap_or("visual_probe_ready"),
            started.elapsed().as_millis(),
            detail.unwrap_or_default()
        );
        *self
            .visual_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(VisualState {
            context: context.clone(),
            diagnostic: Some(diagnostic),
            last_probe: attempted.then(Instant::now),
        });
        context
    }

    /// 只允许只读观察请求触发暂态图形故障恢复；写操作仍检查当前有效能力。
    pub fn check_for_dispatch(&self, operation: &RemoteOperation) -> anyhow::Result<()> {
        if !matches!(operation, RemoteOperation::VisualObserve { .. }) {
            return self.check_current(operation);
        }
        if let Some(reason) = self.hard_visual_reason() {
            return Err(RuntimeDenied {
                code: reason.to_owned(),
            }
            .into());
        }
        let context = self.snapshot();
        if !context.visual_probe_allowed() {
            return Err(RuntimeDenied {
                code: context
                    .visual_disabled_reason()
                    .unwrap_or("host_context_unknown")
                    .to_owned(),
            }
            .into());
        }
        Ok(())
    }

    pub fn check_current(&self, operation: &RemoteOperation) -> anyhow::Result<()> {
        self.check(operation)?;
        if operation.is_visual()
            && let Some(reason) = self.snapshot().visual_disabled_reason()
        {
            return Err(RuntimeDenied {
                code: reason.to_owned(),
            }
            .into());
        }
        Ok(())
    }

    pub fn check(&self, operation: &RemoteOperation) -> anyhow::Result<()> {
        if operation.is_visual() {
            if !self.policy.visual_allowed() {
                return Err(RuntimeDenied {
                    code: "product_policy_disabled".into(),
                }
                .into());
            }
            if let Some(reason) = self.latest_context().visual_disabled_reason() {
                return Err(RuntimeDenied {
                    code: reason.to_owned(),
                }
                .into());
            }
        }
        if self.policy.read_only() {
            let policy = DefaultPolicy::default();
            let normalized = super::normalize_operation(&policy, operation.clone());
            if matches!(
                operation,
                RemoteOperation::UploadFileChunk { .. }
                    | RemoteOperation::CompleteUploadFile { .. }
            ) || !matches!(
                policy.evaluate_with_mode(
                    PermissionMode::ReadOnly,
                    remoteops_domain::EventSource::Human,
                    &normalized
                ),
                PolicyDecision::Allow
            ) {
                return Err(RuntimeDenied {
                    code: "product_policy_disabled".into(),
                }
                .into());
            }
        }
        Ok(())
    }
}

pub(crate) fn native_facts() -> TargetRuntimeFacts {
    let native = remoteops_native_host::collect();
    TargetRuntimeFacts {
        process_session_id: native.process_session_id,
        interactive_desktop: match native.interactive_desktop {
            Some(true) => CapabilityAvailability::Available,
            Some(false) => CapabilityAvailability::Unavailable,
            None => CapabilityAvailability::Unknown,
        },
        observed_at: chrono::Utc::now(),
        source: format!("native_host:{}", native.reason),
    }
}

pub(crate) async fn prepare(
    options: &AgentRuntimeOptions,
    environment: &EnvironmentProfile,
    facts: TargetRuntimeFacts,
) -> anyhow::Result<(RuntimeBoundary, Arc<dyn VisualProvider>, CapabilitySet)> {
    options.product.validate()?;
    if options.pairing_mode == PairingMode::Unknown {
        anyhow::bail!("未知的控制码签发模式");
    }
    let reason = if !options.capability_policy.visual_allowed()
        || !options.visual_enabled
        || options.run_mode == AgentRunMode::WindowsService
    {
        Some("product_policy_disabled")
    } else if options.run_mode == AgentRunMode::Unknown {
        Some("host_context_unknown")
    } else if facts.interactive_desktop != CapabilityAvailability::Available {
        Some("no_interactive_desktop")
    } else {
        Some("visual_probe_pending")
    };
    let capabilities = capabilities_from_environment(environment, false);
    let statuses =
        capability_statuses(&capabilities, reason, options.capability_policy.read_only());
    let context = TargetContext {
        product_id: options.product.id.clone(),
        name: options.product.name.clone(),
        version: options.product.version.clone(),
        run_mode: options.run_mode,
        facts,
        capabilities: statuses,
        guidance: options.guidance.clone(),
        ..TargetContext::default()
    };
    context
        .validate()
        .map_err(anyhow::Error::msg)
        .context("产品目标上下文无效")?;
    // visual_enabled=false 是旧入口也不可绕过的硬上限。
    let policy = if !options.visual_enabled || options.run_mode == AgentRunMode::WindowsService {
        if options.capability_policy.read_only() {
            AgentCapabilityPolicy::headless().with_read_only()
        } else {
            AgentCapabilityPolicy::headless()
        }
    } else {
        options.capability_policy
    };
    let mut boundary = RuntimeBoundary {
        context: Arc::new(context),
        policy,
        ..RuntimeBoundary::default()
    };
    let provider: Arc<dyn VisualProvider> = Arc::new(DeferredVisualProvider {
        boundary: boundary.clone(),
        factory: options
            .visual_provider_factory
            .clone()
            .unwrap_or_else(|| Arc::new(remoteops_visual::default_visual_provider)),
        provider: OnceLock::new(),
    });
    let context = boundary
        .refresh_visual_with_facts(
            provider.as_ref(),
            boundary.context.facts.clone(),
            Duration::ZERO,
            VISUAL_PROBE_BUDGET,
        )
        .await;
    let capabilities = capabilities_from_environment(environment, context.visual_allowed());
    boundary.context = Arc::new(context);
    Ok((boundary, provider, capabilities))
}

fn set_visual_status(context: &mut TargetContext, reason: Option<&str>) {
    for capability in &mut context.capabilities {
        if capability.capability == "visual" {
            capability.status = if reason.is_some() {
                CapabilityAvailability::Unavailable
            } else {
                CapabilityAvailability::Available
            };
            capability.reasons = reason.into_iter().map(str::to_owned).collect();
        }
    }
}

// 工厂本身不接触图形，只有可信宿主事实允许时才初始化一次真实 Provider。
struct DeferredVisualProvider {
    boundary: RuntimeBoundary,
    factory: VisualProviderFactory,
    provider: OnceLock<Arc<dyn VisualProvider>>,
}
impl DeferredVisualProvider {
    fn get(&self) -> Result<&Arc<dyn VisualProvider>, VisualProviderError> {
        if let Some(reason) = self.boundary.hard_visual_reason() {
            return Err(VisualProviderError::Rejected(reason.to_owned()));
        }
        if self.boundary.latest_context().facts.interactive_desktop
            != CapabilityAvailability::Available
        {
            return Err(VisualProviderError::NoInteractiveDesktop);
        }
        Ok(self.provider.get_or_init(|| (self.factory)()))
    }
    fn check(&self, operation: &RemoteOperation) -> Result<(), VisualProviderError> {
        self.boundary.check_current(operation).map_err(|error| {
            VisualProviderError::Rejected(error.downcast_ref::<RuntimeDenied>().map_or_else(
                || "host_context_unknown".to_owned(),
                |denied| denied.code.clone(),
            ))
        })
    }
}

#[async_trait::async_trait]
impl VisualProvider for DeferredVisualProvider {
    async fn observe(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        screenshot: bool,
        tree: bool,
    ) -> Result<remoteops_domain::VisualObservation, VisualProviderError> {
        self.get()?
            .observe(request_id, session_id, screenshot, tree)
            .await
    }
    async fn wait_for(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        condition: &str,
        timeout: u64,
    ) -> Result<remoteops_domain::VisualObservation, VisualProviderError> {
        self.check(&RemoteOperation::VisualWaitFor {
            condition: condition.to_owned(),
            timeout_millis: timeout,
        })?;
        self.get()?
            .wait_for(request_id, session_id, condition, timeout)
            .await
    }
    async fn invoke(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &remoteops_domain::VisualTarget,
        action: &str,
    ) -> Result<remoteops_domain::VisualActionResult, VisualProviderError> {
        self.check(&RemoteOperation::VisualInvoke {
            target: target.clone(),
            action: action.to_owned(),
        })?;
        self.get()?
            .invoke(request_id, session_id, target, action)
            .await
    }
    async fn type_text(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &remoteops_domain::VisualTarget,
        text: &str,
    ) -> Result<remoteops_domain::VisualActionResult, VisualProviderError> {
        self.check(&RemoteOperation::VisualTypeText {
            target: target.clone(),
            text: text.to_owned(),
        })?;
        self.get()?
            .type_text(request_id, session_id, target, text)
            .await
    }
    async fn send_input(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &remoteops_domain::VisualTarget,
        input: &str,
    ) -> Result<remoteops_domain::VisualActionResult, VisualProviderError> {
        self.check(&RemoteOperation::VisualSendInput {
            target: target.clone(),
            input: input.to_owned(),
        })?;
        self.get()?
            .send_input(request_id, session_id, target, input)
            .await
    }
    async fn stop(&self, session_id: SessionId) -> Result<(), VisualProviderError> {
        self.check(&RemoteOperation::VisualStop)?;
        if let Some(reason) = self.boundary.hard_visual_reason() {
            return Err(VisualProviderError::Rejected(reason.to_owned()));
        }
        if let Some(provider) = self.provider.get() {
            provider.stop(session_id).await
        } else {
            Ok(())
        }
    }
}

fn capability_statuses(
    capabilities: &CapabilitySet,
    reason: Option<&str>,
    read_only: bool,
) -> Vec<TargetCapabilityStatus> {
    let mut statuses = capabilities
        .iter()
        .filter(|capability| **capability != Capability::Visual)
        .map(|capability| TargetCapabilityStatus {
            capability: serde_json::to_value(capability)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            status: CapabilityAvailability::Available,
            reasons: Vec::new(),
        })
        .collect::<Vec<_>>();
    statuses.push(TargetCapabilityStatus {
        capability: "visual".into(),
        status: if reason.is_none() {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        },
        reasons: reason.into_iter().map(str::to_owned).collect(),
    });
    statuses.push(TargetCapabilityStatus {
        capability: "mutating_operations".into(),
        status: if read_only {
            CapabilityAvailability::Unavailable
        } else {
            CapabilityAvailability::Available
        },
        reasons: if read_only {
            vec!["product_policy_disabled".into()]
        } else {
            Vec::new()
        },
    });
    statuses
}

#[cfg(test)]
pub(crate) fn visual_operations() -> Vec<RemoteOperation> {
    let target = remoteops_domain::VisualTarget::Control {
        window_fingerprint: "window".into(),
        automation_id: Some("control".into()),
        name: None,
        control_type: None,
        target_fingerprint: "target".into(),
    };
    vec![
        RemoteOperation::VisualObserve {
            include_screenshot: true,
            include_ui_tree: true,
        },
        RemoteOperation::VisualWaitFor {
            condition: r#"{"kind":"window","title":"test"}"#.into(),
            timeout_millis: 500,
        },
        RemoteOperation::VisualInvoke {
            target: target.clone(),
            action: "invoke".into(),
        },
        RemoteOperation::VisualTypeText {
            target: target.clone(),
            text: "test".into(),
        },
        RemoteOperation::VisualSendInput {
            target,
            input: "click".into(),
        },
        RemoteOperation::VisualStop,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct ReadyProvider(AtomicUsize, AtomicBool, Duration);
    impl ReadyProvider {
        fn ready() -> Self {
            Self(AtomicUsize::new(0), AtomicBool::new(false), Duration::ZERO)
        }
    }
    #[async_trait::async_trait]
    impl VisualProvider for ReadyProvider {
        async fn observe(
            &self,
            request_id: RequestId,
            session_id: SessionId,
            screenshot: bool,
            tree: bool,
        ) -> Result<remoteops_domain::VisualObservation, remoteops_visual::VisualProviderError>
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            assert!(!screenshot, "启动和刷新不应截图");
            assert!(!tree, "启动和刷新不应遍历 UIA");
            tokio::time::sleep(self.2).await;
            if self.1.load(Ordering::SeqCst) {
                return Err(VisualProviderError::Protocol(
                    "token=secret_probe_value password=test_secret".into(),
                ));
            }
            let mut observation = remoteops_visual::MockVisualProvider
                .observe(request_id, session_id, screenshot, tree)
                .await?;
            observation.active_window_fingerprint = Some("window".into());
            observation.windows.push(remoteops_domain::VisualWindow {
                window_id: "1".into(),
                process_id: 1,
                process_name: "test".into(),
                title: "test".into(),
                automation_id: None,
                session_id: "7".into(),
                left: 0,
                top: 0,
                width: 100,
                height: 100,
                fingerprint: "window".into(),
            });
            Ok(observation)
        }
        async fn wait_for(
            &self,
            request_id: RequestId,
            session_id: SessionId,
            condition: &str,
            timeout: u64,
        ) -> Result<remoteops_domain::VisualObservation, remoteops_visual::VisualProviderError>
        {
            remoteops_visual::MockVisualProvider
                .wait_for(request_id, session_id, condition, timeout)
                .await
        }
        async fn invoke(
            &self,
            request_id: RequestId,
            session_id: SessionId,
            target: &remoteops_domain::VisualTarget,
            action: &str,
        ) -> Result<remoteops_domain::VisualActionResult, remoteops_visual::VisualProviderError>
        {
            remoteops_visual::MockVisualProvider
                .invoke(request_id, session_id, target, action)
                .await
        }
        async fn type_text(
            &self,
            request_id: RequestId,
            session_id: SessionId,
            target: &remoteops_domain::VisualTarget,
            text: &str,
        ) -> Result<remoteops_domain::VisualActionResult, remoteops_visual::VisualProviderError>
        {
            remoteops_visual::MockVisualProvider
                .type_text(request_id, session_id, target, text)
                .await
        }
        async fn send_input(
            &self,
            request_id: RequestId,
            session_id: SessionId,
            target: &remoteops_domain::VisualTarget,
            input: &str,
        ) -> Result<remoteops_domain::VisualActionResult, remoteops_visual::VisualProviderError>
        {
            remoteops_visual::MockVisualProvider
                .send_input(request_id, session_id, target, input)
                .await
        }
        async fn stop(
            &self,
            session_id: SessionId,
        ) -> Result<(), remoteops_visual::VisualProviderError> {
            remoteops_visual::MockVisualProvider.stop(session_id).await
        }
    }

    fn available_facts() -> TargetRuntimeFacts {
        TargetRuntimeFacts {
            process_session_id: Some(7),
            interactive_desktop: CapabilityAvailability::Available,
            source: "native_test_fixture".into(),
            observed_at: chrono::Utc::now(),
        }
    }

    fn provider_options(provider: Arc<ReadyProvider>) -> AgentRuntimeOptions {
        let mut options = AgentRuntimeOptions::interactive(ProductProfile::default());
        options.visual_enabled = true;
        options.visual_provider_factory = Some(Arc::new(move || provider.clone()));
        options
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "需要真实已解锁交互桌面，仅轻量只读探测"]
    async fn native_interactive_startup_uses_lightweight_probe_and_reports_diagnostic() {
        let options = AgentRuntimeOptions::interactive(ProductProfile::default());
        let (boundary, _, capabilities) =
            prepare(&options, &EnvironmentProfile::empty(), native_facts())
                .await
                .unwrap();
        let diagnostic = boundary.visual_diagnostic().unwrap();
        println!("{diagnostic}");
        assert!(capabilities.contains(Capability::Visual), "{diagnostic}");
        assert!(boundary.snapshot().visual_allowed(), "{diagnostic}");
    }

    #[tokio::test]
    async fn startup_allows_observation_slower_than_old_five_second_budget() {
        let provider = Arc::new(ReadyProvider(
            AtomicUsize::new(0),
            AtomicBool::new(false),
            Duration::from_secs(6),
        ));
        let (boundary, _, capabilities) = prepare(
            &provider_options(provider),
            &EnvironmentProfile::empty(),
            available_facts(),
        )
        .await
        .unwrap();
        assert!(capabilities.contains(Capability::Visual));
        assert!(boundary.latest_context().visual_allowed());
        assert!(
            boundary
                .visual_diagnostic()
                .unwrap()
                .starts_with("visual_probe_ready;")
        );
    }

    #[tokio::test]
    async fn failed_probe_can_recover_without_restarting_or_replacing_initial_context() {
        let provider = Arc::new(ReadyProvider::ready());
        provider.1.store(true, Ordering::SeqCst);
        let (boundary, deferred, _) = prepare(
            &provider_options(provider.clone()),
            &EnvironmentProfile::empty(),
            available_facts(),
        )
        .await
        .unwrap();
        assert_eq!(
            boundary.context.visual_disabled_reason(),
            Some("visual_probe_error")
        );
        let diagnostic = boundary.visual_diagnostic().unwrap();
        assert!(diagnostic.contains("elapsed_ms="));
        assert!(!diagnostic.contains("secret_probe_value"));
        assert!(!diagnostic.contains("test_secret"));
        provider.1.store(false, Ordering::SeqCst);
        let context = boundary
            .refresh_visual_with_facts(
                deferred.as_ref(),
                available_facts(),
                Duration::ZERO,
                VISUAL_PROBE_BUDGET,
            )
            .await;
        assert!(context.visual_allowed());
        assert_eq!(
            boundary.context.visual_disabled_reason(),
            Some("visual_probe_error")
        );
        for operation in visual_operations() {
            boundary.check(&operation).unwrap();
        }
    }

    #[tokio::test]
    async fn desktop_recovery_constructs_deferred_provider_once() {
        let count = Arc::new(AtomicUsize::new(0));
        let constructions = count.clone();
        let provider = Arc::new(ReadyProvider::ready());
        let injected = provider.clone();
        let mut options = provider_options(provider.clone());
        options.visual_provider_factory = Some(Arc::new(move || {
            constructions.fetch_add(1, Ordering::SeqCst);
            injected.clone()
        }));
        let mut facts = available_facts();
        facts.interactive_desktop = CapabilityAvailability::Unavailable;
        let (boundary, deferred, _) =
            prepare(&options, &EnvironmentProfile::empty(), facts.clone())
                .await
                .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 0);
        boundary
            .refresh_visual_with_facts(
                deferred.as_ref(),
                facts,
                Duration::ZERO,
                VISUAL_PROBE_BUDGET,
            )
            .await;
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let context = boundary
            .refresh_visual_with_facts(
                deferred.as_ref(),
                available_facts(),
                Duration::ZERO,
                VISUAL_PROBE_BUDGET,
            )
            .await;
        assert!(context.visual_allowed());
        boundary
            .refresh_visual_with_facts(
                deferred.as_ref(),
                available_facts(),
                Duration::ZERO,
                VISUAL_PROBE_BUDGET,
            )
            .await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(provider.0.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn timeout_is_distinct_and_refresh_is_cooldown_limited() {
        let provider = Arc::new(ReadyProvider(
            AtomicUsize::new(0),
            AtomicBool::new(false),
            Duration::from_millis(50),
        ));
        let (boundary, deferred, _) = prepare(
            &provider_options(provider.clone()),
            &EnvironmentProfile::empty(),
            available_facts(),
        )
        .await
        .unwrap();
        let context = boundary
            .refresh_visual_with_facts(
                deferred.as_ref(),
                available_facts(),
                Duration::ZERO,
                Duration::from_millis(5),
            )
            .await;
        assert_eq!(
            context.visual_disabled_reason(),
            Some("visual_probe_timeout")
        );
        let calls = provider.0.load(Ordering::SeqCst);
        let context = boundary
            .refresh_visual_with_facts(
                deferred.as_ref(),
                available_facts(),
                Duration::from_secs(3),
                VISUAL_PROBE_BUDGET,
            )
            .await;
        assert_eq!(
            context.visual_disabled_reason(),
            Some("visual_probe_timeout")
        );
        assert_eq!(provider.0.load(Ordering::SeqCst), calls);
    }

    #[tokio::test]
    async fn recovering_one_target_does_not_change_another_target() {
        let first = Arc::new(ReadyProvider::ready());
        first.1.store(true, Ordering::SeqCst);
        let second = Arc::new(ReadyProvider::ready());
        second.1.store(true, Ordering::SeqCst);
        let (a, provider_a, _) = prepare(
            &provider_options(first.clone()),
            &EnvironmentProfile::empty(),
            available_facts(),
        )
        .await
        .unwrap();
        let (b, _, _) = prepare(
            &provider_options(second),
            &EnvironmentProfile::empty(),
            available_facts(),
        )
        .await
        .unwrap();
        first.1.store(false, Ordering::SeqCst);
        a.refresh_visual_with_facts(
            provider_a.as_ref(),
            available_facts(),
            Duration::ZERO,
            VISUAL_PROBE_BUDGET,
        )
        .await;
        assert!(a.latest_context().visual_allowed());
        assert_eq!(
            b.latest_context().visual_disabled_reason(),
            Some("visual_probe_error")
        );
    }

    #[tokio::test]
    async fn interactive_product_keeps_ready_visual_capability() {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let provider = Arc::new(ReadyProvider::ready());
        let injected = provider.clone();
        let mut options = AgentRuntimeOptions::interactive(ProductProfile::default());
        options.visual_enabled = true;
        options.visual_provider_factory = Some(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            injected.clone()
        }));
        let facts = TargetRuntimeFacts {
            process_session_id: Some(7),
            interactive_desktop: CapabilityAvailability::Available,
            source: "native_test_fixture".into(),
            observed_at: chrono::Utc::now(),
        };
        let (boundary, _, capabilities) = prepare(&options, &EnvironmentProfile::empty(), facts)
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(provider.0.load(Ordering::SeqCst), 1);
        assert!(capabilities.contains(Capability::Visual));
        for operation in visual_operations() {
            boundary.check(&operation).unwrap();
        }
    }

    #[tokio::test]
    async fn headless_product_never_constructs_or_probes_provider() {
        for run_mode in [
            AgentRunMode::WindowsService,
            AgentRunMode::Console,
            AgentRunMode::Interactive,
            AgentRunMode::Unknown,
        ] {
            for available in [
                CapabilityAvailability::Available,
                CapabilityAvailability::Unavailable,
                CapabilityAvailability::Unknown,
            ] {
                let constructions = Arc::new(AtomicUsize::new(0));
                let counter = constructions.clone();
                let mut options = AgentRuntimeOptions::service(ProductProfile::default());
                options.run_mode = run_mode;
                options.visual_enabled = true;
                options.visual_provider_factory = Some(Arc::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    panic!("禁止构造真实 Provider");
                }));
                let facts = TargetRuntimeFacts {
                    interactive_desktop: available,
                    process_session_id: Some(7),
                    source: "native_test_fixture".into(),
                    observed_at: chrono::Utc::now(),
                };
                let (boundary, provider, capabilities) =
                    prepare(&options, &EnvironmentProfile::empty(), facts)
                        .await
                        .unwrap();
                let refreshed = boundary
                    .refresh_visual_with_facts(
                        provider.as_ref(),
                        available_facts(),
                        Duration::ZERO,
                        VISUAL_PROBE_BUDGET,
                    )
                    .await;
                assert_eq!(
                    refreshed.visual_disabled_reason(),
                    Some(if run_mode == AgentRunMode::Unknown {
                        "host_context_unknown"
                    } else {
                        "product_policy_disabled"
                    })
                );
                assert!(
                    provider
                        .observe(RequestId::new(), SessionId::new(), true, true)
                        .await
                        .is_err()
                );
                assert!(provider.stop(SessionId::new()).await.is_err());
                assert_eq!(constructions.load(Ordering::SeqCst), 0);
                assert!(!capabilities.contains(Capability::Visual));
                for operation in visual_operations() {
                    let dispatch_error = boundary.check_for_dispatch(&operation).unwrap_err();
                    assert_eq!(
                        dispatch_error.downcast_ref::<RuntimeDenied>().unwrap().code,
                        "product_policy_disabled"
                    );
                    let error = boundary.check(&operation).unwrap_err();
                    assert_eq!(
                        error.downcast_ref::<RuntimeDenied>().unwrap().code,
                        "product_policy_disabled"
                    );
                }
                assert!(boundary.check(&RemoteOperation::GetTargetContext).is_ok());
            }
        }
    }

    #[tokio::test]
    async fn disabled_legacy_visual_flag_is_a_hard_ceiling() {
        let mut options = AgentRuntimeOptions::interactive(ProductProfile::default());
        options.visual_enabled = false;
        options.visual_provider_factory =
            Some(Arc::new(|| panic!("false 不能调用传入的真实 Provider")));
        let facts = TargetRuntimeFacts {
            interactive_desktop: CapabilityAvailability::Available,
            source: "native_test_fixture".into(),
            observed_at: chrono::Utc::now(),
            ..TargetRuntimeFacts::default()
        };
        let (boundary, _, _) = prepare(&options, &EnvironmentProfile::empty(), facts)
            .await
            .unwrap();
        for operation in visual_operations() {
            assert!(boundary.check(&operation).is_err());
        }
    }

    #[test]
    fn read_only_product_reclassifies_spoofed_commands() {
        let boundary = RuntimeBoundary {
            policy: AgentCapabilityPolicy::default().with_read_only(),
            ..RuntimeBoundary::default()
        };
        for readonly in [true, false] {
            let operation = RemoteOperation::RunCommand {
                shell: remoteops_domain::ShellKind::PowerShell,
                command: "Remove-Item important.txt".into(),
                readonly,
            };
            assert_eq!(
                boundary
                    .check(&operation)
                    .unwrap_err()
                    .downcast_ref::<RuntimeDenied>()
                    .unwrap()
                    .code,
                "product_policy_disabled"
            );
        }
        assert!(boundary.check(&RemoteOperation::ListProcesses).is_ok());
    }

    #[tokio::test]
    async fn contexts_and_guidance_remain_product_specific() {
        let mut first = AgentRuntimeOptions::service(ProductProfile {
            id: "first".into(),
            ..ProductProfile::default()
        });
        first.guidance = vec![TargetGuidance {
            id: "first-guide".into(),
            version: "1".into(),
            source: "built_in_product".into(),
            text: "This text cannot grant visual access".into(),
            references: Vec::new(),
        }];
        let second = AgentRuntimeOptions::service(ProductProfile {
            id: "second".into(),
            ..ProductProfile::default()
        });
        let (a, _, _) = prepare(
            &first,
            &EnvironmentProfile::empty(),
            TargetRuntimeFacts::default(),
        )
        .await
        .unwrap();
        let (b, _, _) = prepare(
            &second,
            &EnvironmentProfile::empty(),
            TargetRuntimeFacts::default(),
        )
        .await
        .unwrap();
        assert_ne!(a.context.product_id, b.context.product_id);
        assert_eq!(b.context.guidance, Vec::<TargetGuidance>::new());
        assert!(a.check(&visual_operations()[0]).is_err());
    }
}
