use super::*;
use remoteops_domain::{AgentBootId, ControllerInstanceId, SessionId};

struct Fixture {
    store: PortalStore,
    source: CreatedPrincipal,
    destination: CreatedPrincipal,
    asset: CreatedAsset,
    target: TargetSnapshot,
    consumer: ConsumerIdentity,
}

fn fixture() -> Fixture {
    let owner = ControllerOwnerId::new();
    let store = PortalStore::in_memory(owner).unwrap();
    let principal = |name: &str| {
        store
            .create_principal(CreatePrincipalRequest {
                name: name.into(),
                owner_id: owner,
                allowed_assets: vec!["linux".into(), "windows".into()],
            })
            .unwrap()
    };
    let source = principal("source");
    let destination = principal("destination");
    let asset = store
        .create_asset(CreateAssetRequest {
            principal_id: source.principal.principal_id,
            asset_id: "linux".into(),
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
    Fixture {
        store,
        source,
        destination,
        asset,
        target,
        consumer: ConsumerIdentity {
            owner_id: owner,
            controller_id: ControllerInstanceId::new(),
            installation_id: Some(Uuid::new_v4()),
        },
    }
}

fn issue(
    f: &Fixture,
    principal_id: Uuid,
    asset_id: &str,
    target: TargetSnapshot,
) -> PortalResult<IssuedInvitation> {
    f.store.issue(
        principal_id,
        asset_id,
        target,
        IssueInvitationRequest {
            idempotency_key: Uuid::new_v4(),
            replace_active: false,
            expected_revision: None,
        },
        &[],
        Utc::now(),
    )
}

fn transfer_request(f: &Fixture) -> TransferAssetRequest {
    TransferAssetRequest {
        expected_principal_id: f.source.principal.principal_id,
        destination_principal_id: f.destination.principal.principal_id,
        expected_revision: f
            .store
            .get_asset(&f.asset.asset.target_id)
            .unwrap()
            .revision,
        idempotency_key: Uuid::new_v4(),
    }
}

fn acl_request(revision: u64, add: &[&str], remove: &[&str]) -> UpdatePrincipalAssetsRequest {
    UpdatePrincipalAssetsRequest {
        expected_revision: revision,
        idempotency_key: Uuid::new_v4(),
        add: add.iter().map(ToString::to_string).collect(),
        remove: remove.iter().map(ToString::to_string).collect(),
    }
}

#[test]
fn acl_append_keeps_token_and_existing_connection_and_replays_original_snapshot() {
    let f = fixture();
    let pid = f.source.principal.principal_id;
    let issued = issue(&f, pid, "linux", f.target).unwrap();
    let connection = f
        .store
        .redeem(
            &issued.code,
            f.consumer,
            issued.invitation.target,
            Uuid::new_v4(),
            Utc::now(),
        )
        .unwrap();
    let request = acl_request(1, &["mac"], &[]);
    let operation = f
        .store
        .update_principal_assets(pid, request.clone(), "admin_session:127.0.0.1")
        .unwrap();
    assert!(operation.affected_target_ids.is_empty());
    assert_eq!(operation.previous_principal.as_ref().unwrap().revision, 1);
    assert_eq!(operation.principal.as_ref().unwrap().revision, 2);
    assert!(
        f.store
            .connection_authorized(connection.connection_id, f.consumer, connection.target)
            .is_ok()
    );
    assert_eq!(
        f.store
            .authenticate_principal(&f.source.token)
            .unwrap()
            .revision,
        2
    );
    assert!(matches!(
        f.store.revoke_principal(pid),
        Err(PortalError::Conflict)
    ));
    assert!(matches!(
        f.store.create_asset(CreateAssetRequest {
            principal_id: pid,
            asset_id: "mac".into()
        }),
        Err(PortalError::Conflict)
    ));
    f.store
        .complete_admin_operation(operation.operation_id)
        .unwrap();
    let next = f
        .store
        .update_principal_assets(pid, acl_request(2, &["bsd"], &[]), "admin_token")
        .unwrap();
    f.store.complete_admin_operation(next.operation_id).unwrap();
    let replay = f
        .store
        .update_principal_assets(pid, request.clone(), "admin_token")
        .unwrap();
    assert_eq!(replay.operation_id, operation.operation_id);
    assert_eq!(replay.principal.unwrap().revision, 2);
    assert_eq!(f.store.get_principal(pid).unwrap().revision, 3);
    let mut different = request;
    different.add.push("different".into());
    assert!(matches!(
        f.store
            .update_principal_assets(pid, different, "admin_token"),
        Err(PortalError::IdempotencyConflict)
    ));
    assert!(
        f.store
            .resume(
                &connection.resume_token,
                f.consumer,
                connection.target,
                Utc::now()
            )
            .is_ok()
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn acl_remove_all_revokes_only_owned_targets_and_keeps_bound_agent_identity() {
    let f = fixture();
    let pid = f.source.principal.principal_id;
    let issued = issue(&f, pid, "linux", f.target).unwrap();
    let connection = f
        .store
        .redeem(
            &issued.code,
            f.consumer,
            issued.invitation.target,
            Uuid::new_v4(),
            Utc::now(),
        )
        .unwrap();
    let other_asset = f
        .store
        .create_asset(CreateAssetRequest {
            principal_id: f.destination.principal.principal_id,
            asset_id: "linux".into(),
        })
        .unwrap();
    let other_target = TargetSnapshot {
        agent_id: AgentInstanceId::new(),
        ..f.target
    };
    f.store
        .validate_agent_enrollment(
            &other_asset.asset.target_id,
            &other_asset.enrollment_token,
            other_target.agent_id,
        )
        .unwrap();
    let other_code = issue(
        &f,
        f.destination.principal.principal_id,
        "linux",
        other_target,
    )
    .unwrap();
    let operation = f
        .store
        .update_principal_assets(
            pid,
            acl_request(1, &[], &["linux", "windows"]),
            "admin_token",
        )
        .unwrap();
    assert_eq!(
        operation.affected_target_ids,
        vec![f.asset.asset.target_id.clone()]
    );
    assert!(
        operation
            .principal
            .as_ref()
            .unwrap()
            .allowed_assets
            .is_empty()
    );
    let relay_status = f
        .store
        .target_status(&f.asset.asset.target_id, Utc::now())
        .unwrap();
    assert_eq!(relay_status.asset.agent_id, Some(f.target.agent_id));
    assert!(relay_status.connection.is_none());
    assert!(matches!(
        f.store.status(pid, "linux", Utc::now()),
        Err(PortalError::Forbidden)
    ));
    assert!(
        f.store
            .resume(
                &connection.resume_token,
                f.consumer,
                connection.target,
                Utc::now()
            )
            .is_err()
    );
    assert!(
        f.store
            .connection_authorized(connection.connection_id, f.consumer, connection.target)
            .is_err()
    );
    assert!(
        f.store
            .redeem(
                &issued.code,
                f.consumer,
                connection.target,
                Uuid::new_v4(),
                Utc::now()
            )
            .is_err()
    );
    assert!(
        f.store
            .invitation_target(&other_code.code, Utc::now())
            .is_ok()
    );
    assert!(
        f.store
            .validate_agent_enrollment(
                &f.asset.asset.target_id,
                &f.asset.enrollment_token,
                f.target.agent_id
            )
            .is_ok()
    );
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
            .validate_agent_enrollment(&f.asset.asset.target_id, "invalid", f.target.agent_id)
            .is_err()
    );
    assert_eq!(
        f.store
            .get_asset(&f.asset.asset.target_id)
            .unwrap()
            .agent_id,
        Some(f.target.agent_id)
    );
    f.store
        .complete_admin_operation(operation.operation_id)
        .unwrap();
    let events = f.store.admin_audit(10).unwrap();
    let event = events
        .iter()
        .find(|v| v.operation_id == Some(operation.operation_id))
        .unwrap();
    assert_eq!(event.state, "completed");
    assert!(event.completed_at.is_some());
    assert_eq!(
        event
            .previous_principal
            .as_ref()
            .unwrap()
            .allowed_assets
            .len(),
        2
    );
    assert!(event.principal.as_ref().unwrap().allowed_assets.is_empty());
}

#[test]
#[allow(clippy::too_many_lines)]
fn transfer_preserves_identity_blocks_pending_authorization_and_does_not_revoke_on_replay() {
    let f = fixture();
    let pid = f.source.principal.principal_id;
    let issued = issue(&f, pid, "linux", f.target).unwrap();
    let old = f
        .store
        .redeem(
            &issued.code,
            f.consumer,
            issued.invitation.target,
            Uuid::new_v4(),
            Utc::now(),
        )
        .unwrap();
    let request = transfer_request(&f);
    let operation = f
        .store
        .transfer_asset(
            &f.asset.asset.target_id,
            request.clone(),
            "admin_session:127.0.0.1",
        )
        .unwrap();
    let transferred = operation.asset.as_ref().unwrap();
    assert_eq!(transferred.agent_id, Some(f.target.agent_id));
    assert_eq!(transferred.target_id, f.asset.asset.target_id);
    assert_eq!(transferred.asset_id, "linux");
    assert_eq!(
        transferred.principal_id,
        f.destination.principal.principal_id
    );
    assert_eq!(transferred.revision, request.expected_revision + 1);
    assert!(f.store.asset_for_principal(pid, "linux").is_err());
    assert!(
        f.store
            .resume(&old.resume_token, f.consumer, old.target, Utc::now())
            .is_err()
    );
    assert!(matches!(
        issue(&f, transferred.principal_id, "linux", f.target),
        Err(PortalError::Conflict)
    ));
    assert!(matches!(
        f.store.revoke_principal(pid),
        Err(PortalError::Conflict)
    ));
    assert!(matches!(
        f.store.update_principal_assets(
            transferred.principal_id,
            acl_request(1, &["mac"], &[]),
            "admin_token"
        ),
        Err(PortalError::Conflict)
    ));
    assert_eq!(f.store.pending_admin_operations().unwrap().len(), 1);
    assert_eq!(
        f.store
            .transfer_asset(&f.asset.asset.target_id, request.clone(), "admin_token")
            .unwrap()
            .operation_id,
        operation.operation_id
    );
    assert!(
        f.store
            .validate_agent_enrollment(
                &transferred.target_id,
                &f.asset.enrollment_token,
                f.target.agent_id
            )
            .is_ok()
    );
    f.store
        .complete_admin_operation(operation.operation_id)
        .unwrap();
    let new_code = issue(&f, transferred.principal_id, "linux", f.target).unwrap();
    let new_connection = f
        .store
        .redeem(
            &new_code.code,
            f.consumer,
            new_code.invitation.target,
            Uuid::new_v4(),
            Utc::now(),
        )
        .unwrap();
    let replay = f
        .store
        .transfer_asset(&f.asset.asset.target_id, request, "admin_token")
        .unwrap();
    assert_eq!(replay.state, "completed");
    f.store
        .complete_admin_operation(replay.operation_id)
        .unwrap();
    assert!(
        f.store
            .connection_authorized(
                new_connection.connection_id,
                f.consumer,
                new_connection.target
            )
            .is_ok()
    );
    let audit = f
        .store
        .admin_audit(10)
        .unwrap()
        .into_iter()
        .find(|v| v.operation_id == Some(operation.operation_id))
        .unwrap();
    assert_eq!(audit.previous_asset.unwrap().principal_id, pid);
    assert_eq!(audit.asset.unwrap().principal_id, transferred.principal_id);
    assert_eq!(audit.actor, "admin_session:127.0.0.1");
}

#[test]
fn invalid_transfer_is_atomic_and_records_specific_failure_without_secrets() {
    let f = fixture();
    let issued = issue(&f, f.source.principal.principal_id, "linux", f.target).unwrap();
    let original = f.store.get_asset(&f.asset.asset.target_id).unwrap();
    let mut wrong_revision = transfer_request(&f);
    wrong_revision.expected_revision -= 1;
    assert!(matches!(
        f.store
            .transfer_asset(&f.asset.asset.target_id, wrong_revision, "admin_token"),
        Err(PortalError::RevisionConflict)
    ));
    let mut wrong_owner = transfer_request(&f);
    wrong_owner.expected_principal_id = f.destination.principal.principal_id;
    wrong_owner.destination_principal_id = f.source.principal.principal_id;
    assert!(matches!(
        f.store
            .transfer_asset(&f.asset.asset.target_id, wrong_owner, "admin_token"),
        Err(PortalError::OwnershipConflict)
    ));
    f.store
        .create_asset(CreateAssetRequest {
            principal_id: f.destination.principal.principal_id,
            asset_id: "linux".into(),
        })
        .unwrap();
    assert!(matches!(
        f.store.transfer_asset(
            &f.asset.asset.target_id,
            transfer_request(&f),
            "admin_token"
        ),
        Err(PortalError::NameConflict)
    ));
    assert_eq!(
        f.store
            .get_asset(&f.asset.asset.target_id)
            .unwrap()
            .revision,
        original.revision
    );
    assert!(f.store.invitation_target(&issued.code, Utc::now()).is_ok());
    assert!(f.store.pending_admin_operations().unwrap().is_empty());
    let audit = f.store.admin_audit(10).unwrap();
    assert_eq!(audit.len(), 3);
    assert!(
        audit
            .iter()
            .all(|v| v.state == "failed" && v.operation_id.is_none())
    );
    let json = serde_json::to_string(&audit).unwrap();
    assert!(!json.contains(&issued.code));
    assert!(!json.contains(&f.asset.enrollment_token));
    assert!(!json.contains(&f.source.token));
    assert!(
        json.contains("revision_conflict")
            && json.contains("ownership_conflict")
            && json.contains("name_conflict")
    );
}

#[test]
fn acl_validates_overlap_limits_and_revision_and_allows_empty_after_remove() {
    let f = fixture();
    let pid = f.source.principal.principal_id;
    assert!(matches!(
        f.store
            .update_principal_assets(pid, acl_request(1, &["linux"], &["linux"]), "admin_token"),
        Err(PortalError::InvalidInput)
    ));
    assert!(matches!(
        f.store
            .update_principal_assets(pid, acl_request(2, &["mac"], &[]), "admin_token"),
        Err(PortalError::RevisionConflict)
    ));
    let mut oversized = acl_request(1, &[], &[]);
    oversized.add = (0..1024).map(|v| format!("asset-{v}")).collect();
    assert!(matches!(
        f.store
            .update_principal_assets(pid, oversized, "admin_token"),
        Err(PortalError::InvalidInput)
    ));
    assert_eq!(f.store.get_principal(pid).unwrap().revision, 1);
}

#[test]
fn transfer_unenrolled_asset_preserves_original_enrollment_proof() {
    let f = fixture();
    let unbound = f
        .store
        .create_asset(CreateAssetRequest {
            principal_id: f.source.principal.principal_id,
            asset_id: "windows".into(),
        })
        .unwrap();
    let mut request = transfer_request(&f);
    request.expected_revision = 1;
    let operation = f
        .store
        .transfer_asset(&unbound.asset.target_id, request, "admin_token")
        .unwrap();
    let enrolled = f
        .store
        .validate_agent_enrollment(
            &unbound.asset.target_id,
            &unbound.enrollment_token,
            AgentInstanceId::new(),
        )
        .unwrap();
    assert_eq!(enrolled.principal_id, f.destination.principal.principal_id);
    assert_eq!(enrolled.revision, 3);
    assert!(operation.asset.unwrap().agent_id.is_none());
}

#[test]
fn transfer_requires_destination_acl_and_rejects_revoked_source() {
    let f = fixture();
    let destination = f.destination.principal.principal_id;
    let removed = f
        .store
        .update_principal_assets(destination, acl_request(1, &[], &["linux"]), "admin_token")
        .unwrap();
    f.store
        .complete_admin_operation(removed.operation_id)
        .unwrap();
    assert!(matches!(
        f.store.transfer_asset(
            &f.asset.asset.target_id,
            transfer_request(&f),
            "admin_token"
        ),
        Err(PortalError::Forbidden)
    ));
    let restored = f
        .store
        .update_principal_assets(destination, acl_request(2, &["linux"], &[]), "admin_token")
        .unwrap();
    f.store
        .complete_admin_operation(restored.operation_id)
        .unwrap();
    f.store
        .revoke_principal(f.source.principal.principal_id)
        .unwrap();
    assert!(
        f.store
            .get_principal(f.source.principal.principal_id)
            .unwrap()
            .revoked
    );
    assert!(matches!(
        f.store.transfer_asset(
            &f.asset.asset.target_id,
            transfer_request(&f),
            "admin_token"
        ),
        Err(PortalError::Unauthorized)
    ));
}

#[test]
fn schema_v1_migration_preserves_keys_credentials_assets_and_pending_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("portal.db");
    let owner = ControllerOwnerId::new();
    let store = PortalStore::open(&path, owner).unwrap();
    let principal = store
        .create_principal(CreatePrincipalRequest {
            name: "old".into(),
            owner_id: owner,
            allowed_assets: vec!["linux".into()],
        })
        .unwrap();
    let asset = store
        .create_asset(CreateAssetRequest {
            principal_id: principal.principal.principal_id,
            asset_id: "linux".into(),
        })
        .unwrap();
    let key = store.key.to_vec();
    {
        let db = store.db().unwrap();
        let mut old = serde_json::to_value(&principal.principal).unwrap();
        old.as_object_mut().unwrap().remove("revision");
        db.execute("UPDATE portal_principals SET data=?1", [old.to_string()])
            .unwrap();
        db.execute_batch("DROP TABLE portal_admin_audit; DROP TABLE portal_admin_targets; DROP TABLE portal_admin_operations; PRAGMA user_version=1;").unwrap();
    }
    drop(store);
    assert!(matches!(
        PortalStore::open(&path, ControllerOwnerId::new()),
        Err(PortalError::Storage)
    ));
    {
        let raw = Connection::open(&path).unwrap();
        assert_eq!(
            raw.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let stored_key: Vec<u8> = raw
            .query_row("SELECT key FROM portal_meta WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored_key, key);
    }
    let upgraded = PortalStore::open(&path, owner).unwrap();
    assert_eq!(upgraded.key.as_slice(), key.as_slice());
    assert_eq!(
        upgraded
            .authenticate_principal(&principal.token)
            .unwrap()
            .revision,
        1
    );
    let target = AgentInstanceId::new();
    upgraded
        .validate_agent_enrollment(&asset.asset.target_id, &asset.enrollment_token, target)
        .unwrap();
    let request = acl_request(1, &[], &["linux"]);
    let operation = upgraded
        .update_principal_assets(
            principal.principal.principal_id,
            request.clone(),
            "admin_token",
        )
        .unwrap();
    drop(upgraded);
    let reopened = PortalStore::open(&path, owner).unwrap();
    assert_eq!(
        reopened.pending_admin_operations().unwrap()[0].operation_id,
        operation.operation_id
    );
    assert_eq!(
        reopened
            .update_principal_assets(
                principal.principal.principal_id,
                request.clone(),
                "admin_token"
            )
            .unwrap()
            .operation_id,
        operation.operation_id
    );
    reopened
        .complete_admin_operation(operation.operation_id)
        .unwrap();
    assert!(reopened.pending_admin_operations().unwrap().is_empty());
    assert_eq!(
        reopened
            .update_principal_assets(principal.principal.principal_id, request, "admin_token")
            .unwrap()
            .state,
        "completed"
    );
    assert!(
        reopened
            .validate_agent_enrollment(&asset.asset.target_id, &asset.enrollment_token, target)
            .is_ok()
    );
}

#[test]
fn concurrent_conflicting_transfers_commit_only_one_destination() {
    use std::sync::{Arc, Barrier};
    let f = fixture();
    let third = f
        .store
        .create_principal(CreatePrincipalRequest {
            name: "third".into(),
            owner_id: f.source.principal.owner_id,
            allowed_assets: vec!["linux".into()],
        })
        .unwrap();
    let first = transfer_request(&f);
    let second = TransferAssetRequest {
        destination_principal_id: third.principal.principal_id,
        idempotency_key: Uuid::new_v4(),
        ..first.clone()
    };
    let store = Arc::new(f.store);
    let barrier = Arc::new(Barrier::new(3));
    let jobs = [first, second]
        .into_iter()
        .map(|request| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let target = f.asset.asset.target_id.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.transfer_asset(&target, request, "admin_token")
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = jobs
        .into_iter()
        .map(|job| job.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(store.pending_admin_operations().unwrap().len(), 1);
    assert_eq!(
        store.get_asset(&f.asset.asset.target_id).unwrap().revision,
        3
    );
}

#[test]
fn transfer_rejects_foreign_owner_and_revoked_destination_without_mutating_asset() {
    let f = fixture();
    let destination = f.destination.principal.principal_id;
    let original = f.store.get_asset(&f.asset.asset.target_id).unwrap();
    let mut foreign = f.destination.principal.clone();
    foreign.owner_id = ControllerOwnerId::new();
    f.store
        .db()
        .unwrap()
        .execute(
            "UPDATE portal_principals SET data=?1 WHERE id=?2",
            params![encode(&foreign).unwrap(), destination.to_string()],
        )
        .unwrap();
    assert!(matches!(
        f.store.transfer_asset(
            &f.asset.asset.target_id,
            transfer_request(&f),
            "admin_token"
        ),
        Err(PortalError::Forbidden)
    ));
    f.store
        .db()
        .unwrap()
        .execute(
            "UPDATE portal_principals SET data=?1 WHERE id=?2",
            params![
                encode(&f.destination.principal).unwrap(),
                destination.to_string()
            ],
        )
        .unwrap();
    f.store.revoke_principal(destination).unwrap();
    assert!(matches!(
        f.store.transfer_asset(
            &f.asset.asset.target_id,
            transfer_request(&f),
            "admin_token"
        ),
        Err(PortalError::Unauthorized)
    ));
    let after = f.store.get_asset(&f.asset.asset.target_id).unwrap();
    assert_eq!(after.principal_id, original.principal_id);
    assert_eq!(after.revision, original.revision);
    assert!(f.store.pending_admin_operations().unwrap().is_empty());
}

#[test]
fn invalid_resource_is_redacted_from_failure_audit() {
    let f = fixture();
    assert!(matches!(
        f.store
            .transfer_asset(&f.source.token, transfer_request(&f), "admin_token"),
        Err(PortalError::InvalidInput)
    ));
    f.store
        .record_admin_failure(
            "admin_token",
            "transfer_asset",
            &f.asset.enrollment_token,
            &PortalError::InvalidInput,
        )
        .unwrap();
    let audit = f.store.admin_audit(10).unwrap();
    assert_eq!(audit.len(), 2);
    assert!(
        audit
            .iter()
            .all(|event| event.resource == "invalid_resource")
    );
    let json = serde_json::to_string(&audit).unwrap();
    assert!(!json.contains(&f.source.token));
    assert!(!json.contains(&f.asset.enrollment_token));
}

#[test]
fn failed_operation_insert_rolls_back_acl_and_session_revocation() {
    let f = fixture();
    let pid = f.source.principal.principal_id;
    let code = issue(&f, pid, "linux", f.target).unwrap();
    let connection = f
        .store
        .redeem(
            &code.code,
            f.consumer,
            code.invitation.target,
            Uuid::new_v4(),
            Utc::now(),
        )
        .unwrap();
    f.store.db().unwrap().execute_batch("CREATE TRIGGER reject_admin_operation BEFORE INSERT ON portal_admin_operations BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    assert!(matches!(
        f.store
            .update_principal_assets(pid, acl_request(1, &[], &["linux"]), "admin_token"),
        Err(PortalError::Storage)
    ));
    assert_eq!(f.store.get_principal(pid).unwrap().revision, 1);
    assert!(
        f.store
            .get_principal(pid)
            .unwrap()
            .allowed_assets
            .contains(&"linux".into())
    );
    assert!(
        f.store
            .resume(
                &connection.resume_token,
                f.consumer,
                connection.target,
                Utc::now()
            )
            .is_ok()
    );
    assert!(f.store.pending_admin_operations().unwrap().is_empty());
    let audits = f.store.admin_audit(10).unwrap();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].error_code.as_deref(), Some("storage_unavailable"));
}
