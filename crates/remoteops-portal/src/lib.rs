//! 门户单目标授权存储；SQLite 事务是邀请消费和撤销的持久边界。
mod types;
pub use types::*;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit as _, Mac as _};
use rand::{RngCore as _, rngs::OsRng};
use remoteops_domain::{AgentInstanceId, ControllerOwnerId};
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};
use std::{
    fs::OpenOptions,
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;
use zeroize::Zeroizing;

/// 门户持久授权操作的返回结果。
pub type PortalResult<T> = Result<T, PortalError>;
const APPLICATION_ID: i64 = 0x524f_5054;
const SCHEMA_VERSION: i64 = 1;
const INVITATION_SECONDS: i64 = 600;

/// 门户存储拥有独立数据库；运行态修改由 Relay 在同一目标状态锁内协调。
pub struct PortalStore {
    // 事务共用同一连接，运行态协调由上层状态锁负责。
    connection: Mutex<Connection>,
    // 仅在本地授权数据库内持久保存的派生主密钥。
    key: Zeroizing<Vec<u8>>,
    // 此 Relay 部署接受的控制者所有者。
    owner_id: ControllerOwnerId,
}

impl PortalStore {
    /// 打开持久存储；已有未知或损坏数据库不会被覆盖。
    ///
    /// # Errors
    /// 路径不可用、数据库损坏或版本及所有者不匹配时返回错误。
    pub fn open(path: impl AsRef<Path>, owner_id: ControllerOwnerId) -> PortalResult<Self> {
        let path = path.as_ref();
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::symlink_metadata(path)
                    .map_err(|_| PortalError::Storage)?
                    .file_type()
                    .is_symlink()
                {
                    return Err(PortalError::Storage);
                }
            }
            Err(_) => return Err(PortalError::Storage),
        }
        let connection = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        Self::initialize(connection, owner_id, true)
    }

    /// 创建仅用于隔离测试的内存存储。
    ///
    /// # Errors
    /// 所有者标识无效或数据库初始化失败时返回错误。
    pub fn in_memory(owner_id: ControllerOwnerId) -> PortalResult<Self> {
        Self::initialize(Connection::open_in_memory()?, owner_id, false)
    }

    fn initialize(
        mut connection: Connection,
        owner_id: ControllerOwnerId,
        durable: bool,
    ) -> PortalResult<Self> {
        if owner_id.as_uuid().is_nil() {
            return Err(PortalError::InvalidInput);
        }
        connection.busy_timeout(Duration::from_secs(3))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        if durable {
            let mode: String = connection.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
            if mode != "wal" {
                return Err(PortalError::Storage);
            }
        }
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let application: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if application == 0 && version == 0 {
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            if count != 0 {
                return Err(PortalError::Storage);
            }
            tx.execute_batch(SCHEMA)?;
            let mut key = Zeroizing::new(vec![0_u8; 32]);
            OsRng.fill_bytes(&mut key);
            tx.execute(
                "INSERT INTO portal_meta (id,owner_id,key) VALUES (1,?1,?2)",
                params![owner_id.to_string(), key.as_slice()],
            )?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if application != APPLICATION_ID || version != SCHEMA_VERSION {
            return Err(PortalError::Storage);
        }
        let (stored_owner, key): (String, Vec<u8>) =
            tx.query_row("SELECT owner_id,key FROM portal_meta WHERE id=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        if stored_owner != owner_id.to_string() || key.len() != 32 {
            return Err(PortalError::Storage);
        }
        let check: String = tx.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok"
            || tx
                .prepare("PRAGMA foreign_key_check")?
                .query([])?
                .next()?
                .is_some()
        {
            return Err(PortalError::Storage);
        }
        tx.commit()?;
        Ok(Self {
            connection: Mutex::new(connection),
            key: Zeroizing::new(key),
            owner_id,
        })
    }

    fn db(&self) -> PortalResult<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| PortalError::Storage)
    }
    fn mac(&self, domain: &[u8], value: &[u8]) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("固定长度 HMAC 密钥有效");
        mac.update(domain);
        mac.update(value);
        mac.finalize().into_bytes().into()
    }
    fn code(&self, id: Uuid, nonce: i64) -> String {
        let bytes = self.mac(
            b"remoteops-portal-code-v1\0",
            format!("{id}:{nonce}").as_bytes(),
        );
        // 拒绝采样使九位码分布均匀；被拒绝时增加 nonce。
        format!(
            "{:09}",
            u32::from_be_bytes(bytes[..4].try_into().expect("摘要长度固定")) % 1_000_000_000
        )
    }
    fn resume_token(&self, id: Uuid) -> String {
        format!(
            "ropr1.{id}.{}",
            URL_SAFE_NO_PAD.encode(self.mac(b"remoteops-portal-resume-v1\0", id.as_bytes()))
        )
    }

    /// 管理员创建集成主体，明文凭据仅此次返回。
    ///
    /// # Errors
    /// 所有者、名称或资产白名单无效，或持久化失败时返回错误。
    pub fn create_principal(
        &self,
        request: CreatePrincipalRequest,
    ) -> PortalResult<CreatedPrincipal> {
        if request.owner_id != self.owner_id
            || !valid_label(&request.name)
            || request.allowed_assets.is_empty()
            || request.allowed_assets.len() > 1024
            || request.allowed_assets.iter().any(|s| !valid_asset(s))
        {
            return Err(PortalError::InvalidInput);
        }
        let mut assets = request.allowed_assets;
        assets.sort();
        assets.dedup();
        let principal = Principal {
            principal_id: Uuid::new_v4(),
            name: request.name,
            owner_id: self.owner_id,
            allowed_assets: assets,
            revoked: false,
        };
        let token = format!("ropp1.{}.{}", principal.principal_id, random_secret());
        self.db()?.execute(
            "INSERT INTO portal_principals(id,credential_hash,data,revoked) VALUES(?1,?2,?3,0)",
            params![
                principal.principal_id.to_string(),
                hash(token.as_bytes()).as_slice(),
                encode(&principal)?
            ],
        )?;
        Ok(CreatedPrincipal { principal, token })
    }

    /// 认证门户调用方；不能以请求正文中的系统标识代替凭据。
    ///
    /// # Errors
    /// 凭据无效、主体已撤销或存储不可用时返回错误。
    pub fn authenticate_principal(&self, token: &str) -> PortalResult<Principal> {
        let id = parse_token(token, "ropp1")?;
        let db = self.db()?;
        let expected: Option<Vec<u8>> = db
            .query_row(
                "SELECT credential_hash FROM portal_principals WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if !expected.is_some_and(|v| bool::from(v.ct_eq(hash(token.as_bytes()).as_slice()))) {
            return Err(PortalError::Unauthorized);
        }
        principal(&db, id)
    }

    /// 管理员登记受控资产；白名单与集成归属由服务端验证。
    ///
    /// # Errors
    /// 主体无权登记、资产已存在、参数无效或持久化失败时返回错误。
    pub fn create_asset(&self, request: CreateAssetRequest) -> PortalResult<CreatedAsset> {
        if !valid_asset(&request.asset_id) {
            return Err(PortalError::InvalidInput);
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owner = principal(&tx, request.principal_id)?;
        check_acl(&owner, &request.asset_id)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM portal_assets WHERE principal_id=?1 AND asset_id=?2)",
            params![request.principal_id.to_string(), request.asset_id],
            |r| r.get(0),
        )?;
        if exists {
            return Err(PortalError::Conflict);
        }
        let asset = Asset {
            target_id: Uuid::new_v4().to_string(),
            asset_id: request.asset_id,
            principal_id: request.principal_id,
            agent_id: None,
            revision: 1,
        };
        let token = format!("rope1.{}.{}", asset.target_id, random_secret());
        tx.execute("INSERT INTO portal_assets(target_id,principal_id,asset_id,enrollment_hash,agent_id,revision) VALUES(?1,?2,?3,?4,NULL,1)",params![asset.target_id,asset.principal_id.to_string(),asset.asset_id,hash(token.as_bytes()).as_slice()])?;
        tx.commit()?;
        Ok(CreatedAsset {
            asset,
            enrollment_token: token,
        })
    }

    /// 一次登记锁定准确 Agent 身份；同身份重试允许，其他身份不能抢占。
    ///
    /// # Errors
    /// 登记凭据无效、资产无权访问、身份冲突或持久化失败时返回错误。
    pub fn validate_agent_enrollment(
        &self,
        target_id: &str,
        token: &str,
        agent_id: AgentInstanceId,
    ) -> PortalResult<Asset> {
        if agent_id.as_uuid().is_nil() || parse_token(token, "rope1")?.to_string() != target_id {
            return Err(PortalError::Unauthorized);
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut asset = read_asset(&tx, target_id)?;
        let owner = principal(&tx, asset.principal_id)?;
        check_acl(&owner, &asset.asset_id)?;
        let expected: Vec<u8> = tx.query_row(
            "SELECT enrollment_hash FROM portal_assets WHERE target_id=?1",
            [target_id],
            |r| r.get(0),
        )?;
        if !bool::from(expected.ct_eq(hash(token.as_bytes()).as_slice())) {
            return Err(PortalError::Unauthorized);
        }
        match asset.agent_id {
            Some(existing) if existing != agent_id => return Err(PortalError::Conflict),
            None => {
                if find_agent(&tx, agent_id)?.is_some() {
                    return Err(PortalError::Conflict);
                }
                asset.agent_id = Some(agent_id);
                asset.revision += 1;
                tx.execute(
                    "UPDATE portal_assets SET agent_id=?1,revision=?2 WHERE target_id=?3",
                    params![
                        agent_id.to_string(),
                        sql_revision(asset.revision)?,
                        target_id
                    ],
                )?;
            }
            _ => {}
        }
        tx.commit()?;
        Ok(asset)
    }

    /// 已登记资产即使集成被撤销也保持受管，不能退回自动签码模式。
    ///
    /// # Errors
    /// 数据库读取或记录解析失败时返回错误。
    pub fn asset_for_agent(&self, agent_id: AgentInstanceId) -> PortalResult<Option<Asset>> {
        find_agent(&*self.db()?, agent_id)
    }

    /// 读取属于已认证主体白名单中的资产。
    ///
    /// # Errors
    /// 主体无权访问、资产不存在或存储不可用时返回错误。
    pub fn asset_for_principal(&self, principal_id: Uuid, asset_id: &str) -> PortalResult<Asset> {
        authorized_asset(&*self.db()?, principal_id, asset_id)
    }

    /// 显式签发十分钟一次码；重签原子撤销未使用码，活动连接默认冲突。
    ///
    /// # Errors
    /// 主体无权访问、目标不匹配、活动授权或幂等参数冲突、邀请失效或持久化失败时返回错误。
    pub fn issue(
        &self,
        principal_id: Uuid,
        asset_id: &str,
        target: TargetSnapshot,
        request: IssueInvitationRequest,
        reserved_codes: &[String],
        now: DateTime<Utc>,
    ) -> PortalResult<IssuedInvitation> {
        if request.idempotency_key.is_nil() {
            return Err(PortalError::InvalidInput);
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let asset = authorized_asset(&tx, principal_id, asset_id)?;
        check_target(&asset, target)?;
        let fingerprint = encode(&(asset_id, target.agent_id, target.boot_id, &request))?;
        let prior:Option<(String,String,i64)>=tx.query_row("SELECT id,request_data,nonce FROM portal_invitations WHERE principal_id=?1 AND idempotency_key=?2",params![principal_id.to_string(),request.idempotency_key.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((id, stored, nonce)) = prior {
            if stored != fingerprint {
                return Err(PortalError::Conflict);
            }
            let invitation = read_invitation(&tx, &id, now)?;
            require_issued(&invitation)?;
            let code = self.code(parse_id(&id)?, nonce);
            tx.commit()?;
            return Ok(IssuedInvitation {
                invitation,
                code,
                replaced_connection: None,
            });
        }
        let active = active_connection(&tx, &asset.target_id)?;
        if active.as_ref().is_some_and(|c| same_boot(c.target, target))
            && (!request.replace_active || request.expected_revision != Some(asset.revision))
        {
            return Err(PortalError::Conflict);
        }
        if request
            .expected_revision
            .is_some_and(|v| v != asset.revision)
        {
            return Err(PortalError::Conflict);
        }
        if request.replace_active && request.expected_revision.is_none() {
            return Err(PortalError::InvalidInput);
        }
        tx.execute("UPDATE portal_invitations SET state='revoked',revision=revision+1 WHERE target_id=?1 AND state='issued'",[&asset.target_id])?;
        if let Some(ref connection) = active {
            close_row(&tx, connection.connection_id)?;
        }
        let revision = asset.revision.checked_add(1).ok_or(PortalError::Storage)?;
        tx.execute(
            "UPDATE portal_assets SET revision=?1 WHERE target_id=?2",
            params![sql_revision(revision)?, asset.target_id],
        )?;
        let target = TargetSnapshot {
            session_id: remoteops_domain::SessionId::new(),
            ..target
        };
        let id = Uuid::new_v4();
        let mut chosen = None;
        for nonce in 0_i64..128 {
            let bytes = self.mac(
                b"remoteops-portal-code-v1\0",
                format!("{id}:{nonce}").as_bytes(),
            );
            if u32::from_be_bytes(bytes[..4].try_into().map_err(|_| PortalError::Storage)?)
                >= 4_000_000_000
            {
                continue;
            }
            let code = self.code(id, nonce);
            if reserved_codes.contains(&code) {
                continue;
            }
            let digest = self.mac(b"remoteops-portal-code-index-v1\0", code.as_bytes());
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM portal_invitations WHERE code_hash=?1)",
                [digest.as_slice()],
                |r| r.get(0),
            )?;
            if !exists {
                chosen = Some((nonce, code, digest));
                break;
            }
        }
        let (nonce, code, digest) = chosen.ok_or(PortalError::Storage)?;
        let expires = now
            .checked_add_signed(chrono::Duration::seconds(INVITATION_SECONDS))
            .ok_or(PortalError::InvalidInput)?;
        tx.execute("INSERT INTO portal_invitations(id,target_id,principal_id,target,revision,expires_at,state,code_hash,nonce,idempotency_key,request_data) VALUES(?1,?2,?3,?4,?5,?6,'issued',?7,?8,?9,?10)",params![id.to_string(),asset.target_id,principal_id.to_string(),encode(&target)?,sql_revision(revision)?,expires.timestamp(),digest.as_slice(),nonce,request.idempotency_key.to_string(),fingerprint])?;
        let invitation = read_invitation(&tx, &id.to_string(), now)?;
        tx.commit()?;
        Ok(IssuedInvitation {
            invitation,
            code,
            replaced_connection: active,
        })
    }

    /// 已认证控制端兑换前读取邀请目标；宿主仍须核对实时身份及在线状态。
    ///
    /// # Errors
    /// 控制码无效、邀请过期或撤销、主体无权访问或存储不可用时返回错误。
    pub fn invitation_target(
        &self,
        code: &str,
        now: DateTime<Utc>,
    ) -> PortalResult<TargetSnapshot> {
        let db = self.db()?;
        let id = self.lookup_code(&db, code)?;
        let invitation = read_invitation(&db, &id, now)?;
        let asset = read_asset(&db, &invitation.target_id)?;
        let owner = principal(&db, asset.principal_id)?;
        check_acl(&owner, &asset.asset_id)?;
        if invitation.state == "revoked" {
            return Err(PortalError::Revoked);
        }
        if invitation.state == "expired" {
            return Err(PortalError::Expired);
        }
        Ok(invitation.target)
    }
    /// 本数据库保留门户码摘要墓碑，消费、过期和撤销后均不与自动码复用。
    ///
    /// # Errors
    /// 数据库查询失败时返回错误。
    pub fn code_reserved(&self, code: &str, _now: DateTime<Utc>) -> PortalResult<bool> {
        if code.len() != 9 || !code.bytes().all(|v| v.is_ascii_digit()) {
            return Ok(false);
        }
        Ok(self.db()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM portal_invitations WHERE code_hash=?1)",
            params![
                self.mac(b"remoteops-portal-code-index-v1\0", code.as_bytes())
                    .as_slice()
            ],
            |r| r.get(0),
        )?)
    }
    fn lookup_code(&self, db: &Connection, code: &str) -> PortalResult<String> {
        if code.len() != 9 || !code.bytes().all(|v| v.is_ascii_digit()) {
            return Err(PortalError::Unauthorized);
        }
        db.query_row(
            "SELECT id FROM portal_invitations WHERE code_hash=?1",
            [self
                .mac(b"remoteops-portal-code-index-v1\0", code.as_bytes())
                .as_slice()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(PortalError::Unauthorized)
    }

    /// 原子消费邀请；同消费者、同请求恢复同一连接，不产生第二次授权。
    ///
    /// # Errors
    /// 身份或目标不匹配、邀请无效或已消费、请求冲突或持久化失败时返回错误。
    pub fn redeem(
        &self,
        code: &str,
        consumer: ConsumerIdentity,
        target: TargetSnapshot,
        request_id: Uuid,
        now: DateTime<Utc>,
    ) -> PortalResult<RedeemedConnection> {
        self.check_consumer(consumer)?;
        if request_id.is_nil() {
            return Err(PortalError::InvalidInput);
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = self.lookup_code(&tx, code)?;
        let invitation = read_invitation(&tx, &id, now)?;
        let asset = read_asset(&tx, &invitation.target_id)?;
        let owner = principal(&tx, asset.principal_id)?;
        check_acl(&owner, &asset.asset_id)?;
        check_target(&asset, target)?;
        if !same_target(invitation.target, target) {
            return Err(PortalError::TargetMismatch);
        }
        let prior: Option<(String, String)> = tx
            .query_row(
                "SELECT id,request_id FROM portal_connections WHERE invitation_id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((connection_id, original_request)) = prior {
            let connection = read_connection(&tx, &connection_id)?;
            if connection.state != "active" {
                return Err(PortalError::Revoked);
            }
            if connection.consumer != consumer || original_request != request_id.to_string() {
                return Err(PortalError::Conflict);
            }
            let result = self.redeemed(connection);
            tx.commit()?;
            return Ok(result);
        }
        require_issued(&invitation)?;
        if active_connection(&tx, &asset.target_id)?.is_some() {
            return Err(PortalError::Conflict);
        }
        let connection_id = Uuid::new_v4();
        let token = self.resume_token(connection_id);
        tx.execute("INSERT INTO portal_connections(id,invitation_id,target_id,principal_id,target,consumer,request_id,state,revision,token_hash) VALUES(?1,?2,?3,?4,?5,?6,?7,'active',1,?8)",params![connection_id.to_string(),id,asset.target_id,asset.principal_id.to_string(),encode(&target)?,encode(&consumer)?,request_id.to_string(),hash(token.as_bytes()).as_slice()])?;
        if tx.execute(
            "UPDATE portal_invitations SET state='redeemed' WHERE id=?1 AND state='issued'",
            [&id],
        )? != 1
        {
            return Err(PortalError::Conflict);
        }
        let connection = read_connection(&tx, &connection_id.to_string())?;
        tx.commit()?;
        Ok(self.redeemed(connection))
    }

    fn check_consumer(&self, c: ConsumerIdentity) -> PortalResult<()> {
        if c.owner_id != self.owner_id
            || c.controller_id.as_uuid().is_nil()
            || c.installation_id.is_some_and(|v| v.is_nil())
        {
            Err(PortalError::Unauthorized)
        } else {
            Ok(())
        }
    }
    fn redeemed(&self, connection: ConnectionSummary) -> RedeemedConnection {
        RedeemedConnection {
            connection_id: connection.connection_id,
            target: connection.target,
            consumer: connection.consumer,
            resume_token: self.resume_token(connection.connection_id),
            connection,
        }
    }

    /// 查找高熵恢复凭据绑定的准确目标，不接受旧码作为恢复凭据。
    ///
    /// # Errors
    /// 恢复凭据无效、授权已撤销或存储不可用时返回错误。
    pub fn connection_target(&self, token: &str) -> PortalResult<TargetSnapshot> {
        Ok(Self::read_token_connection(&*self.db()?, token)?.target)
    }

    fn read_token_connection(db: &Connection, token: &str) -> PortalResult<ConnectionSummary> {
        let id = parse_token(token, "ropr1")?;
        let expected: Option<Vec<u8>> = db
            .query_row(
                "SELECT token_hash FROM portal_connections WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if !expected.is_some_and(|v| bool::from(v.ct_eq(hash(token.as_bytes()).as_slice()))) {
            return Err(PortalError::Unauthorized);
        }
        let connection = read_connection(db, &id.to_string())?;
        authorize_connection(db, &connection)?;
        Ok(connection)
    }

    /// 同控制实例、同安装身份、同运行同会话恢复；传输代次变化不扩大范围。
    ///
    /// # Errors
    /// 恢复凭据无效、消费者或目标不匹配、授权已撤销或存储不可用时返回错误。
    pub fn resume(
        &self,
        token: &str,
        consumer: ConsumerIdentity,
        target: TargetSnapshot,
        _now: DateTime<Utc>,
    ) -> PortalResult<RedeemedConnection> {
        self.check_consumer(consumer)?;
        let db = self.db()?;
        let connection = Self::read_token_connection(&db, token)?;
        if connection.consumer != consumer {
            return Err(PortalError::Unauthorized);
        }
        if !same_target(connection.target, target) {
            return Err(PortalError::TargetMismatch);
        }
        Ok(self.redeemed(connection))
    }

    /// 转发操作前复查持久授权，防止撤销后沿用运行态缓存。
    ///
    /// # Errors
    /// 连接不存在、身份或目标不匹配、授权已撤销或存储不可用时返回错误。
    pub fn connection_authorized(
        &self,
        id: Uuid,
        consumer: ConsumerIdentity,
        target: TargetSnapshot,
    ) -> PortalResult<ConnectionSummary> {
        let db = self.db()?;
        let connection = read_connection(&db, &id.to_string())?;
        authorize_connection(&db, &connection)?;
        if connection.consumer != consumer {
            return Err(PortalError::Unauthorized);
        }
        if !same_target(connection.target, target) {
            return Err(PortalError::TargetMismatch);
        }
        Ok(connection)
    }

    /// 撤销未消费邀请，已兑换的邀请须显式关闭连接。
    ///
    /// # Errors
    /// 邀请不存在、主体无权访问、邀请已兑换或持久化失败时返回错误。
    pub fn revoke_invitation(
        &self,
        principal_id: Uuid,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> PortalResult<InvitationSummary> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let invitation = read_invitation(&tx, &id.to_string(), now)?;
        authorize_target(&tx, principal_id, &invitation.target_id)?;
        if invitation.state == "redeemed" {
            return Err(PortalError::Conflict);
        }
        tx.execute("UPDATE portal_invitations SET state='revoked',revision=revision+1 WHERE id=?1 AND state='issued'",[id.to_string()])?;
        let result = read_invitation(&tx, &id.to_string(), now)?;
        tx.commit()?;
        Ok(result)
    }

    /// 关闭指定连接并吊销其恢复令牌；重复关闭不改变结果。
    ///
    /// # Errors
    /// 连接不存在、主体无权访问、修订号冲突或持久化失败时返回错误。
    pub fn close_connection(
        &self,
        principal_id: Uuid,
        id: Uuid,
        expected_revision: Option<u64>,
        _now: DateTime<Utc>,
    ) -> PortalResult<ConnectionSummary> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let connection = read_connection(&tx, &id.to_string())?;
        authorize_target(&tx, principal_id, &connection.target_id)?;
        if connection.state == "active"
            && expected_revision.is_some_and(|v| v != connection.revision)
        {
            return Err(PortalError::Conflict);
        }
        close_row(&tx, id)?;
        let result = read_connection(&tx, &id.to_string())?;
        tx.commit()?;
        Ok(result)
    }

    /// Agent 重启、断开授权或换绑时失效该目标所有门户能力。
    ///
    /// # Errors
    /// 目标不存在或持久化失败时返回错误。
    pub fn close_target(&self, target_id: &str) -> PortalResult<Vec<ConnectionSummary>> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        read_asset(&tx, target_id)?;
        let connections = active_connection(&tx, target_id)?
            .into_iter()
            .collect::<Vec<_>>();
        tx.execute("UPDATE portal_invitations SET state='revoked',revision=revision+1 WHERE target_id=?1 AND state='issued'",[target_id])?;
        for connection in &connections {
            close_row(&tx, connection.connection_id)?;
        }
        tx.commit()?;
        Ok(connections)
    }

    /// 管理员撤销主体并返回需要立即清理的活动连接。
    ///
    /// # Errors
    /// 主体不存在或持久化失败时返回错误。
    pub fn revoke_principal(&self, id: Uuid) -> PortalResult<Vec<ConnectionSummary>> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM portal_principals WHERE id=?1)",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(PortalError::NotFound);
        }
        let ids = {
            let mut statement = tx.prepare(
                "SELECT id FROM portal_connections WHERE principal_id=?1 AND state='active'",
            )?;
            statement
                .query_map([id.to_string()], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut connections = Vec::new();
        for cid in ids {
            let c = read_connection(&tx, &cid)?;
            close_row(&tx, c.connection_id)?;
            connections.push(c);
        }
        tx.execute(
            "UPDATE portal_principals SET revoked=1 WHERE id=?1",
            [id.to_string()],
        )?;
        tx.execute("UPDATE portal_invitations SET state='revoked',revision=revision+1 WHERE principal_id=?1 AND state='issued'",[id.to_string()])?;
        tx.commit()?;
        Ok(connections)
    }

    /// 返回已授权资产状态，不回显短码与恢复令牌。
    ///
    /// # Errors
    /// 主体无权访问、资产不存在或存储不可用时返回错误。
    pub fn status(
        &self,
        principal_id: Uuid,
        asset_id: &str,
        now: DateTime<Utc>,
    ) -> PortalResult<AssetStatus> {
        let db = self.db()?;
        let asset = authorized_asset(&db, principal_id, asset_id)?;
        let id: Option<String> = db
            .query_row(
                "SELECT id FROM portal_invitations WHERE target_id=?1 ORDER BY rowid DESC LIMIT 1",
                [&asset.target_id],
                |r| r.get(0),
            )
            .optional()?;
        let invitation = id.map(|id| read_invitation(&db, &id, now)).transpose()?;
        let connection = active_connection(&db, &asset.target_id)?;
        Ok(AssetStatus {
            asset,
            invitation,
            connection,
        })
    }

    /// 返回调用方资产范围内的单目标连接状态。
    ///
    /// # Errors
    /// 连接不存在、主体无权访问或存储不可用时返回错误。
    pub fn connection(&self, principal_id: Uuid, id: Uuid) -> PortalResult<ConnectionSummary> {
        let db = self.db()?;
        let connection = read_connection(&db, &id.to_string())?;
        authorize_target(&db, principal_id, &connection.target_id)?;
        Ok(connection)
    }
}

fn sql_revision(value: u64) -> PortalResult<i64> {
    i64::try_from(value).map_err(|_| PortalError::Storage)
}
fn unsigned_revision(value: i64) -> PortalResult<u64> {
    u64::try_from(value).map_err(|_| PortalError::Storage)
}
fn hash(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}
fn random_secret() -> String {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    OsRng.fill_bytes(bytes.as_mut());
    URL_SAFE_NO_PAD.encode(bytes.as_ref())
}
fn parse_token(token: &str, prefix: &str) -> PortalResult<Uuid> {
    if token.len() > 128 {
        return Err(PortalError::Unauthorized);
    }
    let mut parts = token.split('.');
    if parts.next() != Some(prefix) {
        return Err(PortalError::Unauthorized);
    }
    let id = parts
        .next()
        .and_then(|s| s.parse::<Uuid>().ok())
        .filter(|v| !v.is_nil())
        .ok_or(PortalError::Unauthorized)?;
    let secret = parts.next().ok_or(PortalError::Unauthorized)?;
    if parts.next().is_some()
        || secret.len() != 43
        || URL_SAFE_NO_PAD.decode(secret).map_or(true, |v| {
            v.len() != 32 || URL_SAFE_NO_PAD.encode(v) != secret
        })
    {
        return Err(PortalError::Unauthorized);
    }
    Ok(id)
}
fn encode<T: Serialize>(value: &T) -> PortalResult<String> {
    serde_json::to_string(value).map_err(|_| PortalError::Storage)
}
fn decode<T: DeserializeOwned>(value: &str) -> PortalResult<T> {
    serde_json::from_str(value).map_err(|_| PortalError::Storage)
}
fn parse_id(value: &str) -> PortalResult<Uuid> {
    value.parse().map_err(|_| PortalError::Storage)
}
fn valid_label(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
fn valid_asset(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'-' | b'_' | b'.' | b':'))
}
fn principal(db: &Connection, id: Uuid) -> PortalResult<Principal> {
    let (data, revoked): (String, bool) = db
        .query_row(
            "SELECT data,revoked FROM portal_principals WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(PortalError::Unauthorized)?;
    if revoked {
        return Err(PortalError::Unauthorized);
    }
    let principal: Principal = decode(&data)?;
    if principal.principal_id != id || principal.revoked {
        return Err(PortalError::Storage);
    }
    Ok(principal)
}
fn check_acl(principal: &Principal, asset_id: &str) -> PortalResult<()> {
    if principal.allowed_assets.iter().any(|v| v == asset_id) {
        Ok(())
    } else {
        Err(PortalError::Forbidden)
    }
}
fn read_asset(db: &Connection, target_id: &str) -> PortalResult<Asset> {
    let row: Option<(String, String, Option<String>, i64)> = db
        .query_row(
            "SELECT asset_id,principal_id,agent_id,revision FROM portal_assets WHERE target_id=?1",
            [target_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (asset_id, principal_id, agent_id, revision) = row.ok_or(PortalError::NotFound)?;
    Ok(Asset {
        target_id: target_id.into(),
        asset_id,
        principal_id: parse_id(&principal_id)?,
        agent_id: agent_id
            .map(|v| v.parse().map_err(|_| PortalError::Storage))
            .transpose()?,
        revision: unsigned_revision(revision)?,
    })
}
fn authorized_asset(db: &Connection, pid: Uuid, asset_id: &str) -> PortalResult<Asset> {
    let p = principal(db, pid)?;
    check_acl(&p, asset_id)?;
    let target: Option<String> = db
        .query_row(
            "SELECT target_id FROM portal_assets WHERE principal_id=?1 AND asset_id=?2",
            params![pid.to_string(), asset_id],
            |r| r.get(0),
        )
        .optional()?;
    read_asset(db, &target.ok_or(PortalError::NotFound)?)
}
fn authorize_target(db: &Connection, pid: Uuid, target_id: &str) -> PortalResult<Asset> {
    let asset = read_asset(db, target_id)?;
    if asset.principal_id != pid {
        return Err(PortalError::Forbidden);
    }
    let p = principal(db, pid)?;
    check_acl(&p, &asset.asset_id)?;
    Ok(asset)
}
fn find_agent(db: &Connection, id: AgentInstanceId) -> PortalResult<Option<Asset>> {
    let target: Option<String> = db
        .query_row(
            "SELECT target_id FROM portal_assets WHERE agent_id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    target.map(|v| read_asset(db, &v)).transpose()
}
fn check_target(asset: &Asset, target: TargetSnapshot) -> PortalResult<()> {
    if asset.agent_id != Some(target.agent_id)
        || target.boot_id.as_uuid().is_nil()
        || target.session_id.as_uuid().is_nil()
        || target.agent_generation == 0
    {
        Err(PortalError::TargetMismatch)
    } else {
        Ok(())
    }
}
fn same_boot(a: TargetSnapshot, b: TargetSnapshot) -> bool {
    a.agent_id == b.agent_id && a.boot_id == b.boot_id
}
fn same_target(a: TargetSnapshot, b: TargetSnapshot) -> bool {
    a.agent_id == b.agent_id && a.boot_id == b.boot_id && a.session_id == b.session_id
}
fn read_invitation(
    db: &Connection,
    id: &str,
    now: DateTime<Utc>,
) -> PortalResult<InvitationSummary> {
    let row: Option<(String, String, i64, i64, String)> = db
        .query_row(
            "SELECT target_id,target,revision,expires_at,state FROM portal_invitations WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let (target_id, target, revision, expires, state) = row.ok_or(PortalError::NotFound)?;
    let expires_at = DateTime::from_timestamp(expires, 0).ok_or(PortalError::Storage)?;
    let state = if state == "issued" && now >= expires_at {
        "expired".into()
    } else {
        state
    };
    Ok(InvitationSummary {
        invitation_id: parse_id(id)?,
        target_id,
        target: decode(&target)?,
        revision: unsigned_revision(revision)?,
        expires_at,
        state,
    })
}
fn require_issued(inv: &InvitationSummary) -> PortalResult<()> {
    match inv.state.as_str() {
        "issued" => Ok(()),
        "expired" => Err(PortalError::Expired),
        "revoked" => Err(PortalError::Revoked),
        _ => Err(PortalError::Conflict),
    }
}
fn read_connection(db: &Connection, id: &str) -> PortalResult<ConnectionSummary> {
    let row:Option<(String,String,String,String,String,i64,String)>=db.query_row("SELECT invitation_id,target_id,principal_id,target,consumer,revision,state FROM portal_connections WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    let (invitation_id, target_id, principal_id, target, consumer, revision, state) =
        row.ok_or(PortalError::NotFound)?;
    Ok(ConnectionSummary {
        connection_id: parse_id(id)?,
        invitation_id: parse_id(&invitation_id)?,
        target_id,
        principal_id: parse_id(&principal_id)?,
        target: decode(&target)?,
        consumer: decode(&consumer)?,
        revision: unsigned_revision(revision)?,
        state,
    })
}
fn active_connection(db: &Connection, target: &str) -> PortalResult<Option<ConnectionSummary>> {
    let id: Option<String> = db
        .query_row(
            "SELECT id FROM portal_connections WHERE target_id=?1 AND state='active'",
            [target],
            |r| r.get(0),
        )
        .optional()?;
    id.map(|v| read_connection(db, &v)).transpose()
}
fn authorize_connection(db: &Connection, c: &ConnectionSummary) -> PortalResult<()> {
    if c.state != "active" {
        return Err(PortalError::Revoked);
    }
    let asset = authorize_target(db, c.principal_id, &c.target_id)?;
    check_target(&asset, c.target)
}
fn close_row(db: &Connection, id: Uuid) -> PortalResult<()> {
    db.execute("UPDATE portal_connections SET state='closed',revision=revision+1 WHERE id=?1 AND state='active'",[id.to_string()])?;
    db.execute("UPDATE portal_invitations SET state='revoked',revision=revision+1 WHERE id=(SELECT invitation_id FROM portal_connections WHERE id=?1) AND state!='revoked'",[id.to_string()])?;
    Ok(())
}

const SCHEMA:&str="
CREATE TABLE portal_meta(id INTEGER PRIMARY KEY CHECK(id=1),owner_id TEXT NOT NULL,key BLOB NOT NULL CHECK(length(key)=32)) STRICT;
CREATE TABLE portal_principals(id TEXT PRIMARY KEY,credential_hash BLOB NOT NULL CHECK(length(credential_hash)=32),data TEXT NOT NULL,revoked INTEGER NOT NULL CHECK(revoked IN(0,1))) STRICT;
CREATE TABLE portal_assets(target_id TEXT PRIMARY KEY,principal_id TEXT NOT NULL REFERENCES portal_principals(id),asset_id TEXT NOT NULL,enrollment_hash BLOB NOT NULL CHECK(length(enrollment_hash)=32),agent_id TEXT UNIQUE,revision INTEGER NOT NULL CHECK(revision>0),UNIQUE(principal_id,asset_id)) STRICT;
CREATE TABLE portal_invitations(id TEXT PRIMARY KEY,target_id TEXT NOT NULL REFERENCES portal_assets(target_id),principal_id TEXT NOT NULL REFERENCES portal_principals(id),target TEXT NOT NULL,revision INTEGER NOT NULL CHECK(revision>0),expires_at INTEGER NOT NULL,state TEXT NOT NULL CHECK(state IN('issued','redeemed','revoked')),code_hash BLOB NOT NULL UNIQUE CHECK(length(code_hash)=32),nonce INTEGER NOT NULL,idempotency_key TEXT NOT NULL,request_data TEXT NOT NULL,UNIQUE(principal_id,idempotency_key)) STRICT;
CREATE UNIQUE INDEX one_issued_per_target ON portal_invitations(target_id) WHERE state='issued';
CREATE TABLE portal_connections(id TEXT PRIMARY KEY,invitation_id TEXT NOT NULL UNIQUE REFERENCES portal_invitations(id),target_id TEXT NOT NULL REFERENCES portal_assets(target_id),principal_id TEXT NOT NULL REFERENCES portal_principals(id),target TEXT NOT NULL,consumer TEXT NOT NULL,request_id TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN('active','closed')),revision INTEGER NOT NULL CHECK(revision>0),token_hash BLOB NOT NULL CHECK(length(token_hash)=32)) STRICT;
CREATE UNIQUE INDEX one_active_per_target ON portal_connections(target_id) WHERE state='active';
";

#[cfg(test)]
mod tests;
