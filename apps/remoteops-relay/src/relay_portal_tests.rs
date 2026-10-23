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
    struct AdminManagedTarget {
        hello: AgentHello,
        target_id: String,
        agent_resume_token: String,
        session: SessionId,
        request: RequestId,
        code: String,
        connection_resume_token: String,
        receiver: mpsc::Receiver<WireMessage>,
    }

    async fn prepare_admin_managed_target(
        relay: &Relay,
        principal_id: Uuid,
        token: &str,
        asset_id: &str,
        controller: ControllerInstanceId,
        generation: u64,
    ) -> AdminManagedTarget {
        let asset = relay
            .portal_create_asset(CreateAssetRequest {
                principal_id,
                asset_id: asset_id.into(),
            })
            .await
            .unwrap();
        let target_id = asset.asset.target_id;
        let mut hello = hello(AgentInstanceId::new(), None);
        hello.pairing_mode = PairingMode::ManagedOnDemand;
        hello.target_context = Some(remoteops_domain::TargetContext::default());
        hello.managed_enrollment = Some(ManagedAgentEnrollment {
            target_id: target_id.clone(),
            token: asset.enrollment_token,
        });
        let (sender, mut receiver) = test_channel();
        let registration = relay.register_agent(hello.clone(), sender).await.unwrap();
        assert!(
            relay
                .commit_agent_resume(hello.agent_instance_id, registration.connection_generation)
                .await
        );
        assert!(
            relay
                .finalize_agent_resume(hello.agent_instance_id, registration.connection_generation)
                .await
                .is_some()
        );
        let issued = relay
            .portal_issue(token, asset_id, issue_request())
            .await
            .unwrap();
        let session = issued.invitation.target.session_id;
        let paired = relay
            .pair_controller(controller, generation, first_pair(&issued.code))
            .await;
        assert!(paired.error.is_none());
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
        while receiver.try_recv().is_ok() {}
        AdminManagedTarget {
            hello,
            target_id,
            agent_resume_token: registration.welcome.resume_token,
            session,
            request,
            code: issued.code,
            connection_resume_token: paired.connection_resume_token.unwrap(),
            receiver,
        }
    }

    async fn admin_test_principal(
        relay: &Relay,
        name: &str,
        assets: &[&str],
    ) -> remoteops_portal::CreatedPrincipal {
        relay
            .portal_create_principal(CreatePrincipalRequest {
                name: name.into(),
                owner_id: test_owner_id(),
                allowed_assets: assets.iter().map(|asset| (*asset).into()).collect(),
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn managed_admin_transfer_preserves_other_target_and_agent_identity() {
        use remoteops_portal::{TransferAssetRequest, UpdatePrincipalAssetsRequest};
        let mut relay = relay(Duration::minutes(10));
        relay.portal_store = Some(Arc::new(PortalStore::in_memory(test_owner_id()).unwrap()));
        let destination = admin_test_principal(&relay, "center", &["windows"]).await;
        let source = admin_test_principal(&relay, "linux-test", &["linux"]).await;
        let (controller, generation, _receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        let mut windows = prepare_admin_managed_target(
            &relay,
            destination.principal.principal_id,
            &destination.token,
            "windows",
            controller,
            generation,
        )
        .await;
        let mut linux = prepare_admin_managed_target(
            &relay,
            source.principal.principal_id,
            &source.token,
            "linux",
            controller,
            generation,
        )
        .await;
        let updated = relay
            .portal_update_principal_assets(
                destination.principal.principal_id,
                UpdatePrincipalAssetsRequest {
                    expected_revision: destination.principal.revision,
                    idempotency_key: Uuid::new_v4(),
                    add: vec!["linux".into()],
                    remove: vec![],
                },
                "runtime-test",
            )
            .await
            .unwrap();
        assert_eq!(updated.state, "completed");
        {
            let state = relay.state.lock().await;
            assert!(state.session_bindings.contains_key(&windows.session));
            assert!(state.session_bindings.contains_key(&linux.session));
        }
        let before = relay.portal_get_asset(&linux.target_id).await.unwrap();
        let request = TransferAssetRequest {
            expected_principal_id: source.principal.principal_id,
            destination_principal_id: destination.principal.principal_id,
            expected_revision: before.revision,
            idempotency_key: Uuid::new_v4(),
        };
        let operation = relay
            .portal_transfer_asset(&linux.target_id, request.clone(), "runtime-test")
            .await
            .unwrap();
        assert_eq!(operation.state, "completed");
        let after = relay.portal_get_asset(&linux.target_id).await.unwrap();
        assert_eq!(after.target_id, before.target_id);
        assert_eq!(after.asset_id, before.asset_id);
        assert_eq!(after.agent_id, before.agent_id);
        assert_eq!(after.principal_id, destination.principal.principal_id);
        {
            let state = relay.state.lock().await;
            let agent = &state.agents[&linux.hello.agent_instance_id];
            assert_eq!(
                agent.managed_target_id.as_deref(),
                Some(linux.target_id.as_str())
            );
            assert_eq!(
                agent.resume_token.as_deref(),
                Some(linux.agent_resume_token.as_str())
            );
            assert_eq!(agent.hello.boot_id, linux.hello.boot_id);
            assert!(agent.ready && agent.sender.is_some());
            assert!(!state.session_bindings.contains_key(&linux.session));
            assert!(!state.in_flight.contains_key(&linux.request));
            assert!(state.session_bindings.contains_key(&windows.session));
            assert!(state.in_flight.contains_key(&windows.request));
            assert!(
                state.controllers[&controller]
                    .sessions
                    .contains(&windows.session)
            );
        }
        assert!(matches!(
            windows.receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let mut revoked = false;
        while let Ok(message) = linux.receiver.try_recv() {
            revoked |= matches!(message, WireMessage::ControllerBindingRevoked { session_id, .. }
                if session_id == linux.session);
        }
        assert!(revoked);
        assert!(relay.portal_status(&source.token, "linux").await.is_err());
        assert!(
            relay
                .portal_issue(&source.token, "linux", issue_request())
                .await
                .is_err()
        );
        assert!(
            relay
                .portal_status(&destination.token, "windows")
                .await
                .is_ok()
        );
        assert!(
            relay
                .portal_status(&destination.token, "linux")
                .await
                .is_ok()
        );
        assert!(
            relay
                .pair_controller(controller, generation, first_pair(&linux.code))
                .await
                .error
                .is_some()
        );
        assert!(
            relay
                .pair_controller(
                    controller,
                    generation,
                    resume_pair(linux.session, &linux.connection_resume_token)
                )
                .await
                .error
                .is_some()
        );

        // 原登记证明与恢复令牌继续恢复同一 Agent，迁移不要求重新部署。
        linux.hello.resume_token = Some(linux.agent_resume_token.clone());
        let (sender, receiver) = test_channel();
        let resumed = relay
            .register_agent(linux.hello.clone(), sender)
            .await
            .unwrap();
        assert_eq!(
            resumed.welcome.managed_target_id.as_deref(),
            Some(linux.target_id.as_str())
        );
        assert!(
            relay
                .commit_agent_resume(linux.hello.agent_instance_id, resumed.connection_generation)
                .await
        );
        assert!(
            relay
                .finalize_agent_resume(linux.hello.agent_instance_id, resumed.connection_generation)
                .await
                .is_some()
        );
        linux.receiver = receiver;
        let issued = relay
            .portal_issue(&destination.token, "linux", issue_request())
            .await
            .unwrap();
        let paired = relay
            .pair_controller(controller, generation, first_pair(&issued.code))
            .await;
        assert!(paired.error.is_none());
        let new_session = issued.invitation.target.session_id;
        let replayed = relay
            .portal_transfer_asset(&linux.target_id, request, "runtime-test")
            .await
            .unwrap();
        assert_eq!(replayed.operation_id, operation.operation_id);
        let state = relay.state.lock().await;
        assert!(state.session_bindings.contains_key(&new_session));
        assert!(state.session_bindings.contains_key(&windows.session));
    }

    #[tokio::test]
    async fn managed_admin_acl_changes_only_end_removed_targets() {
        use remoteops_portal::UpdatePrincipalAssetsRequest;
        let mut relay = relay(Duration::minutes(10));
        relay.portal_store = Some(Arc::new(PortalStore::in_memory(test_owner_id()).unwrap()));
        let principal = admin_test_principal(&relay, "center", &["windows", "linux"]).await;
        let id = principal.principal.principal_id;
        let (controller, generation, _receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        let windows = prepare_admin_managed_target(
            &relay,
            id,
            &principal.token,
            "windows",
            controller,
            generation,
        )
        .await;
        let linux = prepare_admin_managed_target(
            &relay,
            id,
            &principal.token,
            "linux",
            controller,
            generation,
        )
        .await;
        let operation = relay
            .portal_update_principal_assets(
                id,
                UpdatePrincipalAssetsRequest {
                    expected_revision: principal.principal.revision,
                    idempotency_key: Uuid::new_v4(),
                    add: vec!["future".into()],
                    remove: vec!["linux".into()],
                },
                "runtime-test",
            )
            .await
            .unwrap();
        assert_eq!(operation.state, "completed");
        {
            let state = relay.state.lock().await;
            assert!(state.session_bindings.contains_key(&windows.session));
            assert!(state.in_flight.contains_key(&windows.request));
            assert!(!state.session_bindings.contains_key(&linux.session));
            assert!(!state.in_flight.contains_key(&linux.request));
            assert_eq!(
                state.agents[&linux.hello.agent_instance_id]
                    .resume_token
                    .as_deref(),
                Some(linux.agent_resume_token.as_str())
            );
        }
        assert!(
            relay
                .portal_status(&principal.token, "windows")
                .await
                .is_ok()
        );
        assert!(
            relay
                .portal_issue(&principal.token, "linux", issue_request())
                .await
                .is_err()
        );
        assert!(
            relay
                .pair_controller(controller, generation, first_pair(&linux.code))
                .await
                .error
                .is_some()
        );
        assert!(
            relay
                .pair_controller(
                    controller,
                    generation,
                    resume_pair(linux.session, &linux.connection_resume_token)
                )
                .await
                .error
                .is_some()
        );
        let latest = relay.portal_get_principal(id).await.unwrap();
        assert!(latest.allowed_assets.contains(&"future".to_owned()));
        assert!(!latest.allowed_assets.contains(&"linux".to_owned()));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn managed_admin_removed_acl_preserves_agent_reconnect() {
        use remoteops_portal::UpdatePrincipalAssetsRequest;
        let mut fixture = ManagedFixture::new().await;
        let enrollment = fixture.hello.managed_enrollment.as_ref().unwrap().clone();
        let before = fixture
            .relay
            .portal_get_asset(&enrollment.target_id)
            .await
            .unwrap();
        let issued = fixture.issue().await;
        let old_session = issued.invitation.target.session_id;
        let (controller, generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        let paired = fixture
            .relay
            .pair_controller(controller, generation, first_pair(&issued.code))
            .await;
        assert!(paired.error.is_none());
        let old_resume = paired.connection_resume_token.unwrap();
        let principal = fixture
            .relay
            .portal_get_principal(fixture.principal_id)
            .await
            .unwrap();
        fixture
            .relay
            .portal_update_principal_assets(
                fixture.principal_id,
                UpdatePrincipalAssetsRequest {
                    expected_revision: principal.revision,
                    idempotency_key: Uuid::new_v4(),
                    add: vec![],
                    remove: vec!["asset-a".into()],
                },
                "runtime-test",
            )
            .await
            .unwrap();

        // 门户授权撤销不删除受管部署身份，也不阻止同次启动和新启动恢复。
        for new_boot in [false, true] {
            fixture.reconnect_agent(new_boot).await;
            assert_eq!(
                fixture.registration.welcome.managed_target_id.as_deref(),
                Some(enrollment.target_id.as_str())
            );
            assert!(fixture.registration.welcome.pairing_code.is_none());
            let current = fixture
                .relay
                .portal_get_asset(&enrollment.target_id)
                .await
                .unwrap();
            assert_eq!(current.agent_id, before.agent_id);
            assert_eq!(current.target_id, before.target_id);
            let proof = fixture.hello.managed_enrollment.as_ref().unwrap();
            assert_eq!(proof.target_id, enrollment.target_id);
            assert_eq!(proof.token, enrollment.token);
            assert!(
                fixture.relay.state.lock().await.agents[&fixture.hello.agent_instance_id].ready
            );
            assert!(
                fixture
                    .relay
                    .portal_status(&fixture.token, "asset-a")
                    .await
                    .is_err()
            );
            assert!(
                fixture
                    .relay
                    .portal_issue(&fixture.token, "asset-a", issue_request())
                    .await
                    .is_err()
            );
            assert!(
                fixture
                    .relay
                    .pair_controller(
                        controller,
                        generation,
                        resume_pair(old_session, &old_resume)
                    )
                    .await
                    .error
                    .is_some()
            );
        }
        let mut downgraded = fixture.hello.clone();
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
        let principal = fixture
            .relay
            .portal_get_principal(fixture.principal_id)
            .await
            .unwrap();
        fixture
            .relay
            .portal_update_principal_assets(
                fixture.principal_id,
                UpdatePrincipalAssetsRequest {
                    expected_revision: principal.revision,
                    idempotency_key: Uuid::new_v4(),
                    add: vec!["asset-a".into()],
                    remove: vec![],
                },
                "runtime-test",
            )
            .await
            .unwrap();
        assert!(
            fixture
                .relay
                .portal_status(&fixture.token, "asset-a")
                .await
                .is_ok()
        );
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, first_pair(&issued.code))
                .await
                .error
                .is_some()
        );
        assert!(
            fixture
                .relay
                .pair_controller(
                    controller,
                    generation,
                    resume_pair(old_session, &old_resume)
                )
                .await
                .error
                .is_some()
        );
        let fresh = fixture.issue().await;
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, first_pair(&fresh.code))
                .await
                .error
                .is_none()
        );
    }

    #[tokio::test]
    async fn managed_admin_transfer_revokes_unredeemed_invitation() {
        use remoteops_portal::TransferAssetRequest;
        let fixture = ManagedFixture::new().await;
        let destination = admin_test_principal(&fixture.relay, "destination", &["asset-a"]).await;
        let issued = fixture.issue().await;
        let target_id = &fixture.hello.managed_enrollment.as_ref().unwrap().target_id;
        let asset = fixture.relay.portal_get_asset(target_id).await.unwrap();
        let (controller, generation, _receiver) =
            register_controller(&fixture.relay, ControllerKind::Ai).await;
        let operation = fixture
            .relay
            .portal_transfer_asset(
                target_id,
                TransferAssetRequest {
                    expected_principal_id: fixture.principal_id,
                    destination_principal_id: destination.principal.principal_id,
                    expected_revision: asset.revision,
                    idempotency_key: Uuid::new_v4(),
                },
                "runtime-test",
            )
            .await
            .unwrap();
        assert_eq!(operation.state, "completed");
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, first_pair(&issued.code))
                .await
                .error
                .is_some()
        );
        assert!(
            fixture
                .relay
                .portal_store
                .as_ref()
                .unwrap()
                .invitation_target(&issued.code, Utc::now())
                .is_err()
        );
        let fresh = fixture
            .relay
            .portal_issue(&destination.token, "asset-a", issue_request())
            .await
            .unwrap();
        assert!(
            fixture
                .relay
                .pair_controller(controller, generation, first_pair(&fresh.code))
                .await
                .error
                .is_none()
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn managed_admin_pending_transfer_reconciles_after_restart() {
        use remoteops_portal::TransferAssetRequest;
        let state_file = TestStateFile::new("managed-transfer-restart");
        let relay = persisted_relay(&state_file.path, Duration::minutes(10)).unwrap();
        let source = admin_test_principal(&relay, "source", &["linux"]).await;
        let destination = admin_test_principal(&relay, "destination", &["linux"]).await;
        let (controller, generation, _controller_receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        let mut linux = prepare_admin_managed_target(
            &relay,
            source.principal.principal_id,
            &source.token,
            "linux",
            controller,
            generation,
        )
        .await;
        let before = relay.portal_get_asset(&linux.target_id).await.unwrap();
        let request = TransferAssetRequest {
            expected_principal_id: source.principal.principal_id,
            destination_principal_id: destination.principal.principal_id,
            expected_revision: before.revision,
            idempotency_key: Uuid::new_v4(),
        };
        // 模拟 SQLite 已提交、Relay 尚未清理旧会话即退出的进程边界。
        let pending = relay
            .portal_store
            .as_ref()
            .unwrap()
            .transfer_asset(&linux.target_id, request.clone(), "runtime-test")
            .unwrap();
        assert_eq!(pending.state, "pending");
        assert!(
            relay
                .state
                .lock()
                .await
                .session_bindings
                .contains_key(&linux.session)
        );
        drop(relay);

        let restored = persisted_relay(&state_file.path, Duration::minutes(10)).unwrap();
        let completed = restored
            .portal_admin_operation(pending.operation_id)
            .await
            .unwrap();
        assert_eq!(completed.state, "completed");
        assert!(
            restored
                .portal_store
                .as_ref()
                .unwrap()
                .pending_admin_operations()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            restored
                .portal_get_asset(&linux.target_id)
                .await
                .unwrap()
                .principal_id,
            destination.principal.principal_id
        );
        {
            let state = restored.state.lock().await;
            assert!(state.controllers.is_empty());
            assert!(state.session_bindings.is_empty());
            assert!(state.in_flight.is_empty());
            let agent = &state.agents[&linux.hello.agent_instance_id];
            assert_ne!(agent.session_id, linux.session);
            assert_eq!(
                agent.resume_token.as_deref(),
                Some(linux.agent_resume_token.as_str())
            );
            assert_eq!(
                agent.managed_target_id.as_deref(),
                Some(linux.target_id.as_str())
            );
        }
        linux.hello.resume_token = Some(linux.agent_resume_token.clone());
        let (sender, _agent_receiver) = test_channel();
        let resumed = restored.register_agent(linux.hello, sender).await.unwrap();
        assert!(
            restored
                .commit_agent_resume(before.agent_id.unwrap(), resumed.connection_generation)
                .await
        );
        assert!(
            restored
                .finalize_agent_resume(before.agent_id.unwrap(), resumed.connection_generation)
                .await
                .is_some()
        );
        let (sender, _controller_receiver) = test_channel();
        let generation = restored
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
        assert!(
            restored
                .pair_controller(controller, generation, first_pair(&linux.code))
                .await
                .error
                .is_some()
        );
        assert!(
            restored
                .pair_controller(
                    controller,
                    generation,
                    resume_pair(linux.session, &linux.connection_resume_token)
                )
                .await
                .error
                .is_some()
        );
        let issued = restored
            .portal_issue(&destination.token, "linux", issue_request())
            .await
            .unwrap();
        assert!(
            restored
                .pair_controller(controller, generation, first_pair(&issued.code))
                .await
                .error
                .is_none()
        );
        let replayed = restored
            .portal_transfer_asset(&linux.target_id, request, "runtime-test")
            .await
            .unwrap();
        assert_eq!(replayed.operation_id, pending.operation_id);
        assert!(
            restored
                .state
                .lock()
                .await
                .session_bindings
                .contains_key(&issued.invitation.target.session_id)
        );

        // 再次读取真实 JSON，确认启动收敛后的会话边界已经持久化。
        drop(restored);
        let persisted = persisted_relay(&state_file.path, Duration::minutes(10)).unwrap();
        assert_ne!(
            persisted.state.lock().await.agents[&before.agent_id.unwrap()].session_id,
            linux.session
        );
        assert_eq!(
            persisted
                .portal_admin_operation(pending.operation_id)
                .await
                .unwrap()
                .state,
            "completed"
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn managed_admin_transfer_json_failure_blocks_until_idempotent_retry() {
        use remoteops_portal::TransferAssetRequest;
        let mut relay = relay(Duration::minutes(10));
        relay.portal_store = Some(Arc::new(PortalStore::in_memory(test_owner_id()).unwrap()));
        let source = admin_test_principal(&relay, "source", &["linux"]).await;
        let destination = admin_test_principal(&relay, "destination", &["linux"]).await;
        let (controller, generation, _receiver) =
            register_controller(&relay, ControllerKind::Ai).await;
        let linux = prepare_admin_managed_target(
            &relay,
            source.principal.principal_id,
            &source.token,
            "linux",
            controller,
            generation,
        )
        .await;
        let before = relay.portal_get_asset(&linux.target_id).await.unwrap();
        let request = TransferAssetRequest {
            expected_principal_id: source.principal.principal_id,
            destination_principal_id: destination.principal.principal_id,
            expected_revision: before.revision,
            idempotency_key: Uuid::new_v4(),
        };
        let state_file = TestStateFile::new("managed-transfer-recovery");
        std::fs::create_dir_all(&state_file.root).unwrap();
        let blocker = state_file.root.join("not-a-directory");
        std::fs::write(&blocker, b"block parent creation").unwrap();
        relay.state_path = Some(blocker.join("relay-state.json"));
        assert!(matches!(
            relay
                .portal_transfer_asset(&linux.target_id, request.clone(), "runtime-test")
                .await,
            Err(PortalError::Storage)
        ));
        let pending = relay
            .portal_store
            .as_ref()
            .unwrap()
            .pending_admin_operations()
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, "pending");
        let operation_id = pending[0].operation_id;
        assert_eq!(
            relay
                .portal_admin_operation(operation_id)
                .await
                .unwrap()
                .state,
            "pending"
        );
        assert_eq!(
            relay
                .portal_get_asset(&linux.target_id)
                .await
                .unwrap()
                .principal_id,
            destination.principal.principal_id
        );
        assert!(
            relay
                .portal_issue(&destination.token, "linux", issue_request())
                .await
                .is_err()
        );
        assert!(
            relay
                .pair_controller(controller, generation, first_pair(&linux.code))
                .await
                .error
                .is_some()
        );
        assert!(
            relay
                .pair_controller(
                    controller,
                    generation,
                    resume_pair(linux.session, &linux.connection_resume_token)
                )
                .await
                .error
                .is_some()
        );
        {
            let state = relay.state.lock().await;
            assert!(!state.session_bindings.contains_key(&linux.session));
            assert!(!state.in_flight.contains_key(&linux.request));
        }
        relay.state_path = Some(state_file.path.clone());
        let completed = relay
            .portal_transfer_asset(&linux.target_id, request.clone(), "runtime-test")
            .await
            .unwrap();
        assert_eq!(completed.operation_id, operation_id);
        assert_eq!(completed.state, "completed");
        assert!(state_file.path.exists());
        assert!(
            relay
                .portal_store
                .as_ref()
                .unwrap()
                .pending_admin_operations()
                .unwrap()
                .is_empty()
        );
        let issued = relay
            .portal_issue(&destination.token, "linux", issue_request())
            .await
            .unwrap();
        let paired = relay
            .pair_controller(controller, generation, first_pair(&issued.code))
            .await;
        assert!(paired.error.is_none());
        let new_session = issued.invitation.target.session_id;
        let token = paired.connection_resume_token.unwrap();
        let replayed = relay
            .portal_transfer_asset(&linux.target_id, request, "runtime-test")
            .await
            .unwrap();
        assert_eq!(replayed.operation_id, operation_id);
        assert!(
            relay
                .state
                .lock()
                .await
                .session_bindings
                .contains_key(&new_session)
        );
        assert!(
            relay
                .pair_controller(controller, generation, resume_pair(new_session, &token))
                .await
                .error
                .is_none()
        );
    }
}
