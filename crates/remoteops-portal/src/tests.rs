use super::*;
use remoteops_domain::{AgentBootId, ControllerInstanceId, SessionId};
use std::sync::{Arc, Barrier};

struct Fixture {
    store: PortalStore,
    principal: CreatedPrincipal,
    asset: CreatedAsset,
    target: TargetSnapshot,
    consumer: ConsumerIdentity,
}
fn fixture() -> Fixture {
    let owner = ControllerOwnerId::new();
    let store = PortalStore::in_memory(owner).unwrap();
    let principal = store
        .create_principal(CreatePrincipalRequest {
            name: "ops".into(),
            owner_id: owner,
            allowed_assets: vec!["asset-a".into()],
        })
        .unwrap();
    let asset = store
        .create_asset(CreateAssetRequest {
            asset_id: "asset-a".into(),
            principal_id: principal.principal.principal_id,
        })
        .unwrap();
    let target = TargetSnapshot {
        agent_id: AgentInstanceId::new(),
        boot_id: AgentBootId::new(),
        session_id: SessionId::new(),
        agent_generation: 1,
    };
    store
        .validate_agent_enrollment(
            &asset.asset.target_id,
            &asset.enrollment_token,
            target.agent_id,
        )
        .unwrap();
    let consumer = ConsumerIdentity {
        owner_id: owner,
        controller_id: ControllerInstanceId::new(),
        installation_id: Some(Uuid::new_v4()),
    };
    Fixture {
        store,
        principal,
        asset,
        target,
        consumer,
    }
}
fn request() -> IssueInvitationRequest {
    IssueInvitationRequest {
        idempotency_key: Uuid::new_v4(),
        replace_active: false,
        expected_revision: None,
    }
}
fn issue(f: &Fixture, now: DateTime<Utc>) -> IssuedInvitation {
    f.store
        .issue(
            f.principal.principal.principal_id,
            "asset-a",
            f.target,
            request(),
            &[],
            now,
        )
        .unwrap()
}

#[test]
fn enrollment_binds_exact_agent_and_principal_acl_is_enforced() {
    let f = fixture();
    assert!(f.store.authenticate_principal(&f.principal.token).is_ok());
    assert!(matches!(
        f.store.authenticate_principal("forged"),
        Err(PortalError::Unauthorized)
    ));
    assert!(matches!(
        f.store
            .asset_for_principal(f.principal.principal.principal_id, "other"),
        Err(PortalError::Forbidden)
    ));
    assert!(matches!(
        f.store.validate_agent_enrollment(
            &f.asset.asset.target_id,
            &f.asset.enrollment_token,
            AgentInstanceId::new()
        ),
        Err(PortalError::Conflict)
    ));
    assert!(
        f.store
            .validate_agent_enrollment(
                &f.asset.asset.target_id,
                &f.asset.enrollment_token,
                f.target.agent_id
            )
            .is_ok()
    );
}

#[test]
fn issue_is_idempotent_and_reissue_revokes_old_code() {
    let f = fixture();
    let now = Utc::now();
    let req = request();
    let first = f
        .store
        .issue(
            f.principal.principal.principal_id,
            "asset-a",
            f.target,
            req,
            &[],
            now,
        )
        .unwrap();
    let again = f
        .store
        .issue(
            f.principal.principal.principal_id,
            "asset-a",
            f.target,
            req,
            &[],
            now,
        )
        .unwrap();
    assert!(first.code == again.code, "同一请求应恢复相同短码");
    assert_eq!(
        first.invitation.invitation_id,
        again.invitation.invitation_id
    );
    let next = issue(&f, now);
    assert!(first.code != next.code, "重签必须更换短码");
    assert!(matches!(
        f.store.redeem(
            &first.code,
            f.consumer,
            first.invitation.target,
            Uuid::new_v4(),
            now
        ),
        Err(PortalError::Revoked)
    ));
    assert_eq!(
        f.store
            .status(f.principal.principal.principal_id, "asset-a", now)
            .unwrap()
            .invitation
            .unwrap()
            .invitation_id,
        next.invitation.invitation_id
    );
}

#[test]
fn expiry_applies_only_to_first_redemption_and_response_loss_is_recoverable() {
    let f = fixture();
    let now = Utc::now();
    let invitation = issue(&f, now);
    let request_id = Uuid::new_v4();
    let redeemed = f
        .store
        .redeem(
            &invitation.code,
            f.consumer,
            invitation.invitation.target,
            request_id,
            now,
        )
        .unwrap();
    let later = now + chrono::Duration::hours(1);
    let recovered = f
        .store
        .redeem(
            &invitation.code,
            f.consumer,
            invitation.invitation.target,
            request_id,
            later,
        )
        .unwrap();
    assert!(
        redeemed.resume_token == recovered.resume_token,
        "同请求恢复令牌应一致"
    );
    assert!(matches!(
        f.store.redeem(
            &invitation.code,
            f.consumer,
            invitation.invitation.target,
            Uuid::new_v4(),
            later
        ),
        Err(PortalError::Conflict)
    ));
    let mut transport = redeemed.target;
    transport.agent_generation += 1;
    assert!(
        f.store
            .resume(&redeemed.resume_token, f.consumer, transport, later)
            .is_ok()
    );
    let mut other = f.consumer;
    other.controller_id = ControllerInstanceId::new();
    assert!(matches!(
        f.store
            .resume(&redeemed.resume_token, other, transport, later),
        Err(PortalError::Unauthorized)
    ));
    let g = fixture();
    let inv = issue(&g, now);
    assert!(matches!(
        g.store.redeem(
            &inv.code,
            g.consumer,
            inv.invitation.target,
            Uuid::new_v4(),
            now + chrono::Duration::seconds(600)
        ),
        Err(PortalError::Expired)
    ));
}

#[test]
fn close_revoke_and_boot_change_deny_restore() {
    let f = fixture();
    let now = Utc::now();
    let invitation = issue(&f, now);
    let connection = f
        .store
        .redeem(
            &invitation.code,
            f.consumer,
            invitation.invitation.target,
            Uuid::new_v4(),
            now,
        )
        .unwrap();
    assert!(matches!(
        f.store.issue(
            f.principal.principal.principal_id,
            "asset-a",
            f.target,
            request(),
            &[],
            now
        ),
        Err(PortalError::Conflict)
    ));
    f.store
        .close_connection(
            f.principal.principal.principal_id,
            connection.connection_id,
            None,
            now,
        )
        .unwrap();
    assert!(matches!(
        f.store
            .resume(&connection.resume_token, f.consumer, connection.target, now),
        Err(PortalError::Revoked)
    ));
    let invitation = issue(&f, now);
    let connection = f
        .store
        .redeem(
            &invitation.code,
            f.consumer,
            invitation.invitation.target,
            Uuid::new_v4(),
            now,
        )
        .unwrap();
    let next = TargetSnapshot {
        boot_id: AgentBootId::new(),
        session_id: SessionId::new(),
        agent_generation: 3,
        ..f.target
    };
    assert!(matches!(
        f.store
            .resume(&connection.resume_token, f.consumer, next, now),
        Err(PortalError::TargetMismatch)
    ));
    let new = f
        .store
        .issue(
            f.principal.principal.principal_id,
            "asset-a",
            next,
            request(),
            &[],
            now,
        )
        .unwrap();
    assert!(new.replaced_connection.is_some());
    assert!(matches!(
        f.store
            .resume(&connection.resume_token, f.consumer, connection.target, now),
        Err(PortalError::Revoked)
    ));
    f.store
        .revoke_principal(f.principal.principal.principal_id)
        .unwrap();
    assert!(matches!(
        f.store
            .redeem(&new.code, f.consumer, next, Uuid::new_v4(), now),
        Err(PortalError::Unauthorized)
    ));
    assert!(
        f.store
            .asset_for_agent(f.target.agent_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn independent_handles_have_one_redemption_winner_and_restart_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("portal.sqlite");
    let owner = ControllerOwnerId::new();
    let store = PortalStore::open(&path, owner).unwrap();
    let principal = store
        .create_principal(CreatePrincipalRequest {
            name: "ops".into(),
            owner_id: owner,
            allowed_assets: vec!["a".into()],
        })
        .unwrap();
    let asset = store
        .create_asset(CreateAssetRequest {
            asset_id: "a".into(),
            principal_id: principal.principal.principal_id,
        })
        .unwrap();
    let target = TargetSnapshot {
        agent_id: AgentInstanceId::new(),
        boot_id: AgentBootId::new(),
        session_id: SessionId::new(),
        agent_generation: 1,
    };
    store
        .validate_agent_enrollment(
            &asset.asset.target_id,
            &asset.enrollment_token,
            target.agent_id,
        )
        .unwrap();
    let now = Utc::now();
    let invitation = store
        .issue(
            principal.principal.principal_id,
            "a",
            target,
            request(),
            &[],
            now,
        )
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let store = PortalStore::open(&path, owner).unwrap();
            let code = invitation.code.clone();
            let target = invitation.invitation.target;
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let consumer = ConsumerIdentity {
                    owner_id: owner,
                    controller_id: ControllerInstanceId::new(),
                    installation_id: None,
                };
                barrier.wait();
                (
                    consumer,
                    store.redeem(&code, consumer, target, Uuid::new_v4(), now),
                )
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|(_, r)| r.is_ok()).count(), 1);
    let (consumer, winner) = results.into_iter().find(|(_, r)| r.is_ok()).unwrap();
    let winner = winner.unwrap();
    drop(store);
    let reopened = PortalStore::open(&path, owner).unwrap();
    assert!(
        reopened
            .resume(&winner.resume_token, consumer, winner.target, now)
            .is_ok()
    );
    assert!(PortalStore::open(&path, ControllerOwnerId::new()).is_err());
}

#[test]
fn database_never_stores_plain_credentials_and_failed_issue_is_atomic() {
    let f = fixture();
    let now = Utc::now();
    let first = issue(&f, now);
    {
        let db = f.store.db().unwrap();
        db.execute_batch("CREATE TRIGGER reject_issue BEFORE INSERT ON portal_invitations BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
        let values: String = db
            .query_row(
                "SELECT group_concat(data) FROM portal_principals",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!values.contains(&f.principal.token));
    }
    assert!(matches!(
        f.store.issue(
            f.principal.principal.principal_id,
            "asset-a",
            f.target,
            request(),
            &[],
            now
        ),
        Err(PortalError::Storage)
    ));
    assert!(
        f.store
            .redeem(
                &first.code,
                f.consumer,
                first.invitation.target,
                Uuid::new_v4(),
                now
            )
            .is_ok()
    );
}

#[test]
fn replacement_requires_asset_revision_and_always_uses_new_session() {
    let f = fixture();
    let now = Utc::now();
    let first = issue(&f, now);
    assert_ne!(first.invitation.target.session_id, f.target.session_id);
    let connection = f
        .store
        .redeem(
            &first.code,
            f.consumer,
            first.invitation.target,
            Uuid::new_v4(),
            now,
        )
        .unwrap();
    let revision = f
        .store
        .status(f.principal.principal.principal_id, "asset-a", now)
        .unwrap()
        .asset
        .revision;
    for expected_revision in [None, Some(revision + 1)] {
        let result = f.store.issue(
            f.principal.principal.principal_id,
            "asset-a",
            first.invitation.target,
            IssueInvitationRequest {
                idempotency_key: Uuid::new_v4(),
                replace_active: true,
                expected_revision,
            },
            &[],
            now,
        );
        assert!(result.is_err());
        assert!(
            f.store
                .resume(&connection.resume_token, f.consumer, connection.target, now)
                .is_ok()
        );
    }
    let request = IssueInvitationRequest {
        idempotency_key: Uuid::new_v4(),
        replace_active: true,
        expected_revision: Some(revision),
    };
    let next = f
        .store
        .issue(
            f.principal.principal.principal_id,
            "asset-a",
            first.invitation.target,
            request,
            &[],
            now,
        )
        .unwrap();
    assert_ne!(
        first.invitation.target.session_id,
        next.invitation.target.session_id
    );
    assert!(
        f.store
            .resume(&connection.resume_token, f.consumer, connection.target, now)
            .is_err()
    );
    let retry = f
        .store
        .issue(
            f.principal.principal.principal_id,
            "asset-a",
            next.invitation.target,
            request,
            &[],
            now,
        )
        .unwrap();
    assert_eq!(
        retry.invitation.target.session_id,
        next.invitation.target.session_id
    );
    assert!(retry.code == next.code, "相同幂等请求不得创建第二个短码");
}

#[test]
fn historical_codes_stay_reserved_after_expiry_and_revocation() {
    let f = fixture();
    let now = Utc::now();
    let invitation = issue(&f, now);
    assert!(f.store.code_reserved(&invitation.code, now).unwrap());
    f.store
        .revoke_invitation(
            f.principal.principal.principal_id,
            invitation.invitation.invitation_id,
            now,
        )
        .unwrap();
    assert!(
        f.store
            .code_reserved(&invitation.code, now + chrono::Duration::days(30))
            .unwrap()
    );
    assert!(!f.store.code_reserved("invalid", now).unwrap());
}

#[test]
fn concurrent_identical_redemptions_return_one_connection() {
    let f = fixture();
    let now = Utc::now();
    let invitation = issue(&f, now);
    let request_id = Uuid::new_v4();
    let store = Arc::new(f.store);
    let barrier = Arc::new(Barrier::new(2));
    let workers = (0..2)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            let code = invitation.code.clone();
            let target = invitation.invitation.target;
            let consumer = f.consumer;
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .redeem(&code, consumer, target, request_id, now)
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results[0].connection_id, results[1].connection_id);
    assert!(
        results[0].resume_token == results[1].resume_token,
        "完全相同的请求恢复同一连接凭据"
    );
}
