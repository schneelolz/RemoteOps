fn rdp_graphics_context() -> remoteops_domain::TargetContext {
    use remoteops_domain::{
        AgentRunMode, CapabilityAvailability, TargetCapabilityStatus, TargetRuntimeFacts,
    };
    remoteops_domain::TargetContext {
        product_id: "rdp-test-agent".into(),
        version: "0.4.0-test".into(),
        run_mode: AgentRunMode::Interactive,
        facts: TargetRuntimeFacts {
            process_session_id: Some(2),
            observed_at: Utc::now(),
            interactive_desktop: CapabilityAvailability::Available,
            source: "native_host:native_windows_api".into(),
        },
        capabilities: vec![TargetCapabilityStatus {
            capability: "visual".into(),
            status: CapabilityAvailability::Unavailable,
            reasons: vec!["visual_probe_pending".into()],
        }],
        ..Default::default()
    }
}

fn graphics_report(
    id: RequestId,
    session: SessionId,
    context: &remoteops_domain::TargetContext,
) -> WireMessage {
    WireMessage::RemoteResponse(remoteops_protocol::RemoteResponse {
        request_id: id,
        session_id: session,
        exit_code: Some(0),
        error_code: None,
        summary: String::new(),
        details: Some(serde_json::to_value(context).unwrap()),
        payload_base64: None,
        sha256: None,
    })
}

// 每次转发会先下发绑定、控制状态，再下发实际操作。
fn received_visual_request(rx: &mut mpsc::Receiver<WireMessage>, id: RequestId) -> bool {
    while let Ok(message) = rx.try_recv() {
        if matches!(message, WireMessage::AuthorizedRemoteRequest(r) if r.request.request_id == id)
        {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn visual_context_refresh_routes_screenshot_and_still_enforces_lock_and_revocation() {
    use remoteops_domain::CapabilityAvailability;
    let relay = relay(Duration::minutes(10));
    let device = AgentInstanceId::new();
    let (agent, mut from_agent, session) = ready_agent(&relay, device, None).await;
    let initial = rdp_graphics_context();
    relay
        .state
        .lock()
        .await
        .agents
        .get_mut(&device)
        .unwrap()
        .hello
        .target_context = Some(initial.clone());
    let (ai, generation, mut from_controller) =
        register_controller(&relay, ControllerKind::Ai).await;
    let pair = relay
        .pair_controller(
            ai,
            generation,
            PairRequest {
                request_id: RequestId::new(),
                pairing_code: agent.welcome.pairing_code.clone(),
                expected_agent_id: Some(device),
                expected_session_id: None,
                connection_resume_token: None,
                permission_mode: PermissionMode::ApprovalRequired,
            },
        )
        .await;
    assert!(pair.error.is_none());
    let sender = controller_sender(&relay, ai).await;
    while try_recv_legacy(&mut from_agent).is_ok() {}
    while try_recv_legacy(&mut from_controller).is_ok() {}
    let screenshot = || {
        test_remote_request(
            session,
            RemoteOperation::VisualObserve {
                include_screenshot: true,
                include_ui_tree: false,
            },
        )
    };
    relay
        .forward_controller_request(ai, generation, screenshot(), &sender)
        .await;
    assert!(
        matches!(try_recv_legacy(&mut from_controller), Ok(WireMessage::Error{code,..}) if code=="visual_probe_pending")
    );
    assert!(try_recv_legacy(&mut from_agent).is_err());
    let query = test_remote_request(session, RemoteOperation::GetTargetContext);
    relay
        .forward_controller_request(ai, generation, query.clone(), &sender)
        .await;
    assert!(received_visual_request(&mut from_agent, query.request_id));
    let mut ready = initial.clone();
    ready.capabilities[0].status = CapabilityAvailability::Available;
    ready.capabilities[0].reasons.clear();
    let response = graphics_report(query.request_id, session, &ready);
    relay
        .forward_agent_message(device, agent.connection_generation + 1, response.clone())
        .await;
    relay
        .forward_controller_request(ai, generation, screenshot(), &sender)
        .await;
    assert!(
        matches!(try_recv_legacy(&mut from_controller),Ok(WireMessage::Error{code,..}) if code=="visual_probe_pending")
    );
    relay
        .forward_agent_message(device, agent.connection_generation, response)
        .await;
    assert!(matches!(
        try_recv_legacy(&mut from_controller),
        Ok(WireMessage::RemoteResponse(_))
    ));
    let request = screenshot();
    relay
        .forward_controller_request(ai, generation, request.clone(), &sender)
        .await;
    assert!(received_visual_request(&mut from_agent, request.request_id));
    // 图形就绪不代表同意写操作。撤为只读后，即使携带先前的授权也不能注入输入。
    let mut write = test_remote_request(
        session,
        RemoteOperation::VisualTypeText {
            target: remoteops_domain::VisualTarget::Control {
                window_fingerprint: "test-window".into(),
                target_fingerprint: "test-control".into(),
                automation_id: None,
                name: None,
                control_type: None,
            },
            text: "fixture".into(),
        },
    );
    write.control_proof = test_control_proof(&relay, session, ControlBasis::SingleApproval).await;
    relay
        .update_agent_permission_mode(
            device,
            agent.connection_generation,
            PermissionMode::ReadOnly,
        )
        .await;
    while from_controller.try_recv().is_ok() {}
    relay
        .forward_controller_request(ai, generation, write.clone(), &sender)
        .await;
    assert!(
        matches!(try_recv_legacy(&mut from_controller),Ok(WireMessage::Error{code,..}) if code=="control_authorization_required")
    );
    assert!(!received_visual_request(&mut from_agent, write.request_id));
    let lock_query = test_remote_request(session, RemoteOperation::GetTargetContext);
    relay
        .forward_controller_request(ai, generation, lock_query.clone(), &sender)
        .await;
    assert!(received_visual_request(
        &mut from_agent,
        lock_query.request_id
    ));
    let mut locked = ready.clone();
    locked.facts.interactive_desktop = CapabilityAvailability::Unavailable;
    relay
        .forward_agent_message(
            device,
            agent.connection_generation,
            graphics_report(lock_query.request_id, session, &locked),
        )
        .await;
    assert!(matches!(
        try_recv_legacy(&mut from_controller),
        Ok(WireMessage::RemoteResponse(_))
    ));
    relay
        .forward_controller_request(ai, generation, screenshot(), &sender)
        .await;
    assert!(
        matches!(try_recv_legacy(&mut from_controller),Ok(WireMessage::Error{code,..}) if code=="no_interactive_desktop")
    );
    // 现场解除 AI 配对后，到达的成功回执不能复活已撤销的访问。
    let last_query = test_remote_request(session, RemoteOperation::GetTargetContext);
    relay
        .forward_controller_request(ai, generation, last_query.clone(), &sender)
        .await;
    let token = {
        let state = relay.state.lock().await;
        state.session_bindings[&session]
            .ai
            .as_ref()
            .unwrap()
            .binding_token
            .clone()
    };
    relay
        .disconnect_binding_from_agent(device, agent.connection_generation, session, &token)
        .await;
    relay
        .forward_agent_message(
            device,
            agent.connection_generation,
            graphics_report(last_query.request_id, session, &ready),
        )
        .await;
    while try_recv_legacy(&mut from_controller).is_ok() {}
    relay
        .forward_controller_request(ai, generation, screenshot(), &sender)
        .await;
    assert!(
        matches!(try_recv_legacy(&mut from_controller),Ok(WireMessage::Error{code,..}) if code=="session_not_owned")
    );
}

#[test]
fn visual_context_refresh_discards_reordered_results_and_respects_product_boundary() {
    use remoteops_domain::CapabilityAvailability;
    let initial = rdp_graphics_context();
    let mut ready = initial.clone();
    ready.capabilities[0].status = CapabilityAvailability::Available;
    ready.capabilities[0].reasons.clear();
    let session = SessionId::new();
    let mut state = visual_context::VisualContext::default();
    let first = RequestId::new();
    let old_query = state.begin(Some(&initial)).unwrap();
    for change in [
        "wrong_product",
        "old_timestamp",
        "other_session",
        "invalid_schema",
        "locked",
        "ready",
    ] {
        let req = RequestId::new();
        let sequence = state.begin(Some(&initial)).unwrap();
        let mut report = ready.clone();
        match change {
            "wrong_product" => report.product_id = "other".into(),
            "old_timestamp" => report.facts.observed_at -= Duration::seconds(5),
            "other_session" => report.facts.process_session_id = Some(3),
            "invalid_schema" => report.schema_version = 99,
            "locked" => report.facts.interactive_desktop = CapabilityAvailability::Unavailable,
            _ => {}
        }
        state.finish(
            sequence,
            Some(&initial),
            &graphics_report(req, session, &report),
        );
        state.finish(
            old_query,
            Some(&initial),
            &graphics_report(first, session, &ready),
        );
        assert_eq!(
            state.disabled_reason(Some(&initial)).is_none(),
            change == "ready"
        );
    }
    let latest = state.begin(Some(&initial)).unwrap();
    let upcoming = state.begin(Some(&initial)).unwrap();
    let id = RequestId::new();
    let mut locked_report = ready.clone();
    locked_report.facts.interactive_desktop = CapabilityAvailability::Unavailable;
    state.finish(
        latest,
        Some(&initial),
        &graphics_report(id, session, &locked_report),
    );
    assert_eq!(
        state.disabled_reason(Some(&initial)),
        Some("no_interactive_desktop")
    );
    state.finish(
        upcoming,
        Some(&initial),
        &graphics_report(id, session, &ready),
    );
    assert_eq!(state.disabled_reason(Some(&initial)), None);
    let failed_query = state.begin(Some(&initial)).unwrap();
    state.finish(
        failed_query,
        Some(&initial),
        &WireMessage::Error {
            request_id: Some(id),
            code: "test_unavailable".into(),
            message: String::new(),
        },
    );
    assert_eq!(
        state.disabled_reason(Some(&initial)),
        Some("host_context_unknown")
    );
    // Agent 启动时桌面已锁，解锁后仍须收到同一交互会话的有效报告。
    let mut locked_at_start = locked_report;
    locked_at_start.capabilities[0].status = CapabilityAvailability::Unavailable;
    locked_at_start.capabilities[0].reasons = vec!["no_interactive_desktop".into()];
    assert!(!locked_at_start.visual_probe_allowed());
    let mut restarted = visual_context::VisualContext::default();
    assert_eq!(
        restarted.disabled_reason(Some(&locked_at_start)),
        Some("no_interactive_desktop")
    );
    let next = restarted.begin(Some(&locked_at_start)).unwrap();
    restarted.finish(
        next,
        Some(&locked_at_start),
        &graphics_report(id, session, &ready),
    );
    assert!(restarted.disabled_reason(Some(&locked_at_start)).is_none());
    let mut restricted = initial.clone();
    restricted.capabilities[0].reasons = vec!["product_policy_disabled".into()];
    assert!(state.begin(Some(&restricted)).is_none());
    assert_eq!(
        state.disabled_reason(Some(&restricted)),
        Some("product_policy_disabled")
    );
}
