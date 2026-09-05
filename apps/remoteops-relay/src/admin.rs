use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration as StdDuration, Instant},
};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Json as ExtractJson, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use remoteops_domain::{
    AgentInstanceId, ControllerInstanceId, ControllerOwnerId, PermissionMode, SessionId,
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpListener, sync::Mutex};
use uuid::Uuid;

use crate::relay::{AdminAuditEvent, Relay};

#[derive(Clone)]
pub(crate) struct AdminState {
    pub relay: Arc<Relay>,
    pub token: Arc<String>,
    pub username: Option<Arc<String>>,
    pub password: Option<Arc<String>>,
    pub sessions: Arc<Mutex<HashMap<String, Instant>>>,
    pub secure_cookie: bool,
    login_attempts: Arc<Mutex<HashMap<(String, String), LoginAttempt>>>,
}

#[derive(Clone, Debug)]
struct LoginAttempt {
    failures: u32,
    blocked_until: Instant,
    last_seen: Instant,
}

const MAX_LOGIN_ATTEMPTS: usize = 2_048;
const LOGIN_ATTEMPT_TTL: StdDuration = StdDuration::from_mins(15);
const MAX_USERNAME_BYTES: usize = 256;
const MAX_PASSWORD_BYTES: usize = 1_024;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AdminSnapshot {
    pub overview: AdminOverview,
    pub identity: AdminIdentity,
    pub agents: Vec<AdminAgent>,
    pub controllers: Vec<AdminController>,
    pub sessions: Vec<AdminSession>,
    pub audit: Vec<AdminAuditEvent>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AdminOverview {
    pub owner_id: ControllerOwnerId,
    pub version: String,
    pub uptime_seconds: u64,
    pub online_agents: usize,
    pub active_sessions: usize,
    pub connected_controllers: usize,
    pub pending_approvals: usize,
    pub in_flight_requests: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AdminIdentity {
    pub owner_id: ControllerOwnerId,
    pub human_token_configured: bool,
    pub ai_token_configured: bool,
    pub ai_token_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AdminAgent {
    pub agent_instance_id: AgentInstanceId,
    pub session_id: SessionId,
    pub hostname: String,
    pub mac_address: Option<String>,
    pub operating_system: String,
    pub state: String,
    pub pairing_code_configured: bool,
    pub lease_expires_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub connection_generation: u64,
    pub ready: bool,
    pub ever_paired: bool,
    pub permission_mode: PermissionMode,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AdminController {
    pub controller_instance_id: ControllerInstanceId,
    pub kind: String,
    pub owner_id: ControllerOwnerId,
    pub connection_generation: u64,
    pub session_ids: Vec<SessionId>,
    pub hostname: Option<String>,
    pub mac_address: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AdminControllerBinding {
    pub controller_instance_id: ControllerInstanceId,
    pub kind: String,
    pub owner_id: ControllerOwnerId,
    pub permission_mode: PermissionMode,
    pub controller_hostname: Option<String>,
    pub controller_mac_address: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AdminSession {
    pub session_id: SessionId,
    pub agent_instance_id: AgentInstanceId,
    pub hostname: String,
    pub mac_address: Option<String>,
    pub operating_system: String,
    pub state: String,
    pub role: String,
    pub permission_mode: PermissionMode,
    pub owner_id: Option<ControllerOwnerId>,
    pub controller_bindings: Vec<AdminControllerBinding>,
    pub pending_approvals: usize,
    pub in_flight_requests: usize,
    pub lease_expires_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub connection_generation: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AdminActionOutcome {
    pub success: bool,
    pub changed: bool,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChangePasswordRequest {
    #[serde(rename = "current_password")]
    current: String,
    #[serde(rename = "new_password")]
    new: String,
    #[serde(rename = "confirm_password")]
    confirm: String,
}

#[derive(Debug, Serialize)]
struct LoginResponse {
    authenticated: bool,
    username: String,
}

#[derive(Debug, Serialize)]
struct SessionResponse {
    authenticated: bool,
    username: Option<String>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AuditQuery {
    limit: Option<usize>,
}

pub(crate) async fn serve(
    listener: TcpListener,
    relay: Arc<Relay>,
    token: Option<String>,
    username: Option<String>,
    password: Option<String>,
    secure_cookie: bool,
) -> anyhow::Result<()> {
    let state = AdminState {
        relay,
        token: Arc::new(token.unwrap_or_default()),
        username: username.map(Arc::new),
        password: password.map(Arc::new),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        secure_cookie,
        login_attempts: Arc::new(Mutex::new(HashMap::new())),
    };
    let app = Router::new()
        .route("/api/admin/login", post(login))
        .route("/api/admin/logout", post(logout))
        .route("/api/admin/session", get(session_status))
        .route("/api/admin/password", post(change_password))
        .route("/api/admin/overview", get(overview))
        .route("/api/admin/identity", get(identity))
        .route("/api/admin/agents", get(agents))
        .route("/api/admin/sessions", get(sessions))
        .route(
            "/api/admin/sessions/closed/clear",
            post(clear_closed_sessions),
        )
        .route("/api/admin/sessions/{session_id}", get(session_detail))
        .route(
            "/api/admin/sessions/{session_id}/close",
            post(close_session),
        )
        .route(
            "/api/admin/sessions/{session_id}/emergency-stop",
            post(emergency_stop),
        )
        .route("/api/admin/audit", get(audit))
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .layer(DefaultBodyLimit::max(8 * 1024))
        .with_state(state)
        .fallback(index);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("管理 HTTP 服务已停止")
}

async fn login(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ExtractJson(request): ExtractJson<LoginRequest>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    if request.username.len() > MAX_USERNAME_BYTES || request.password.len() > MAX_PASSWORD_BYTES {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "用户名或密码长度超出限制".to_owned(),
            }),
        ));
    }
    let peer = peer.to_string();
    if !login_allowed(&state, &peer, &request.username).await {
        state.relay.admin_auth_failure(&peer).await;
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse {
                error: "登录尝试过于频繁，请稍后重试".to_owned(),
            }),
        ));
    }
    let username_valid = state
        .username
        .as_ref()
        .is_some_and(|username| constant_time_eq(request.username.as_bytes(), username.as_bytes()));
    let password_valid = if state.relay.admin_password_configured().await {
        state.relay.admin_password_matches(&request.password).await
    } else {
        state.password.as_ref().is_some_and(|password| {
            constant_time_eq(request.password.as_bytes(), password.as_bytes())
        })
    };
    let valid = username_valid && password_valid;
    if !valid {
        record_login_failure(&state, &peer, &request.username).await;
        let source = request_source(&headers);
        state.relay.admin_auth_failure(&source).await;
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "用户名或密码错误".to_owned(),
            }),
        ));
    }
    clear_login_failures(&state, &peer, &request.username).await;
    let session_id = Uuid::new_v4().simple().to_string();
    state.sessions.lock().await.insert(
        session_id.clone(),
        Instant::now() + StdDuration::from_hours(8),
    );
    state.relay.admin_auth_success(&request.username).await;
    let cookie = session_cookie(&session_id, state.secure_cookie, false);
    let mut response = Json(LoginResponse {
        authenticated: true,
        username: request.username,
    })
    .into_response();
    response.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        cookie.parse().expect("管理 Session Cookie 应为合法 Header"),
    );
    Ok(response)
}

async fn change_password(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ExtractJson(request): ExtractJson<ChangePasswordRequest>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    if request.new != request.confirm {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "两次输入的新密码不一致".to_owned(),
            }),
        ));
    }
    let outcome = state
        .relay
        .admin_change_password(
            &request.current,
            &request.new,
            &request_source(&headers),
            state.password.as_deref().map(String::as_str),
        )
        .await;
    if !outcome.success {
        let status = if outcome.message == "当前密码不正确" {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::BAD_REQUEST
        };
        return Err((
            status,
            Json(ErrorResponse {
                error: outcome.message,
            }),
        ));
    }
    let mut response = Json(outcome).into_response();
    response.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        session_cookie("", state.secure_cookie, true)
            .parse()
            .expect("管理 Session Cookie 应为合法 Header"),
    );
    state.sessions.lock().await.clear();
    Ok(response)
}

async fn session_status(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<SessionResponse>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    Ok(Json(SessionResponse {
        authenticated: true,
        username: state.username.as_ref().map(|value| value.as_ref().clone()),
    }))
}

async fn logout(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    if let Some(session_id) = cookie_value(&headers, "remoteops_admin_session") {
        state.sessions.lock().await.remove(&session_id);
    }
    let mut response = Json(serde_json::json!({"authenticated": false})).into_response();
    response.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        session_cookie("", state.secure_cookie, true)
            .parse()
            .expect("管理 Session Cookie 应为合法 Header"),
    );
    Ok(response)
}

async fn overview(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<AdminOverview>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    Ok(Json(state.relay.admin_snapshot().await.overview))
}

async fn identity(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<AdminIdentity>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    Ok(Json(state.relay.admin_snapshot().await.identity))
}

async fn agents(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<Vec<AdminAgent>>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    Ok(Json(state.relay.admin_snapshot().await.agents))
}

async fn sessions(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<Vec<AdminSession>>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    Ok(Json(state.relay.admin_snapshot().await.sessions))
}

async fn session_detail(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<Json<AdminSession>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    let session_id = parse_session(&session_id)?;
    state
        .relay
        .admin_snapshot()
        .await
        .sessions
        .into_iter()
        .find(|session| session.session_id == session_id)
        .map(Json)
        .ok_or_else(|| not_found("Session 不存在"))
}

async fn close_session(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<Json<AdminActionOutcome>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    let session_id = parse_session(&session_id)?;
    Ok(Json(
        state
            .relay
            .admin_close_session(session_id, "admin_api")
            .await,
    ))
}

async fn clear_closed_sessions(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<AdminActionOutcome>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    Ok(Json(
        state.relay.admin_purge_closed_sessions("admin_api").await,
    ))
}

async fn emergency_stop(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<Json<AdminActionOutcome>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    let session_id = parse_session(&session_id)?;
    Ok(Json(
        state
            .relay
            .admin_emergency_stop(session_id, "admin_api")
            .await,
    ))
}

async fn audit(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Vec<AdminAuditEvent>>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    let mut events = state.relay.admin_snapshot().await.audit;
    events.reverse();
    events.truncate(query.limit.unwrap_or(100).min(1_000));
    Ok(Json(events))
}

async fn authorize(
    state: &AdminState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if let Some(session_id) = cookie_value(headers, "remoteops_admin_session") {
        let mut sessions = state.sessions.lock().await;
        if sessions
            .get(&session_id)
            .is_some_and(|expires_at| *expires_at > Instant::now())
        {
            return Ok(());
        }
        sessions.remove(&session_id);
    }
    let expected = format!("Bearer {}", state.token);
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if constant_time_eq(supplied.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        state
            .relay
            .admin_auth_failure(&request_source(headers))
            .await;
        Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "管理 Token 无效".to_owned(),
            }),
        ))
    }
}

fn request_source(headers: &HeaderMap) -> String {
    let Some(value) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    else {
        return "admin_http".to_owned();
    };
    if value.len() > 128 || value.chars().any(char::is_control) {
        return "admin_http".to_owned();
    }
    let addresses = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if addresses.is_empty() || addresses.iter().any(|part| part.parse::<IpAddr>().is_err()) {
        return "admin_http".to_owned();
    }
    // This is audit metadata only. Authentication and rate limiting use the TCP peer.
    addresses.join(",")
}

async fn login_allowed(state: &AdminState, peer: &str, username: &str) -> bool {
    let now = Instant::now();
    let mut attempts = state.login_attempts.lock().await;
    attempts.retain(|_, entry| now.duration_since(entry.last_seen) < LOGIN_ATTEMPT_TTL);
    attempts
        .get(&(peer.to_owned(), username.to_owned()))
        .is_none_or(|entry| entry.blocked_until <= now)
}

async fn record_login_failure(state: &AdminState, peer: &str, username: &str) {
    let now = Instant::now();
    let mut attempts = state.login_attempts.lock().await;
    attempts.retain(|_, entry| now.duration_since(entry.last_seen) < LOGIN_ATTEMPT_TTL);
    if attempts.len() >= MAX_LOGIN_ATTEMPTS
        && let Some(oldest) = attempts
            .iter()
            .min_by_key(|(_, entry)| entry.last_seen)
            .map(|(key, _)| key.clone())
    {
        attempts.remove(&oldest);
    }
    let key = (peer.to_owned(), username.to_owned());
    let entry = attempts.entry(key).or_insert(LoginAttempt {
        failures: 0,
        blocked_until: now,
        last_seen: now,
    });
    entry.failures = entry.failures.saturating_add(1);
    let delay = 2u64.saturating_pow(entry.failures.min(6));
    entry.blocked_until = now + StdDuration::from_secs(delay.min(60));
    entry.last_seen = now;
}

async fn clear_login_failures(state: &AdminState, peer: &str, username: &str) {
    state
        .login_attempts
        .lock()
        .await
        .remove(&(peer.to_owned(), username.to_owned()));
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (key, value) = cookie.trim().split_once('=')?;
                (key == name).then(|| value.to_owned())
            })
        })
}

fn session_cookie(session_id: &str, secure: bool, expired: bool) -> String {
    let mut cookie = format!(
        "remoteops_admin_session={session_id}; HttpOnly; SameSite=Strict; Path=/; {}",
        if expired {
            "Max-Age=0"
        } else {
            "Max-Age=28800"
        }
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

fn parse_session(value: &str) -> Result<SessionId, (StatusCode, Json<ErrorResponse>)> {
    value.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Session ID 格式无效".to_owned(),
            }),
        )
    })
}

fn not_found(message: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: message.to_owned(),
        }),
    )
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let maximum = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..maximum {
        difference |= usize::from(left.get(index).copied().unwrap_or_default())
            ^ usize::from(right.get(index).copied().unwrap_or_default());
    }
    difference == 0
}

async fn index() -> Response {
    (
        [(
            axum::http::header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; script-src 'self'; script-src-attr 'unsafe-inline'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        )],
        Html(include_str!("../../../relay-admin-prototype/index.html")),
    )
        .into_response()
}
async fn app_js() -> Response {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../../../relay-admin-prototype/app.js"),
    )
        .into_response()
}
async fn style_css() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../../../relay-admin-prototype/style.css"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use remoteops_domain::ControllerOwnerId;

    fn test_state() -> AdminState {
        AdminState {
            relay: Arc::new(Relay::new(
                Duration::minutes(10),
                5,
                ControllerOwnerId::new(),
                "human-token-for-tests-123456".to_owned(),
                "ai-token-for-tests-123456".to_owned(),
            )),
            token: Arc::new(String::new()),
            username: None,
            password: None,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            secure_cookie: false,
            login_attempts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[tokio::test]
    async fn login_limiter_backoffs_and_clears_on_success() {
        let state = test_state();
        assert!(login_allowed(&state, "127.0.0.1:1", "admin").await);
        record_login_failure(&state, "127.0.0.1:1", "admin").await;
        assert!(!login_allowed(&state, "127.0.0.1:1", "admin").await);
        clear_login_failures(&state, "127.0.0.1:1", "admin").await;
        assert!(login_allowed(&state, "127.0.0.1:1", "admin").await);
    }

    #[tokio::test]
    async fn login_limiter_evicts_oldest_entry_at_capacity() {
        let state = test_state();
        for index in 0..=MAX_LOGIN_ATTEMPTS {
            record_login_failure(&state, "127.0.0.1:1", &format!("user-{index}")).await;
        }
        let attempts = state.login_attempts.lock().await;
        assert_eq!(attempts.len(), MAX_LOGIN_ATTEMPTS);
    }

    #[test]
    fn forwarded_for_is_only_used_for_valid_bounded_ip_metadata() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "203.0.113.7, 2001:db8::1".parse().unwrap(),
        );
        assert_eq!(request_source(&headers), "203.0.113.7,2001:db8::1");
        headers.insert("x-forwarded-for", "<script>".parse().unwrap());
        assert_eq!(request_source(&headers), "admin_http");
    }
}
