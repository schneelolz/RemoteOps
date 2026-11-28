use super::*;

pub(super) const SCHEMA: &str = "
CREATE TABLE portal_admin_operations(id TEXT PRIMARY KEY,idempotency_key TEXT NOT NULL UNIQUE,request_data TEXT NOT NULL,source_principal_id TEXT NOT NULL REFERENCES portal_principals(id),destination_principal_id TEXT REFERENCES portal_principals(id),state TEXT NOT NULL CHECK(state IN('pending','completed')),data TEXT NOT NULL) STRICT;
CREATE TABLE portal_admin_targets(operation_id TEXT NOT NULL REFERENCES portal_admin_operations(id),target_id TEXT NOT NULL REFERENCES portal_assets(target_id),PRIMARY KEY(operation_id,target_id)) STRICT;
CREATE INDEX portal_admin_target_lookup ON portal_admin_targets(target_id);
CREATE TABLE portal_admin_audit(id TEXT PRIMARY KEY,operation_id TEXT UNIQUE REFERENCES portal_admin_operations(id),data TEXT NOT NULL) STRICT;
";

impl PortalStore {
    /// 管理员查询主体，包括已撤销主体；不返回凭据或摘要。
    ///
    /// # Errors
    /// 主体不存在或存储不可用时返回错误。
    pub fn get_principal(&self, id: Uuid) -> PortalResult<Principal> {
        read_principal(&*self.db()?, id)
    }

    /// 管理员按准确目标标识查询当前资产，不受主体白名单限制。
    ///
    /// # Errors
    /// 资产不存在或存储不可用时返回错误。
    pub fn get_asset(&self, target_id: &str) -> PortalResult<Asset> {
        read_asset(&*self.db()?, target_id)
    }

    /// 原子增删主体白名单、撤销失去权限的目标授权并记录审计。
    ///
    /// # Errors
    /// 参数、修订号、幂等键或运行态同步状态冲突，或存储不可用时返回错误。
    pub fn update_principal_assets(
        &self,
        id: Uuid,
        request: UpdatePrincipalAssetsRequest,
        actor: &str,
    ) -> PortalResult<AdminOperation> {
        let resource = id.to_string();
        let result = self.update_principal_assets_inner(id, request, actor);
        self.audit_failure(actor, "update_principal_assets", &resource, &result);
        result
    }

    fn update_principal_assets_inner(
        &self,
        id: Uuid,
        mut request: UpdatePrincipalAssetsRequest,
        actor: &str,
    ) -> PortalResult<AdminOperation> {
        validate_actor(actor)?;
        if id.is_nil()
            || request.idempotency_key.is_nil()
            || request.expected_revision == 0
            || request.add.len() > 1024
            || request.remove.len() > 1024
            || request
                .add
                .iter()
                .chain(&request.remove)
                .any(|v| !valid_asset(v))
        {
            return Err(PortalError::InvalidInput);
        }
        request.add.sort();
        request.add.dedup();
        request.remove.sort();
        request.remove.dedup();
        if request.add.iter().any(|v| request.remove.contains(v)) {
            return Err(PortalError::InvalidInput);
        }
        let fingerprint = encode(&("update_principal_assets", id, &request))?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(operation) = prior_operation(&tx, request.idempotency_key, &fingerprint)? {
            return Ok(operation);
        }
        require_principal_ready(&tx, id)?;
        let previous = principal(&tx, id)?;
        if previous.owner_id != self.owner_id {
            return Err(PortalError::Forbidden);
        }
        if previous.revision != request.expected_revision {
            return Err(PortalError::RevisionConflict);
        }
        let mut updated = previous.clone();
        updated
            .allowed_assets
            .retain(|v| !request.remove.contains(v));
        updated.allowed_assets.extend(request.add);
        updated.allowed_assets.sort();
        updated.allowed_assets.dedup();
        if updated.allowed_assets.len() > 1024 {
            return Err(PortalError::InvalidInput);
        }
        updated.revision = next_revision(previous.revision)?;
        let affected = principal_targets(&tx, id)?
            .into_iter()
            .filter(|asset| {
                previous.allowed_assets.contains(&asset.asset_id)
                    && !updated.allowed_assets.contains(&asset.asset_id)
            })
            .map(|asset| asset.target_id)
            .collect::<Vec<_>>();
        for target_id in &affected {
            revoke_target(&tx, target_id)?;
        }
        tx.execute(
            "UPDATE portal_principals SET data=?1 WHERE id=?2",
            params![encode(&updated)?, id.to_string()],
        )?;
        let operation = AdminOperation {
            operation_id: Uuid::new_v4(),
            kind: "update_principal_assets".into(),
            state: "pending".into(),
            affected_target_ids: affected,
            previous_principal: Some(previous),
            principal: Some(updated),
            previous_asset: None,
            asset: None,
            created_at: Utc::now(),
            completed_at: None,
        };
        insert_operation(
            &tx,
            &operation,
            request.idempotency_key,
            &fingerprint,
            id,
            None,
            actor,
        )?;
        tx.commit()?;
        Ok(operation)
    }

    /// 原子迁移资产归属并撤销旧授权，保留 Agent 身份和部署登记证明。
    ///
    /// # Errors
    /// 主体、白名单、归属、修订号、同名资产或幂等状态冲突时返回错误。
    pub fn transfer_asset(
        &self,
        target_id: &str,
        request: TransferAssetRequest,
        actor: &str,
    ) -> PortalResult<AdminOperation> {
        let result = self.transfer_asset_inner(target_id, request, actor);
        self.audit_failure(actor, "transfer_asset", target_id, &result);
        result
    }

    #[allow(clippy::needless_pass_by_value)]
    fn transfer_asset_inner(
        &self,
        target_id: &str,
        request: TransferAssetRequest,
        actor: &str,
    ) -> PortalResult<AdminOperation> {
        validate_actor(actor)?;
        if Uuid::parse_str(target_id).map_or(true, |id| id.is_nil())
            || request.idempotency_key.is_nil()
            || request.expected_principal_id.is_nil()
            || request.destination_principal_id.is_nil()
            || request.expected_revision == 0
            || request.expected_principal_id == request.destination_principal_id
        {
            return Err(PortalError::InvalidInput);
        }
        let fingerprint = encode(&("transfer_asset", target_id, &request))?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(operation) = prior_operation(&tx, request.idempotency_key, &fingerprint)? {
            return Ok(operation);
        }
        let previous = read_asset(&tx, target_id)?;
        if previous.principal_id != request.expected_principal_id {
            return Err(PortalError::OwnershipConflict);
        }
        if previous.revision != request.expected_revision {
            return Err(PortalError::RevisionConflict);
        }
        require_principal_ready(&tx, previous.principal_id)?;
        require_principal_ready(&tx, request.destination_principal_id)?;
        require_target_ready(&tx, target_id)?;
        let source = principal(&tx, previous.principal_id)?;
        let destination = principal(&tx, request.destination_principal_id)?;
        if source.owner_id != self.owner_id || destination.owner_id != self.owner_id {
            return Err(PortalError::Forbidden);
        }
        check_acl(&destination, &previous.asset_id)?;
        let collision: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM portal_assets WHERE principal_id=?1 AND asset_id=?2)",
            params![destination.principal_id.to_string(), previous.asset_id],
            |r| r.get(0),
        )?;
        if collision {
            return Err(PortalError::NameConflict);
        }
        let mut updated = previous.clone();
        updated.principal_id = destination.principal_id;
        updated.revision = next_revision(previous.revision)?;
        revoke_target(&tx, target_id)?;
        tx.execute(
            "UPDATE portal_assets SET principal_id=?1,revision=?2 WHERE target_id=?3",
            params![
                updated.principal_id.to_string(),
                sql_revision(updated.revision)?,
                target_id
            ],
        )?;
        let operation = AdminOperation {
            operation_id: Uuid::new_v4(),
            kind: "transfer_asset".into(),
            state: "pending".into(),
            affected_target_ids: vec![target_id.into()],
            previous_principal: None,
            principal: None,
            previous_asset: Some(previous),
            asset: Some(updated),
            created_at: Utc::now(),
            completed_at: None,
        };
        insert_operation(
            &tx,
            &operation,
            request.idempotency_key,
            &fingerprint,
            request.expected_principal_id,
            Some(request.destination_principal_id),
            actor,
        )?;
        tx.commit()?;
        Ok(operation)
    }

    /// 查询原管理操作的持久结果，不将旧快照解释为当前资源状态。
    ///
    /// # Errors
    /// 操作不存在或持久数据不可用时返回错误。
    pub fn admin_operation(&self, id: Uuid) -> PortalResult<AdminOperation> {
        read_operation(&*self.db()?, id)
    }

    /// 查询需要恢复运行态同步的管理操作，供启动恢复或管理员重试使用。
    ///
    /// # Errors
    /// 持久数据不可用时返回错误。
    pub fn pending_admin_operations(&self) -> PortalResult<Vec<AdminOperation>> {
        let db = self.db()?;
        let mut statement = db.prepare(
            "SELECT data FROM portal_admin_operations WHERE state='pending' ORDER BY rowid",
        )?;
        statement
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|data| decode(&data?))
            .collect()
    }

    /// Relay 持久完成准确目标的运行态同步后解除授权门控；重复完成不再撤权。
    ///
    /// # Errors
    /// 操作不存在或持久化不可用时返回错误。
    pub fn complete_admin_operation(&self, id: Uuid) -> PortalResult<AdminOperation> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut operation = read_operation(&tx, id)?;
        if operation.state == "completed" {
            return Ok(operation);
        }
        operation.state = "completed".into();
        operation.completed_at = Some(Utc::now());
        tx.execute(
            "UPDATE portal_admin_operations SET state='completed',data=?1 WHERE id=?2",
            params![encode(&operation)?, id.to_string()],
        )?;
        let data: String = tx.query_row(
            "SELECT data FROM portal_admin_audit WHERE operation_id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        let mut audit: AdminAuditEvent = decode(&data)?;
        audit.state.clone_from(&operation.state);
        audit.completed_at = operation.completed_at;
        tx.execute(
            "UPDATE portal_admin_audit SET data=?1 WHERE operation_id=?2",
            params![encode(&audit)?, id.to_string()],
        )?;
        tx.commit()?;
        Ok(operation)
    }

    /// 读取最近管理审计，数量范围为一至一千，不回显请求正文或秘密。
    ///
    /// # Errors
    /// 数量无效或持久数据不可用时返回错误。
    pub fn admin_audit(&self, limit: usize) -> PortalResult<Vec<AdminAuditEvent>> {
        if !(1..=1000).contains(&limit) {
            return Err(PortalError::InvalidInput);
        }
        let db = self.db()?;
        let mut statement =
            db.prepare("SELECT data FROM portal_admin_audit ORDER BY rowid DESC LIMIT ?1")?;
        statement
            .query_map(
                [i64::try_from(limit).map_err(|_| PortalError::InvalidInput)?],
                |r| r.get::<_, String>(0),
            )?
            .map(|data| decode(&data?))
            .collect()
    }

    /// 记录管理入口的失败尝试；调用方必须传入认证上下文和非机密资源标识。
    ///
    /// # Errors
    /// 审计来源、资源标识无效或持久化不可用时返回错误。
    pub fn record_admin_failure(
        &self,
        actor: &str,
        kind: &str,
        resource: &str,
        error: &PortalError,
    ) -> PortalResult<()> {
        validate_actor(actor)?;
        if !matches!(kind, "update_principal_assets" | "transfer_asset") {
            return Err(PortalError::InvalidInput);
        }
        let event = AdminAuditEvent {
            event_id: Uuid::new_v4(),
            operation_id: None,
            resource: Uuid::parse_str(resource)
                .map_or_else(|_| "invalid_resource".into(), |id| id.to_string()),
            error_code: Some(error_code(error).into()),
            actor: actor.into(),
            kind: kind.into(),
            state: "failed".into(),
            affected_target_ids: Vec::new(),
            previous_principal: None,
            principal: None,
            previous_asset: None,
            asset: None,
            created_at: Utc::now(),
            completed_at: None,
        };
        self.db()?.execute(
            "INSERT INTO portal_admin_audit(id,operation_id,data) VALUES(?1,NULL,?2)",
            params![event.event_id.to_string(), encode(&event)?],
        )?;
        Ok(())
    }

    fn audit_failure<T>(&self, actor: &str, kind: &str, resource: &str, result: &PortalResult<T>) {
        if let Err(error) = result {
            // 原始错误优先；数据库不可写时不以第二个审计错误覆盖它。
            let _ = self.record_admin_failure(actor, kind, resource, error);
        }
    }
}

fn validate_actor(actor: &str) -> PortalResult<()> {
    if actor.trim().is_empty() || actor.len() > 512 || actor.chars().any(char::is_control) {
        Err(PortalError::InvalidInput)
    } else {
        Ok(())
    }
}

fn next_revision(revision: u64) -> PortalResult<u64> {
    let next = revision.checked_add(1).ok_or(PortalError::Storage)?;
    sql_revision(next)?;
    Ok(next)
}

fn read_principal(db: &Connection, id: Uuid) -> PortalResult<Principal> {
    let (data, revoked): (String, bool) = db
        .query_row(
            "SELECT data,revoked FROM portal_principals WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(PortalError::NotFound)?;
    let mut result: Principal = decode(&data)?;
    if result.principal_id != id || result.revision == 0 {
        return Err(PortalError::Storage);
    }
    result.revoked = revoked;
    Ok(result)
}

fn principal_targets(db: &Connection, id: Uuid) -> PortalResult<Vec<Asset>> {
    let mut statement =
        db.prepare("SELECT target_id FROM portal_assets WHERE principal_id=?1 ORDER BY target_id")?;
    statement
        .query_map([id.to_string()], |r| r.get::<_, String>(0))?
        .map(|id| read_asset(db, &id?))
        .collect()
}

pub(super) fn require_principal_ready(db: &Connection, id: Uuid) -> PortalResult<()> {
    let pending: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM portal_admin_operations WHERE state='pending' AND (source_principal_id=?1 OR destination_principal_id=?1))",
        [id.to_string()], |r| r.get(0),
    )?;
    if pending {
        Err(PortalError::Conflict)
    } else {
        Ok(())
    }
}

pub(super) fn require_target_ready(db: &Connection, target_id: &str) -> PortalResult<()> {
    let pending: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM portal_admin_targets t JOIN portal_admin_operations o ON o.id=t.operation_id WHERE t.target_id=?1 AND o.state='pending')",
        [target_id], |r| r.get(0),
    )?;
    if pending {
        Err(PortalError::Conflict)
    } else {
        Ok(())
    }
}

fn revoke_target(db: &Connection, target_id: &str) -> PortalResult<()> {
    db.execute("UPDATE portal_invitations SET state='revoked',revision=revision+1 WHERE target_id=?1 AND state='issued'", [target_id])?;
    if let Some(connection) = active_connection(db, target_id)? {
        close_row(db, connection.connection_id)?;
    }
    Ok(())
}

fn read_operation(db: &Connection, id: Uuid) -> PortalResult<AdminOperation> {
    let data: String = db
        .query_row(
            "SELECT data FROM portal_admin_operations WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(PortalError::NotFound)?;
    decode(&data)
}

fn prior_operation(
    db: &Connection,
    key: Uuid,
    fingerprint: &str,
) -> PortalResult<Option<AdminOperation>> {
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT request_data,data FROM portal_admin_operations WHERE idempotency_key=?1",
            [key.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(stored, data)| {
        if stored != fingerprint {
            return Err(PortalError::IdempotencyConflict);
        }
        decode(&data)
    })
    .transpose()
}

#[allow(clippy::too_many_arguments)]
fn insert_operation(
    db: &Connection,
    operation: &AdminOperation,
    key: Uuid,
    fingerprint: &str,
    source: Uuid,
    destination: Option<Uuid>,
    actor: &str,
) -> PortalResult<()> {
    db.execute(
        "INSERT INTO portal_admin_operations(id,idempotency_key,request_data,source_principal_id,destination_principal_id,state,data) VALUES(?1,?2,?3,?4,?5,'pending',?6)",
        params![operation.operation_id.to_string(), key.to_string(), fingerprint, source.to_string(), destination.map(|v| v.to_string()), encode(operation)?],
    )?;
    for target_id in &operation.affected_target_ids {
        db.execute(
            "INSERT INTO portal_admin_targets(operation_id,target_id) VALUES(?1,?2)",
            params![operation.operation_id.to_string(), target_id],
        )?;
    }
    let event = AdminAuditEvent {
        event_id: Uuid::new_v4(),
        operation_id: Some(operation.operation_id),
        resource: operation
            .asset
            .as_ref()
            .map_or_else(|| source.to_string(), |v| v.target_id.clone()),
        error_code: None,
        actor: actor.into(),
        kind: operation.kind.clone(),
        state: operation.state.clone(),
        affected_target_ids: operation.affected_target_ids.clone(),
        previous_principal: operation.previous_principal.clone(),
        principal: operation.principal.clone(),
        previous_asset: operation.previous_asset.clone(),
        asset: operation.asset.clone(),
        created_at: operation.created_at,
        completed_at: operation.completed_at,
    };
    db.execute(
        "INSERT INTO portal_admin_audit(id,operation_id,data) VALUES(?1,?2,?3)",
        params![
            event.event_id.to_string(),
            operation.operation_id.to_string(),
            encode(&event)?
        ],
    )?;
    Ok(())
}

fn error_code(error: &PortalError) -> &'static str {
    match error {
        PortalError::InvalidInput => "invalid_input",
        PortalError::Unauthorized => "unauthorized",
        PortalError::Forbidden => "forbidden",
        PortalError::NotFound => "not_found",
        PortalError::Conflict => "conflict",
        PortalError::RevisionConflict => "revision_conflict",
        PortalError::OwnershipConflict => "ownership_conflict",
        PortalError::IdempotencyConflict => "idempotency_conflict",
        PortalError::NameConflict => "name_conflict",
        PortalError::Expired => "expired",
        PortalError::Revoked => "revoked",
        PortalError::TargetMismatch => "target_mismatch",
        PortalError::Storage => "storage_unavailable",
    }
}
