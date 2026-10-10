//! Durable, single-use MCP enrollment. Only domain-separated credential hashes
//! reach SQLite; setup documents and installation tokens never do.

use std::{
    fs::OpenOptions,
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use rand::{RngCore as _, rngs::OsRng};
use remoteops_domain::ControllerOwnerId;
use remoteops_enrollment::{
    AdvertisedEndpoints, ClientSummary, CreateGrantRequest, CreateGrantResponse, GrantSummary,
    RedeemRequest, RedeemResponse, SetupDocument,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, Row, TransactionBehavior, params};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;
use zeroize::Zeroizing;

const APPLICATION_ID: i64 = 0x524f_454e;
const SCHEMA_VERSION: i64 = 1;
const GRANT_DOMAIN: &[u8] = b"remoteops-setup-grant-v1\0";
const INSTALLATION_DOMAIN: &[u8] = b"remoteops-installation-v1\0";

/// Public errors intentionally never contain supplied credentials.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid enrollment input: {0}")]
    InvalidInput(String),
    #[error("invalid enrollment credential")]
    InvalidCredential,
    #[error("setup grant has expired")]
    Expired,
    #[error("setup grant or installation has been revoked")]
    Revoked,
    #[error("setup grant or installation is already bound")]
    AlreadyRedeemed,
    #[error("advertised enrollment endpoints are not configured")]
    NotConfigured,
    #[error("enrollment storage unavailable: {0}")]
    Storage(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage("database operation failed".into())
    }
}

/// Short alias for hosts that expose the store's error type.
pub type Error = StoreError;
/// An authenticated installation always belongs to the server-configured owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedInstallation {
    pub installation_id: Uuid,
    pub owner_id: ControllerOwnerId,
}

/// SQLite-backed enrollment authority. Independent processes coordinate through
/// SQLite IMMEDIATE transactions, not only through this process-local mutex.
pub struct EnrollmentStore {
    connection: Mutex<Connection>,
    owner_id: ControllerOwnerId,
}

/// Convenient host-facing name.
pub type Store = EnrollmentStore;

impl EnrollmentStore {
    /// Opens or creates the durable enrollment database. The parent directory
    /// must already exist. An existing database is never reset on error.
    ///
    /// # Errors
    /// Fails closed on unavailable/corrupt storage, unsupported schemas, or a
    /// configured owner that differs from the persisted owner.
    pub fn open(path: impl AsRef<Path>, owner_id: ControllerOwnerId) -> Result<Self, StoreError> {
        let path = path.as_ref();
        create_private_database_file(path)?;
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        Self::initialize(connection, owner_id, true)
    }

    /// Creates an ephemeral store for tests or explicitly nonpersistent hosts.
    /// # Errors
    /// Returns an error if SQLite initialization fails.
    pub fn in_memory(owner_id: ControllerOwnerId) -> Result<Self, StoreError> {
        Self::initialize(Connection::open_in_memory()?, owner_id, false)
    }

    fn initialize(
        mut connection: Connection,
        owner_id: ControllerOwnerId,
        durable: bool,
    ) -> Result<Self, StoreError> {
        if owner_id.as_uuid().is_nil() {
            return Err(StoreError::InvalidInput("owner must not be nil".into()));
        }
        connection.busy_timeout(Duration::from_secs(1))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        if durable {
            let mode: String =
                connection.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
            if mode != "wal" {
                return Err(corrupt("durable WAL mode is unavailable"));
            }
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let application: i64 =
            transaction.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = transaction.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if application == 0 && version == 0 {
            let tables: i64 = transaction.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            if tables != 0 {
                return Err(corrupt("unrecognized enrollment database"));
            }
            transaction.execute_batch(SCHEMA)?;
            transaction.execute(
                "INSERT INTO enrollment_meta (key, value) VALUES ('owner_id', ?1)",
                [owner_id.to_string()],
            )?;
            transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if application != APPLICATION_ID || version != SCHEMA_VERSION {
            return Err(corrupt("unsupported enrollment database schema"));
        }
        let persisted_owner: String = transaction.query_row(
            "SELECT value FROM enrollment_meta WHERE key = 'owner_id'",
            [],
            |r| r.get(0),
        )?;
        if persisted_owner != owner_id.to_string() {
            return Err(corrupt("configured owner differs from persisted owner"));
        }
        let integrity: String = transaction.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err(corrupt("database integrity check failed"));
        }
        if transaction
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some()
        {
            return Err(corrupt("database relationship check failed"));
        }
        verify_contents(&transaction, owner_id)?;
        transaction.commit()?;
        Ok(Self {
            connection: Mutex::new(connection),
            owner_id,
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| corrupt("database lock is poisoned"))
    }

    /// Returns validated advertised endpoints, if configured.
    /// # Errors
    /// Fails if stored settings cannot be read or validated.
    pub fn settings(&self) -> Result<Option<AdvertisedEndpoints>, StoreError> {
        read_settings(&*self.connection()?)
    }

    /// Atomically updates explicitly advertised endpoints. Existing grants keep
    /// their own endpoint snapshot, including response-loss retries.
    /// # Errors
    /// Rejects invalid endpoints and fails on storage errors.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Owned requests are the host-facing store API"
    )]
    pub fn set_settings(&self, endpoints: AdvertisedEndpoints) -> Result<(), StoreError> {
        endpoints
            .validate()
            .map_err(|_| StoreError::InvalidInput("invalid advertised endpoints".into()))?;
        let value =
            serde_json::to_string(&endpoints).map_err(|_| corrupt("settings encoding failed"))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO enrollment_settings (singleton, endpoints_json) VALUES (1, ?1)
             ON CONFLICT(singleton) DO UPDATE SET endpoints_json = excluded.endpoints_json",
            [value],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Generates a 256-bit one-time grant and returns its secret once.
    /// # Errors
    /// Rejects unsupported lifetimes/names, missing settings, or storage errors.
    pub fn create_grant(
        &self,
        request: CreateGrantRequest,
        now: DateTime<Utc>,
    ) -> Result<CreateGrantResponse, StoreError> {
        let CreateGrantRequest {
            client_name,
            expires_in_hours,
        } = request;
        validate_client_name(client_name.as_deref())?;
        let hours = expires_in_hours.unwrap_or(24);
        if !matches!(hours, 1 | 24 | 168) {
            return Err(StoreError::InvalidInput(
                "grant lifetime must be 1, 24, or 168 hours".into(),
            ));
        }
        let expires_at = now
            .checked_add_signed(chrono::Duration::hours(i64::from(hours)))
            .ok_or_else(|| StoreError::InvalidInput("grant expiry is out of range".into()))?;
        let mut secret = Zeroizing::new([0_u8; 32]);
        OsRng
            .try_fill_bytes(secret.as_mut())
            .map_err(|_| corrupt("secure random generation failed"))?;
        let secret_hash = hash_secret(&secret[..], GRANT_DOMAIN);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let endpoints = read_settings(&transaction)?.ok_or(StoreError::NotConfigured)?;
        let endpoints_json =
            serde_json::to_string(&endpoints).map_err(|_| corrupt("settings encoding failed"))?;
        let setup = SetupDocument {
            version: 1,
            grant_id: Uuid::new_v4(),
            grant_secret: URL_SAFE_NO_PAD.encode(&secret[..]),
            enrollment_url: endpoints.enrollment_url,
            relay: endpoints.relay,
            server_name: endpoints.server_name,
            relay_ca_pem: endpoints.relay_ca_pem,
            enrollment_ca_pem: endpoints.enrollment_ca_pem,
            expires_at,
        };
        let setup_code = setup
            .encode_code()
            .map_err(|_| corrupt("setup encoding failed"))?;
        transaction.execute(
            "INSERT INTO enrollment_grants
             (grant_id, secret_hash, client_name, created_at, expires_at, revoked, endpoints_json)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
            params![
                setup.grant_id.to_string(),
                &secret_hash[..],
                client_name,
                timestamp(now),
                timestamp(expires_at),
                endpoints_json
            ],
        )?;
        transaction.commit()?;
        Ok(CreateGrantResponse { setup, setup_code })
    }

    /// Lists grant metadata without grant secrets or setup codes.
    /// # Errors
    /// Fails on malformed or unavailable stored records.
    pub fn list_grants(&self, now: DateTime<Utc>) -> Result<Vec<GrantSummary>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "{GRANT_SELECT} ORDER BY created_at DESC, grant_id"
        ))?;
        let rows = statement.query_map([], GrantRecord::from_row)?;
        rows.map(|row| {
            let grant = row?.validate()?;
            Ok(GrantSummary {
                grant_id: grant.grant_id,
                client_name: grant.client_name,
                created_at: grant.created_at,
                expires_at: grant.expires_at,
                state: if grant.revoked {
                    "revoked"
                } else if grant.installation_id.is_some() {
                    "redeemed"
                } else if now >= grant.expires_at {
                    "expired"
                } else {
                    "pending"
                }
                .into(),
                installation_id: grant.installation_id,
            })
        })
        .collect()
    }

    /// Revokes future grant redemption, including response-loss retries. This
    /// does not revoke an installed client. Returns true only on a state change.
    /// # Errors
    /// Fails on unavailable storage.
    pub fn revoke_grant(&self, grant_id: Uuid) -> Result<bool, StoreError> {
        self.revoke(
            "UPDATE enrollment_grants SET revoked = 1 WHERE grant_id = ?1 AND revoked = 0",
            grant_id,
        )
    }

    /// Revokes an installed credential and its grant retries. Returns true only
    /// on a state change. Grant and client revocation are independently durable.
    /// # Errors
    /// Fails on unavailable storage.
    pub fn revoke_client(&self, installation_id: Uuid) -> Result<bool, StoreError> {
        self.revoke(
            "UPDATE enrollment_clients SET revoked = 1 WHERE installation_id = ?1 AND revoked = 0",
            installation_id,
        )
    }

    fn revoke(&self, sql: &str, id: Uuid) -> Result<bool, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(sql, [id.to_string()])? != 0;
        transaction.commit()?;
        Ok(changed)
    }

    /// Atomically consumes a grant. Only the exact same installation ID AND
    /// secret may recover a committed response, even after the grant expires.
    /// # Errors
    /// Rejects malformed, mismatched, expired, revoked, or previously bound
    /// credentials; storage failures never fall back to accepting credentials.
    pub fn redeem(
        &self,
        request: RedeemRequest,
        now: DateTime<Utc>,
    ) -> Result<RedeemResponse, StoreError> {
        if request.grant_id.is_nil() || request.installation_id.is_nil() {
            return Err(StoreError::InvalidCredential);
        }
        let grant_hash = credential_hash(&Zeroizing::new(request.grant_secret), GRANT_DOMAIN)?;
        let installation_hash = credential_hash(
            &Zeroizing::new(request.installation_secret),
            INSTALLATION_DOMAIN,
        )?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let grant =
            read_grant(&transaction, request.grant_id)?.ok_or(StoreError::InvalidCredential)?;
        if !bool::from(grant.secret_hash.ct_eq(&grant_hash)) {
            return Err(StoreError::InvalidCredential);
        }
        if grant.revoked {
            return Err(StoreError::Revoked);
        }
        if let Some(installation_id) = grant.installation_id {
            if installation_id != request.installation_id {
                return Err(StoreError::AlreadyRedeemed);
            }
            let client = read_client(&transaction, installation_id)?
                .ok_or_else(|| corrupt("grant client is missing"))?;
            if !bool::from(client.secret_hash.ct_eq(&installation_hash)) {
                return Err(StoreError::InvalidCredential);
            }
            if client.revoked {
                return Err(StoreError::Revoked);
            }
            let response = grant.checked_response(self.owner_id)?;
            transaction.commit()?;
            return Ok(response);
        }
        if now >= grant.expires_at {
            return Err(StoreError::Expired);
        }
        if read_client(&transaction, request.installation_id)?.is_some() {
            return Err(StoreError::AlreadyRedeemed);
        }
        let response = RedeemResponse {
            installation_id: request.installation_id,
            owner_id: self.owner_id,
            relay: grant.endpoints.relay,
            server_name: grant.endpoints.server_name,
            relay_ca_pem: grant.endpoints.relay_ca_pem,
        };
        let response_json =
            serde_json::to_string(&response).map_err(|_| corrupt("response encoding failed"))?;
        transaction.execute(
            "INSERT INTO enrollment_clients
             (installation_id, secret_hash, client_name, created_at, revoked)
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![
                request.installation_id.to_string(),
                &installation_hash[..],
                grant.client_name,
                timestamp(now)
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE enrollment_grants SET installation_id = ?1, response_json = ?2
             WHERE grant_id = ?3 AND installation_id IS NULL AND revoked = 0",
            params![
                request.installation_id.to_string(),
                response_json,
                request.grant_id.to_string()
            ],
        )?;
        if changed != 1 {
            return Err(corrupt("grant binding was not atomic"));
        }
        transaction.commit()?;
        Ok(response)
    }

    /// Authenticates a versioned installation credential and durably updates
    /// last-seen time. Hosts must assign the AI role and bind the installation ID.
    /// # Errors
    /// Rejects malformed, unknown, or revoked credentials and storage failures.
    pub fn authenticate_token(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<AuthenticatedInstallation, StoreError> {
        let (installation_id, secret) = parse_token(token)?;
        let hash = credential_hash(secret, INSTALLATION_DOMAIN)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let client =
            read_client(&transaction, installation_id)?.ok_or(StoreError::InvalidCredential)?;
        if !bool::from(client.secret_hash.ct_eq(&hash)) {
            return Err(StoreError::InvalidCredential);
        }
        if client.revoked {
            return Err(StoreError::Revoked);
        }
        let last_seen = client.last_seen.map_or(now, |previous| previous.max(now));
        transaction.execute(
            "UPDATE enrollment_clients SET last_seen = ?1 WHERE installation_id = ?2",
            params![timestamp(last_seen), installation_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(AuthenticatedInstallation {
            installation_id,
            owner_id: self.owner_id,
        })
    }

    /// Lists client metadata only, never credential hashes or tokens.
    /// # Errors
    /// Fails on malformed or unavailable stored records.
    pub fn list_clients(&self) -> Result<Vec<ClientSummary>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "{CLIENT_SELECT} ORDER BY created_at DESC, installation_id"
        ))?;
        let rows = statement.query_map([], ClientRecord::from_row)?;
        rows.map(|row| {
            let client = row?.validate()?;
            Ok(ClientSummary {
                installation_id: client.installation_id,
                client_name: client.client_name,
                created_at: client.created_at,
                last_seen: client.last_seen,
                revoked: client.revoked,
            })
        })
        .collect()
    }
}

const SCHEMA: &str = "
CREATE TABLE enrollment_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
CREATE TABLE enrollment_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1), endpoints_json TEXT NOT NULL
) STRICT;
CREATE TABLE enrollment_clients (
    installation_id TEXT PRIMARY KEY, secret_hash BLOB NOT NULL CHECK (length(secret_hash) = 32),
    client_name TEXT, created_at TEXT NOT NULL, last_seen TEXT,
    revoked INTEGER NOT NULL CHECK (revoked IN (0, 1))
) STRICT;
CREATE TABLE enrollment_grants (
    grant_id TEXT PRIMARY KEY, secret_hash BLOB NOT NULL CHECK (length(secret_hash) = 32),
    client_name TEXT, created_at TEXT NOT NULL, expires_at TEXT NOT NULL,
    revoked INTEGER NOT NULL CHECK (revoked IN (0, 1)), endpoints_json TEXT NOT NULL,
    installation_id TEXT UNIQUE REFERENCES enrollment_clients(installation_id), response_json TEXT,
    CHECK ((installation_id IS NULL) = (response_json IS NULL))
) STRICT;";

const GRANT_SELECT: &str = "SELECT grant_id, secret_hash, client_name, created_at, expires_at,
    revoked, endpoints_json, installation_id, response_json FROM enrollment_grants";
const CLIENT_SELECT: &str = "SELECT installation_id, secret_hash, client_name, created_at,
    last_seen, revoked FROM enrollment_clients";

fn create_private_database_file(path: &Path) -> Result<(), StoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(file) => file
            .sync_all()
            .map_err(|_| corrupt("database file could not be synchronized")),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|_| corrupt("database file is unavailable"))?;
            if !metadata.is_file() {
                return Err(corrupt("database path must be a regular file"));
            }
            Ok(())
        }
        Err(_) => Err(corrupt("database file is unavailable")),
    }
}

fn corrupt(message: &str) -> StoreError {
    StoreError::Storage(message.into())
}

fn timestamp(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.with_timezone(&Utc))
        .map_err(|_| corrupt("stored timestamp is invalid"))
}

fn parse_id(value: &str) -> Result<Uuid, StoreError> {
    let id = Uuid::parse_str(value).map_err(|_| corrupt("stored identifier is invalid"))?;
    if id.is_nil() || id.to_string() != value {
        return Err(corrupt("stored identifier is invalid"));
    }
    Ok(id)
}

fn stored_hash(value: Vec<u8>) -> Result<[u8; 32], StoreError> {
    value
        .try_into()
        .map_err(|_| corrupt("stored credential hash is invalid"))
}

fn stored_bool(value: i64) -> Result<bool, StoreError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(corrupt("stored revocation state is invalid")),
    }
}

fn validate_client_name(name: Option<&str>) -> Result<(), StoreError> {
    if name.is_some_and(|value| {
        value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control)
    }) {
        return Err(StoreError::InvalidInput(
            "client name must be 1–256 bytes without control characters".into(),
        ));
    }
    Ok(())
}

fn hash_secret(secret: &[u8], domain: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(secret);
    hash.finalize().into()
}

fn credential_hash(secret: &str, domain: &[u8]) -> Result<[u8; 32], StoreError> {
    // Bound input before decoding, and reject alternative encodings of the same
    // 256 bits so every credential has one canonical representation.
    if secret.len() != 43 {
        return Err(StoreError::InvalidCredential);
    }
    let bytes = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(secret)
            .map_err(|_| StoreError::InvalidCredential)?,
    );
    if bytes.len() != 32 || URL_SAFE_NO_PAD.encode(&bytes[..]) != secret {
        return Err(StoreError::InvalidCredential);
    }
    Ok(hash_secret(&bytes, domain))
}

fn parse_token(token: &str) -> Result<(Uuid, &str), StoreError> {
    if token.len() != 5 + 36 + 1 + 43 {
        return Err(StoreError::InvalidCredential);
    }
    let remainder = token
        .strip_prefix("roc1.")
        .ok_or(StoreError::InvalidCredential)?;
    let (id, secret) = remainder
        .split_once('.')
        .ok_or(StoreError::InvalidCredential)?;
    let installation_id = Uuid::parse_str(id).map_err(|_| StoreError::InvalidCredential)?;
    if installation_id.is_nil() || installation_id.to_string() != id {
        return Err(StoreError::InvalidCredential);
    }
    Ok((installation_id, secret))
}

fn read_settings(connection: &Connection) -> Result<Option<AdvertisedEndpoints>, StoreError> {
    let value: Option<String> = connection
        .query_row(
            "SELECT endpoints_json FROM enrollment_settings WHERE singleton = 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    value.map(|json| decode_endpoints(&json)).transpose()
}

fn decode_endpoints(json: &str) -> Result<AdvertisedEndpoints, StoreError> {
    let endpoints: AdvertisedEndpoints =
        serde_json::from_str(json).map_err(|_| corrupt("stored endpoints are invalid"))?;
    endpoints
        .validate()
        .map_err(|_| corrupt("stored endpoints are invalid"))?;
    Ok(endpoints)
}

struct GrantRecord {
    grant_id: String,
    secret_hash: Vec<u8>,
    client_name: Option<String>,
    created_at: String,
    expires_at: String,
    revoked: i64,
    endpoints_json: String,
    installation_id: Option<String>,
    response_json: Option<String>,
}

struct ValidatedGrant {
    grant_id: Uuid,
    secret_hash: [u8; 32],
    client_name: Option<String>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    revoked: bool,
    endpoints: AdvertisedEndpoints,
    installation_id: Option<Uuid>,
    response_json: Option<String>,
}

impl GrantRecord {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            grant_id: row.get(0)?,
            secret_hash: row.get(1)?,
            client_name: row.get(2)?,
            created_at: row.get(3)?,
            expires_at: row.get(4)?,
            revoked: row.get(5)?,
            endpoints_json: row.get(6)?,
            installation_id: row.get(7)?,
            response_json: row.get(8)?,
        })
    }

    fn validate(self) -> Result<ValidatedGrant, StoreError> {
        validate_client_name(self.client_name.as_deref())
            .map_err(|_| corrupt("stored client name is invalid"))?;
        let created_at = parse_timestamp(&self.created_at)?;
        let expires_at = parse_timestamp(&self.expires_at)?;
        if expires_at <= created_at
            || self.installation_id.is_some() != self.response_json.is_some()
        {
            return Err(corrupt("stored grant state is invalid"));
        }
        Ok(ValidatedGrant {
            grant_id: parse_id(&self.grant_id)?,
            secret_hash: stored_hash(self.secret_hash)?,
            client_name: self.client_name,
            created_at,
            expires_at,
            revoked: stored_bool(self.revoked)?,
            endpoints: decode_endpoints(&self.endpoints_json)?,
            installation_id: self.installation_id.as_deref().map(parse_id).transpose()?,
            response_json: self.response_json,
        })
    }
}

impl ValidatedGrant {
    fn checked_response(&self, owner_id: ControllerOwnerId) -> Result<RedeemResponse, StoreError> {
        let json = self
            .response_json
            .as_deref()
            .ok_or_else(|| corrupt("stored response is missing"))?;
        let response: RedeemResponse =
            serde_json::from_str(json).map_err(|_| corrupt("stored response is invalid"))?;
        if Some(response.installation_id) != self.installation_id
            || response.owner_id != owner_id
            || response.relay != self.endpoints.relay
            || response.server_name != self.endpoints.server_name
            || response.relay_ca_pem != self.endpoints.relay_ca_pem
        {
            return Err(corrupt("stored response binding is invalid"));
        }
        Ok(response)
    }
}

struct ClientRecord {
    installation_id: String,
    secret_hash: Vec<u8>,
    client_name: Option<String>,
    created_at: String,
    last_seen: Option<String>,
    revoked: i64,
}

struct ValidatedClient {
    installation_id: Uuid,
    secret_hash: [u8; 32],
    client_name: Option<String>,
    created_at: DateTime<Utc>,
    last_seen: Option<DateTime<Utc>>,
    revoked: bool,
}

impl ClientRecord {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            installation_id: row.get(0)?,
            secret_hash: row.get(1)?,
            client_name: row.get(2)?,
            created_at: row.get(3)?,
            last_seen: row.get(4)?,
            revoked: row.get(5)?,
        })
    }

    fn validate(self) -> Result<ValidatedClient, StoreError> {
        validate_client_name(self.client_name.as_deref())
            .map_err(|_| corrupt("stored client name is invalid"))?;
        Ok(ValidatedClient {
            installation_id: parse_id(&self.installation_id)?,
            secret_hash: stored_hash(self.secret_hash)?,
            client_name: self.client_name,
            created_at: parse_timestamp(&self.created_at)?,
            last_seen: self.last_seen.as_deref().map(parse_timestamp).transpose()?,
            revoked: stored_bool(self.revoked)?,
        })
    }
}

fn read_grant(connection: &Connection, id: Uuid) -> Result<Option<ValidatedGrant>, StoreError> {
    connection
        .query_row(
            &format!("{GRANT_SELECT} WHERE grant_id = ?1"),
            [id.to_string()],
            GrantRecord::from_row,
        )
        .optional()?
        .map(GrantRecord::validate)
        .transpose()
}

fn read_client(connection: &Connection, id: Uuid) -> Result<Option<ValidatedClient>, StoreError> {
    connection
        .query_row(
            &format!("{CLIENT_SELECT} WHERE installation_id = ?1"),
            [id.to_string()],
            ClientRecord::from_row,
        )
        .optional()?
        .map(ClientRecord::validate)
        .transpose()
}

fn verify_contents(connection: &Connection, owner_id: ControllerOwnerId) -> Result<(), StoreError> {
    read_settings(connection)?;
    let mut grants = connection.prepare(GRANT_SELECT)?;
    for row in grants.query_map([], GrantRecord::from_row)? {
        let grant = row?.validate()?;
        if let Some(id) = grant.installation_id {
            grant.checked_response(owner_id)?;
            let client =
                read_client(connection, id)?.ok_or_else(|| corrupt("grant client is missing"))?;
            if client.client_name != grant.client_name {
                return Err(corrupt("client name binding is invalid"));
            }
        }
    }
    let mut clients = connection.prepare(CLIENT_SELECT)?;
    for row in clients.query_map([], ClientRecord::from_row)? {
        let client = row?.validate()?;
        let references: i64 = connection.query_row(
            "SELECT count(*) FROM enrollment_grants WHERE installation_id = ?1",
            [client.installation_id.to_string()],
            |r| r.get(0),
        )?;
        if references != 1 {
            return Err(corrupt("installation grant binding is missing"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
