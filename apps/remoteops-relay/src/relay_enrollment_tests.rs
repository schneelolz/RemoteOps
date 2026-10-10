fn enrolled_relay() -> (Relay, Uuid, String) {
    use remoteops_enrollment::{AdvertisedEndpoints, CreateGrantRequest, RedeemRequest};
    let mut relay = relay(Duration::minutes(10));
    let store = EnrollmentStore::in_memory(test_owner_id()).unwrap();
    store
        .set_settings(AdvertisedEndpoints {
            enrollment_url: "https://relay.example.test/api/mcp/enroll".to_owned(),
            relay: "relay.example.test:7443".to_owned(),
            server_name: "relay.example.test".to_owned(),
            relay_ca_pem: None,
            enrollment_ca_pem: None,
        })
        .unwrap();
    let now = Utc::now();
    let grant = store
        .create_grant(
            CreateGrantRequest {
                client_name: Some("test MCP".to_owned()),
                expires_in_hours: Some(1),
            },
            now,
        )
        .unwrap();
    let id = Uuid::new_v4();
    let secret = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE";
    store
        .redeem(
            RedeemRequest {
                grant_id: grant.setup.grant_id,
                grant_secret: grant.setup.grant_secret,
                installation_id: id,
                installation_secret: secret.to_owned(),
            },
            now,
        )
        .unwrap();
    relay.enrollment_store = Some(Arc::new(store));
    (relay, id, format!("roc1.{id}.{secret}"))
}

fn installation_hello(token: String) -> ControllerHello {
    ControllerHello {
        protocol_version: PROTOCOL_VERSION,
        controller_instance_id: ControllerInstanceId::new(),
        owner_id: test_owner_id(),
        kind: ControllerKind::Ai,
        auth_token: token,
        hostname: None,
        mac_address: None,
    }
}

#[tokio::test]
async fn enrolled_credential_is_owner_bound_ai_only_and_grants_no_sessions() {
    let (relay, installation_id, token) = enrolled_relay();
    let mut hello = installation_hello(token);
    assert_eq!(
        relay.authenticate_controller(&hello).unwrap(),
        Some(installation_id)
    );
    hello.kind = ControllerKind::Human;
    assert!(relay.authenticate_controller(&hello).is_err());
    hello.kind = ControllerKind::Ai;
    hello.owner_id = other_owner_id();
    assert!(relay.authenticate_controller(&hello).is_err());
    hello.owner_id = test_owner_id();
    let (sender, _receiver) = test_channel();
    relay
        .authenticate_and_register_controller(&hello, sender)
        .await
        .unwrap();
    let state = relay.state.lock().await;
    let controller = state
        .controllers
        .get(&hello.controller_instance_id)
        .unwrap();
    assert_eq!(controller.kind, ControllerKind::Ai);
    assert_eq!(controller.installation_id, Some(installation_id));
    assert!(controller.sessions.is_empty());
    assert!(state.session_bindings.is_empty());
    assert!(state.agents.is_empty());
    assert!(state.approvals.is_empty());
}

#[tokio::test]
async fn revoking_installation_disconnects_every_connection_and_clears_bindings_approvals() {
    let (relay, installation_id, token) = enrolled_relay();
    let (agent, mut agent_receiver, session_id) =
        ready_agent(&relay, AgentInstanceId::new(), None).await;
    let hello = installation_hello(token.clone());
    let (sender, _receiver, closed) = outbound_channel();
    let generation = relay
        .authenticate_and_register_controller(&hello, sender)
        .await
        .unwrap();
    let second = installation_hello(token.clone());
    let (sender, _receiver_two, closed_two) = outbound_channel();
    relay
        .authenticate_and_register_controller(&second, sender)
        .await
        .unwrap();
    let (legacy_id, _, _legacy_receiver) = register_controller(&relay, ControllerKind::Human).await;
    let paired = relay
        .pair_controller(
            hello.controller_instance_id,
            generation,
            PairRequest {
                request_id: RequestId::new(),
                pairing_code: agent.welcome.pairing_code,
                permission_mode: PermissionMode::ApprovalRequired,
            },
        )
        .await;
    assert_eq!(
        paired.connection.unwrap().permission_mode,
        PermissionMode::ApprovalRequired
    );
    while agent_receiver.try_recv().is_ok() {}
    let approval = relay
        .request_approval(
            hello.controller_instance_id,
            generation,
            ApprovalRequest {
                request_id: RequestId::new(),
                session_id,
                operation: run_command("Remove-Item C:\\temp\\a.txt", false),
            },
        )
        .await;
    assert_eq!(approval.state, ApprovalState::Pending);
    assert!(!relay.state.lock().await.approvals.is_empty());
    assert!(relay.revoke_mcp_client(installation_id).await.unwrap());
    assert!(!relay.revoke_mcp_client(installation_id).await.unwrap());
    time::timeout(time::Duration::from_secs(1), closed.notified())
        .await
        .unwrap();
    time::timeout(time::Duration::from_secs(1), closed_two.notified())
        .await
        .unwrap();
    let state = relay.state.lock().await;
    assert!(
        !state
            .controllers
            .contains_key(&hello.controller_instance_id)
    );
    assert!(
        !state
            .controllers
            .contains_key(&second.controller_instance_id)
    );
    assert!(
        state.controllers.contains_key(&legacy_id),
        "unrelated legacy controller remains connected"
    );
    assert!(!state.session_bindings.contains_key(&session_id));
    assert!(state.approvals.is_empty());
    assert!(
        matches!(agent_receiver.try_recv(), Ok(WireMessage::ControllerBindingRevoked { session_id: revoked, .. }) if revoked == session_id)
    );
    drop(state);
    assert!(
        relay
            .authenticate_controller(&installation_hello(token))
            .is_err()
    );
}

#[tokio::test]
async fn revoke_serializes_with_authentication_waiting_to_register() {
    let (relay, installation_id, token) = enrolled_relay();
    let relay = Arc::new(relay);
    // Stop registration at the state lock after authentication while it holds the gate.
    let state = relay.state.lock().await;
    let registering_relay = relay.clone();
    let registering = tokio::spawn(async move {
        let hello = installation_hello(token);
        let id = hello.controller_instance_id;
        let (sender, receiver, closed) = outbound_channel();
        let generation = registering_relay
            .authenticate_and_register_controller(&hello, sender)
            .await;
        (id, generation, receiver, closed)
    });
    loop {
        if relay.enrollment_gate.try_lock().is_err() {
            break;
        }
        tokio::task::yield_now().await;
    }
    let revoking_relay = relay.clone();
    let revoking =
        tokio::spawn(async move { revoking_relay.revoke_mcp_client(installation_id).await });
    tokio::task::yield_now().await;
    assert!(
        !revoking.is_finished(),
        "revoke must wait until registration can be removed"
    );
    drop(state);
    let (id, result, _receiver, closed) = registering.await.unwrap();
    assert!(result.is_ok());
    assert!(revoking.await.unwrap().unwrap());
    assert!(!relay.state.lock().await.controllers.contains_key(&id));
    time::timeout(time::Duration::from_secs(1), closed.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn registration_queued_after_revoke_cannot_resurrect_installation() {
    let (relay, installation_id, token) = enrolled_relay();
    let relay = Arc::new(relay);
    let gate = relay.enrollment_gate.lock().await;
    let revoking_relay = relay.clone();
    let revoking =
        tokio::spawn(async move { revoking_relay.revoke_mcp_client(installation_id).await });
    tokio::task::yield_now().await;
    let registering_relay = relay.clone();
    let registering = tokio::spawn(async move {
        let (sender, _receiver) = test_channel();
        registering_relay
            .authenticate_and_register_controller(&installation_hello(token), sender)
            .await
    });
    tokio::task::yield_now().await;
    drop(gate);
    assert!(revoking.await.unwrap().unwrap());
    assert!(registering.await.unwrap().is_err());
    assert!(relay.state.lock().await.controllers.is_empty());
}

#[tokio::test]
async fn revoke_closes_actual_controller_stream_without_another_incoming_frame() {
    let (relay, installation_id, token) = enrolled_relay();
    let relay = Arc::new(relay);
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let serving_relay = relay.clone();
    let serving = tokio::spawn(async move { serving_relay.handle_client(server).await });
    write_frame(
        &mut client,
        &WireMessage::Hello(ClientHello::Controller(installation_hello(token))),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame::<WireMessage, _>(&mut client).await.unwrap(),
        WireMessage::ControllerWelcome { .. }
    ));
    assert!(relay.revoke_mcp_client(installation_id).await.unwrap());
    let result = time::timeout(
        time::Duration::from_secs(1),
        read_frame::<WireMessage, _>(&mut client),
    )
    .await
    .unwrap();
    assert!(
        result.is_err(),
        "revoked connection must close without waiting for a heartbeat"
    );
    serving.await.unwrap().unwrap();
}

#[test]
fn legacy_token_with_installation_prefix_remains_compatible() {
    let relay = Relay::new(
        Duration::minutes(10),
        15,
        test_owner_id(),
        "roc1.legacy-human-token-with-at-least-32-bytes".to_owned(),
        "roc1.legacy-ai-token-with-at-least-32-bytes".to_owned(),
    );
    let mut hello = installation_hello("roc1.legacy-ai-token-with-at-least-32-bytes".to_owned());
    assert_eq!(relay.authenticate_controller(&hello).unwrap(), None);
    hello.kind = ControllerKind::Human;
    hello.auth_token = "roc1.legacy-human-token-with-at-least-32-bytes".to_owned();
    assert_eq!(relay.authenticate_controller(&hello).unwrap(), None);
}

#[test]
fn corrupted_enrollment_database_refuses_relay_startup() {
    let files = TestStateFile::new("corrupt-enrollment");
    drop(persisted_relay(&files.path, Duration::minutes(10)).unwrap());
    std::fs::write(
        enrollment_database_path(&files.path),
        b"not a sqlite database",
    )
    .unwrap();
    assert!(persisted_relay(&files.path, Duration::minutes(10)).is_err());
}

#[test]
fn sqlite_suffix_in_legacy_json_filename_keeps_enrollment_database_distinct() {
    let files = TestStateFile::new("sqlite-suffix-json");
    let state_path = files.root.join("custom-state.sqlite3");
    let database_path = enrollment_database_path(&state_path);
    assert_eq!(
        database_path,
        files.root.join("custom-state.sqlite3.mcp.sqlite3")
    );
    assert_eq!(
        enrollment_database_path(&files.path),
        files.root.join("relay-state.json.mcp.sqlite3")
    );
    drop(persisted_relay(&state_path, Duration::minutes(10)).unwrap());
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    assert!(
        std::fs::read(&database_path)
            .unwrap()
            .starts_with(b"SQLite format 3\0")
    );
    drop(persisted_relay(&state_path, Duration::minutes(10)).unwrap());
    let restored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(
        original, restored,
        "legacy JSON must remain readable and unchanged on restart"
    );
}
