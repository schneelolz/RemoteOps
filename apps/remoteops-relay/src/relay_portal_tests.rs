// 受管生命周期在 Relay 运行态与门户持久授权之间保持同一安全边界。
mod managed_portal_tests {
    use super::*;
    use remoteops_portal::{
        CreateAssetRequest, CreatePrincipalRequest, IssueInvitationRequest, IssuedInvitation,
        PortalError,
    };
    use remoteops_protocol::ManagedAgentEnrollment;

    struct ManagedFixture {
        relay: Relay,
        principal_id: Uuid,
        token: String,
        hello: AgentHello,
        registration: AgentRegistration,
        receiver: mpsc::Receiver<WireMessage>,
    }

    impl ManagedFixture {
        async fn new() -> Self {
            let mut relay = relay(Duration::minutes(10));
            relay.portal_store = Some(Arc::new(PortalStore::in_memory(test_owner_id()).unwrap()));
            let principal = relay
                .portal_create_principal(CreatePrincipalRequest {
                    name: "portal-lifecycle-test".into(),
                    owner_id: test_owner_id(),
                    allowed_assets: vec!["asset-a".into()],
                })
                .await
                .unwrap();
            let asset = relay
                .portal_create_asset(CreateAssetRequest {
                    asset_id: "asset-a".into(),
                    principal_id: principal.principal.principal_id,
                })
                .await
                .unwrap();
            let mut hello = hello(AgentInstanceId::new(), None);
            hello.pairing_mode = PairingMode::ManagedOnDemand;
            hello.target_context = Some(remoteops_domain::TargetContext {
                product_id: "portal-test".into(),
                name: "受管测试目标".into(),
                run_mode: remoteops_domain::AgentRunMode::WindowsService,
                ..Default::default()
            });
            hello.managed_enrollment = Some(ManagedAgentEnrollment {
                target_id: asset.asset.target_id,
                token: asset.enrollment_token,
            });
            let (sender, receiver) = test_channel();
            let registration = relay.register_agent(hello.clone(), sender).await.unwrap();
            assert!(
                relay
                    .commit_agent_resume(
                        hello.agent_instance_id,
                        registration.connection_generation
                    )
                    .await
            );
            assert!(
                relay
                    .finalize_agent_resume(
                        hello.agent_instance_id,
                        registration.connection_generation
                    )
                    .await
                    .is_some()
            );
            Self {
                relay,
                principal_id: principal.principal.principal_id,
                token: principal.token,
                hello,
                registration,
                receiver,
            }
        }

        async fn issue(&self) -> IssuedInvitation {
            self.relay
                .portal_issue(&self.token, "asset-a", issue_request())
                .await
                .unwrap()
        }

        async fn reconnect_agent(&mut self, new_boot: bool) {
            self.hello.resume_token = Some(self.registration.welcome.resume_token.clone());
            if new_boot {
                self.hello.boot_id = AgentBootId::new();
            }
            self.relay
                .mark_agent_disconnected(
                    self.hello.agent_instance_id,
                    self.registration.connection_generation,
                )
                .await;
            let (sender, receiver) = test_channel();
            self.registration = self
                .relay
                .register_agent(self.hello.clone(), sender)
                .await
                .unwrap();
            assert!(
                self.relay
                    .commit_agent_resume(
                        self.hello.agent_instance_id,
                        self.registration.connection_generation
                    )
                    .await
            );
            assert!(
                self.relay
                    .finalize_agent_resume(
                        self.hello.agent_instance_id,
                        self.registration.connection_generation
                    )
                    .await
                    .is_some()
            );
            self.receiver = receiver;
        }
    }

    fn issue_request() -> IssueInvitationRequest {
        IssueInvitationRequest {
            idempotency_key: Uuid::new_v4(),
            replace_active: false,
            expected_revision: None,
        }
    }

    fn first_pair(code: &str) -> PairRequest {
        PairRequest {
            request_id: RequestId::new(),
            pairing_code: Some(PairingCode::parse(code).unwrap()),
            expected_session_id: None,
            permission_mode: PermissionMode::ApprovalRequired,
            connection_resume_token: None,
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn managed_bootstrap_recovers_only_unfinished_authenticated_registration() {
        for commit_before_crash in [false, true] {
            let fixture = ManagedFixture::new().await;
            // 复用已登记资产，重建第一次 Hello 尚未完成的 Relay 记录。
            fixture.relay.state.lock().await.agents.clear();
            let (sender, _receiver) = test_channel();
            let first = fixture
                .relay
                .register_agent(fixture.hello.clone(), sender)
                .await
                .unwrap();
            if commit_before_crash {
                assert!(
                    fixture
                        .relay
                        .commit_agent_resume(
                            fixture.hello.agent_instance_id,
                            first.connection_generation
                        )
                        .await
                );
            }
            let mut restarted = fixture.hello.clone();
            restarted.boot_id = AgentBootId::new();
            let mut wrong_proof = restarted.clone();
            wrong_proof.managed_enrollment.as_mut().unwrap().token = "invalid".into();
            let (sender, _receiver) = test_channel();
            assert!(
                fixture
                    .relay
                    .register_agent(wrong_proof, sender)
                    .await
                    .is_err()
            );
            let (sender, _receiver) = test_channel();
            let recovered = fixture
                .relay
                .register_agent(restarted.clone(), sender)
                .await
                .unwrap();
            assert!(recovered.welcome.pairing_code.is_none());
            assert_ne!(first.welcome.resume_token, recovered.welcome.resume_token);
            let (sender, _receiver) = test_channel();
            assert!(
                fixture
                    .relay
                    .register_agent(fixture.hello.clone(), sender)
                    .await
                    .is_err(),
                "退役启动不能夺回登记"
            );
            assert!(
                !fixture
                    .relay
                    .commit_agent_resume(
                        fixture.hello.agent_instance_id,
                        first.connection_generation
                    )
                    .await
            );
            assert!(
                fixture
                    .relay
                    .commit_agent_resume(
                        fixture.hello.agent_instance_id,
                        recovered.connection_generation
                    )
                    .await
            );
            assert!(
                fixture
                    .relay
                    .finalize_agent_resume(
                        fixture.hello.agent_instance_id,
                        recovered.connection_generation
                    )
                    .await
                    .is_some()
            );
            fixture
                .relay
                .mark_agent_disconnected(
                    fixture.hello.agent_instance_id,
                    recovered.connection_generation,
                )
                .await;
            restarted.boot_id = AgentBootId::new();
            let (sender, _receiver) = test_channel();
            assert!(
                fixture
                    .relay
                    .register_agent(restarted.clone(), sender)
                    .await
                    .is_err(),
                "完成登记后不能仅凭部署证明恢复"
            );
            restarted.resume_token = Some(recovered.welcome.resume_token);
            let (sender, _receiver) = test_channel();
            assert!(
                fixture
                    .relay
                    .register_agent(restarted, sender)
                    .await
                    .is_ok()
            );
        }
    }

    fn resume_pair(session: SessionId, token: &str) -> PairRequest {
        PairRequest {
            request_id: RequestId::new(),
            pairing_code: None,
            expected_session_id: Some(session),
            permission_mode: PermissionMode::ApprovalRequired,
            connection_resume_token: Some(token.into()),
        }
    }

    #[tokio::test]
    async fn managed_final_registration_requires_durable_confirmation() {
        let mut fixture = ManagedFixture::new().await;
        fixture.relay.state.lock().await.agents.clear();
        let (sender, _receiver) = test_channel();
        let registration = fixture
            .relay
            .register_agent(fixture.hello.clone(), sender)
            .await
            .unwrap();
        let id = fixture.hello.agent_instance_id;
        let generation = registration.connection_generation;
        assert!(fixture.relay.commit_agent_resume(id, generation).await);
        let state_file = TestStateFile::new("managed-final-confirmation");
        std::fs::create_dir_all(&state_file.root).unwrap();
        let blocker = state_file.root.join("not-a-directory");
        std::fs::write(&blocker, b"block").unwrap();
        fixture.relay.state_path = Some(blocker.join("state.json"));
        assert!(
            fixture
                .relay
                .finalize_agent_resume(id, generation)
                .await
                .is_none()
        );
        assert!(!fixture.relay.state.lock().await.agents[&id].ready);
        assert!(
            fixture
                .relay
                .portal_issue(&fixture.token, "asset-a", issue_request())
                .await
                .is_err()
        );
        fixture.relay.state_path = Some(state_file.path.clone());
        let mut hello = fixture.hello.clone();
        hello.resume_token = Some(registration.welcome.resume_token);
        let (sender, _receiver) = test_channel();
        let retry = fixture
            .relay
            .register_agent(hello.clone(), sender)
            .await
            .unwrap();
        assert!(
            fixture
                .relay
                .commit_agent_resume(id, retry.connection_generation)
                .await
        );
        assert!(
            fixture
                .relay
                .finalize_agent_resume(id, retry.connection_generation)
                .await
                .is_some()
        );
        fixture
            .relay
            .mark_agent_disconnected(id, retry.connection_generation)
            .await;
        hello.boot_id = AgentBootId::new();
        hello.resume_token = None;
        let (sender, _receiver) = test_channel();
        assert!(fixture.relay.register_agent(hello, sender).await.is_err());
    }

    #[tokio::test]
    async fn managed_same_boot_retry_reconciles_previous_boot_portal_grant() {
        let fixture = ManagedFixture::new().await;
        let issued = fixture.issue().await;
        let (controller, generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        let paired = fixture
            .relay
            .pair_controller(controller, generation, first_pair(&issued.code))
            .await;
        assert!(paired.error.is_none());
        let old_session = paired.connection.unwrap().session_id;
        let old_token = paired.connection_resume_token.unwrap();
        let mut retry = fixture.hello.clone();
        retry.boot_id = AgentBootId::new();
        retry.resume_token = Some(fixture.registration.welcome.resume_token.clone());
        {
            let mut state = fixture.relay.state.lock().await;
            // 模拟新 boot JSON 已提交，但门户清理因存储故障中断的恢复状态。
            end_session(&mut state, old_session, "模拟跨存储中断");
            let agent = state.agents.get_mut(&retry.agent_instance_id).unwrap();
            agent.hello.boot_id = retry.boot_id;
            agent.session_id = SessionId::new();
            agent.ready = false;
        }
        let (sender, _receiver) = test_channel();
        assert!(fixture.relay.register_agent(retry, sender).await.is_ok());
        let status = fixture
            .relay
            .portal_status(&fixture.token, "asset-a")
            .await
            .unwrap();
        assert!(status["connection"].is_null());
        assert!(
            fixture
                .relay
                .portal_store
                .as_ref()
                .unwrap()
                .connection_target(&old_token)
                .is_err()
        );
    }

    #[tokio::test]
    async fn managed_waits_without_code_and_rejects_mode_downgrade_and_old_agent() {
        let fixture = ManagedFixture::new().await;
        assert!(fixture.registration.welcome.pairing_code.is_none());
        assert!(fixture.relay.state.lock().await.pairing_index.is_empty());
        let status = fixture
            .relay
            .portal_status(&fixture.token, "asset-a")
            .await
            .unwrap();
        assert_eq!(status["online"], true);
        assert!(status["invitation"].is_null());
        let mut downgraded = fixture.hello.clone();
        downgraded.resume_token = Some(fixture.registration.welcome.resume_token.clone());
        downgraded.pairing_mode = PairingMode::AutoPerBoot;
        downgraded.managed_enrollment = None;
        let (sender, _receiver) = test_channel();
        assert!(
            fixture
                .relay
                .register_agent(downgraded, sender)
                .await
                .is_err()
        );
        let mut legacy = fixture.hello.clone();
        legacy.protocol_version = remoteops_protocol::LEGACY_PROTOCOL_VERSION;
        let (sender, _receiver) = test_channel();
        assert!(fixture.relay.register_agent(legacy, sender).await.is_err());
        assert!(fixture.relay.state.lock().await.pairing_index.is_empty());
    }

    #[tokio::test]
    async fn managed_issue_is_idempotent_and_legacy_controller_cannot_consume() {
        let fixture = ManagedFixture::new().await;
        let request = issue_request();
        let first = fixture
            .relay
            .portal_issue(&fixture.token, "asset-a", request)
            .await
            .unwrap();
        let retry = fixture
            .relay
            .portal_issue(&fixture.token, "asset-a", request)
            .await
            .unwrap();
        assert!(first.code == retry.code, "同一请求必须返回相同码而不回显它");
        assert_eq!(
            first.invitation.target.session_id,
            retry.invitation.target.session_id
        );
        assert_eq!(first.invitation.expires_at, retry.invitation.expires_at);
        let (controller, generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        fixture
            .relay
            .state
            .lock()
            .await
            .controllers
            .get_mut(&controller)
            .unwrap()
            .protocol_version = remoteops_protocol::LEGACY_PROTOCOL_VERSION;
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, first_pair(&first.code))
                .await
                .error
                .is_some()
        );
        let status = fixture
            .relay
            .portal_store
            .as_ref()
            .unwrap()
            .status(fixture.principal_id, "asset-a", Utc::now())
            .unwrap();
        assert_eq!(status.invitation.unwrap().state, "issued");
        fixture
            .relay
            .state
            .lock()
            .await
            .controllers
            .get_mut(&controller)
            .unwrap()
            .protocol_version = PROTOCOL_VERSION;
        let pair_request = first_pair(&first.code);
        let result = fixture
            .relay
            .pair_controller(controller, generation, pair_request.clone())
            .await;
        assert!(result.error.is_none());
        let retry = fixture
            .relay
            .pair_controller(controller, generation, pair_request)
            .await;
        assert!(retry.error.is_none());
        assert!(
            result.connection_resume_token == retry.connection_resume_token,
            "响应遗失重试返回原恢复凭据"
        );
        let (other, other_generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        assert!(
            fixture
                .relay
                .pair_controller(other, other_generation, first_pair(&first.code))
                .await
                .error
                .is_some()
        );
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, first_pair(&first.code))
                .await
                .error
                .is_some()
        );
    }

    #[tokio::test]
    async fn managed_expired_invitation_is_not_consumed() {
        let fixture = ManagedFixture::new().await;
        let mut state = fixture.relay.state.lock().await;
        let agent = state
            .agents
            .get_mut(&fixture.hello.agent_instance_id)
            .unwrap();
        let issued = fixture
            .relay
            .portal_store
            .as_ref()
            .unwrap()
            .issue(
                fixture.principal_id,
                "asset-a",
                portal_access::target(agent),
                issue_request(),
                &[],
                Utc::now() - Duration::minutes(11),
            )
            .unwrap();
        agent.session_id = issued.invitation.target.session_id;
        drop(state);
        let (controller, generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        let result = fixture
            .relay
            .pair_controller(controller, generation, first_pair(&issued.code))
            .await;
        assert_eq!(result.error_code, Some(PairingErrorCode::InvitationExpired));
        let status = fixture
            .relay
            .portal_store
            .as_ref()
            .unwrap()
            .status(fixture.principal_id, "asset-a", Utc::now())
            .unwrap();
        assert_eq!(status.invitation.unwrap().state, "expired");
        assert!(status.connection.is_none());
    }

    #[tokio::test]
    async fn managed_same_boot_restores_but_new_boot_revokes_old_authority() {
        let mut fixture = ManagedFixture::new().await;
        let issued = fixture.issue().await;
        let session = issued.invitation.target.session_id;
        let (controller, generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        let first = fixture
            .relay
            .pair_controller(controller, generation, first_pair(&issued.code))
            .await;
        assert!(first.error.is_none());
        let token = first.connection_resume_token.unwrap();
        fixture.reconnect_agent(false).await;
        assert!(fixture.registration.welcome.pairing_code.is_none());
        let restored = fixture
            .relay
            .pair_controller(controller, generation, resume_pair(session, &token))
            .await;
        assert!(restored.error.is_none());
        assert_eq!(restored.connection.unwrap().session_id, session);
        fixture.reconnect_agent(true).await;
        assert!(fixture.registration.welcome.pairing_code.is_none());
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, resume_pair(session, &token))
                .await
                .error
                .is_some()
        );
        assert!(
            !fixture
                .relay
                .state
                .lock()
                .await
                .session_bindings
                .contains_key(&session)
        );
        assert!(
            fixture
                .relay
                .portal_store
                .as_ref()
                .unwrap()
                .connection_target(&token)
                .is_err()
        );
        let next = fixture.issue().await;
        assert_ne!(next.invitation.target.session_id, session);
    }

    #[tokio::test]
    async fn managed_portal_close_and_principal_revoke_end_runtime_and_resume() {
        for revoke in [false, true] {
            let fixture = ManagedFixture::new().await;
            let issued = fixture.issue().await;
            let session = issued.invitation.target.session_id;
            let (controller, generation, _receiver) =
                register_controller(&fixture.relay, ControllerKind::Ai).await;
            let paired = fixture
                .relay
                .pair_controller(controller, generation, first_pair(&issued.code))
                .await;
            assert!(paired.error.is_none());
            let token = paired.connection_resume_token.unwrap();
            let status = fixture
                .relay
                .portal_store
                .as_ref()
                .unwrap()
                .status(fixture.principal_id, "asset-a", Utc::now())
                .unwrap();
            let connection = status.connection.unwrap();
            if revoke {
                fixture
                    .relay
                    .portal_revoke_principal(fixture.principal_id)
                    .await
                    .unwrap();
            } else {
                fixture
                    .relay
                    .portal_close(
                        &fixture.token,
                        connection.connection_id,
                        Some(connection.revision),
                    )
                    .await
                    .unwrap();
                fixture
                    .relay
                    .portal_close(
                        &fixture.token,
                        connection.connection_id,
                        Some(connection.revision),
                    )
                    .await
                    .unwrap();
            }
            assert!(
                fixture
                    .relay
                    .pair_controller(controller, generation, resume_pair(session, &token))
                    .await
                    .error
                    .is_some()
            );
            assert!(
                !fixture
                    .relay
                    .state
                    .lock()
                    .await
                    .session_bindings
                    .contains_key(&session)
            );
            assert!(
                fixture
                    .relay
                    .portal_store
                    .as_ref()
                    .unwrap()
                    .connection_target(&token)
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn managed_release_closes_persistent_authority_and_resume() {
        let fixture = ManagedFixture::new().await;
        let issued = fixture.issue().await;
        let session = issued.invitation.target.session_id;
        let (controller, generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        let paired = fixture
            .relay
            .pair_controller(controller, generation, first_pair(&issued.code))
            .await;
        assert!(paired.error.is_none());
        let token = paired.connection_resume_token.unwrap();
        let result = fixture
            .relay
            .release_controller_session(
                controller,
                generation,
                ReleaseSessionRequest {
                    request_id: RequestId::new(),
                    session_id: session,
                },
            )
            .await;
        assert!(result.released);
        assert!(result.error.is_none());
        assert!(
            fixture
                .relay
                .portal_store
                .as_ref()
                .unwrap()
                .connection_target(&token)
                .is_err()
        );
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, resume_pair(session, &token))
                .await
                .error
                .is_some()
        );
        assert!(
            !fixture
                .relay
                .state
                .lock()
                .await
                .session_bindings
                .contains_key(&session)
        );
        assert!(
            fixture
                .relay
                .portal_store
                .as_ref()
                .unwrap()
                .status(fixture.principal_id, "asset-a", Utc::now())
                .unwrap()
                .connection
                .is_none()
        );
    }

    #[tokio::test]
    async fn managed_json_write_failure_is_closed_and_same_key_can_recover() {
        let mut fixture = ManagedFixture::new().await;
        let state_file = TestStateFile::new("managed-issue-recovery");
        std::fs::create_dir_all(&state_file.root).unwrap();
        let blocker = state_file.root.join("not-a-directory");
        std::fs::write(&blocker, b"block parent creation").unwrap();
        fixture.relay.state_path = Some(blocker.join("relay-state.json"));
        let request = issue_request();
        assert!(matches!(
            fixture
                .relay
                .portal_issue(&fixture.token, "asset-a", request)
                .await,
            Err(PortalError::Storage)
        ));
        let status = fixture
            .relay
            .portal_store
            .as_ref()
            .unwrap()
            .status(fixture.principal_id, "asset-a", Utc::now())
            .unwrap();
        let invitation = status.invitation.unwrap();
        let stored = fixture
            .relay
            .portal_store
            .as_ref()
            .unwrap()
            .issue(
                fixture.principal_id,
                "asset-a",
                invitation.target,
                request,
                &[],
                Utc::now(),
            )
            .unwrap();
        let (controller, generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, first_pair(&stored.code))
                .await
                .error
                .is_some()
        );
        fixture.relay.state_path = Some(state_file.path.clone());
        let recovered = fixture
            .relay
            .portal_issue(&fixture.token, "asset-a", request)
            .await
            .unwrap();
        assert_eq!(recovered.invitation.invitation_id, invitation.invitation_id);
        assert_eq!(
            recovered.invitation.target.session_id,
            invitation.target.session_id
        );
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, first_pair(&recovered.code))
                .await
                .error
                .is_none()
        );
    }

    #[tokio::test]
    async fn managed_failed_guess_budget_blocks_even_a_valid_invitation() {
        let fixture = ManagedFixture::new().await;
        let issued = fixture.issue().await;
        let (controller, generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        let wrong = if issued.code == "000000000" {
            "999999999"
        } else {
            "000000000"
        };
        for _ in 0..60 {
            assert!(
                fixture
                    .relay
                    .pair_controller(controller, generation, first_pair(wrong))
                    .await
                    .error
                    .is_some()
            );
        }
        let result = fixture
            .relay
            .pair_controller(controller, generation, first_pair(&issued.code))
            .await;
        assert!(result.error.is_some());
        let status = fixture
            .relay
            .portal_store
            .as_ref()
            .unwrap()
            .status(fixture.principal_id, "asset-a", Utc::now())
            .unwrap();
        assert_eq!(status.invitation.unwrap().state, "issued");
        assert!(status.connection.is_none());
    }

    #[tokio::test]
    async fn managed_controller_transport_recovers_only_with_same_consumer() {
        let fixture = ManagedFixture::new().await;
        let issued = fixture.issue().await;
        let session = issued.invitation.target.session_id;
        let (controller, generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        let paired = fixture
            .relay
            .pair_controller(controller, generation, first_pair(&issued.code))
            .await;
        assert!(paired.error.is_none());
        let token = paired.connection_resume_token.unwrap();
        fixture
            .relay
            .remove_controller(controller, generation)
            .await;
        let (sender, _receiver) = test_channel();
        let new_generation = fixture
            .relay
            .register_controller(
                controller,
                test_owner_id(),
                ControllerKind::Ai,
                None,
                None,
                sender,
            )
            .await
            .unwrap();
        assert_ne!(generation, new_generation);
        assert!(
            fixture
                .relay
                .pair_controller(controller, new_generation, resume_pair(session, &token))
                .await
                .error
                .is_none()
        );
        fixture
            .relay
            .state
            .lock()
            .await
            .controllers
            .get_mut(&controller)
            .unwrap()
            .installation_id = Some(Uuid::new_v4());
        assert!(
            fixture
                .relay
                .pair_controller(controller, new_generation, resume_pair(session, &token))
                .await
                .error
                .is_some()
        );
        fixture
            .relay
            .state
            .lock()
            .await
            .controllers
            .get_mut(&controller)
            .unwrap()
            .installation_id = None;
        let (other, other_generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        assert!(
            fixture
                .relay
                .pair_controller(other, other_generation, resume_pair(session, &token))
                .await
                .error
                .is_some()
        );
        assert!(
            fixture
                .relay
                .pair_controller(controller, new_generation, resume_pair(session, &token))
                .await
                .error
                .is_none()
        );
    }

    #[tokio::test]
    async fn managed_replace_requires_revision_and_revokes_agent_binding() {
        let mut fixture = ManagedFixture::new().await;
        let issued = fixture.issue().await;
        let session = issued.invitation.target.session_id;
        let (controller, generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        let paired = fixture
            .relay
            .pair_controller(controller, generation, first_pair(&issued.code))
            .await;
        assert!(paired.error.is_none());
        let token = paired.connection_resume_token.unwrap();
        while fixture.receiver.try_recv().is_ok() {}
        assert!(matches!(
            fixture
                .relay
                .portal_issue(&fixture.token, "asset-a", issue_request())
                .await,
            Err(PortalError::Conflict)
        ));
        let status = fixture
            .relay
            .portal_store
            .as_ref()
            .unwrap()
            .status(fixture.principal_id, "asset-a", Utc::now())
            .unwrap();
        let mut request = IssueInvitationRequest {
            replace_active: true,
            expected_revision: Some(status.asset.revision + 1),
            ..issue_request()
        };
        assert!(matches!(
            fixture
                .relay
                .portal_issue(&fixture.token, "asset-a", request)
                .await,
            Err(PortalError::Conflict)
        ));
        request.expected_revision = Some(status.asset.revision);
        let replacement = fixture
            .relay
            .portal_issue(&fixture.token, "asset-a", request)
            .await
            .unwrap();
        assert_ne!(replacement.invitation.target.session_id, session);
        assert!(replacement.replaced_connection.is_some());
        let mut notified = false;
        while let Ok(message) = fixture.receiver.try_recv() {
            if matches!(message, WireMessage::ControllerBindingRevoked {session_id, ..} if session_id == session)
            {
                notified = true;
            }
        }
        assert!(notified, "替换必须通知 Agent 撤销原绑定");
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, resume_pair(session, &token))
                .await
                .error
                .is_some()
        );
        assert!(
            !fixture
                .relay
                .state
                .lock()
                .await
                .session_bindings
                .contains_key(&session)
        );
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, first_pair(&replacement.code))
                .await
                .error
                .is_none()
        );
    }

    struct InFlightManagedTarget {
        agent: AgentInstanceId,
        session: SessionId,
        request: RequestId,
        receiver: mpsc::Receiver<WireMessage>,
    }

    async fn prepare_managed_in_flight(
        relay: &Relay,
        principal_id: Uuid,
        token: &str,
        asset_id: &str,
        controller: ControllerInstanceId,
        generation: u64,
    ) -> InFlightManagedTarget {
        let asset = relay
            .portal_create_asset(CreateAssetRequest {
                asset_id: asset_id.into(),
                principal_id,
            })
            .await
            .unwrap();
        let mut hello = hello(AgentInstanceId::new(), None);
        hello.pairing_mode = PairingMode::ManagedOnDemand;
        hello.target_context = Some(remoteops_domain::TargetContext::default());
        hello.managed_enrollment = Some(ManagedAgentEnrollment {
            target_id: asset.asset.target_id,
            token: asset.enrollment_token,
        });
        let agent = hello.agent_instance_id;
        let (sender, mut receiver) = test_channel();
        let registration = relay.register_agent(hello, sender).await.unwrap();
        assert!(
            relay
                .commit_agent_resume(agent, registration.connection_generation)
                .await
        );
        assert!(
            relay
                .finalize_agent_resume(agent, registration.connection_generation)
                .await
                .is_some()
        );
        let issued = relay
            .portal_issue(token, asset_id, issue_request())
            .await
            .unwrap();
        let session = issued.invitation.target.session_id;
        assert!(
            relay
                .pair_controller(controller, generation, first_pair(&issued.code))
                .await
                .error
                .is_none()
        );
        let request = RequestId::new();
        let sender = controller_sender(relay, controller).await;
        relay
            .forward_controller_request(
                controller,
                generation,
                RemoteRequest {
                    request_id: request,
                    session_id: session,
                    source: EventSource::Ai,
                    operation: RemoteOperation::GetTargetContext,
                    approval_id: None,
                    payload_base64: None,
                    control_proof: None,
                },
                &sender,
            )
            .await;
        assert!(relay.state.lock().await.in_flight.contains_key(&request));
        let mut forwarded = false;
        while let Ok(message) = receiver.try_recv() {
            if matches!(message, WireMessage::AuthorizedRemoteRequest(value) if value.request.request_id == request && value.request.operation == RemoteOperation::GetTargetContext)
            {
                forwarded = true;
            }
        }
        assert!(forwarded, "测试必须真实转发只读请求，并保留在途状态");
        InFlightManagedTarget {
            agent,
            session,
            request,
            receiver,
        }
    }

    #[tokio::test]
    async fn managed_principal_revoke_cleans_every_target_before_json_failure_returns() {
        let mut relay = relay(Duration::minutes(10));
        relay.portal_store = Some(Arc::new(PortalStore::in_memory(test_owner_id()).unwrap()));
        let principal = relay
            .portal_create_principal(CreatePrincipalRequest {
                name: "multi-target-revoke".into(),
                owner_id: test_owner_id(),
                allowed_assets: vec!["asset-a".into(), "asset-b".into()],
            })
            .await
            .unwrap();
        let (controller, generation, _controller_receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        let mut targets = Vec::new();
        for asset_id in ["asset-a", "asset-b"] {
            targets.push(
                prepare_managed_in_flight(
                    &relay,
                    principal.principal.principal_id,
                    &principal.token,
                    asset_id,
                    controller,
                    generation,
                )
                .await,
            );
        }
        {
            let state = relay.state.lock().await;
            assert_eq!(state.session_bindings.len(), 2);
            assert_eq!(state.in_flight.len(), 2);
        }
        let state_file = TestStateFile::new("managed-multi-revoke-failure");
        std::fs::create_dir_all(&state_file.root).unwrap();
        let blocker = state_file.root.join("not-a-directory");
        std::fs::write(&blocker, b"block parent creation").unwrap();
        relay.state_path = Some(blocker.join("relay-state.json"));
        assert!(matches!(
            relay
                .portal_revoke_principal(principal.principal.principal_id)
                .await,
            Err(PortalError::Storage)
        ));
        {
            let state = relay.state.lock().await;
            assert!(state.session_bindings.is_empty());
            assert!(state.in_flight.is_empty());
            assert!(state.controllers[&controller].sessions.is_empty());
            for target in &targets {
                assert!(!state.in_flight.contains_key(&target.request));
                assert_ne!(state.agents[&target.agent].session_id, target.session);
            }
        }
        assert!(
            relay
                .portal_store
                .as_ref()
                .unwrap()
                .authenticate_principal(&principal.token)
                .is_err()
        );
        for target in &mut targets {
            let mut revoked = false;
            while let Ok(message) = target.receiver.try_recv() {
                if matches!(message, WireMessage::ControllerBindingRevoked {session_id, ..} if session_id == target.session)
                {
                    revoked = true;
                }
            }
            assert!(
                revoked,
                "每一个受管目标都必须收到旧绑定撤销，不能被首个落盘错误中断"
            );
        }
    }
}
