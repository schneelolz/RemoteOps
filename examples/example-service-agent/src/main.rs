fn main() -> anyhow::Result<()> {
    let profile = remoteops_agent::ProductProfile {
        id: "example-service-agent".into(),
        name: "Example Service Agent".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        directory_name: "ExampleServiceAgent".into(),
        config_file_name: "agent-config.json".into(),
        state_file_name: "agent-state.json".into(),
        env_prefix: "EXAMPLE_SERVICE_AGENT".into(),
        endpoint: remoteops_agent::EndpointPolicy::Fixed {
            relay: "relay.example.invalid:7443".into(),
            server_name: "relay.example.invalid".into(),
        },
        default_full_control: false,
        legacy_remoteops_paths: false,
    };
    let guidance = remoteops_domain::TargetGuidance {
        id: "example-service-operations".into(),
        version: "1".into(),
        source: "built_in_product".into(),
        text: "此目标为无图形服务。优先使用进程、服务、文件和网络诊断；FullAccess不能启用截图或桌面输入。".into(),
        references: Vec::new(),
    };
    remoteops_agent_service::run(
        profile,
        remoteops_agent_service::ServiceOptions::new("ExampleServiceAgent")
            .managed()
            .with_guidance(vec![guidance]),
    )
}
