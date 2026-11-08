//! 已认证管理后台与 MCP 复用同一工具管理服务。
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use remoteops_tool_center::{ToolCenterCommand, ToolCenterRequest, ToolCenterResponse};
use uuid::Uuid;

use super::{AdminState, ErrorResponse, authorize};

type ApiResult = Result<Json<ToolCenterResponse>, (StatusCode, Json<ErrorResponse>)>;

pub(super) fn router() -> Router<AdminState> {
    Router::new()
        .route("/api/admin/tools", get(snapshot).post(manage))
        // 256 KiB 二进制块的 Base64 编码及管理元数据均有界。
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(middleware::from_fn(no_store))
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
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

fn admin_actor(state: &AdminState) -> String {
    state.username.as_deref().map_or_else(
        || "admin:token".into(),
        |username| format!("admin:{username}"),
    )
}

async fn snapshot(State(state): State<AdminState>, headers: HeaderMap) -> ApiResult {
    authorize(&state, &headers).await?;
    Ok(Json(
        state
            .relay
            .admin_tools_request(
                &admin_actor(&state),
                ToolCenterRequest {
                    request_id: Uuid::new_v4().to_string(),
                    expected_revision: 0,
                    command: ToolCenterCommand::Snapshot,
                },
            )
            .await,
    ))
}

async fn manage(
    State(state): State<AdminState>,
    headers: HeaderMap,
    request: Result<Json<ToolCenterRequest>, JsonRejection>,
) -> ApiResult {
    authorize(&state, &headers).await?;
    let Json(request) = request.map_err(|rejection| {
        (
            if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            },
            Json(ErrorResponse {
                error: "工具管理请求无效或超过大小限制".into(),
            }),
        )
    })?;
    Ok(Json(
        state
            .relay
            .admin_tools_request(&admin_actor(&state), request)
            .await,
    ))
}
