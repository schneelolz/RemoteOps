fn main() -> anyhow::Result<()> {
    let profile = remoteops_agent::ProductProfile {
        id: "remoteops-agent-service".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        ..remoteops_agent::ProductProfile::default()
    };
    remoteops_agent_service::run(profile, remoteops_agent_service::ServiceOptions::default())
}
