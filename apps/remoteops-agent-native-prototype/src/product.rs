//! Win32 入口只转换启动选项；产品行为由共享桌面宿主实现。

pub use remoteops_agent_desktop_host::{
    LogLevel, NativeAppearance, NativeLanguage, ProductAction, ProductPhase, ProductSnapshot,
};

/// 保留现有 Win32 构造入口的薄适配器，不持有独立业务状态。
pub struct ProductHost(remoteops_agent_desktop_host::ProductHost);
impl ProductHost {
    /// 将 Win32 启动参数转换为同一份共享产品宿主选项。
    pub fn new(options: &crate::Options) -> Self {
        Self(remoteops_agent_desktop_host::ProductHost::new(
            &remoteops_agent_desktop_host::HostOptions {
                demo: options.demo,
                self_test: options.self_test,
                product_demo: options.product_demo,
                agent_config: options.agent_config.clone(),
                ui_flavor: remoteops_agent_desktop_host::UiFlavor::Win32,
                host_version: env!("CARGO_PKG_VERSION").into(),
                #[cfg(feature = "agent-runtime")]
                product_profile: options.product_profile.clone(),
                #[cfg(feature = "agent-runtime")]
                bundled_tools: options.bundled_tools.clone(),
            },
        ))
    }
}
impl std::ops::Deref for ProductHost {
    type Target = remoteops_agent_desktop_host::ProductHost;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for ProductHost {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
