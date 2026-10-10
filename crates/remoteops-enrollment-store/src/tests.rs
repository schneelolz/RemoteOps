use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use super::*;

struct TestDatabase {
    directory: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "remoteops-enrollment-store-test-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        Self { directory }
    }
    fn path(&self) -> PathBuf {
        self.directory.join("enrollment.sqlite3")
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn endpoints() -> AdvertisedEndpoints {
    AdvertisedEndpoints {
        enrollment_url: "https://enroll.example.test/api/mcp/enroll".into(),
        relay: "relay.example.test:7443".into(),
        server_name: "relay.example.test".into(),
        relay_ca_pem: None,
        enrollment_ca_pem: None,
    }
}

fn store() -> EnrollmentStore {
    let store = EnrollmentStore::in_memory(ControllerOwnerId::new()).unwrap();
    store.set_settings(endpoints()).unwrap();
    store
}

fn create(store: &EnrollmentStore, now: DateTime<Utc>) -> CreateGrantResponse {
    store
        .create_grant(
            CreateGrantRequest {
                client_name: Some("My workstation".into()),
                expires_in_hours: Some(1),
            },
            now,
        )
        .unwrap()
}

fn request(grant: &CreateGrantResponse) -> RedeemRequest {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    RedeemRequest {
        grant_id: grant.setup.grant_id,
        grant_secret: grant.setup.grant_secret.clone(),
        installation_id: Uuid::new_v4(),
        installation_secret: URL_SAFE_NO_PAD.encode(bytes),
    }
}

fn token(request: &RedeemRequest) -> String {
    format!(
        "roc1.{}.{}",
        request.installation_id, request.installation_secret
    )
}

#[test]
fn first_redemption_is_atomic_and_identity_is_server_assigned() {
    let store = store();
    let now = Utc::now();
    let grant = create(&store, now);
    let request = request(&grant);
    let response = store.redeem(request.clone(), now).unwrap();
    assert_eq!(response.owner_id, store.owner_id);
    assert_eq!(response.installation_id, request.installation_id);
    assert_eq!(response.relay, endpoints().relay);
    let authenticated = store.authenticate_token(&token(&request), now).unwrap();
    assert_eq!(authenticated.owner_id, store.owner_id);
    assert_eq!(authenticated.installation_id, request.installation_id);
    let clients = store.list_clients().unwrap();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0].client_name.as_deref(), Some("My workstation"));
    assert_eq!(clients[0].last_seen, Some(now));
    let grants = store.list_grants(now).unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].state, "redeemed");
    assert_eq!(grants[0].installation_id, Some(request.installation_id));
}

#[test]
fn response_loss_retry_survives_expiry_restart_and_settings_changes() {
    let database = TestDatabase::new();
    let owner = ControllerOwnerId::new();
    let now = Utc::now();
    let (request, expected) = {
        let store = EnrollmentStore::open(database.path(), owner).unwrap();
        store.set_settings(endpoints()).unwrap();
        let grant = create(&store, now);
        let request = request(&grant);
        let mut changed = endpoints();
        changed.relay = "replacement.example.test:8443".into();
        changed.server_name = "replacement.example.test".into();
        store.set_settings(changed).unwrap();
        let response = store.redeem(request.clone(), now).unwrap();
        assert_eq!(
            response.relay,
            endpoints().relay,
            "grant snapshot survives pre-redemption settings change"
        );
        (request, serde_json::to_value(response).unwrap())
    };
    let store = EnrollmentStore::open(database.path(), owner).unwrap();
    let retry = store
        .redeem(request.clone(), now + chrono::Duration::days(30))
        .unwrap();
    assert_eq!(serde_json::to_value(retry).unwrap(), expected);
    assert_eq!(store.list_clients().unwrap().len(), 1);
    assert!(
        store
            .authenticate_token(&token(&request), now + chrono::Duration::days(30))
            .is_ok()
    );
}

#[test]
fn expired_unconsumed_grants_fail_at_boundary() {
    let store = store();
    let now = Utc::now();
    let grant = create(&store, now);
    let request = request(&grant);
    assert!(matches!(
        store.redeem(request, grant.setup.expires_at),
        Err(StoreError::Expired)
    ));
    assert!(store.list_clients().unwrap().is_empty());
    assert_eq!(
        store.list_grants(grant.setup.expires_at).unwrap()[0].state,
        "expired"
    );
}

#[test]
fn only_exact_installation_id_and_secret_can_retry() {
    let store = store();
    let now = Utc::now();
    let grant = create(&store, now);
    let good = request(&grant);
    store.redeem(good.clone(), now).unwrap();
    let mut wrong = good.clone();
    wrong.installation_id = Uuid::new_v4();
    assert!(matches!(
        store.redeem(wrong, now),
        Err(StoreError::AlreadyRedeemed)
    ));
    let mut wrong = good.clone();
    wrong.installation_secret = URL_SAFE_NO_PAD.encode([1; 32]);
    assert!(matches!(
        store.redeem(wrong, now),
        Err(StoreError::InvalidCredential)
    ));
    assert!(store.redeem(good, now).is_ok());
}

#[test]
fn grant_revocation_blocks_retries_but_preserves_installed_credential() {
    let database = TestDatabase::new();
    let owner = ControllerOwnerId::new();
    let now = Utc::now();
    let request = {
        let store = EnrollmentStore::open(database.path(), owner).unwrap();
        store.set_settings(endpoints()).unwrap();
        let grant = create(&store, now);
        let request = request(&grant);
        store.redeem(request.clone(), now).unwrap();
        assert!(store.revoke_grant(grant.setup.grant_id).unwrap());
        assert!(!store.revoke_grant(grant.setup.grant_id).unwrap());
        assert!(!store.revoke_grant(Uuid::new_v4()).unwrap());
        request
    };
    let store = EnrollmentStore::open(database.path(), owner).unwrap();
    assert!(matches!(
        store.redeem(request.clone(), now),
        Err(StoreError::Revoked)
    ));
    assert!(store.authenticate_token(&token(&request), now).is_ok());
    assert_eq!(store.list_grants(now).unwrap()[0].state, "revoked");
}

#[test]
fn unredeemed_grant_revocation_and_client_revocation_are_durable() {
    let database = TestDatabase::new();
    let owner = ControllerOwnerId::new();
    let now = Utc::now();
    let (unredeemed, client) = {
        let store = EnrollmentStore::open(database.path(), owner).unwrap();
        store.set_settings(endpoints()).unwrap();
        let unredeemed = create(&store, now);
        store.revoke_grant(unredeemed.setup.grant_id).unwrap();
        let client = request(&create(&store, now));
        store.redeem(client.clone(), now).unwrap();
        assert!(store.revoke_client(client.installation_id).unwrap());
        assert!(!store.revoke_client(client.installation_id).unwrap());
        assert!(!store.revoke_client(Uuid::new_v4()).unwrap());
        (request(&unredeemed), client)
    };
    let store = EnrollmentStore::open(database.path(), owner).unwrap();
    assert!(matches!(
        store.redeem(unredeemed, now),
        Err(StoreError::Revoked)
    ));
    assert!(matches!(
        store.redeem(client.clone(), now),
        Err(StoreError::Revoked)
    ));
    assert!(matches!(
        store.authenticate_token(&token(&client), now),
        Err(StoreError::Revoked)
    ));
    assert!(store.list_clients().unwrap()[0].revoked);
}

#[test]
fn installation_collision_never_replaces_existing_credentials() {
    let store = store();
    let now = Utc::now();
    let first = request(&create(&store, now));
    store.redeem(first.clone(), now).unwrap();
    let mut second = request(&create(&store, now));
    second.installation_id = first.installation_id;
    assert!(matches!(
        store.redeem(second.clone(), now),
        Err(StoreError::AlreadyRedeemed)
    ));
    assert!(store.authenticate_token(&token(&first), now).is_ok());
    assert!(matches!(
        store.authenticate_token(&token(&second), now),
        Err(StoreError::InvalidCredential)
    ));
    second
        .installation_secret
        .clone_from(&first.installation_secret);
    assert!(matches!(
        store.redeem(second, now),
        Err(StoreError::AlreadyRedeemed)
    ));
    assert_eq!(store.list_clients().unwrap().len(), 1);
}

#[test]
fn concurrent_redemptions_through_one_handle_have_exactly_one_winner() {
    let store = Arc::new(store());
    let now = Utc::now();
    let grant = create(&store, now);
    let barrier = Arc::new(Barrier::new(12));
    let workers: Vec<_> = (0..12)
        .map(|_| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let request = request(&grant);
            thread::spawn(move || {
                barrier.wait();
                store.redeem(request, now)
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| matches!(error, StoreError::AlreadyRedeemed))
    );
    assert_eq!(store.list_clients().unwrap().len(), 1);
}

#[test]
fn independent_sqlite_handles_atomically_consume_a_single_grant() {
    let database = TestDatabase::new();
    let owner = ControllerOwnerId::new();
    let store = EnrollmentStore::open(database.path(), owner).unwrap();
    store.set_settings(endpoints()).unwrap();
    let now = Utc::now();
    let grant = create(&store, now);
    let handles: Vec<_> = (0..8)
        .map(|_| EnrollmentStore::open(database.path(), owner).unwrap())
        .collect();
    let barrier = Arc::new(Barrier::new(handles.len()));
    let workers: Vec<_> = handles
        .into_iter()
        .map(|store| {
            let barrier = Arc::clone(&barrier);
            let request = request(&grant);
            thread::spawn(move || {
                barrier.wait();
                store.redeem(request, now)
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| matches!(error, StoreError::AlreadyRedeemed))
    );
    assert_eq!(store.list_clients().unwrap().len(), 1);
}

#[test]
fn simultaneous_authorized_retries_return_the_same_committed_response() {
    let store = Arc::new(store());
    let now = Utc::now();
    let request = request(&create(&store, now));
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let request = request.clone();
            thread::spawn(move || {
                barrier.wait();
                serde_json::to_value(store.redeem(request, now).unwrap()).unwrap()
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert!(results.windows(2).all(|values| values[0] == values[1]));
    assert_eq!(store.list_clients().unwrap().len(), 1);
}

#[test]
fn database_and_wal_contain_hashes_but_no_setup_or_installation_secrets() {
    let database = TestDatabase::new();
    let owner = ControllerOwnerId::new();
    let store = EnrollmentStore::open(database.path(), owner).unwrap();
    store.set_settings(endpoints()).unwrap();
    let now = Utc::now();
    let grant = create(&store, now);
    let request = request(&grant);
    store.redeem(request.clone(), now).unwrap();
    store.authenticate_token(&token(&request), now).unwrap();
    let token = token(&request);
    let setup_json = serde_json::to_string(&grant.setup).unwrap();
    let decoded_grant = URL_SAFE_NO_PAD.decode(&request.grant_secret).unwrap();
    let decoded_installation = URL_SAFE_NO_PAD
        .decode(&request.installation_secret)
        .unwrap();
    let secrets = [
        grant.setup_code.as_bytes(),
        setup_json.as_bytes(),
        token.as_bytes(),
        request.grant_secret.as_bytes(),
        request.installation_secret.as_bytes(),
        decoded_grant.as_slice(),
        decoded_installation.as_slice(),
    ];
    let scan = || {
        for entry in std::fs::read_dir(&database.directory).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            for secret in secrets {
                assert!(
                    !bytes.windows(secret.len()).any(|window| window == secret),
                    "plaintext credential reached durable storage"
                );
            }
        }
    };
    assert!(
        database
            .path()
            .with_file_name("enrollment.sqlite3-wal")
            .exists()
    );
    scan();
    let connection = store.connection().unwrap();
    let grant_hash: Vec<u8> = connection
        .query_row("SELECT secret_hash FROM enrollment_grants", [], |r| {
            r.get(0)
        })
        .unwrap();
    let installation_hash: Vec<u8> = connection
        .query_row("SELECT secret_hash FROM enrollment_clients", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        grant_hash,
        credential_hash(&request.grant_secret, GRANT_DOMAIN).unwrap()
    );
    assert_eq!(
        installation_hash,
        credential_hash(&request.installation_secret, INSTALLATION_DOMAIN).unwrap()
    );
    drop(connection);
    drop(store);
    scan();
    assert!(
        EnrollmentStore::open(database.path(), owner)
            .unwrap()
            .authenticate_token(&token, now)
            .is_ok()
    );
}

#[test]
fn settings_are_validated_persisted_and_required_before_grant_creation() {
    let database = TestDatabase::new();
    let owner = ControllerOwnerId::new();
    let store = EnrollmentStore::open(database.path(), owner).unwrap();
    assert!(store.settings().unwrap().is_none());
    assert!(matches!(
        store.create_grant(
            CreateGrantRequest {
                client_name: None,
                expires_in_hours: None
            },
            Utc::now()
        ),
        Err(StoreError::NotConfigured)
    ));
    let mut bad = endpoints();
    bad.enrollment_url = "http://relay.example.test/api/mcp/enroll".into();
    assert!(matches!(
        store.set_settings(bad),
        Err(StoreError::InvalidInput(_))
    ));
    assert!(store.settings().unwrap().is_none());
    store.set_settings(endpoints()).unwrap();
    drop(store);
    let store = EnrollmentStore::open(database.path(), owner).unwrap();
    assert_eq!(store.settings().unwrap(), Some(endpoints()));
}

#[test]
fn grant_expiry_options_and_names_are_bounded() {
    let store = store();
    let now = Utc::now();
    for (requested, expected) in [(None, 24), (Some(1), 1), (Some(24), 24), (Some(168), 168)] {
        let grant = store
            .create_grant(
                CreateGrantRequest {
                    client_name: None,
                    expires_in_hours: requested,
                },
                now,
            )
            .unwrap();
        assert_eq!(
            grant.setup.expires_at,
            now + chrono::Duration::hours(expected)
        );
        assert_eq!(grant.setup.grant_secret.len(), 43);
        assert_eq!(
            SetupDocument::decode(&grant.setup_code).unwrap().grant_id,
            grant.setup.grant_id
        );
    }
    for lifetime in [0, 2, 48, 169, u32::MAX] {
        assert!(matches!(
            store.create_grant(
                CreateGrantRequest {
                    client_name: None,
                    expires_in_hours: Some(lifetime)
                },
                now
            ),
            Err(StoreError::InvalidInput(_))
        ));
    }
    for name in [
        String::new(),
        "   ".into(),
        "name\nlog injection".into(),
        "a".repeat(257),
    ] {
        assert!(matches!(
            store.create_grant(
                CreateGrantRequest {
                    client_name: Some(name),
                    expires_in_hours: None
                },
                now
            ),
            Err(StoreError::InvalidInput(_))
        ));
    }
}

#[test]
fn malformed_and_wrong_secrets_do_not_consume_the_grant() {
    let store = store();
    let now = Utc::now();
    let good = request(&create(&store, now));
    for invalid in [
        String::new(),
        "password".into(),
        URL_SAFE_NO_PAD.encode([1; 31]),
        format!("{}=", good.installation_secret),
        URL_SAFE_NO_PAD.encode([2; 33]),
        "a".repeat(100_000),
    ] {
        let mut bad = good.clone();
        bad.installation_secret = invalid.clone();
        assert!(matches!(
            store.redeem(bad, now),
            Err(StoreError::InvalidCredential)
        ));
        let mut bad = good.clone();
        bad.grant_secret = invalid;
        assert!(matches!(
            store.redeem(bad, now),
            Err(StoreError::InvalidCredential)
        ));
    }
    let mut wrong = good.clone();
    wrong.grant_secret = URL_SAFE_NO_PAD.encode([0; 32]);
    assert!(matches!(
        store.redeem(wrong, now),
        Err(StoreError::InvalidCredential)
    ));
    assert!(store.list_clients().unwrap().is_empty());
    assert!(store.redeem(good, now).is_ok());
}

#[test]
fn malformed_unknown_and_version_mismatched_tokens_fail_closed() {
    let store = store();
    let now = Utc::now();
    let request = request(&create(&store, now));
    store.redeem(request.clone(), now).unwrap();
    let valid = token(&request);
    for invalid in [
        String::new(),
        request.installation_secret.clone(),
        valid.replace("roc1.", "roc2."),
        format!("{valid}.extra"),
        format!("roc1.{}.{}", Uuid::nil(), request.installation_secret),
        format!("roc1.{}.{}", Uuid::new_v4(), request.installation_secret),
        format!(
            "roc1.{}.{}",
            request.installation_id,
            URL_SAFE_NO_PAD.encode([0; 32])
        ),
    ] {
        assert!(matches!(
            store.authenticate_token(&invalid, now),
            Err(StoreError::InvalidCredential)
        ));
    }
    assert_eq!(store.list_clients().unwrap()[0].last_seen, None);
}

#[test]
fn last_seen_does_not_regress() {
    let store = store();
    let now = Utc::now();
    let request = request(&create(&store, now));
    store.redeem(request.clone(), now).unwrap();
    let later = now + chrono::Duration::hours(1);
    store.authenticate_token(&token(&request), later).unwrap();
    store.authenticate_token(&token(&request), now).unwrap();
    assert_eq!(store.list_clients().unwrap()[0].last_seen, Some(later));
}

#[test]
fn failed_commit_path_never_consumes_grant_or_leaves_a_client() {
    let store = store();
    let now = Utc::now();
    let request = request(&create(&store, now));
    store.connection().unwrap().execute_batch(
        "CREATE TRIGGER deny_binding BEFORE UPDATE ON enrollment_grants BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;"
    ).unwrap();
    assert!(matches!(
        store.redeem(request.clone(), now),
        Err(StoreError::Storage(_))
    ));
    assert!(store.list_clients().unwrap().is_empty());
    assert_eq!(store.list_grants(now).unwrap()[0].state, "pending");
    store
        .connection()
        .unwrap()
        .execute_batch("DROP TRIGGER deny_binding;")
        .unwrap();
    assert!(store.redeem(request, now).is_ok());
}

#[test]
fn authentication_storage_failure_never_returns_an_identity() {
    let store = store();
    let now = Utc::now();
    let request = request(&create(&store, now));
    store.redeem(request.clone(), now).unwrap();
    store.connection().unwrap().execute_batch(
        "CREATE TRIGGER deny_seen BEFORE UPDATE ON enrollment_clients BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;"
    ).unwrap();
    assert!(matches!(
        store.authenticate_token(&token(&request), now),
        Err(StoreError::Storage(_))
    ));
}

#[test]
fn corrupt_non_sqlite_storage_is_never_reset() {
    let database = TestDatabase::new();
    let bytes = b"not a SQLite database; preserve this evidence";
    std::fs::write(database.path(), bytes).unwrap();
    assert!(matches!(
        EnrollmentStore::open(database.path(), ControllerOwnerId::new()),
        Err(StoreError::Storage(_))
    ));
    assert_eq!(std::fs::read(database.path()).unwrap(), bytes);
}

#[test]
fn owner_change_and_unknown_schema_are_rejected() {
    let database = TestDatabase::new();
    let owner = ControllerOwnerId::new();
    drop(EnrollmentStore::open(database.path(), owner).unwrap());
    assert!(matches!(
        EnrollmentStore::open(database.path(), ControllerOwnerId::new()),
        Err(StoreError::Storage(_))
    ));
    let connection = Connection::open(database.path()).unwrap();
    connection.pragma_update(None, "user_version", 999).unwrap();
    drop(connection);
    assert!(matches!(
        EnrollmentStore::open(database.path(), owner),
        Err(StoreError::Storage(_))
    ));
}

#[test]
fn malformed_settings_and_response_snapshots_fail_closed_on_restart() {
    let database = TestDatabase::new();
    let owner = ControllerOwnerId::new();
    let store = EnrollmentStore::open(database.path(), owner).unwrap();
    store.set_settings(endpoints()).unwrap();
    store
        .connection()
        .unwrap()
        .execute("UPDATE enrollment_settings SET endpoints_json = '{}'", [])
        .unwrap();
    assert!(matches!(store.settings(), Err(StoreError::Storage(_))));
    drop(store);
    assert!(matches!(
        EnrollmentStore::open(database.path(), owner),
        Err(StoreError::Storage(_))
    ));

    let database = TestDatabase::new();
    let store = EnrollmentStore::open(database.path(), owner).unwrap();
    store.set_settings(endpoints()).unwrap();
    let now = Utc::now();
    let request = request(&create(&store, now));
    let mut response = store.redeem(request.clone(), now).unwrap();
    response.owner_id = ControllerOwnerId::new();
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE enrollment_grants SET response_json = ?1",
            [serde_json::to_string(&response).unwrap()],
        )
        .unwrap();
    assert!(matches!(
        store.redeem(request, now),
        Err(StoreError::Storage(_))
    ));
    drop(store);
    assert!(matches!(
        EnrollmentStore::open(database.path(), owner),
        Err(StoreError::Storage(_))
    ));
}

#[test]
fn new_database_uses_full_wal_durability_and_private_file_permissions() {
    let database = TestDatabase::new();
    let store = EnrollmentStore::open(database.path(), ControllerOwnerId::new()).unwrap();
    let connection = store.connection().unwrap();
    let journal: String = connection
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    let synchronous: i64 = connection
        .pragma_query_value(None, "synchronous", |r| r.get(0))
        .unwrap();
    let foreign_keys: i64 = connection
        .pragma_query_value(None, "foreign_keys", |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    assert_eq!(synchronous, 2);
    assert_eq!(foreign_keys, 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(database.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[cfg(unix)]
#[test]
fn database_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;
    let database = TestDatabase::new();
    let actual = database.directory.join("actual.sqlite3");
    std::fs::write(&actual, b"").unwrap();
    symlink(actual, database.path()).unwrap();
    assert!(matches!(
        EnrollmentStore::open(database.path(), ControllerOwnerId::new()),
        Err(StoreError::Storage(_))
    ));
}
