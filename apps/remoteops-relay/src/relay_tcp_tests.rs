use remoteops_protocol::tcp_forward::{
    TcpForwardFrame as TcpFrame, TcpForwardPolicy, TcpForwardRule,
};

fn tcp_rule(agent: Option<AgentInstanceId>) -> TcpForwardRule {
    TcpForwardRule {
        id: "test-web".into(),
        host: "127.0.0.1".into(),
        port: 8080,
        agent_id: agent,
        installation_id: None,
        purpose: "隔离测试".into(),
        expires_at: None,
    }
}

#[tokio::test]
async fn tcp_policy_compare_and_save_preserves_previous_revision_on_conflict() {
    let file = TestStateFile::new("tcp-policy");
    let relay = Relay::new_with_state_path(
        Duration::minutes(10),
        5,
        ControllerOwnerId::new(),
        HUMAN_TOKEN.into(),
        AI_TOKEN.into(),
        file.path.clone(),
    )
    .unwrap();
    let policy = TcpForwardPolicy {
        rules: vec![tcp_rule(None)],
        ..Default::default()
    };
    let saved = relay
        .update_tcp_policy("test", 0, policy.clone())
        .await
        .unwrap();
    assert_eq!(saved.revision, 1);
    assert!(
        relay
            .update_tcp_policy("test", 0, TcpForwardPolicy::default())
            .await
            .is_err()
    );
    assert_eq!(relay.tcp_policy().await, saved);
    let persisted = decode_persisted_state(&std::fs::read_to_string(&file.path).unwrap()).unwrap();
    assert_eq!(persisted.tcp_forward_policy, saved);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn tcp_requires_current_proof_and_policy_and_revocation_closes_both_ends() {
    let (relay, agent_id, registration, mut agent_rx, controller, generation, session) =
        control_fixture(PermissionMode::ControllerApproved).await;
    {
        let mut state = relay.state.lock().await;
        let agent = state.agents.get_mut(&agent_id).unwrap();
        agent.hello.capabilities = CapabilitySet::new([Capability::Cmd, Capability::TcpForward]);
        let mut context = remoteops_domain::TargetContext::default();
        context
            .capabilities
            .push(remoteops_domain::TargetCapabilityStatus {
                capability: "tcp_forward_v2".into(),
                status: remoteops_domain::CapabilityAvailability::Available,
                reasons: Vec::new(),
            });
        agent.hello.target_context = Some(context);
    }
    let initial = current_control(&relay, session).await;
    let current = change_control(
        &relay,
        controller,
        generation,
        initial,
        ControllerControlMode::FullAccess,
        ControlSource::Mcp,
    )
    .await
    .state
    .unwrap();
    while agent_rx.try_recv().is_ok() {}
    let request = RemoteRequest {
        request_id: RequestId::new(),
        session_id: session,
        source: EventSource::Ai,
        operation: RemoteOperation::CreateTcpForward {
            host: "127.0.0.1".into(),
            port: 8080,
            local_address: "127.0.0.1:18088".into(),
        },
        approval_id: None,
        payload_base64: None,
        control_proof: None,
    };
    let (sender, mut replies) = test_channel();
    relay
        .forward_controller_request(controller, generation, request.clone(), &sender)
        .await;
    assert!(
        matches!(replies.try_recv().unwrap(),WireMessage::Error{code,..} if code=="control_authorization_required")
    );
    let mut request = request;
    request.control_proof = Some(remoteops_protocol::ControlProof {
        state: current,
        basis: ControlBasis::FullAccess,
    });
    relay
        .forward_controller_request(controller, generation, request.clone(), &sender)
        .await;
    assert!(
        matches!(replies.try_recv().unwrap(),WireMessage::Error{message,..} if message.contains("tcp_target_not_approved"))
    );
    relay
        .update_tcp_policy(
            "test",
            0,
            TcpForwardPolicy {
                rules: vec![tcp_rule(Some(agent_id))],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    request.request_id = RequestId::new();
    relay
        .forward_controller_request(controller, generation, request.clone(), &sender)
        .await;
    let mut authorized = false;
    while let Ok(message) = agent_rx.try_recv() {
        if let WireMessage::TcpForwardAuthorized {
            authorized: message,
            rules,
        } = message
        {
            assert_eq!(message.request, request);
            assert_eq!(rules.len(), 1);
            authorized = true;
        }
    }
    assert!(authorized);
    let connection = RequestId::new();
    relay
        .tcp_from_controller(
            controller,
            generation,
            TcpFrame::Connect {
                forward_id: request.request_id,
                connection_id: connection,
            },
        )
        .await;
    assert!(matches!(
        agent_rx.try_recv().unwrap(),
        WireMessage::TcpForwardV2(TcpFrame::Connect { .. })
    ));
    relay
        .update_tcp_policy("test", 1, TcpForwardPolicy::default())
        .await
        .unwrap();
    assert!(matches!(
        agent_rx.try_recv().unwrap(),
        WireMessage::TcpForwardV2(TcpFrame::Stop { .. })
    ));
    relay
        .tcp_from_agent(
            agent_id,
            registration.connection_generation,
            TcpFrame::Connected {
                forward_id: request.request_id,
                connection_id: connection,
                error: None,
            },
        )
        .await;
    assert!(agent_rx.try_recv().is_err());
}

#[tokio::test]
async fn control_initialization_cannot_accept_stale_agent_ack_and_is_audited() {
    let (relay, agent_id, registration, _rx, controller, generation, session) =
        control_fixture(PermissionMode::ControllerApproved).await;
    let expected = {
        let mut state = relay.state.lock().await;
        let control = state
            .session_bindings
            .get_mut(&session)
            .unwrap()
            .ai
            .as_mut()
            .unwrap()
            .control_state
            .as_mut()
            .unwrap();
        control.initialization_pending = true;
        control.clone()
    };
    relay
        .complete_control_initialization(
            agent_id,
            registration.connection_generation + 1,
            expected.clone(),
        )
        .await;
    assert!(
        current_control(&relay, session)
            .await
            .initialization_pending
    );
    let (sender, mut rx) = test_channel();
    let request = RemoteRequest {
        request_id: RequestId::new(),
        session_id: session,
        source: EventSource::Ai,
        operation: RemoteOperation::CreateTcpForward {
            host: "127.0.0.1".into(),
            port: 8080,
            local_address: "127.0.0.1:10880".into(),
        },
        approval_id: None,
        payload_base64: None,
        control_proof: Some(remoteops_protocol::ControlProof {
            state: expected.clone(),
            basis: ControlBasis::SingleApproval,
        }),
    };
    relay
        .forward_controller_request(controller, generation, request, &sender)
        .await;
    assert!(
        matches!(rx.try_recv().unwrap(),WireMessage::Error{code,..} if code=="control_authorization_required")
    );
    relay
        .complete_control_initialization(
            agent_id,
            registration.connection_generation,
            expected.clone(),
        )
        .await;
    let current = current_control(&relay, session).await;
    assert!(!current.initialization_pending);
    assert!(current.revision > expected.revision);
    assert!(
        relay
            .state
            .lock()
            .await
            .audit_log
            .iter()
            .any(|event| event.action == "control_initialized")
    );
}
