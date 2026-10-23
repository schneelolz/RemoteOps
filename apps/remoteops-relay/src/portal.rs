//! 版本化门户 HTTP 入口；集成凭据与全局管理凭据严格分开。
use crate::admin::{AdminState, authorize};
use axum::{
    Json, Router,
    extract::{
        ConnectInfo, DefaultBodyLimit, Path, Query, Request, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, post},
};
use remoteops_portal::{
    AdminAuditEvent as PortalAdminAuditEvent, AdminOperation, Asset, ConnectionSummary,
    CreateAssetRequest, CreatePrincipalRequest, CreatedAsset, CreatedPrincipal, InvitationSummary,
    IssueInvitationRequest, IssuedInvitation, PortalError, Principal, TransferAssetRequest,
    UpdatePrincipalAssetsRequest,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use uuid::Uuid;

type RateLimitEntries = HashMap<Option<IpAddr>, (Instant, u32)>;
type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorBody>)>;

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    error: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseRequest {
    expected_revision: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortalAuditQuery {
    limit: Option<usize>,
}

pub(crate) fn router() -> Router<AdminState> {
    Router::new()
        .route("/api/admin/portal/principals", post(create_principal))
        .route(
            "/api/admin/portal/principals/{principal_id}",
            get(admin_principal),
        )
        .route(
            "/api/admin/portal/principals/{principal_id}/assets",
            post(update_principal_assets),
        )
        .route(
            "/api/admin/portal/principals/{principal_id}/revoke",
            post(revoke_principal),
        )
        .route("/api/admin/portal/assets", post(create_asset))
        .route("/api/admin/portal/assets/{target_id}", get(admin_asset))
        .route(
            "/api/admin/portal/assets/{target_id}/transfer",
            post(transfer_asset),
        )
        .route(
            "/api/admin/portal/operations/{operation_id}",
            get(admin_operation),
        )
        .route("/api/admin/portal/audit", get(admin_audit))
        .route(
            "/api/portal/v1/assets/{asset_id}/remote-access",
            get(status),
        )
        .route("/api/portal/v1/assets/{asset_id}/invitations", post(issue))
        .route(
            "/api/portal/v1/invitations/{invitation_id}",
            delete(revoke_invitation),
        )
        .route(
            "/api/portal/v1/connections/{connection_id}",
            get(connection),
        )
        .route(
            "/api/portal/v1/connections/{connection_id}/close",
            post(close),
        )
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(middleware::from_fn(response_headers))
}

async fn response_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn bearer(headers: &HeaderMap) -> Result<&str, (StatusCode, Json<ErrorBody>)> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| v.len() <= 128 && v.starts_with("ropp1."))
        .ok_or_else(|| error(PortalError::Unauthorized))
}

async fn require_admin(
    state: &AdminState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<ErrorBody>)> {
    authorize(state, headers)
        .await
        .map_err(|_| error(PortalError::Unauthorized))
}

// 只采用 TCP peer；不能由调用方通过 X-Forwarded-For 改变预算。
fn rate_limit(peer: IpAddr) -> Result<(), (StatusCode, Json<ErrorBody>)> {
    static LIMITS: OnceLock<Mutex<RateLimitEntries>> = OnceLock::new();
    let now = Instant::now();
    let mut entries = LIMITS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| error(PortalError::Storage))?;
    entries.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_mins(1));
    if entries.len() >= 1024 && !entries.contains_key(&Some(peer)) {
        return Err(limited());
    }
    if entries.get(&None).is_some_and(|(_, count)| *count >= 1024)
        || entries
            .get(&Some(peer))
            .is_some_and(|(_, count)| *count >= 120)
    {
        return Err(limited());
    }
    for key in [None, Some(peer)] {
        entries.entry(key).or_insert((now, 0)).1 += 1;
    }
    Ok(())
}
fn limited() -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(ErrorBody {
            code: "rate_limited",
            error: "门户请求过于频繁".into(),
        }),
    )
}

async fn create_principal(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Result<Json<CreatePrincipalRequest>, JsonRejection>,
) -> ApiResult<CreatedPrincipal> {
    require_admin(&state, &headers).await?;
    rate_limit(peer.ip())?;
    let Json(request) = request.map_err(invalid_json)?;
    state
        .relay
        .portal_create_principal(request)
        .await
        .map(Json)
        .map_err(error)
}

async fn admin_principal(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Principal> {
    require_admin(&state, &headers).await?;
    rate_limit(peer.ip())?;
    state
        .relay
        .portal_get_principal(id)
        .await
        .map(Json)
        .map_err(error)
}

async fn admin_asset(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(target_id): Path<String>,
) -> ApiResult<Asset> {
    require_admin(&state, &headers).await?;
    rate_limit(peer.ip())?;
    state
        .relay
        .portal_get_asset(&target_id)
        .await
        .map(Json)
        .map_err(error)
}

async fn update_principal_assets(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    request: Result<Json<UpdatePrincipalAssetsRequest>, JsonRejection>,
) -> ApiResult<AdminOperation> {
    require_admin(&state, &headers).await?;
    rate_limit(peer.ip())?;
    // 只记录已验证的管理身份类型及实际 TCP peer，不接受正文自报操作者。
    let actor = format!("relay_admin@{}", peer.ip());
    let request = admin_payload(
        &state,
        &actor,
        "update_principal_assets",
        &id.to_string(),
        request,
    )
    .await?;
    state
        .relay
        .portal_update_principal_assets(id, request, &actor)
        .await
        .map(Json)
        .map_err(error)
}

async fn transfer_asset(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(target_id): Path<String>,
    request: Result<Json<TransferAssetRequest>, JsonRejection>,
) -> ApiResult<AdminOperation> {
    require_admin(&state, &headers).await?;
    rate_limit(peer.ip())?;
    let actor = format!("relay_admin@{}", peer.ip());
    let request = admin_payload(&state, &actor, "transfer_asset", &target_id, request).await?;
    state
        .relay
        .portal_transfer_asset(&target_id, request, &actor)
        .await
        .map(Json)
        .map_err(error)
}

async fn admin_payload<T>(
    state: &AdminState,
    actor: &str,
    kind: &str,
    resource: &str,
    request: Result<Json<T>, JsonRejection>,
) -> Result<T, (StatusCode, Json<ErrorBody>)> {
    match request {
        Ok(Json(value)) => Ok(value),
        Err(rejection) => {
            let _ = state
                .relay
                .portal_record_admin_failure(actor, kind, resource, &PortalError::InvalidInput)
                .await;
            Err(invalid_json(rejection))
        }
    }
}

async fn admin_operation(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<AdminOperation> {
    require_admin(&state, &headers).await?;
    rate_limit(peer.ip())?;
    state
        .relay
        .portal_admin_operation(id)
        .await
        .map(Json)
        .map_err(error)
}

async fn admin_audit(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    query: Result<Query<PortalAuditQuery>, QueryRejection>,
) -> ApiResult<Vec<PortalAdminAuditEvent>> {
    require_admin(&state, &headers).await?;
    rate_limit(peer.ip())?;
    let Query(query) = query.map_err(|_| error(PortalError::InvalidInput))?;
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(error(PortalError::InvalidInput));
    }
    state
        .relay
        .portal_admin_audit(limit)
        .await
        .map(Json)
        .map_err(error)
}
async fn create_asset(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Result<Json<CreateAssetRequest>, JsonRejection>,
) -> ApiResult<CreatedAsset> {
    require_admin(&state, &headers).await?;
    rate_limit(peer.ip())?;
    let Json(request) = request.map_err(invalid_json)?;
    state
        .relay
        .portal_create_asset(request)
        .await
        .map(Json)
        .map_err(error)
}
async fn revoke_principal(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Vec<ConnectionSummary>> {
    require_admin(&state, &headers).await?;
    rate_limit(peer.ip())?;
    state
        .relay
        .portal_revoke_principal(id)
        .await
        .map(Json)
        .map_err(error)
}
async fn status(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(asset_id): Path<String>,
) -> ApiResult<serde_json::Value> {
    rate_limit(peer.ip())?;
    state
        .relay
        .portal_status(bearer(&headers)?, &asset_id)
        .await
        .map(Json)
        .map_err(error)
}
async fn issue(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(asset_id): Path<String>,
    request: Result<Json<IssueInvitationRequest>, JsonRejection>,
) -> ApiResult<IssuedInvitation> {
    rate_limit(peer.ip())?;
    let Json(request) = request.map_err(invalid_json)?;
    state
        .relay
        .portal_issue(bearer(&headers)?, &asset_id, request)
        .await
        .map(Json)
        .map_err(error)
}
async fn revoke_invitation(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<InvitationSummary> {
    rate_limit(peer.ip())?;
    state
        .relay
        .portal_revoke_invitation(bearer(&headers)?, id)
        .await
        .map(Json)
        .map_err(error)
}
async fn connection(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<ConnectionSummary> {
    rate_limit(peer.ip())?;
    state
        .relay
        .portal_connection(bearer(&headers)?, id)
        .await
        .map(Json)
        .map_err(error)
}
async fn close(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    request: Result<Json<CloseRequest>, JsonRejection>,
) -> ApiResult<ConnectionSummary> {
    rate_limit(peer.ip())?;
    let Json(request) = request.map_err(invalid_json)?;
    state
        .relay
        .portal_close(bearer(&headers)?, id, request.expected_revision)
        .await
        .map(Json)
        .map_err(error)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "作为 map_err 回调接收拥有所有权的拒绝值"
)]
fn invalid_json(rejection: JsonRejection) -> (StatusCode, Json<ErrorBody>) {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(ErrorBody {
                code: "payload_too_large",
                error: "门户请求正文超过上限".into(),
            }),
        )
    } else {
        error(PortalError::InvalidInput)
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "作为 map_err 回调接收拥有所有权的错误值"
)]
fn error(value: PortalError) -> (StatusCode, Json<ErrorBody>) {
    let (status, code) = match &value {
        PortalError::InvalidInput => (StatusCode::BAD_REQUEST, "invalid_request"),
        PortalError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
        PortalError::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
        PortalError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        PortalError::Conflict => (StatusCode::CONFLICT, "conflict"),
        PortalError::RevisionConflict => (StatusCode::CONFLICT, "revision_conflict"),
        PortalError::OwnershipConflict => (StatusCode::CONFLICT, "ownership_conflict"),
        PortalError::IdempotencyConflict => (StatusCode::CONFLICT, "idempotency_conflict"),
        PortalError::NameConflict => (StatusCode::CONFLICT, "name_conflict"),
        PortalError::Expired => (StatusCode::GONE, "expired"),
        PortalError::Revoked => (StatusCode::GONE, "revoked"),
        PortalError::TargetMismatch => (StatusCode::CONFLICT, "target_mismatch"),
        PortalError::Storage => (StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    };
    (
        status,
        Json(ErrorBody {
            code,
            error: value.to_string(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::Relay;
    use remoteops_domain::ControllerOwnerId;
    use std::{path::PathBuf, sync::Arc};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    include!("portal_admin_tests.rs");

    struct HttpFixture {
        address: SocketAddr,
        task: tokio::task::JoinHandle<anyhow::Result<()>>,
        root: PathBuf,
        temp: PathBuf,
    }
    impl Drop for HttpFixture {
        fn drop(&mut self) {
            self.task.abort();
            if self.root.parent() == Some(self.temp.as_path()) {
                // 仅清理本测试创建的随机目录；文件句柄仍占用时留给环境清理。
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
    }
    impl HttpFixture {
        async fn new(owner: ControllerOwnerId) -> Self {
            let temp = std::env::temp_dir().canonicalize().unwrap();
            let root = temp.join(format!("remoteops-portal-http-{}", Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let relay = Arc::new(
                Relay::new_with_state_path(
                    chrono::Duration::minutes(10),
                    5,
                    owner,
                    "test-human".into(),
                    "test-ai".into(),
                    root.join("state.json"),
                )
                .unwrap(),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(crate::admin::serve(
                listener,
                relay,
                Some("test-admin".into()),
                None,
                None,
                false,
            ));
            Self {
                address,
                task,
                root,
                temp,
            }
        }
        async fn request(
            &self,
            method: &str,
            path: &str,
            token: Option<&str>,
            body: &str,
        ) -> (u16, serde_json::Value) {
            let mut stream = tokio::net::TcpStream::connect(self.address).await.unwrap();
            let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
            let request = format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut bytes = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut bytes))
                .await
                .unwrap()
                .unwrap();
            let response = String::from_utf8(bytes).unwrap();
            let (headers, body) = response.split_once("\r\n\r\n").unwrap();
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("cache-control: no-store")
            );
            let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
            let body = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
            (status, body)
        }
        async fn shutdown(mut self) {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "单一真实 HTTP 场景顺序核验认证与请求边界"
    )]
    async fn actual_http_separates_admin_and_portal_authority_and_bounds_requests() {
        let owner = ControllerOwnerId::new();
        let fixture = HttpFixture::new(owner).await;
        let create =
            serde_json::json!({"name":"ops-test","owner_id":owner,"allowed_assets":["asset-a"]})
                .to_string();
        let (status, _) = fixture
            .request("POST", "/api/admin/portal/principals", None, &create)
            .await;
        assert_eq!(status, 401);
        let (status, created) = fixture
            .request(
                "POST",
                "/api/admin/portal/principals",
                Some("test-admin"),
                &create,
            )
            .await;
        assert_eq!(status, 200);
        let token = created["token"].as_str().unwrap();
        let principal = created["principal"]["principal_id"].as_str().unwrap();
        let (status, _) = fixture
            .request("POST", "/api/admin/portal/principals", Some(token), &create)
            .await;
        assert_eq!(status, 401);
        let asset = serde_json::json!({"asset_id":"asset-a","principal_id":principal}).to_string();
        let (status, registered) = fixture
            .request(
                "POST",
                "/api/admin/portal/assets",
                Some("test-admin"),
                &asset,
            )
            .await;
        assert_eq!(status, 200);
        assert!(registered["enrollment_token"].is_string());
        for token in [None, Some("test-admin")] {
            let (status, _) = fixture
                .request(
                    "GET",
                    "/api/portal/v1/assets/asset-a/remote-access",
                    token,
                    "",
                )
                .await;
            assert_eq!(status, 401);
        }
        let (status, state) = fixture
            .request(
                "GET",
                "/api/portal/v1/assets/asset-a/remote-access",
                Some(token),
                "",
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(state["online"], false);
        assert!(state.get("enrollment_token").is_none());
        assert!(state.get("token").is_none());
        let (status, _) = fixture
            .request(
                "GET",
                "/api/portal/v1/assets/asset-b/remote-access",
                Some(token),
                "",
            )
            .await;
        assert_eq!(status, 403);
        let invalid=serde_json::json!({"idempotency_key":Uuid::new_v4(),"system_id":"forged","replace_active":false}).to_string();
        let (status, _) = fixture
            .request(
                "POST",
                "/api/portal/v1/assets/asset-a/invitations",
                Some(token),
                &invalid,
            )
            .await;
        assert_eq!(status, 400);
        let (status, _) = fixture
            .request(
                "POST",
                "/api/portal/v1/assets/asset-a/invitations",
                Some(token),
                &"x".repeat(33 * 1024),
            )
            .await;
        assert_eq!(status, 413);
        let revoke = format!("/api/admin/portal/principals/{principal}/revoke");
        let (status, _) = fixture
            .request("POST", &revoke, Some("test-admin"), "{}")
            .await;
        assert_eq!(status, 200);
        let (status, _) = fixture
            .request(
                "GET",
                "/api/portal/v1/assets/asset-a/remote-access",
                Some(token),
                "",
            )
            .await;
        assert_eq!(status, 401);
        fixture.shutdown().await;
    }
}
