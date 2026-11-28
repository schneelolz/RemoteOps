use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::Utc;
use remoteops_resources::{ResourceIndex, validate_manifest};
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    ManagedTool, PackageReceipt, PackageSource, PlatformTest, Result, ToolAudit, ToolCatalog,
    ToolCenterCommand, ToolCenterRequest, ToolCenterResponse, ToolCenterSnapshot, error,
    model::redact_url,
    packages::{self, Upload},
    storage, valid_id, valid_sha,
};

/// 后台与 MCP 共用的工具管理服务；元数据与包文件分开持久化。
#[derive(Clone)]
pub struct ToolCenter {
    connection: Arc<Mutex<Connection>>,
    root: PathBuf,
    uploads: Arc<Mutex<HashMap<String, Upload>>>,
    cached: Arc<RwLock<ToolCatalog>>,
    changes: tokio::sync::broadcast::Sender<ToolCatalog>,
}

impl ToolCenter {
    /// 打开独立的 `SQLite` 和包目录；未知数据库格式拒绝打开。
    ///
    /// # Errors
    /// 路径不安全、数据库损坏或版本不受支持时返回固定脱敏错误。
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(storage)?;
        reject_link(root)?;
        for name in ["packages", "uploads"] {
            let directory = root.join(name);
            std::fs::create_dir_all(&directory).map_err(storage)?;
            reject_link(&directory)?;
        }
        let database = root.join("tools.sqlite3");
        if database.exists() {
            reject_link(&database)?;
        }
        let mut connection = Connection::open_with_flags(
            &database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(storage)?;
        connection
            .busy_timeout(Duration::from_secs(2))
            .map_err(storage)?;
        connection
            .pragma_update(None, "trusted_schema", false)
            .map_err(storage)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(storage)?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(storage)?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let app: i64 = tx
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .map_err(storage)?;
        let version: i64 = tx
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(storage)?;
        if app == 0 && version == 0 {
            let count: i64 = tx
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                    [],
                    |r| r.get(0),
                )
                .map_err(storage)?;
            if count != 0 {
                return Err(error("unsupported_tool_store", "工具数据库格式不受支持"));
            }
            tx.execute_batch(SCHEMA).map_err(storage)?;
            tx.pragma_update(None, "application_id", 0x524f_5443_i64)
                .map_err(storage)?;
            tx.pragma_update(None, "user_version", 2).map_err(storage)?;
        } else if app == 0x524f_5443 && version == 1 {
            tx.execute_batch(
                "ALTER TABLE policy ADD COLUMN guide_markdown TEXT NOT NULL DEFAULT '';",
            )
            .map_err(storage)?;
            tx.pragma_update(None, "user_version", 2).map_err(storage)?;
        } else if app != 0x524f_5443 || version != 2 {
            return Err(error("unsupported_tool_store", "工具数据库格式不受支持"));
        }
        tx.commit().map_err(storage)?;
        let cached = catalog(&connection)?;
        let (changes, _) = tokio::sync::broadcast::channel(256);
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            root: root.into(),
            uploads: Arc::default(),
            cached: Arc::new(RwLock::new(cached)),
            changes,
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| storage("mutex"))
    }

    /// 读取供普通列表展示的脱敏资料。
    ///
    /// # Errors
    /// 数据库或已保存元数据无效时拒绝返回部分快照。
    pub fn snapshot(&self) -> Result<ToolCenterSnapshot> {
        let connection = self.connection()?;
        snapshot(&connection)
    }

    /// 取得包含真实外链地址的 Agent 目录；只供已认证的能力协商连接使用。
    ///
    /// # Errors
    /// 数据库或目录契约无效时拒绝。
    pub fn catalog(&self) -> Result<ToolCatalog> {
        let connection = self.connection()?;
        catalog(&connection)
    }

    /// 读取本进程最后提交的精确目录，不等待数据库或包操作。
    #[must_use]
    pub fn cached_catalog(&self) -> ToolCatalog {
        self.cached
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// 订阅每次已提交的目录修订；消费者遇到丢帧必须让 Agent 重新连接。
    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ToolCatalog> {
        self.changes.subscribe()
    }

    /// 返回托管包的本地路径；调用方仍须检查当前运行策略及权限。
    ///
    /// # Errors
    /// 摘要无效、未登记或文件不可用时拒绝。
    pub fn package_path(&self, sha256: &str) -> Result<PathBuf> {
        if !valid_sha(sha256) {
            return Err(error("package_not_found", "托管包不存在"));
        }
        let connection = self.connection()?;
        let found: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM packages WHERE sha256=?1 AND source_key='hosted')",
                [sha256],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if !found {
            return Err(error("package_not_found", "托管包不存在"));
        }
        let path = self.root.join("packages").join(format!("{sha256}.zip"));
        reject_link(&path)?;
        if !path.is_file() {
            return Err(error("package_not_found", "托管包不存在"));
        }
        Ok(path)
    }

    /// 执行已获外层授权的管理请求，检查上传操作者、幂等键及修订号。
    ///
    /// 慢速包下载和 ZIP 校验不持有数据库锁；不会阻塞中继心跳。
    pub async fn execute(&self, actor: &str, request: ToolCenterRequest) -> ToolCenterResponse {
        self.execute_authorized(actor, request, Arc::new(RwLock::new(true)))
            .await
    }

    /// 在外层可撤销授权下执行管理；提交持有授权读锁，撤权写锁返回后不会再提交。
    pub async fn execute_authorized(
        &self,
        actor: &str,
        request: ToolCenterRequest,
        authorization: Arc<RwLock<bool>>,
    ) -> ToolCenterResponse {
        let cancelled = CancelOnDrop(Arc::new(AtomicBool::new(false)));
        let result = self
            .execute_inner(actor, &request, &cancelled.0, &authorization)
            .await;
        match result {
            Ok(response) => response,
            Err(failure) => {
                let center = self.clone();
                let logged = request.clone();
                let operator = actor.to_string();
                let code = failure.code.clone();
                let revision = tokio::task::spawn_blocking(move || {
                    center.record_failure(&operator, &logged, &code)
                })
                .await
                .unwrap_or(0);
                ToolCenterResponse {
                    request_id: request.request_id,
                    revision,
                    error: Some(failure),
                    data: Value::Null,
                }
            }
        }
    }

    async fn execute_inner(
        &self,
        actor: &str,
        request: &ToolCenterRequest,
        cancelled: &Arc<AtomicBool>,
        authorization: &Arc<RwLock<bool>>,
    ) -> Result<ToolCenterResponse> {
        if actor.trim().is_empty() || actor.len() > 256 || !valid_id(&request.request_id) {
            return Err(error("invalid_management_request", "操作者或请求标识无效"));
        }
        let center = self.clone();
        let checked = request.clone();
        let operator = actor.to_string();
        if let Some(response) =
            tokio::task::spawn_blocking(move || center.check_request(&operator, &checked))
                .await
                .map_err(storage)??
        {
            return Ok(response);
        }
        let receipt = self.prepare_receipt(actor, &request.command).await?;
        let center = self.clone();
        let committed = request.clone();
        let operator = actor.to_string();
        let cancelled = Arc::clone(cancelled);
        let authorization = Arc::clone(authorization);
        tokio::task::spawn_blocking(move || {
            center.commit_request(&operator, &committed, receipt, &cancelled, &authorization)
        })
        .await
        .map_err(storage)?
    }

    fn record_failure(&self, actor: &str, request: &ToolCenterRequest, code: &str) -> u64 {
        let revision = self
            .connection()
            .and_then(|c| policy(&c))
            .map_or(0, |p| p.0);
        // 审计只记录固定动作与错误码。
        let _ = self.connection().and_then(|c| c.execute("INSERT INTO audit(request_id,actor,action,result,revision,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![bounded(&request.request_id,128),bounded(actor,256),request.command.action(),code,sql_number(revision)?,Utc::now().to_rfc3339()]).map(|_|()).map_err(storage));
        revision
    }

    fn check_request(
        &self,
        actor: &str,
        request: &ToolCenterRequest,
    ) -> Result<Option<ToolCenterResponse>> {
        let connection = self.connection()?;
        let fingerprint = fingerprint(request)?;
        if let Some(response) = replay(&connection, actor, &request.request_id, &fingerprint)? {
            return Ok(Some(response));
        }
        if !matches!(request.command, ToolCenterCommand::Snapshot)
            && policy(&connection)?.0 != request.expected_revision
        {
            return Err(error(
                "revision_conflict",
                "目录已被其他操作者修改，请刷新后重试",
            ));
        }
        Ok(None)
    }

    fn commit_request(
        &self,
        actor: &str,
        request: &ToolCenterRequest,
        receipt: Option<PackageReceipt>,
        cancelled: &AtomicBool,
        authorization: &RwLock<bool>,
    ) -> Result<ToolCenterResponse> {
        let fingerprint = fingerprint(request)?;
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let authorized = authorization.read().map_err(storage)?;
        if !*authorized {
            return Err(error("tool_management_denied", "工具管理权限已撤销"));
        }
        if let Some(response) = replay(&tx, actor, &request.request_id, &fingerprint)? {
            return Ok(response);
        }
        let (mut revision, _) = policy(&tx)?;
        if !matches!(request.command, ToolCenterCommand::Snapshot)
            && revision != request.expected_revision
        {
            return Err(error(
                "revision_conflict",
                "目录已被其他操作者修改，请刷新后重试",
            ));
        }
        check_cancelled(cancelled)?;
        let (data, changed) = self.apply_command(&tx, actor, &request.command, receipt)?;
        if changed {
            revision = revision
                .checked_add(1)
                .ok_or_else(|| storage("revision overflow"))?;
            tx.execute(
                "UPDATE policy SET revision=?1 WHERE id=1",
                [sql_number(revision)?],
            )
            .map_err(storage)?;
        }
        let response = ToolCenterResponse {
            request_id: request.request_id.clone(),
            revision,
            error: None,
            data,
        };
        tx.execute(
            "INSERT INTO requests(actor,request_id,fingerprint,response_json) VALUES(?1,?2,?3,?4)",
            params![
                actor,
                request.request_id,
                fingerprint,
                serde_json::to_string(&response).map_err(storage)?
            ],
        )
        .map_err(storage)?;
        tx.execute("INSERT INTO audit(request_id,actor,action,result,revision,created_at) VALUES(?1,?2,?3,'ok',?4,?5)", params![request.request_id,actor,request.command.action(),sql_number(revision)?,Utc::now().to_rfc3339()]).map_err(storage)?;
        check_cancelled(cancelled)?;
        tx.commit().map_err(storage)?;
        if changed {
            // 保持连接锁直到缓存和通知都已更新；取消请求不能丢失已提交的停用修订。
            let committed = catalog(&connection)?;
            *self
                .cached
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = committed.clone();
            let _ = self.changes.send(committed);
        }
        Ok(response)
    }
    async fn prepare_receipt(
        &self,
        actor: &str,
        command: &ToolCenterCommand,
    ) -> Result<Option<PackageReceipt>> {
        Ok(match command {
            ToolCenterCommand::ImportExternal {
                url,
                expected_sha256,
            } => Some(
                packages::import_external(
                    &self.root.join("uploads"),
                    url,
                    expected_sha256.as_deref(),
                )
                .await?,
            ),
            ToolCenterCommand::FinishUpload { upload_id } => {
                let center = self.clone();
                let operator = actor.to_string();
                let id = upload_id.clone();
                let upload =
                    tokio::task::spawn_blocking(move || center.take_upload(&operator, &id))
                        .await
                        .map_err(storage)??;
                Some(packages::finish(upload, &self.root.join("packages")).await?)
            }
            _ => None,
        })
    }

    fn take_upload(&self, actor: &str, upload_id: &str) -> Result<Upload> {
        let mut uploads = self.uploads.lock().map_err(storage)?;
        packages::expire(&mut uploads);
        let upload = uploads
            .get(upload_id)
            .filter(|u| u.actor == actor)
            .ok_or_else(|| error("upload_not_found", "上传不存在或不属于当前操作者"))?;
        if upload.offset != upload.size {
            return Err(error("incomplete_upload", "上传尚未完成"));
        }
        uploads
            .remove(upload_id)
            .ok_or_else(|| error("upload_not_found", "上传不存在"))
    }

    fn apply_command(
        &self,
        tx: &Transaction<'_>,
        actor: &str,
        command: &ToolCenterCommand,
        receipt: Option<PackageReceipt>,
    ) -> Result<(Value, bool)> {
        let mut changed = false;
        let data = match command {
            ToolCenterCommand::Snapshot => serde_json::to_value(snapshot(tx)?).map_err(storage)?,
            ToolCenterCommand::SetPolicy { enabled } => {
                tx.execute("UPDATE policy SET enabled=?1 WHERE id=1", [enabled])
                    .map_err(storage)?;
                changed = true;
                json!({"enabled":enabled})
            }
            ToolCenterCommand::SetGuide { guide_markdown } => {
                if guide_markdown.len() > 32 * 1024 || guide_markdown.contains('\0') {
                    return Err(error("invalid_guide", "全局说明超过 32 KiB 或含 NUL"));
                }
                changed = guide(tx)? != *guide_markdown;
                tx.execute(
                    "UPDATE policy SET guide_markdown=?1 WHERE id=1",
                    [guide_markdown],
                )
                .map_err(storage)?;
                json!({"guide_markdown":guide_markdown})
            }
            ToolCenterCommand::PutTool { tool } => {
                let tool = prepare_tool(tx, tool.clone())?;
                save_tool(tx, &tool)?;
                changed = true;
                json!({"tool_id": tool.id, "enabled":tool.enabled})
            }
            ToolCenterCommand::SetToolEnabled { tool_id, enabled } => {
                let mut tool =
                    read_tool(tx, tool_id)?.ok_or_else(|| error("tool_not_found", "工具不存在"))?;
                tool.enabled = *enabled;
                save_tool(tx, &tool)?;
                changed = true;
                json!({"tool_id":tool_id,"enabled":enabled})
            }
            ToolCenterCommand::RecordTest {
                tool_id,
                version,
                sha256,
                record,
            } => {
                let mut tool =
                    read_tool(tx, tool_id)?.ok_or_else(|| error("tool_not_found", "工具不存在"))?;
                record_test(&mut tool, version, sha256, record.clone(), actor)?;
                save_tool(tx, &tool)?;
                changed = true;
                json!({"tool_id":tool_id,"version":version,"sha256":sha256})
            }
            ToolCenterCommand::BeginUpload { .. }
            | ToolCenterCommand::UploadChunk { .. }
            | ToolCenterCommand::CancelUpload { .. } => self.apply_upload(actor, command)?,
            ToolCenterCommand::ImportExternal { .. } | ToolCenterCommand::FinishUpload { .. } => {
                let receipt = receipt.ok_or_else(|| storage("receipt"))?;
                let source_key = source_key(&receipt.source);
                tx.execute("INSERT INTO packages(sha256,source_key,size,source_json) VALUES(?1,?2,?3,?4) ON CONFLICT(sha256,source_key) DO UPDATE SET source_json=excluded.source_json WHERE size=excluded.size", params![receipt.sha256, source_key, sql_number(receipt.size)?, serde_json::to_string(&receipt.source).map_err(storage)?]).map_err(storage)?;
                let mut public = receipt;
                redact_source(&mut public.source);
                serde_json::to_value(public).map_err(storage)?
            }
        };
        Ok((data, changed))
    }

    fn apply_upload(&self, actor: &str, command: &ToolCenterCommand) -> Result<Value> {
        Ok(match command {
            ToolCenterCommand::BeginUpload { size, sha256 } => {
                let mut uploads = self.uploads.lock().map_err(storage)?;
                let id = packages::begin(
                    &self.root.join("uploads"),
                    actor,
                    *size,
                    sha256,
                    &mut uploads,
                )?;
                json!({"upload_id":id,"offset":0,"chunk_size":crate::PACKAGE_CHUNK_SIZE})
            }
            ToolCenterCommand::UploadChunk {
                upload_id,
                offset,
                data_base64,
            } => {
                let mut uploads = self.uploads.lock().map_err(storage)?;
                packages::expire(&mut uploads);
                let upload = uploads
                    .get_mut(upload_id)
                    .ok_or_else(|| error("upload_not_found", "上传不存在"))?;
                let offset = packages::chunk(upload, actor, *offset, data_base64)?;
                json!({"upload_id":upload_id,"offset":offset})
            }
            ToolCenterCommand::CancelUpload { upload_id } => {
                let mut uploads = self.uploads.lock().map_err(storage)?;
                if uploads.get(upload_id).is_none_or(|u| u.actor != actor) {
                    return Err(error("upload_not_found", "上传不存在或不属于当前操作者"));
                }
                uploads.remove(upload_id);
                json!({"cancelled":true})
            }
            _ => return Err(error("invalid_management_request", "上传动作无效")),
        })
    }
}

fn catalog(connection: &Connection) -> Result<ToolCatalog> {
    let (revision, enabled) = policy(connection)?;
    let mut tools = Vec::new();
    if enabled {
        for managed in read_tools(connection)?
            .into_iter()
            .filter(|tool| tool.enabled)
        {
            let documentation = remoteops_resources::ToolDocumentation {
                name: managed.name,
                purpose: managed.purpose,
                instructions: managed.instructions,
                examples: managed.examples,
                dependencies: managed.dependencies,
            };
            for version in managed.versions {
                for mut package in version.packages {
                    if package.file_verified {
                        package.tool.documentation = documentation.clone();
                        package.tool.platform_tests = package
                            .tests
                            .iter()
                            .map(|test| remoteops_resources::PlatformEvidence {
                                os: test.os.clone(),
                                arch: test.arch.clone(),
                                windows_build: test.windows_build,
                                desktop: test.desktop,
                                status: test.status.clone(),
                            })
                            .collect();
                        tools.push(package.tool);
                    }
                }
            }
        }
    }
    Ok(ToolCatalog {
        revision,
        enabled,
        guide_markdown: guide(connection)?,
        tools,
    })
}

fn reject_link(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(storage)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(error("unsafe_tool_path", "工具存储路径含链接或重解析点"));
        }
    }
    if metadata.file_type().is_symlink() {
        return Err(error("unsafe_tool_path", "工具存储路径含链接"));
    }
    Ok(())
}

fn bounded(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn fingerprint(request: &ToolCenterRequest) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(request).map_err(storage)?)
    ))
}

fn replay(
    connection: &Connection,
    actor: &str,
    id: &str,
    fingerprint: &str,
) -> Result<Option<ToolCenterResponse>> {
    let prior: Option<(String, String)> = connection
        .query_row(
            "SELECT fingerprint,response_json FROM requests WHERE actor=?1 AND request_id=?2",
            params![actor, id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    prior
        .map(|(old, response)| {
            if old != fingerprint {
                return Err(error("request_id_conflict", "请求标识已用于不同内容"));
            }
            serde_json::from_str(&response).map_err(storage)
        })
        .transpose()
}

fn policy(connection: &Connection) -> Result<(u64, bool)> {
    connection
        .query_row("SELECT revision,enabled FROM policy WHERE id=1", [], |r| {
            Ok((read_number(r, 0)?, r.get(1)?))
        })
        .map_err(storage)
}

fn guide(connection: &Connection) -> Result<String> {
    connection
        .query_row("SELECT guide_markdown FROM policy WHERE id=1", [], |row| {
            row.get(0)
        })
        .map_err(storage)
}

fn read_tool(connection: &Connection, id: &str) -> Result<Option<ManagedTool>> {
    let value: Option<String> = connection
        .query_row("SELECT document FROM tools WHERE id=?1", [id], |r| r.get(0))
        .optional()
        .map_err(storage)?;
    value
        .map(|s| serde_json::from_str(&s).map_err(storage))
        .transpose()
}

fn read_tools(connection: &Connection) -> Result<Vec<ManagedTool>> {
    let mut statement = connection
        .prepare("SELECT document FROM tools ORDER BY id")
        .map_err(storage)?;
    let rows = statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(storage)?;
    rows.map(|row| serde_json::from_str(&row.map_err(storage)?).map_err(storage))
        .collect()
}

fn snapshot(connection: &Connection) -> Result<ToolCenterSnapshot> {
    let (revision, enabled) = policy(connection)?;
    let mut tools = read_tools(connection)?;
    for tool in &mut tools {
        for version in &mut tool.versions {
            for package in &mut version.packages {
                redact_source(&mut package.source);
                if matches!(package.source, PackageSource::External { .. }) {
                    package.tool.package.url = redact_url(&package.tool.package.url);
                }
                package.tool.documentation_url =
                    redact_url_if_absolute(&package.tool.documentation_url);
            }
        }
    }
    let mut statement = connection.prepare("SELECT request_id,actor,action,result,revision,created_at FROM audit ORDER BY id DESC LIMIT 100").map_err(storage)?;
    let audit = statement
        .query_map([], |r| {
            Ok(ToolAudit {
                request_id: r.get(0)?,
                actor: r.get(1)?,
                action: r.get(2)?,
                result: r.get(3)?,
                revision: read_number(r, 4)?,
                created_at: r.get(5)?,
            })
        })
        .map_err(storage)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage)?;
    Ok(ToolCenterSnapshot {
        revision,
        enabled,
        guide_markdown: guide(connection)?,
        tools,
        audit,
    })
}

fn redact_url_if_absolute(value: &str) -> String {
    if url::Url::parse(value).is_ok() {
        redact_url(value)
    } else {
        value.into()
    }
}

fn redact_source(source: &mut PackageSource) {
    if let PackageSource::External { url } = source {
        *url = redact_url(url);
    }
}

fn source_key(source: &PackageSource) -> String {
    match source {
        PackageSource::Hosted => "hosted".into(),
        PackageSource::External { url } => format!("external:{:x}", Sha256::digest(url.as_bytes())),
    }
}

fn save_tool(tx: &Transaction<'_>, tool: &ManagedTool) -> Result<()> {
    let mut tools = read_tools(tx)?;
    tools.retain(|old| old.id != tool.id);
    tools.push(tool.clone());
    if tools.len() > 128 || serde_json::to_vec(&tools).map_err(storage)?.len() > 2 * 1024 * 1024 {
        return Err(error(
            "tool_center_limit",
            "工具中心资料超过数量或 2 MiB 限制",
        ));
    }
    let descriptors = tools
        .iter()
        .flat_map(|t| &t.versions)
        .flat_map(|v| &v.packages)
        .map(|p| p.tool.clone())
        .collect();
    validate_manifest(
        &serde_json::to_vec(&ResourceIndex {
            schema_version: remoteops_resources::RESOURCE_TOOLS_SCHEMA_VERSION,
            tools: descriptors,
        })
        .map_err(storage)?,
    )
    .map_err(|e| error(&e.code, &e.message))?;
    tx.execute("INSERT INTO tools(id,document) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET document=excluded.document", params![tool.id,serde_json::to_string(tool).map_err(storage)?]).map(|_| ()).map_err(storage)
}

fn package_identity(package: &crate::ToolPackage) -> String {
    let platforms = package
        .tool
        .platforms
        .iter()
        .map(|p| format!("{}:{}", p.os, p.arch))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    format!("{}:{platforms}", package.tool.package.sha256)
}

fn prepare_tool(tx: &Transaction<'_>, mut tool: ManagedTool) -> Result<ManagedTool> {
    if !valid_id(&tool.id)
        || tool.name.trim().is_empty()
        || tool.name.len() > 256
        || tool.purpose.trim().is_empty()
        || tool.instructions.trim().is_empty()
        || tool.versions.is_empty()
        || tool.versions.len() > 128
    {
        return Err(error(
            "invalid_tool",
            "工具标识、名称、用途、说明或版本无效",
        ));
    }
    if serde_json::to_vec(&tool).map_err(storage)?.len() > 2 * 1024 * 1024 {
        return Err(error("tool_limit", "工具资料超过 2 MiB"));
    }
    let existing = read_tool(tx, &tool.id)?;
    tool.enabled = existing.as_ref().is_some_and(|old| old.enabled);
    let mut versions = BTreeSet::new();
    for version in &mut tool.versions {
        if !valid_id(&version.version)
            || version.version == "latest"
            || !versions.insert(version.version.clone())
            || version.packages.is_empty()
            || version.packages.len() > 32
        {
            return Err(error("invalid_tool_version", "版本或平台包无效"));
        }
        let previous_version = existing.as_ref().and_then(|old| {
            old.versions
                .iter()
                .find(|old| old.version == version.version)
        });
        let mut identities = BTreeSet::new();
        for package in &mut version.packages {
            if package.tool.id != tool.id
                || package.tool.version != version.version
                || !identities.insert(package_identity(package))
            {
                return Err(error(
                    "invalid_tool_package",
                    "工具、版本与平台包身份不一致",
                ));
            }
            let previous = previous_version.and_then(|old| {
                old.packages
                    .iter()
                    .find(|old| package_identity(old) == package_identity(package))
            });
            prepare_package(tx, package, previous)?;
        }
        if let Some(previous) = previous_version {
            let old = previous
                .packages
                .iter()
                .map(package_identity)
                .collect::<BTreeSet<_>>();
            if !old.is_subset(&identities) {
                return Err(error(
                    "immutable_tool_version",
                    "已有版本的包内容不能替换或删除，请新增版本",
                ));
            }
            // 同平台内容变更必须新增版本，不能把新摘要作为额外平台包绕过。
            for old_package in &previous.packages {
                for new_package in &version.packages {
                    if old_package.tool.package.sha256 != new_package.tool.package.sha256
                        && old_package.tool.platforms.iter().any(|old| {
                            new_package
                                .tool
                                .platforms
                                .iter()
                                .any(|new| old.os == new.os && old.arch == new.arch)
                        })
                    {
                        return Err(error(
                            "immutable_tool_version",
                            "相同版本和平台不能更换包内容",
                        ));
                    }
                }
            }
        }
    }
    if existing.as_ref().is_some_and(|old| {
        old.versions
            .iter()
            .any(|version| !versions.contains(&version.version))
    }) {
        return Err(error("immutable_tool_version", "不能删除已登记版本"));
    }
    Ok(tool)
}

fn prepare_package(
    tx: &Transaction<'_>,
    package: &mut crate::ToolPackage,
    previous: Option<&crate::ToolPackage>,
) -> Result<()> {
    // 仅约束新执行配置，不阻断旧数据库读取、目录同步或资料修改。
    if package.tool.startup.interface == remoteops_resources::ExecutionInterface::StdioMcp
        && !package.tool.startup.browser_executable.is_empty()
        && package.tool.startup.browser_adapter.is_none()
        && previous.is_none_or(|old| old.tool.startup != package.tool.startup)
    {
        return Err(error(
            "browser_adapter_required",
            "浏览器 MCP 新增或启动配置变更必须显式设置 browser_adapter 为 playwright 或 agent_browser",
        ));
    }
    resolve_external_source(tx, &package.tool.package.sha256, &mut package.source)?;
    let size: Option<u64> = tx
        .query_row(
            "SELECT size FROM packages WHERE sha256=?1 AND source_key=?2",
            params![package.tool.package.sha256, source_key(&package.source)],
            |r| read_number(r, 0),
        )
        .optional()
        .map_err(storage)?;
    if size != Some(package.tool.package.size) {
        return Err(error(
            "unverified_package",
            "包尚未完成文件校验，请先上传或导入外链",
        ));
    }
    package.file_verified = true;
    package.tests = previous
        .filter(|old| {
            old.tool.startup == package.tool.startup
                && old.tool.requires_desktop == package.tool.requires_desktop
        })
        .map_or_else(Vec::new, |old| old.tests.clone());
    package.tool.package.url = match &package.source {
        PackageSource::Hosted => {
            format!("relay://packages/{}.zip", package.tool.package.sha256)
        }
        PackageSource::External { url } => {
            packages::external_url(url)?;
            url.clone()
        }
    };
    package.tool.platform_tests.clear();
    for platform in &mut package.tool.platforms {
        if platform.status != "unsupported" {
            platform.status = "pending".into();
        }
    }
    apply_tests(package);
    let index = ResourceIndex {
        schema_version: remoteops_resources::RESOURCE_TOOLS_SCHEMA_VERSION,
        tools: vec![package.tool.clone()],
    };
    validate_manifest(&serde_json::to_vec(&index).map_err(storage)?)
        .map_err(|e| error(&e.code, &e.message))?;
    Ok(())
}

fn resolve_external_source(
    tx: &Transaction<'_>,
    sha256: &str,
    source: &mut PackageSource,
) -> Result<()> {
    let PackageSource::External { url } = source else {
        return Ok(());
    };
    if packages::external_url(url)?.query().is_some() {
        return Ok(());
    }
    // 普通回执及列表去掉签名；按已经校验的摘要与基址恢复最近的完整地址。
    let mut statement=tx.prepare("SELECT source_json FROM packages WHERE sha256=?1 AND source_key<>'hosted' ORDER BY rowid DESC").map_err(storage)?;
    let rows = statement
        .query_map([sha256], |r| r.get::<_, String>(0))
        .map_err(storage)?;
    for row in rows {
        let candidate: PackageSource =
            serde_json::from_str(&row.map_err(storage)?).map_err(storage)?;
        if let PackageSource::External { url: actual } = candidate
            && redact_url(&actual) == *url
        {
            *url = actual;
            break;
        }
    }
    Ok(())
}

fn record_test(
    tool: &mut ManagedTool,
    version: &str,
    sha256: &str,
    mut record: PlatformTest,
    actor: &str,
) -> Result<()> {
    if !matches!(record.status.as_str(), "passed" | "failed" | "pending")
        || record.environment.trim().is_empty()
        || record.result.trim().is_empty()
        || record.environment.len() > 8192
        || record.result.len() > 32768
        || chrono::DateTime::parse_from_rfc3339(&record.tested_at).is_err()
        || (record.os == "windows" && record.status != "pending" && record.windows_build.is_none())
    {
        return Err(error(
            "invalid_platform_test",
            "测试环境、时间、结果或 Windows build 无效",
        ));
    }
    record.recorded_by = actor.into();
    let package = tool
        .versions
        .iter_mut()
        .find(|v| v.version == version)
        .and_then(|v| {
            v.packages.iter_mut().find(|p| {
                p.tool.package.sha256 == sha256
                    && p.tool
                        .platforms
                        .iter()
                        .any(|p| p.os == record.os && p.arch == record.arch)
            })
        })
        .ok_or_else(|| error("tool_package_not_found", "工具版本或测试平台不存在"))?;
    if record.status == "passed" && package.tool.requires_desktop && !record.desktop {
        return Err(error(
            "desktop_test_required",
            "该工具需要真实交互桌面测试，headless 结果不能标记通过",
        ));
    }
    if package.tests.len() >= 256 {
        return Err(error("platform_test_limit", "平台测试记录数量超过限制"));
    }
    package.tests.push(record);
    apply_tests(package);
    Ok(())
}

fn apply_tests(package: &mut crate::ToolPackage) {
    package.tool.platform_tests = package
        .tests
        .iter()
        .map(|test| remoteops_resources::PlatformEvidence {
            os: test.os.clone(),
            arch: test.arch.clone(),
            windows_build: test.windows_build,
            desktop: test.desktop,
            status: test.status.clone(),
        })
        .collect();
    for platform in &mut package.tool.platforms {
        if platform.status == "unsupported" {
            continue;
        }
        if let Some(test) = package
            .tests
            .iter()
            .rev()
            .find(|t| t.os == platform.os && t.arch == platform.arch)
        {
            platform.status =
                if test.status == "passed" && (!package.tool.requires_desktop || test.desktop) {
                    "passed"
                } else {
                    "pending"
                }
                .into();
            platform.os_versions.clone_from(&test.environment);
            if platform.os == "windows" && test.status == "passed" {
                platform.min_windows_build = test.windows_build;
                platform.max_windows_build = test.windows_build;
            }
        }
    }
}

const SCHEMA: &str = "
CREATE TABLE policy(id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL,enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),guide_markdown TEXT NOT NULL DEFAULT '');
INSERT INTO policy(id,revision,enabled) VALUES(1,0,0);
CREATE TABLE tools(id TEXT PRIMARY KEY,document TEXT NOT NULL);
CREATE TABLE packages(sha256 TEXT NOT NULL,source_key TEXT NOT NULL,size INTEGER NOT NULL,source_json TEXT NOT NULL,PRIMARY KEY(sha256,source_key));
CREATE TABLE requests(actor TEXT NOT NULL,request_id TEXT NOT NULL,fingerprint TEXT NOT NULL,response_json TEXT NOT NULL,PRIMARY KEY(actor,request_id));
CREATE TABLE audit(id INTEGER PRIMARY KEY,request_id TEXT NOT NULL,actor TEXT NOT NULL,action TEXT NOT NULL,result TEXT NOT NULL,revision INTEGER NOT NULL,created_at TEXT NOT NULL);
";

fn sql_number(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(storage)
}

fn read_number(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let number: i64 = row.get(index)?;
    u64::try_from(number).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, number))
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(error("management_cancelled", "管理请求已取消"));
    }
    Ok(())
}
