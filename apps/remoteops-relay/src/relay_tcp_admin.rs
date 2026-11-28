//! TCP 目标规则仅由已认证管理员在线维护。
use super::{AdminState, ErrorResponse, authorize};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use remoteops_protocol::tcp_forward::TcpForwardPolicy;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    expected_revision: u64,
    policy: TcpForwardPolicy,
}
pub(super) fn router() -> Router<AdminState> {
    Router::new()
        .route("/api/admin/tcp-forward-policy", get(snapshot).put(update))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(axum::middleware::from_fn(super::tools::no_store))
}
async fn snapshot(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<TcpForwardPolicy>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    Ok(Json(state.relay.tcp_policy().await))
}
async fn update(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<Update>,
) -> Result<Json<TcpForwardPolicy>, (StatusCode, Json<ErrorResponse>)> {
    authorize(&state, &headers).await?;
    request
        .policy
        .validate()
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?;
    let actor = state.username.as_deref().map_or("admin:token", |name| name);
    state
        .relay
        .update_tcp_policy(actor, request.expected_revision, request.policy)
        .await
        .map(Json)
        .map_err(|error| {
            let error = error.to_string();
            let code = if error.starts_with("tcp_policy_conflict") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (code, Json(ErrorResponse { error }))
        })
}
