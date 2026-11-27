// 启动边界同时约束控制码、逻辑会话和恢复令牌的容错窗口。

#[tokio::test]
async fn boot_unconfirmed_rotation_limit_preserves_current_recovery() {
    let relay = relay(Duration::minutes(10));
    let agent_id = AgentInstanceId::new();
    let (first, _, _) = ready_agent(&relay, agent_id, None).await;
    let token = first.welcome.resume_token;
    let mut current_boot = AgentBootId::new();
    for _ in 0..MAX_UNCONFIRMED_BOOTS {
        current_boot = AgentBootId::new();
        let (sender, _receiver) = test_channel();
        relay
            .register_agent(
                boot_test_hello(agent_id, current_boot, Some(token.clone())),
                sender,
            )
            .await
            .expect("上限内允许携带有效凭据的新启动");
    }
    let (sender, _receiver) = test_channel();
    assert!(
        relay
            .register_agent(
                boot_test_hello(agent_id, AgentBootId::new(), Some(token.clone())),
                sender
            )
            .await
            .is_err(),
        "未确认的启动记录不能无限增长"
    );
    let (sender, _receiver) = test_channel();
    let recovered = relay
        .register_agent(boot_test_hello(agent_id, current_boot, Some(token)), sender)
        .await
        .expect("达到上限不能阻止当前启动恢复");
    assert!(
        relay
            .commit_agent_resume(agent_id, recovered.connection_generation)
            .await
    );
    assert!(
        relay
            .finalize_agent_resume(agent_id, recovered.connection_generation)
            .await
            .is_some()
    );
    assert!(
        relay.state.lock().await.agents[&agent_id]
            .retired_boots
            .is_empty()
    );
    let (sender, _receiver) = test_channel();
    assert!(
        relay
            .register_agent(
                boot_test_hello(
                    agent_id,
                    AgentBootId::new(),
                    Some(recovered.welcome.resume_token)
                ),
                sender
            )
            .await
            .is_ok(),
        "完成令牌交接后释放退役记录配额"
    );
}

fn boot_test_hello(
    agent_id: AgentInstanceId,
    boot_id: AgentBootId,
    token: Option<String>,
) -> AgentHello {
    let mut value = hello(agent_id, token);
    value.boot_id = boot_id;
    value
}

#[tokio::test]
async fn short_code_capability_preserves_existing_lease_and_handles_agent_downgrade() {
    let relay = relay(Duration::minutes(10));
    let agent = AgentInstanceId::new();
    let mut hello = hello(agent, None);
    let (sender, _receiver) = test_channel();
    let register = |hello| relay.register_agent(hello, sender.clone());
    let first = register(hello.clone()).await.unwrap();
    let numeric = first.welcome.pairing_code.unwrap();
    assert!(numeric.as_str().bytes().all(|b| b.is_ascii_digit()));
    assert!(relay.commit_agent_resume(agent, first.connection_generation).await);
    relay.finalize_agent_resume(agent, first.connection_generation).await.unwrap();
    hello.resume_token = Some(first.welcome.resume_token);
    hello.supports_mixed_pairing_code = true;
    let same_boot = register(hello.clone()).await.unwrap();
    assert_eq!(same_boot.welcome.pairing_code.as_ref(), Some(&numeric));
    assert!(relay.commit_agent_resume(agent, same_boot.connection_generation).await);
    relay.finalize_agent_resume(agent, same_boot.connection_generation).await.unwrap();
    hello.resume_token = Some(same_boot.welcome.resume_token);
    hello.boot_id = AgentBootId::new();
    let upgraded = register(hello.clone()).await.unwrap();
    let mixed = upgraded.welcome.pairing_code.unwrap();
    assert!(mixed.is_mixed_short());
    assert!(!mixed.as_str().contains(['I', 'O']));
    assert!(relay.commit_agent_resume(agent, upgraded.connection_generation).await);
    relay.finalize_agent_resume(agent, upgraded.connection_generation).await.unwrap();
    let (controller, generation, _rx) = register_controller(&relay, ControllerKind::Human).await;
    let request = PairRequest {
        pairing_code: Some(AccessCode::parse(mixed.display_grouped().to_ascii_lowercase()).unwrap()),
        request_id: RequestId::new(),
        expected_agent_id: Some(agent),
        expected_session_id: None,
        connection_resume_token: None,
        permission_mode: PermissionMode::ApprovalRequired,
    };
    assert!(relay.pair_controller(controller, generation, request).await.connection.is_some());
    hello.resume_token = Some(upgraded.welcome.resume_token);
    hello.supports_mixed_pairing_code = false;
    let fallback = register(hello).await.unwrap();
    assert!(fallback.welcome.pairing_code.unwrap().as_str().bytes().all(|b| b.is_ascii_digit()));
    assert!(!relay.state.lock().await.pairing_index.contains_key(&mixed));
}

async fn boot_seed_runtime(
    relay: &Relay,
    agent_id: AgentInstanceId,
    session_id: SessionId,
    controller: ControllerInstanceId,
    generation: u64,
) -> (ApprovalId, RequestId, String) {
    let control = current_control(relay, session_id).await;
    let mut state = relay.state.lock().await;
    let binding = state.session_bindings[&session_id]
        .ai
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(binding.control_state.as_ref(), Some(&control));
    let approval_id = ApprovalId::new();
    state.approvals.insert(
        approval_id,
        RelayApprovalRecord {
            session_id,
            owner_id: test_owner_id(),
            operation: run_command("Get-Date", true),
            approval_id,
            state: ApprovalState::Approved,
            expires_at: Utc::now() + Duration::minutes(5),
            controller_id: controller,
            controller_generation: generation,
            controller_kind: ControllerKind::Ai,
        },
    );
    let request_id = RequestId::new();
    state.in_flight.insert(
        request_id,
        InFlightRequest {
            context_query: None,
            binding_token: binding.binding_token.clone(),
            agent_id,
            session_id,
            controller_id: controller,
            owner_id: test_owner_id(),
            controller_generation: generation,
            controller_kind: ControllerKind::Ai,
            source: EventSource::Ai,
            approval: ApprovalState::Approved,
            previous_takeover: Some(false),
            control_state: Some(control),
        },
    );
    (approval_id, request_id, binding.binding_token)
}

#[tokio::test]
async fn boot_restart_rotates_code_and_session_and_revokes_all_old_runtime() {
    let (relay, agent_id, first, _agent_receiver, ai, generation, session_id) =
        control_fixture(PermissionMode::ControllerApproved).await;
    let (approval_id, request_id, _) =
        boot_seed_runtime(&relay, agent_id, session_id, ai, generation).await;
    let (human, human_generation, mut human_receiver) =
        register_controller(&relay, ControllerKind::Human).await;
    assert!(
        relay
            .pair_controller(
                human,
                human_generation,
                PairRequest {
                    expected_agent_id: None,
                    connection_resume_token: None,
                    request_id: RequestId::new(),
                    pairing_code: first.welcome.pairing_code.clone(),
                    expected_session_id: None,
                    permission_mode: PermissionMode::ApprovalRequired,
                }
            )
            .await
            .error
            .is_none()
    );
    while human_receiver.try_recv().is_ok() {}
    let new_boot = AgentBootId::new();
    let (sender, _receiver) = test_channel();
    let next = relay
        .register_agent(
            boot_test_hello(agent_id, new_boot, Some(first.welcome.resume_token)),
            sender,
        )
        .await
        .expect("已认证的新启动应成功");
    assert_ne!(next.welcome.pairing_code, first.welcome.pairing_code);
    {
        let state = relay.state.lock().await;
        let agent = &state.agents[&agent_id];
        assert_eq!(agent.hello.boot_id, new_boot);
        assert_ne!(agent.session_id, session_id);
        assert!(!agent.ready);
        assert!(
            !state
                .pairing_index
                .contains_key(first.welcome.pairing_code.as_ref().unwrap())
        );
        assert_eq!(
            state
                .pairing_index
                .get(next.welcome.pairing_code.as_ref().unwrap()),
            Some(&agent_id)
        );
        assert!(!state.session_bindings.contains_key(&session_id));
        assert!(!state.approvals.contains_key(&approval_id));
        assert!(!state.in_flight.contains_key(&request_id));
        assert!(!state.controllers[&ai].sessions.contains(&session_id));
        assert!(!state.controllers[&human].sessions.contains(&session_id));
    }
    assert!(
        matches!(human_receiver.try_recv(), Ok(WireMessage::ConnectionRemoved { session_id: ended, .. }) if ended == session_id)
    );
    assert!(
        relay
            .commit_agent_resume(agent_id, next.connection_generation)
            .await
    );
    let descriptor = relay
        .finalize_agent_resume(agent_id, next.connection_generation)
        .await
        .unwrap();
    assert_ne!(descriptor.session_id, session_id);
    let state = relay.state.lock().await;
    assert!(
        !state.session_bindings.contains_key(&descriptor.session_id),
        "新会话不能继承旧权限"
    );
}

#[tokio::test]
async fn boot_rotation_persistence_failure_preserves_old_identity_and_runtime() {
    let (mut relay, agent_id, first, _receiver, ai, generation, session_id) =
        control_fixture(PermissionMode::ControllerApproved).await;
    let (approval_id, request_id, binding_token) =
        boot_seed_runtime(&relay, agent_id, session_id, ai, generation).await;
    let original = relay.state.lock().await.agents[&agent_id].clone();
    let state_file = TestStateFile::new("boot-rollback");
    std::fs::create_dir_all(&state_file.root).unwrap();
    let blocker = state_file.root.join("not-a-directory");
    std::fs::write(&blocker, b"block parent creation").unwrap();
    relay.state_path = Some(blocker.join("relay-state.json"));
    let (sender, _receiver) = test_channel();
    assert!(
        relay
            .register_agent(
                boot_test_hello(
                    agent_id,
                    AgentBootId::new(),
                    Some(first.welcome.resume_token)
                ),
                sender,
            )
            .await
            .is_err()
    );
    let state = relay.state.lock().await;
    let restored = &state.agents[&agent_id];
    assert_eq!(restored.hello, original.hello);
    assert_eq!(restored.session_id, original.session_id);
    assert_eq!(restored.lease, original.lease);
    assert_eq!(restored.resume_token, original.resume_token);
    assert_eq!(
        restored.connection_generation,
        original.connection_generation
    );
    assert_eq!(restored.retired_boots, original.retired_boots);
    assert_eq!(restored.ready, original.ready);
    assert!(restored.sender.is_some());
    assert!(restored.pending_resume.is_none());
    assert_eq!(state.pairing_index.len(), 1);
    assert_eq!(
        state
            .pairing_index
            .get(original.lease.pairing_code.as_ref().unwrap()),
        Some(&agent_id)
    );
    let binding = state.session_bindings[&session_id].ai.as_ref().unwrap();
    assert_eq!(binding.binding_token, binding_token);
    assert!(binding.control_state.is_some());
    assert!(state.controllers[&ai].sessions.contains(&session_id));
    assert!(state.approvals.contains_key(&approval_id));
    assert!(state.in_flight.contains_key(&request_id));
}

#[tokio::test]
async fn boot_handshake_retries_are_idempotent_at_each_stage_and_after_relay_restart() {
    for restart_relay in [false, true] {
        // 依次模拟 Welcome 后、提交后未落盘、Agent 已落盘、最终确认后断线。
        for stage in 0..4 {
            let file = TestStateFile::new("boot-handshake-stage");
            let mut relay = persisted_relay(&file.path, Duration::minutes(10)).unwrap();
            let agent_id = AgentInstanceId::new();
            let (first, _first_receiver, original_session) =
                ready_agent(&relay, agent_id, None).await;
            let new_boot = AgentBootId::new();
            let old_token = first.welcome.resume_token;
            let (sender, _receiver) = test_channel();
            let next = relay
                .register_agent(
                    boot_test_hello(agent_id, new_boot, Some(old_token.clone())),
                    sender,
                )
                .await
                .unwrap();
            let new_session = relay.state.lock().await.agents[&agent_id].session_id;
            assert_ne!(new_session, original_session);
            assert_ne!(next.welcome.pairing_code, first.welcome.pairing_code);
            if stage > 0 {
                assert!(
                    relay
                        .commit_agent_resume(agent_id, next.connection_generation)
                        .await
                );
            }
            if stage == 3 {
                assert!(
                    relay
                        .finalize_agent_resume(agent_id, next.connection_generation)
                        .await
                        .is_some()
                );
            }
            if restart_relay {
                drop(relay);
                relay = persisted_relay(&file.path, Duration::minutes(10)).unwrap();
            }
            let token = if stage >= 2 {
                next.welcome.resume_token.clone()
            } else {
                old_token
            };
            let (sender, _receiver) = test_channel();
            let retry = relay
                .register_agent(boot_test_hello(agent_id, new_boot, Some(token)), sender)
                .await
                .unwrap();
            assert_eq!(retry.welcome.pairing_code, next.welcome.pairing_code);
            assert_eq!(
                relay.state.lock().await.agents[&agent_id].session_id,
                new_session
            );
            assert!(retry.connection_generation > next.connection_generation);
            assert!(
                !relay
                    .commit_agent_resume(agent_id, next.connection_generation)
                    .await
            );
            assert!(
                relay
                    .finalize_agent_resume(agent_id, next.connection_generation)
                    .await
                    .is_none()
            );
            assert!(
                relay
                    .commit_agent_resume(agent_id, retry.connection_generation)
                    .await
            );
            assert_eq!(
                relay
                    .finalize_agent_resume(agent_id, retry.connection_generation)
                    .await
                    .unwrap()
                    .session_id,
                new_session
            );
        }
    }
}

#[tokio::test]
async fn boot_retired_start_cannot_reclaim_identity_with_tolerated_token_after_restart() {
    let file = TestStateFile::new("boot-retired-token");
    let relay = persisted_relay(&file.path, Duration::minutes(10)).unwrap();
    let agent_id = AgentInstanceId::new();
    let (first, _receiver, _) = ready_agent(&relay, agent_id, None).await;
    let old_token = first.welcome.resume_token;
    let new_boot = AgentBootId::new();
    let (sender, _receiver) = test_channel();
    let next = relay
        .register_agent(
            boot_test_hello(agent_id, new_boot, Some(old_token.clone())),
            sender,
        )
        .await
        .unwrap();
    assert!(
        relay
            .commit_agent_resume(agent_id, next.connection_generation)
            .await
    );
    {
        let state = relay.state.lock().await;
        assert!(
            resume_token_matches(&state.agents[&agent_id], Some(&old_token)),
            "测试必须处于旧令牌容错窗口"
        );
    }
    let (sender, _receiver) = test_channel();
    assert!(
        relay
            .register_agent(hello(agent_id, Some(old_token.clone())), sender)
            .await
            .err()
            .expect("旧启动必须被拒绝")
            .to_string()
            .contains("退役")
    );
    drop(relay);
    let relay = persisted_relay(&file.path, Duration::minutes(10)).unwrap();
    let (sender, _receiver) = test_channel();
    assert!(
        relay
            .register_agent(hello(agent_id, Some(old_token.clone())), sender)
            .await
            .err()
            .expect("旧启动必须被拒绝")
            .to_string()
            .contains("退役")
    );
    let (sender, _receiver) = test_channel();
    let retry = relay
        .register_agent(boot_test_hello(agent_id, new_boot, Some(old_token)), sender)
        .await
        .unwrap();
    assert_eq!(retry.welcome.pairing_code, next.welcome.pairing_code);
}

#[tokio::test]
async fn boot_initial_anonymous_retry_cannot_cross_a_process_boundary() {
    for committed in [false, true] {
        let file = TestStateFile::new("boot-initial-anonymous");
        let relay = persisted_relay(&file.path, Duration::minutes(10)).unwrap();
        let agent_id = AgentInstanceId::new();
        let (sender, _receiver) = test_channel();
        let first = relay
            .register_agent(hello(agent_id, None), sender)
            .await
            .unwrap();
        if committed {
            assert!(
                relay
                    .commit_agent_resume(agent_id, first.connection_generation)
                    .await
            );
        }
        drop(relay);
        let relay = persisted_relay(&file.path, Duration::minutes(10)).unwrap();
        let (sender, _receiver) = test_channel();
        assert!(
            relay
                .register_agent(boot_test_hello(agent_id, AgentBootId::new(), None), sender)
                .await
                .is_err()
        );
        let (sender, _receiver) = test_channel();
        let same_boot = relay
            .register_agent(hello(agent_id, None), sender)
            .await
            .expect("未完成登记允许同次启动重试");
        assert_eq!(same_boot.welcome.pairing_code, first.welcome.pairing_code);
    }
}

#[tokio::test]
async fn boot_legacy_state_migrates_and_first_authenticated_connection_rotates_code() {
    let file = TestStateFile::new("boot-v1-migration");
    let relay = persisted_relay(&file.path, Duration::minutes(10)).unwrap();
    let agent_id = AgentInstanceId::new();
    let (first, _receiver, old_session) = ready_agent(&relay, agent_id, None).await;
    drop(relay);
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file.path).unwrap()).unwrap();
    value["version"] = 1.into();
    let agent = value["agents"][0].as_object_mut().unwrap();
    agent.remove("legacy_boot");
    agent.remove("retired_boots");
    agent["hello"].as_object_mut().unwrap().remove("boot_id");
    std::fs::write(&file.path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let relay = persisted_relay(&file.path, Duration::minutes(10)).expect("v1 状态应迁移");
    assert!(relay.state.lock().await.agents[&agent_id].legacy_boot);
    let (sender, _receiver) = test_channel();
    let next = relay
        .register_agent(hello(agent_id, Some(first.welcome.resume_token)), sender)
        .await
        .unwrap();
    assert_ne!(next.welcome.pairing_code, first.welcome.pairing_code);
    let state = relay.state.lock().await;
    assert_ne!(state.agents[&agent_id].session_id, old_session);
    assert!(!state.agents[&agent_id].legacy_boot);
    assert!(
        !state
            .pairing_index
            .contains_key(first.welcome.pairing_code.as_ref().unwrap())
    );
}

#[tokio::test]
async fn boot_v2_state_rejects_missing_start_identifier() {
    let file = TestStateFile::new("boot-invalid-v2");
    let relay = persisted_relay(&file.path, Duration::minutes(10)).unwrap();
    let (_first, _receiver, _) = ready_agent(&relay, AgentInstanceId::new(), None).await;
    drop(relay);
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file.path).unwrap()).unwrap();
    value["agents"][0]["hello"]
        .as_object_mut()
        .unwrap()
        .remove("boot_id");
    std::fs::write(&file.path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    assert!(persisted_relay(&file.path, Duration::minutes(10)).is_err());
}

#[tokio::test]
async fn boot_pairing_expected_session_is_checked_before_any_binding() {
    let relay = relay(Duration::minutes(10));
    let agent_id = AgentInstanceId::new();
    let (first, mut agent_receiver, session_id) = ready_agent(&relay, agent_id, None).await;
    let (controller, generation, _receiver) = register_controller(&relay, ControllerKind::Ai).await;
    let result = relay
        .pair_controller(
            controller,
            generation,
            PairRequest {
                expected_agent_id: None,
                connection_resume_token: None,
                request_id: RequestId::new(),
                pairing_code: first.welcome.pairing_code,
                expected_session_id: Some(SessionId::new()),
                permission_mode: PermissionMode::FullAccess,
            },
        )
        .await;
    assert_eq!(result.error_code, Some(PairingErrorCode::SessionMismatch));
    assert!(result.connection.is_none());
    let state = relay.state.lock().await;
    assert!(!state.session_bindings.contains_key(&session_id));
    assert!(state.controllers[&controller].sessions.is_empty());
    assert!(!state.agents[&agent_id].ever_paired);
    assert!(agent_receiver.try_recv().is_err());
}

#[tokio::test]
async fn boot_pairing_distinguishes_recoverable_offline_from_ended_session() {
    let relay = relay(Duration::minutes(10));
    let agent_id = AgentInstanceId::new();
    let (first, _agent_receiver, session_id) = ready_agent(&relay, agent_id, None).await;
    let (controller, generation, _receiver) = register_controller(&relay, ControllerKind::Ai).await;
    relay
        .mark_agent_disconnected(agent_id, first.connection_generation)
        .await;
    let request = PairRequest {
        expected_agent_id: None,
        connection_resume_token: None,
        request_id: RequestId::new(),
        pairing_code: first.welcome.pairing_code.clone(),
        expected_session_id: Some(session_id),
        permission_mode: PermissionMode::FullAccess,
    };
    assert_eq!(
        relay
            .pair_controller(controller, generation, request.clone())
            .await
            .error_code,
        Some(PairingErrorCode::AgentOffline)
    );
    {
        let mut state = relay.state.lock().await;
        state.agents.get_mut(&agent_id).unwrap().lease.expires_at =
            Utc::now() - Duration::seconds(1);
    }
    assert_eq!(
        relay
            .pair_controller(controller, generation, request.clone())
            .await
            .error_code,
        Some(PairingErrorCode::AgentOffline)
    );
    let mut manual = request.clone();
    manual.expected_session_id = None;
    assert_eq!(
        relay
            .pair_controller(controller, generation, manual)
            .await
            .error_code,
        Some(PairingErrorCode::UnknownOrExpired)
    );
    let (sender, _receiver) = test_channel();
    relay
        .register_agent(
            boot_test_hello(
                agent_id,
                AgentBootId::new(),
                Some(first.welcome.resume_token),
            ),
            sender,
        )
        .await
        .unwrap();
    assert_eq!(
        relay
            .pair_controller(controller, generation, request)
            .await
            .error_code,
        Some(PairingErrorCode::SessionEnded)
    );
}

async fn start_test_pairing_agent(
    relay: &Relay,
    agent_id: AgentInstanceId,
    boot: AgentBootId,
    token: Option<String>,
    fingerprint: Option<&str>,
) -> (AgentRegistration, mpsc::Receiver<WireMessage>, SessionId) {
    let mut hello = boot_test_hello(agent_id, boot, token);
    hello.test_key_fingerprint = fingerprint.map(str::to_owned);
    hello.supports_mixed_pairing_code = true;
    let (sender, receiver) = test_channel();
    let registration = relay.register_agent(hello, sender).await.unwrap();
    assert!(
        relay
            .commit_agent_resume(agent_id, registration.connection_generation)
            .await
    );
    assert!(
        relay
            .finalize_agent_resume(agent_id, registration.connection_generation)
            .await
            .is_some()
    );
    let session_id = relay.state.lock().await.agents[&agent_id].session_id;
    (registration, receiver, session_id)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_pairing_resumes_across_agent_and_relay_restarts() {
    let state_file = TestStateFile::new("test-pairing");
    let path = &state_file.path;
    let lifetime = Duration::minutes(10);
    let relay = persisted_relay(path, lifetime).unwrap();
    let agent = AgentInstanceId::new();
    let boot = AgentBootId::new();
    let key = "a".repeat(64);
    let (first, _rx, session) =
        start_test_pairing_agent(&relay, agent, boot, None, Some(&key)).await;
    assert!(first.welcome.test_key_accepted);
    let code = first.welcome.pairing_code.clone().unwrap();
    assert!(code.is_mixed_short());
    relay
        .mark_agent_disconnected(agent, first.connection_generation)
        .await;
    let (first, _rx, reconnected_session) = start_test_pairing_agent(
        &relay,
        agent,
        boot,
        Some(first.welcome.resume_token),
        Some(&key),
    )
    .await;
    assert_eq!(first.welcome.pairing_code.as_ref(), Some(&code));
    assert_eq!(reconnected_session, session);
    let before_renew = {
        let mut state = relay.state.lock().await;
        let record = state.agents.get_mut(&agent).unwrap();
        record.last_seen -= Duration::seconds(30);
        let test = record.test_pairing.as_mut().unwrap();
        test.expires_at -= Duration::minutes(15);
        test.expires_at
    };
    assert!(
        relay
            .renew_agent(agent, first.connection_generation)
            .await
            .is_some()
    );
    assert!(
        relay.state.lock().await.agents[&agent]
            .test_pairing
            .as_ref()
            .unwrap()
            .expires_at
            > before_renew
    );
    relay
        .mark_agent_disconnected(agent, first.connection_generation)
        .await;
    let (controller, generation, _rx) = register_controller(&relay, ControllerKind::Ai).await;
    let request = PairRequest {
        request_id: RequestId::new(),
        pairing_code: Some(code.clone()),
        expected_session_id: None,
        expected_agent_id: Some(agent),
        connection_resume_token: None,
        permission_mode: PermissionMode::ApprovalRequired,
    };
    assert_eq!(
        relay
            .pair_controller(controller, generation, request.clone())
            .await
            .error_code,
        Some(PairingErrorCode::AgentOffline)
    );

    let restored = persisted_relay(path, lifetime).unwrap();
    let (next, _rx, new_session) = start_test_pairing_agent(
        &restored,
        agent,
        AgentBootId::new(),
        Some(first.welcome.resume_token),
        Some(&key),
    )
    .await;
    assert_eq!(next.welcome.pairing_code.as_ref(), Some(&code));
    assert_ne!(new_session, session);
    let (controller, generation, _rx) = register_controller(&restored, ControllerKind::Ai).await;
    let wrong = PairRequest {
        expected_agent_id: Some(AgentInstanceId::new()),
        ..request.clone()
    };
    assert!(
        restored
            .pair_controller(controller, generation, wrong)
            .await
            .connection
            .is_none()
    );
    assert!(restored.state.lock().await.session_bindings.is_empty());
    assert_eq!(
        restored
            .pair_controller(controller, generation, request.clone())
            .await
            .connection
            .unwrap()
            .session_id,
        new_session
    );

    let (normal, _rx, _) = start_test_pairing_agent(
        &restored,
        agent,
        AgentBootId::new(),
        Some(next.welcome.resume_token),
        None,
    )
    .await;
    assert!(!normal.welcome.test_key_accepted);
    assert_ne!(normal.welcome.pairing_code.as_ref(), Some(&code));
    assert!(
        restored
            .pair_controller(controller, generation, request)
            .await
            .connection
            .is_none()
    );
    {
        let state = restored.state.lock().await;
        assert_eq!(
            state.agents[&agent].test_pairing.as_ref().unwrap().code,
            code
        );
        assert!(!state.pairing_index.contains_key(&code));
    }
    let (again, _rx, _) = start_test_pairing_agent(
        &restored,
        agent,
        AgentBootId::new(),
        Some(normal.welcome.resume_token),
        Some(&key),
    )
    .await;
    assert_eq!(again.welcome.pairing_code.as_ref(), Some(&code));
    let (rotated, _rx, session) = start_test_pairing_agent(
        &restored,
        agent,
        AgentBootId::new(),
        Some(again.welcome.resume_token),
        Some(&"b".repeat(64)),
    )
    .await;
    assert_ne!(rotated.welcome.pairing_code.as_ref(), Some(&code));
    {
        let state = restored.state.lock().await;
        assert!(!state.pairing_index.contains_key(&code));
        assert_eq!(state.agents[&agent].session_id, session);
    }
}

#[tokio::test]
async fn test_pairing_expiry_releases_code_even_before_identity_expires() {
    let relay = relay(Duration::minutes(10));
    let agent = AgentInstanceId::new();
    let boot = AgentBootId::new();
    let key = "a".repeat(64);
    let (first, _rx, session) =
        start_test_pairing_agent(&relay, agent, boot, None, Some(&key)).await;
    let old = first.welcome.pairing_code.clone().unwrap();
    {
        let mut state = relay.state.lock().await;
        let deadline = state.agents[&agent]
            .test_pairing
            .as_ref()
            .unwrap()
            .expires_at;
        assert!(!reclaim_expired_test_codes(
            &mut state,
            deadline - Duration::seconds(1)
        ));
        assert!(reclaim_expired_test_codes(&mut state, deadline));
        assert!(!state.pairing_index.contains_key(&old));
        assert!(state.agents[&agent].resume_token.is_some());
    }
    let (next, _rx, new_session) = start_test_pairing_agent(
        &relay,
        agent,
        boot,
        Some(first.welcome.resume_token),
        Some(&key),
    )
    .await;
    assert!(next.welcome.pairing_code.is_some());
    assert_ne!(session, new_session);
}

#[tokio::test]
async fn concurrent_test_registrations_allocate_separate_codes() {
    let relay = relay(Duration::minutes(10));
    let key = "a".repeat(64);
    let left = start_test_pairing_agent(
        &relay,
        AgentInstanceId::new(),
        AgentBootId::new(),
        None,
        Some(&key),
    );
    let right = start_test_pairing_agent(
        &relay,
        AgentInstanceId::new(),
        AgentBootId::new(),
        None,
        Some(&key),
    );
    let ((left, _left_rx, _), (right, _right_rx, _)) = tokio::join!(left, right);
    assert_ne!(left.welcome.pairing_code, right.welcome.pairing_code);
}
