async fn control_fixture(
    permission: PermissionMode,
) -> (
    Relay,
    AgentInstanceId,
    AgentRegistration,
    mpsc::Receiver<WireMessage>,
    ControllerInstanceId,
    u64,
    SessionId,
) {
    let relay = relay(Duration::minutes(10));
    let agent_id = AgentInstanceId::new();
    let (agent, receiver, session_id) = ready_agent(&relay, agent_id, None).await;
    let (ai_id, generation, _controller_receiver) =
        register_controller(&relay, ControllerKind::Ai).await;
    assert!(
        relay
            .pair_controller(
                ai_id,
                generation,
                PairRequest {
                    request_id: RequestId::new(),
                    pairing_code: agent.welcome.pairing_code.clone(),
                    permission_mode: permission,
                }
            )
            .await
            .error
            .is_none()
    );
    (
        relay, agent_id, agent, receiver, ai_id, generation, session_id,
    )
}

async fn current_control(relay: &Relay, session_id: SessionId) -> SessionControlState {
    refresh_control_state(&mut *relay.state.lock().await, session_id).expect("应有在线AI授权状态")
}

async fn change_control(
    relay: &Relay,
    controller: ControllerInstanceId,
    generation: u64,
    expected: SessionControlState,
    mode: ControllerControlMode,
    source: ControlSource,
) -> ControlStateResult {
    let (sender, mut receiver) = test_channel();
    relay
        .handle_controller_control_state(
            controller,
            generation,
            ControlStateRequest {
                request_id: RequestId::new(),
                session_id: expected.session_id,
                expected: Some(expected),
                mode: Some(mode),
                source: Some(source),
            },
            &sender,
        )
        .await;
    match receiver.try_recv().expect("应立即确认") {
        WireMessage::ControlStateResult(result) => result,
        other => panic!("预期授权结果，实际{other:?}"),
    }
}

#[tokio::test]
async fn control_state_rejects_forged_source_stale_revision_and_generation() {
    let (relay, agent_id, agent, mut receiver, ai, generation, session) =
        control_fixture(PermissionMode::ControllerApproved).await;
    let initial = current_control(&relay, session).await;
    assert_eq!(initial.mode, ControllerControlMode::StepByStep);
    let forged = change_control(
        &relay,
        ai,
        generation,
        initial.clone(),
        ControllerControlMode::FullAccess,
        ControlSource::Local,
    )
    .await;
    assert!(forged.error.is_some());
    assert_eq!(current_control(&relay, session).await, initial);
    let full = change_control(
        &relay,
        ai,
        generation,
        initial.clone(),
        ControllerControlMode::FullAccess,
        ControlSource::Mcp,
    )
    .await;
    assert!(full.error.is_none());
    let full = full.state.unwrap();
    assert!(
        full.expires_at
            .is_some_and(|expiry| expiry > Utc::now() + Duration::minutes(59))
    );
    assert!(full.revision > initial.revision);
    assert!(
        change_control(
            &relay,
            ai,
            generation,
            initial,
            ControllerControlMode::StepByStep,
            ControlSource::Mcp
        )
        .await
        .error
        .is_some()
    );
    assert!(
        change_control(
            &relay,
            ai,
            generation + 1,
            full.clone(),
            ControllerControlMode::StepByStep,
            ControlSource::Mcp
        )
        .await
        .error
        .is_some()
    );
    let (sender, mut results) = test_channel();
    relay
        .handle_agent_control_state(
            agent_id,
            agent.connection_generation,
            ControlStateRequest {
                request_id: RequestId::new(),
                session_id: session,
                expected: Some(full.clone()),
                mode: Some(ControllerControlMode::FullAccess),
                source: Some(ControlSource::Local),
            },
            &sender,
        )
        .await;
    let WireMessage::ControlStateResult(local) = results.try_recv().unwrap() else {
        panic!("预期结果")
    };
    let local = local.state.unwrap();
    assert_eq!(local.source, Some(ControlSource::Local));
    assert_eq!(local.expires_at, None);
    assert!(receiver.try_recv().is_ok());
    assert!(
        change_control(
            &relay,
            ai,
            generation,
            full,
            ControllerControlMode::StepByStep,
            ControlSource::Mcp
        )
        .await
        .error
        .is_some()
    );
}

#[tokio::test]
async fn control_state_local_change_rejects_wrong_agent_session_and_mcp_source() {
    let (relay, agent_id, agent, _receiver, _, _, session) =
        control_fixture(PermissionMode::ControllerApproved).await;
    let current = current_control(&relay, session).await;
    for (target, generation, source) in [
        (
            session,
            agent.connection_generation + 1,
            ControlSource::Local,
        ),
        (
            SessionId::new(),
            agent.connection_generation,
            ControlSource::Local,
        ),
        (session, agent.connection_generation, ControlSource::Mcp),
    ] {
        let (sender, mut receiver) = test_channel();
        relay
            .handle_agent_control_state(
                agent_id,
                generation,
                ControlStateRequest {
                    request_id: RequestId::new(),
                    session_id: target,
                    expected: Some(current.clone()),
                    mode: Some(ControllerControlMode::FullAccess),
                    source: Some(source),
                },
                &sender,
            )
            .await;
        let WireMessage::ControlStateResult(result) = receiver.try_recv().unwrap() else {
            panic!("预期结果")
        };
        assert!(result.error.is_some());
        assert_eq!(current_control(&relay, session).await, current);
    }
}

#[tokio::test]
async fn control_state_limits_and_legacy_heartbeats_never_grant_full_access() {
    for (permission, expected_mode) in [
        (PermissionMode::ReadOnly, ControllerControlMode::ReadOnly),
        (
            PermissionMode::ApprovalRequired,
            ControllerControlMode::ExternalApproval,
        ),
    ] {
        let (relay, _, _, mut receiver, ai, generation, session) =
            control_fixture(permission).await;
        let current = current_control(&relay, session).await;
        assert_eq!(current.mode, expected_mode);
        assert!(
            change_control(
                &relay,
                ai,
                generation,
                current.clone(),
                ControllerControlMode::FullAccess,
                ControlSource::Mcp
            )
            .await
            .error
            .is_some()
        );
        relay
            .update_controller_control_modes(
                ai,
                generation,
                vec![ControllerControlModeUpdate {
                    session_id: session,
                    mode: ControllerControlMode::FullAccess,
                }],
            )
            .await;
        assert_eq!(current_control(&relay, session).await, current);
        assert!(std::iter::from_fn(|| receiver.try_recv().ok()).any(
            |message| matches!(message, WireMessage::ControlStateUpdated(state) if state == current)
        ));
    }
}

#[tokio::test]
async fn control_state_revokes_on_reconnect_and_repair_and_ignores_old_proofs() {
    let (relay, agent_id, agent, _receiver, ai, generation, session) =
        control_fixture(PermissionMode::ControllerApproved).await;
    let old = change_control(
        &relay,
        ai,
        generation,
        current_control(&relay, session).await,
        ControllerControlMode::FullAccess,
        ControlSource::Mcp,
    )
    .await
    .state
    .unwrap();
    relay
        .mark_agent_disconnected(agent_id, agent.connection_generation)
        .await;
    assert!(
        relay.state.lock().await.session_bindings[&session]
            .ai
            .as_ref()
            .unwrap()
            .control_state
            .is_none()
    );
    let (resumed, _receiver, _) =
        ready_agent(&relay, agent_id, Some(agent.welcome.resume_token)).await;
    let current = current_control(&relay, session).await;
    assert_eq!(current.mode, ControllerControlMode::StepByStep);
    assert_eq!(current.agent_generation, resumed.connection_generation);
    assert!(
        change_control(
            &relay,
            ai,
            generation,
            old,
            ControllerControlMode::FullAccess,
            ControlSource::Mcp
        )
        .await
        .error
        .is_some()
    );
    assert!(
        relay
            .release_controller_session(
                ai,
                generation,
                ReleaseSessionRequest {
                    request_id: RequestId::new(),
                    session_id: session
                }
            )
            .await
            .released
    );
    relay
        .pair_controller(
            ai,
            generation,
            PairRequest {
                request_id: RequestId::new(),
                pairing_code: resumed.welcome.pairing_code,
                permission_mode: PermissionMode::ControllerApproved,
            },
        )
        .await;
    assert_ne!(
        current_control(&relay, session).await.revision,
        current.revision
    );
    assert!(
        change_control(
            &relay,
            ai,
            generation,
            current,
            ControllerControlMode::FullAccess,
            ControlSource::Mcp
        )
        .await
        .error
        .is_some()
    );
}

#[tokio::test]
async fn control_proof_blocks_legacy_stale_and_expired_writes_but_allows_current_confirmation() {
    let (relay, _, _, mut receiver, ai, generation, session) =
        control_fixture(PermissionMode::ControllerApproved).await;
    while receiver.try_recv().is_ok() {}
    let (sender, mut errors) = test_channel();
    let mut request = RemoteRequest {
        request_id: RequestId::new(),
        session_id: session,
        source: EventSource::Ai,
        operation: run_command("Set-Content C:\\temp\\control-test.txt ok", false),
        approval_id: None,
        payload_base64: None,
        control_proof: None,
    };
    relay
        .forward_controller_request(ai, generation, request.clone(), &sender)
        .await;
    assert!(
        matches!(errors.try_recv().unwrap(), WireMessage::Error {code, ..} if code == "control_authorization_required")
    );
    assert!(receiver.try_recv().is_err());
    let current = current_control(&relay, session).await;
    request.control_proof = Some(remoteops_protocol::ControlProof {
        state: current.clone(),
        basis: ControlBasis::SingleApproval,
    });
    relay
        .forward_controller_request(ai, generation, request.clone(), &sender)
        .await;
    let authorized = recv_authorized(&mut receiver).await;
    assert_eq!(
        authorized.authorization.permission_mode,
        PermissionMode::ControllerApproved
    );
    let full = change_control(
        &relay,
        ai,
        generation,
        current,
        ControllerControlMode::FullAccess,
        ControlSource::Mcp,
    )
    .await
    .state
    .unwrap();
    while receiver.try_recv().is_ok() {}
    request.request_id = RequestId::new();
    relay
        .forward_controller_request(ai, generation, request.clone(), &sender)
        .await;
    assert!(matches!(
        errors.try_recv().unwrap(),
        WireMessage::Error { .. }
    ));
    {
        let mut state = relay.state.lock().await;
        state
            .session_bindings
            .get_mut(&session)
            .unwrap()
            .ai
            .as_mut()
            .unwrap()
            .control_state
            .as_mut()
            .unwrap()
            .expires_at = Some(Utc::now() - Duration::seconds(1));
    }
    request.control_proof = Some(remoteops_protocol::ControlProof {
        state: full,
        basis: ControlBasis::FullAccess,
    });
    relay
        .forward_controller_request(ai, generation, request, &sender)
        .await;
    assert!(matches!(
        errors.try_recv().unwrap(),
        WireMessage::Error { .. }
    ));
    assert_eq!(
        current_control(&relay, session).await.mode,
        ControllerControlMode::Expired
    );
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn control_state_successful_operations_renew_without_invalidating_revision() {
    let (relay, agent_id, agent, mut receiver, ai, generation, session) =
        control_fixture(PermissionMode::ControllerApproved).await;
    let full = change_control(
        &relay,
        ai,
        generation,
        current_control(&relay, session).await,
        ControllerControlMode::FullAccess,
        ControlSource::Mcp,
    )
    .await
    .state
    .unwrap();
    {
        let mut state = relay.state.lock().await;
        state
            .session_bindings
            .get_mut(&session)
            .unwrap()
            .ai
            .as_mut()
            .unwrap()
            .control_state
            .as_mut()
            .unwrap()
            .expires_at = Some(Utc::now() + Duration::minutes(1));
    }
    for exit_code in [1, 0] {
        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id: session,
            source: EventSource::Ai,
            operation: run_command("Get-Process", true),
            approval_id: None,
            payload_base64: None,
            control_proof: None,
        };
        let (sender, _) = test_channel();
        relay
            .forward_controller_request(ai, generation, request.clone(), &sender)
            .await;
        let _ = recv_authorized(&mut receiver).await;
        relay
            .forward_agent_message(
                agent_id,
                agent.connection_generation,
                WireMessage::RemoteResponse(remoteops_protocol::RemoteResponse {
                    request_id: request.request_id,
                    session_id: session,
                    exit_code: Some(exit_code),
                    summary: "done".to_owned(),
                    error_code: None,
                    details: None,
                    payload_base64: None,
                    sha256: None,
                }),
            )
            .await;
        let latest = current_control(&relay, session).await;
        assert_eq!(latest.revision, full.revision);
        assert_eq!(
            latest.expires_at.unwrap() > Utc::now() + Duration::minutes(59),
            exit_code == 0
        );
    }
    let latest = current_control(&relay, session).await;
    assert!(control::control_versions_match(&latest, &full));
    assert!(
        change_control(
            &relay,
            ai,
            generation,
            full,
            ControllerControlMode::StepByStep,
            ControlSource::Mcp
        )
        .await
        .error
        .is_none()
    );
}

#[tokio::test]
async fn upload_continuations_require_original_current_control_proof() {
    let (relay, _, _, mut receiver, ai, generation, session) =
        control_fixture(PermissionMode::ControllerApproved).await;
    let original = current_control(&relay, session).await;
    let transfer_id = remoteops_domain::FileTransferId::new();
    let operations = [
        RemoteOperation::UploadFileChunk {
            transfer_id,
            offset: 0,
            size: 1,
            sha256: "hash".to_owned(),
        },
        RemoteOperation::CompleteUploadFile { transfer_id },
    ];
    for operation in &operations {
        assert_eq!(relay.policy.classify(operation), RiskLevel::ReadOnly);
        assert!(control::requires_control_proof(&relay.policy, operation));
    }
    while receiver.try_recv().is_ok() {}
    let (sender, mut errors) = test_channel();
    for operation in &operations {
        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id: session,
            source: EventSource::Ai,
            operation: operation.clone(),
            approval_id: None,
            payload_base64: None,
            control_proof: None,
        };
        relay
            .forward_controller_request(ai, generation, request.clone(), &sender)
            .await;
        assert!(
            matches!(errors.try_recv().unwrap(), WireMessage::Error {code, ..} if code == "control_authorization_required")
        );
        let mut request = request;
        request.control_proof = Some(remoteops_protocol::ControlProof {
            state: original.clone(),
            basis: ControlBasis::SingleApproval,
        });
        relay
            .forward_controller_request(ai, generation, request, &sender)
            .await;
        let _ = recv_authorized(&mut receiver).await;
    }
    let current = change_control(
        &relay,
        ai,
        generation,
        original.clone(),
        ControllerControlMode::StepByStep,
        ControlSource::Mcp,
    )
    .await
    .state
    .unwrap();
    assert_ne!(current.revision, original.revision);
    while receiver.try_recv().is_ok() {}
    for operation in operations {
        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id: session,
            source: EventSource::Ai,
            operation,
            approval_id: None,
            payload_base64: None,
            control_proof: Some(remoteops_protocol::ControlProof {
                state: original.clone(),
                basis: ControlBasis::SingleApproval,
            }),
        };
        relay
            .forward_controller_request(ai, generation, request, &sender)
            .await;
        assert!(
            matches!(errors.try_recv().unwrap(), WireMessage::Error {code, ..} if code == "control_authorization_required")
        );
        assert!(receiver.try_recv().is_err());
    }
    assert!(!control::requires_control_proof(
        &relay.policy,
        &RemoteOperation::AbortUploadFile { transfer_id }
    ));
}

#[tokio::test]
async fn agent_heartbeat_expires_and_resynchronizes_only_current_generation() {
    let (relay, agent_id, agent, mut receiver, ai, generation, session) =
        control_fixture(PermissionMode::ControllerApproved).await;
    let full = change_control(
        &relay,
        ai,
        generation,
        current_control(&relay, session).await,
        ControllerControlMode::FullAccess,
        ControlSource::Mcp,
    )
    .await
    .state
    .unwrap();
    {
        let mut state = relay.state.lock().await;
        state
            .session_bindings
            .get_mut(&session)
            .unwrap()
            .ai
            .as_mut()
            .unwrap()
            .control_state
            .as_mut()
            .unwrap()
            .expires_at = Some(Utc::now() - Duration::seconds(1));
    }
    while receiver.try_recv().is_ok() {}
    relay
        .sync_agent_control_state(agent_id, agent.connection_generation + 1)
        .await;
    assert!(receiver.try_recv().is_err());
    relay
        .sync_agent_control_state(agent_id, agent.connection_generation)
        .await;
    let WireMessage::ControlStateUpdated(expired) = receiver.try_recv().unwrap() else {
        panic!("应补发权威授权")
    };
    assert_eq!(expired.mode, ControllerControlMode::Expired);
    assert!(expired.revision > full.revision);
    relay
        .sync_agent_control_state(agent_id, agent.connection_generation)
        .await;
    assert!(
        matches!(receiver.try_recv().unwrap(), WireMessage::ControlStateUpdated(state) if state == expired)
    );
}
