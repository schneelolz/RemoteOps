use std::sync::Arc;

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use remoteops_domain::{
    AgentInstanceId, ControllerInstanceId, ControllerOwnerId, PermissionMode, SessionId,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::relay::{AdminAuditEvent, Relay};

#[derive(Clone)]
pub(crate) struct AdminState {
    pub relay: Arc<Relay>,
    pub token: Arc<String>,
}

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
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AdminControllerBinding {
    pub controller_instance_id: ControllerInstanceId,
    pub kind: String,
    pub owner_id: ControllerOwnerId,
    pub permission_mode: PermissionMode,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AdminSession {
    pub session_id: SessionId,
    pub agent_instance_id: AgentInstanceId,
    pub hostname: String,
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
    token: String,
) -> anyhow::Result<()> {
    let state = AdminState {
        relay,
        token: Arc::new(token),
    };
    let app = Router::new()
        .route("/api/admin/overview", get(overview))
        .route("/api/admin/identity", get(identity))
        .route("/api/admin/agents", get(agents))
        .route("/api/admin/sessions", get(sessions))
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
        .with_state(state)
        .fallback(index);
    axum::serve(listener, app)
        .await
        .context("管理 HTTP 服务已停止")
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
    let expected = format!("Bearer {}", state.token);
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if constant_time_eq(supplied.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        let source = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("admin_http");
        state.relay.admin_auth_failure(source).await;
        Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "管理 Token 无效".to_owned(),
            }),
        ))
    }
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

async fn index() -> Html<&'static str> {
    Html(include_str!("../../../relay-admin-prototype/index.html"))
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
