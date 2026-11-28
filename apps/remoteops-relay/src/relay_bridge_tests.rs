// 长码与普通短码共享授权检查，但不共享实例及租约。
#[test]
fn host_business_requires_explicit_v22_target_capability() {
    let mut value = bridge_hello(AgentInstanceId::new(), None);
    assert!(!supports_host_business(&value));
    let mut context = remoteops_domain::TargetContext::default();
    context.capabilities.push(remoteops_domain::TargetCapabilityStatus { capability:"host_business_v1".into(),status:remoteops_domain::CapabilityAvailability::Available,reasons:vec![] });
    value.target_context = Some(context);
    assert!(supports_host_business(&value));
    value.protocol_version = PROTOCOL_VERSION;
    assert!(!supports_host_business(&value));
}

async fn bridge_registration(relay: &Relay, value: AgentHello) -> AgentRegistration {
    let id = value.agent_instance_id;
    let (sender, _receiver) = test_channel();
    let registration = relay.register_agent(value, sender).await.unwrap();
    assert!(relay.commit_agent_resume(id, registration.connection_generation).await);
    assert!(relay.finalize_agent_resume(id, registration.connection_generation).await.is_some());
    registration
}

fn bridge_hello(id: AgentInstanceId, token: Option<String>) -> AgentHello {
    let mut value = hello(id, token);
    value.protocol_version = remoteops_protocol::BRIDGE_PROTOCOL_VERSION;
    value.pairing_mode = PairingMode::LongPerBoot;
    value
}

#[tokio::test]
async fn long_code_requires_v22_and_coexists_with_short_code() {
    let relay = relay(Duration::minutes(10));
    let id = AgentInstanceId::new();
    let mut invalid = bridge_hello(id, None);
    invalid.protocol_version = PROTOCOL_VERSION;
    let (sender, _) = test_channel();
    assert!(relay.register_agent(invalid, sender).await.is_err());
    let first = bridge_registration(&relay, bridge_hello(id, None)).await;
    assert!(first.welcome.pairing_code.as_ref().unwrap().is_long());
    let second = bridge_registration(&relay, bridge_hello(AgentInstanceId::new(), None)).await;
    let (ordinary, _, _) = ready_agent(&relay, AgentInstanceId::new(), None).await;
    assert!(!ordinary.welcome.pairing_code.unwrap().is_long());
    assert_ne!(first.welcome.pairing_code, second.welcome.pairing_code);
    let value = bridge_hello(id, Some(first.welcome.resume_token));
    let resumed = bridge_registration(&relay, value.clone()).await;
    assert_eq!(first.welcome.pairing_code, resumed.welcome.pairing_code);
    let mut reboot = value;
    reboot.boot_id = AgentBootId::new();
    reboot.resume_token = Some(resumed.welcome.resume_token);
    let restarted = bridge_registration(&relay, reboot).await;
    assert_ne!(resumed.welcome.pairing_code, restarted.welcome.pairing_code);
}

#[tokio::test]
async fn long_code_expiry_rejects_renewal_and_rotates_session() {
    let relay = relay(Duration::minutes(10));
    let id = AgentInstanceId::new();
    let first = bridge_registration(&relay, bridge_hello(id, None)).await;
    let session = relay.state.lock().await.agents[&id].session_id;
    relay.state.lock().await.agents.get_mut(&id).unwrap().lease.expires_at = Utc::now() - Duration::seconds(1);
    assert!(relay.renew_agent(id, first.connection_generation).await.is_none());
    relay.cleanup_expired().await;
    assert!(!relay.state.lock().await.agents[&id].ready);
    let second = bridge_registration(&relay, bridge_hello(id, Some(first.welcome.resume_token))).await;
    assert_ne!(first.welcome.pairing_code, second.welcome.pairing_code);
    assert_ne!(session, relay.state.lock().await.agents[&id].session_id);
}

#[tokio::test]
#[ignore = "真实十分钟租约验证，候选验收单独运行"]
async fn long_code_real_ten_minute_expiry() {
    let relay = relay(Duration::minutes(10));
    let id = AgentInstanceId::new();
    let first = bridge_registration(&relay, bridge_hello(id, None)).await;
    tokio::time::sleep(std::time::Duration::from_secs(601)).await;
    relay.cleanup_expired().await;
    assert!(!relay.state.lock().await.agents[&id].ready);
    assert!(relay.renew_agent(id, first.connection_generation).await.is_none());
    let second = bridge_registration(&relay, bridge_hello(id, Some(first.welcome.resume_token))).await;
    assert_ne!(first.welcome.pairing_code, second.welcome.pairing_code);
}
