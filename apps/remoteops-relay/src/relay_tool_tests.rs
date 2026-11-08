// 工具管理权限与目标会话、运行开关相互独立。
async fn tool_request(
    relay: &Relay,
    controller: ControllerInstanceId,
    generation: u64,
    command: remoteops_tool_center::ToolCenterCommand,
    revision: u64,
) -> remoteops_tool_center::ToolCenterResponse {
    let (sender, mut receiver) = test_channel();
    let request = remoteops_tool_center::ToolCenterRequest {
        request_id: Uuid::new_v4().to_string(),
        expected_revision: revision,
        command,
    };
    relay
        .dispatch_tool_management(
            controller,
            generation,
            request.request_id.clone(),
            serde_json::to_value(request).unwrap(),
            sender,
            Arc::new(tokio::sync::Semaphore::new(1)),
        )
        .await;
    match time::timeout(time::Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap()
    {
        WireMessage::ToolCenterResponse { response, .. } => {
            serde_json::from_value(response).unwrap()
        }
        other => panic!("错误响应: {other:?}"),
    }
}
#[tokio::test]
async fn tool_management_requires_installation_grant_and_revocation_is_immediate() {
    use remoteops_tool_center::ToolCenterCommand as Command;
    let (mut relay, installation, token) = enrolled_relay();
    let directory = TestStateFile::new("tool-permissions");
    relay.tool_center = Some(Arc::new(
        remoteops_tool_center::ToolCenter::open(&directory.root).unwrap(),
    ));
    let hello = installation_hello(token);
    let (sender, _receiver) = test_channel();
    let generation = relay
        .authenticate_and_register_controller(&hello, sender)
        .await
        .unwrap();
    let denied = tool_request(
        &relay,
        hello.controller_instance_id,
        generation,
        Command::Snapshot,
        0,
    )
    .await;
    assert_eq!(denied.error.unwrap().code, "tool_management_denied");
    relay
        .set_mcp_tool_management(installation, true)
        .await
        .unwrap();
    let snapshot = tool_request(
        &relay,
        hello.controller_instance_id,
        generation,
        Command::Snapshot,
        0,
    )
    .await;
    assert!(snapshot.error.is_none());
    assert_eq!(snapshot.data["enabled"], false);
    assert!(
        relay.state.lock().await.controllers[&hello.controller_instance_id]
            .sessions
            .is_empty()
    );
    let enabled = tool_request(
        &relay,
        hello.controller_instance_id,
        generation,
        Command::SetPolicy { enabled: true },
        snapshot.revision,
    )
    .await;
    assert!(enabled.error.is_none());
    relay
        .set_mcp_tool_management(installation, false)
        .await
        .unwrap();
    let denied = tool_request(
        &relay,
        hello.controller_instance_id,
        generation,
        Command::SetPolicy { enabled: false },
        enabled.revision,
    )
    .await;
    assert_eq!(denied.error.unwrap().code, "tool_management_denied");
    assert!(relay.tool_center().unwrap().snapshot().unwrap().enabled);
    let (legacy, legacy_generation, _) = register_controller(&relay, ControllerKind::Ai).await;
    let denied = tool_request(
        &relay,
        legacy,
        legacy_generation,
        Command::Snapshot,
        enabled.revision,
    )
    .await;
    assert_eq!(denied.error.unwrap().code, "tool_management_denied");
}
#[tokio::test]
async fn tool_catalog_broadcast_skips_legacy_and_reports_applied_revision() {
    use remoteops_tool_center::{ToolCenterCommand, ToolCenterRequest};
    let directory = TestStateFile::new("tool-sync");
    let relay = persisted_relay(&directory.path, Duration::minutes(10)).unwrap();
    let current_id = AgentInstanceId::new();
    let (registration, mut current, _) = ready_agent(&relay, current_id, None).await;
    let legacy_id = AgentInstanceId::new();
    let (_, mut legacy, _) = ready_agent(&relay, legacy_id, None).await;
    relay
        .state
        .lock()
        .await
        .agents
        .get_mut(&legacy_id)
        .unwrap()
        .hello
        .protocol_version = 18;
    let response = relay
        .admin_tools_request(
            "admin:test",
            ToolCenterRequest {
                request_id: Uuid::new_v4().to_string(),
                expected_revision: 0,
                command: ToolCenterCommand::SetPolicy { enabled: true },
            },
        )
        .await;
    assert!(response.error.is_none());
    assert!(matches!(
        time::timeout(time::Duration::from_secs(2), current.recv())
            .await
            .unwrap(),
        Some(WireMessage::ToolCatalog { .. })
    ));
    let guide = relay
        .admin_tools_request(
            "admin:test",
            ToolCenterRequest {
                request_id: Uuid::new_v4().to_string(),
                expected_revision: response.revision,
                command: ToolCenterCommand::SetGuide {
                    guide_markdown: "# broadcast guide".into(),
                },
            },
        )
        .await;
    assert!(guide.error.is_none());
    let message = time::timeout(time::Duration::from_secs(2), current.recv())
        .await
        .unwrap()
        .unwrap();
    match message {
        WireMessage::ToolCatalog { catalog } => {
            assert_eq!(catalog["guide_markdown"], "# broadcast guide");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(legacy.try_recv().is_err());
    relay
        .record_catalog_applied(
            current_id,
            registration.connection_generation,
            guide.revision,
            None,
        )
        .await;
    let status = relay.tool_sync_status().await;
    let nodes = status["nodes"].as_array().unwrap();
    assert_eq!(nodes.iter().filter(|v| v["synced"] == true).count(), 1);
    relay
        .record_catalog_applied(
            current_id,
            registration.connection_generation + 1,
            guide.revision + 1,
            None,
        )
        .await;
    assert_eq!(relay.tool_sync_status().await, status);
}

#[tokio::test]
async fn catalog_sync_status_separates_failure_offline_and_connection_generation() {
    let directory = TestStateFile::new("tool-sync-states");
    let relay = persisted_relay(&directory.path, Duration::minutes(10)).unwrap();
    let id = AgentInstanceId::new();
    let (registration, _messages, _) = ready_agent(&relay, id, None).await;
    let status = || async {
        let status = relay.tool_sync_status().await;
        status["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["agent_instance_id"] == id.to_string())
            .unwrap()
            .clone()
    };
    assert_eq!(status().await["sync_status"], "pending");
    relay
        .record_catalog_applied(id, registration.connection_generation, 0, None)
        .await;
    assert_eq!(status().await["sync_status"], "synced");
    relay
        .record_catalog_applied(
            id,
            registration.connection_generation,
            0,
            Some("private remote error must not be returned".into()),
        )
        .await;
    let failed = status().await;
    assert_eq!(failed["sync_status"], "apply_failed");
    assert_eq!(failed["synced"], false);
    assert!(!failed.to_string().contains("private remote"));
    relay
        .record_catalog_applied(id, registration.connection_generation, 99, None)
        .await;
    assert_eq!(status().await["sync_status"], "update_pending");
    {
        let mut state = relay.state.lock().await;
        state.agents.get_mut(&id).unwrap().ready = false;
    }
    assert_eq!(status().await["sync_status"], "offline");
    {
        let mut state = relay.state.lock().await;
        let agent = state.agents.get_mut(&id).unwrap();
        agent.ready = true;
        agent.connection_generation += 1;
    }
    assert_eq!(status().await["sync_status"], "pending");
    assert_eq!(status().await["applied_revision"], serde_json::Value::Null);
    {
        let mut state = relay.state.lock().await;
        let agent = state.agents.get_mut(&id).unwrap();
        agent.hello.protocol_version = 18;
        agent.ready = false;
    }
    assert_eq!(status().await["sync_status"], "upgrade_required");
}

#[tokio::test]
async fn guide_and_upload_permissions_are_isolated_between_installations() {
    use remoteops_enrollment::{CreateGrantRequest, RedeemRequest};
    use remoteops_tool_center::ToolCenterCommand as Command;
    let (mut relay, first_id, first_token) = enrolled_relay();
    let root = TestStateFile::new("guide-permission-isolation");
    relay.tool_center = Some(Arc::new(
        remoteops_tool_center::ToolCenter::open(&root.root).unwrap(),
    ));
    let store = relay.enrollment_store.as_ref().unwrap();
    let grant = store
        .create_grant(
            CreateGrantRequest {
                client_name: Some("ordinary MCP".into()),
                expires_in_hours: Some(1),
            },
            Utc::now(),
        )
        .unwrap();
    let second_id = Uuid::new_v4();
    let secret = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE";
    store
        .redeem(
            RedeemRequest {
                grant_id: grant.setup.grant_id,
                grant_secret: grant.setup.grant_secret,
                installation_id: second_id,
                installation_secret: secret.into(),
            },
            Utc::now(),
        )
        .unwrap();
    let mut clients = Vec::new();
    for token in [first_token, format!("roc1.{second_id}.{secret}")] {
        let hello = installation_hello(token);
        let (sender, receiver) = test_channel();
        let generation = relay
            .authenticate_and_register_controller(&hello, sender)
            .await
            .unwrap();
        clients.push((hello.controller_instance_id, generation, receiver));
    }
    let guide = || Command::SetGuide {
        guide_markdown: "# selection guide".into(),
    };
    let upload = || Command::BeginUpload {
        size: 1,
        sha256: "a".repeat(64),
    };
    for (id, generation, _) in &clients {
        for command in [guide(), upload()] {
            assert_eq!(
                tool_request(&relay, *id, *generation, command, 0)
                    .await
                    .error
                    .unwrap()
                    .code,
                "tool_management_denied"
            );
        }
    }
    relay.set_mcp_tool_management(first_id, true).await.unwrap();
    let saved = tool_request(&relay, clients[0].0, clients[0].1, guide(), 0).await;
    assert!(saved.error.is_none());
    assert_eq!(
        relay.tool_center().unwrap().cached_catalog().guide_markdown,
        "# selection guide"
    );
    assert!(
        tool_request(&relay, clients[0].0, clients[0].1, upload(), saved.revision)
            .await
            .error
            .is_none()
    );
    for command in [guide(), upload()] {
        assert_eq!(
            tool_request(&relay, clients[1].0, clients[1].1, command, saved.revision)
                .await
                .error
                .unwrap()
                .code,
            "tool_management_denied"
        );
    }
    relay
        .set_mcp_tool_management(first_id, false)
        .await
        .unwrap();
    for command in [guide(), upload()] {
        assert_eq!(
            tool_request(&relay, clients[0].0, clients[0].1, command, saved.revision)
                .await
                .error
                .unwrap()
                .code,
            "tool_management_denied"
        );
    }
}
#[tokio::test]
async fn management_transfer_does_not_block_authenticated_heartbeat() {
    let (relay, installation, token) = enrolled_relay();
    relay
        .set_mcp_tool_management(installation, true)
        .await
        .unwrap();
    let relay = Arc::new(relay);
    let (server, mut client) = tokio::io::duplex(1024 * 1024);
    let task = tokio::spawn({
        let relay = relay.clone();
        async move { relay.handle_client(server).await }
    });
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
    let request_id = Uuid::new_v4().to_string();
    write_frame(&mut client,&WireMessage::ToolCenterRequest{request_id:request_id.clone(),request:serde_json::json!({"request_id":request_id,"expected_revision":0,"command":{"action":"snapshot"}})}).await.unwrap();
    write_frame(
        &mut client,
        &WireMessage::Heartbeat {
            sent_at: Utc::now(),
            controller_modes: vec![],
        },
    )
    .await
    .unwrap();
    let mut heartbeat = false;
    let mut response = false;
    for _ in 0..2 {
        let message = time::timeout(
            time::Duration::from_secs(2),
            read_frame::<WireMessage, _>(&mut client),
        )
        .await
        .unwrap()
        .unwrap();
        heartbeat |= matches!(message, WireMessage::HeartbeatAck { .. });
        response |= matches!(message, WireMessage::ToolCenterResponse { .. });
    }
    assert!(heartbeat && response);
    task.abort();
    let _ = task.await;
}
