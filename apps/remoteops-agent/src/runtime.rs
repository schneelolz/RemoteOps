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
use remoteops_visual::{UnavailableVisualProvider, VisualProvider};
use std::sync::Arc;

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

#[derive(Clone)]
pub(crate) struct RuntimeBoundary {
    pub context: Arc<TargetContext>,
    pub policy: AgentCapabilityPolicy,
}
impl Default for RuntimeBoundary {
    fn default() -> Self {
        Self {
            context: Arc::new(TargetContext::default()),
            policy: AgentCapabilityPolicy::default(),
        }
    }
}
impl RuntimeBoundary {
    pub fn snapshot(&self) -> TargetContext {
        let mut context = (*self.context).clone();
        context.facts = native_facts();
        if context.facts.interactive_desktop != CapabilityAvailability::Available {
            for capability in &mut context.capabilities {
                if capability.capability == "visual"
                    && capability.status == CapabilityAvailability::Available
                {
                    capability.status = CapabilityAvailability::Unavailable;
                    capability.reasons = vec!["no_interactive_desktop".into()];
                }
            }
        }
        context
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
            if let Some(reason) = self.context.visual_disabled_reason() {
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
    let mut reason = if !options.capability_policy.visual_allowed()
        || !options.visual_enabled
        || options.run_mode == AgentRunMode::WindowsService
    {
        Some("product_policy_disabled")
    } else if options.run_mode == AgentRunMode::Unknown {
        Some("host_context_unknown")
    } else if facts.interactive_desktop != CapabilityAvailability::Available {
        Some("no_interactive_desktop")
    } else {
        None
    };
    let provider: Arc<dyn VisualProvider> = if reason.is_none() {
        options
            .visual_provider_factory
            .as_ref()
            .map_or_else(remoteops_visual::default_visual_provider, |factory| {
                factory()
            })
    } else {
        Arc::new(UnavailableVisualProvider)
    };
    if reason.is_none() {
        let observed = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            provider.observe(RequestId::new(), SessionId::new(), false, true),
        )
        .await;
        let ready = observed.is_ok_and(|result| {
            result.is_ok_and(|observation| {
                observation.state == remoteops_domain::VisualSessionState::Ready
                    && !observation.windows.is_empty()
                    && observation.active_window_fingerprint.is_some()
            })
        });
        if !ready {
            reason = Some("visual_probe_failed");
        }
    }
    let capabilities = capabilities_from_environment(environment, reason.is_none());
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
    Ok((
        RuntimeBoundary {
            context: Arc::new(context),
            policy,
        },
        provider,
        capabilities,
    ))
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ReadyProvider(std::sync::atomic::AtomicUsize);
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

    #[tokio::test]
    async fn interactive_product_keeps_ready_visual_capability() {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let provider = Arc::new(ReadyProvider(AtomicUsize::new(0)));
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
                let (boundary, _, capabilities) =
                    prepare(&options, &EnvironmentProfile::empty(), facts)
                        .await
                        .unwrap();
                assert_eq!(constructions.load(Ordering::SeqCst), 0);
                assert!(!capabilities.contains(Capability::Visual));
                for operation in visual_operations() {
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
        assert!(b.context.guidance.is_empty());
        assert!(a.check(&visual_operations()[0]).is_err());
    }
}
