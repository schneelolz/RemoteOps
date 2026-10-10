//! Bounded HTTP host for one-time MCP enrollment. The SQLite crate owns durable rules.
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
};
use chrono::Utc;
use remoteops_enrollment::{
    AdvertisedEndpoints, ClientSummary, CreateGrantRequest, CreateGrantResponse, GrantSummary,
    MAX_SETUP_BYTES, RedeemRequest, RedeemResponse,
};
use remoteops_enrollment_store::StoreError;
use uuid::Uuid;

use crate::admin::{AdminState, ErrorResponse, authorize};

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorResponse>)>;
const BODY_LIMIT: usize = MAX_SETUP_BYTES;
const WINDOW: Duration = Duration::from_secs(60);
const MAX_PEERS: usize = 1024;
const PER_PEER_LIMIT: u32 = 30;
const GLOBAL_LIMIT: u32 = 240;

#[derive(Default)]
pub(crate) struct EnrollmentLimits {
    peers: HashMap<(IpAddr, bool), RateWindow>,
    public: Option<RateWindow>,
    admin: Option<RateWindow>,
}

struct RateWindow {
    started: Instant,
    count: u32,
}

impl EnrollmentLimits {
    fn allow(&mut self, peer: IpAddr, admin: bool, now: Instant) -> bool {
        self.peers
            .retain(|_, window| now.duration_since(window.started) < WINDOW);
        let global = if admin {
            &mut self.admin
        } else {
            &mut self.public
        };
        let global = global.get_or_insert(RateWindow {
            started: now,
            count: 0,
        });
        if now.duration_since(global.started) >= WINDOW {
            *global = RateWindow {
                started: now,
                count: 0,
            };
        }
        if global.count >= GLOBAL_LIMIT {
            return false;
        }
        let key = (peer, admin);
        if !self.peers.contains_key(&key) && self.peers.len() >= MAX_PEERS {
            return false;
        }
        let window = self.peers.entry(key).or_insert(RateWindow {
            started: now,
            count: 0,
        });
        if window.count >= PER_PEER_LIMIT {
            return false;
        }
        global.count += 1;
        window.count += 1;
        true
    }
}

pub(crate) fn router() -> Router<AdminState> {
    Router::new()
        .route(
            "/api/admin/mcp/settings",
            get(settings).put(update_settings),
        )
        .route("/api/admin/mcp/setups", get(setups).post(create_setup))
        .route(
            "/api/admin/mcp/setups/{grant_id}/revoke",
            post(revoke_setup),
        )
        .route("/api/admin/mcp/clients", get(clients))
        .route(
            "/api/admin/mcp/clients/{installation_id}/revoke",
            post(revoke_client),
        )
        .route("/api/mcp/enroll", post(enroll))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
}

async fn no_store(request: Request, next: Next) -> Response {
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

async fn settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> ApiResult<Option<AdvertisedEndpoints>> {
    authorize(&state, &headers).await?;
    Ok(Json(
        state
            .relay
            .enrollment_store()
            .map_err(store_error)?
            .settings()
            .map_err(store_error)?,
    ))
}

async fn update_settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
    request: Result<Json<AdvertisedEndpoints>, JsonRejection>,
) -> ApiResult<AdvertisedEndpoints> {
    authorize(&state, &headers).await?;
    let Json(request) = request.map_err(invalid_json)?;
    let store = state.relay.enrollment_store().map_err(store_error)?;
    store.set_settings(request.clone()).map_err(store_error)?;
    Ok(Json(request))
}

async fn setups(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> ApiResult<Vec<GrantSummary>> {
    authorize(&state, &headers).await?;
    Ok(Json(
        state
            .relay
            .enrollment_store()
            .map_err(store_error)?
            .list_grants(Utc::now())
            .map_err(store_error)?,
    ))
}

async fn create_setup(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Result<Json<CreateGrantRequest>, JsonRejection>,
) -> ApiResult<CreateGrantResponse> {
    authorize(&state, &headers).await?;
    rate_limit(&state, peer.ip(), true).await?;
    let Json(request) = request.map_err(invalid_json)?;
    Ok(Json(
        state
            .relay
            .enrollment_store()
            .map_err(store_error)?
            .create_grant(request, Utc::now())
            .map_err(store_error)?,
    ))
}

async fn revoke_setup(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<serde_json::Value> {
    authorize(&state, &headers).await?;
    let id = parse_id(&id)?;
    let store = state.relay.enrollment_store().map_err(store_error)?;
    if !store
        .list_grants(Utc::now())
        .map_err(store_error)?
        .iter()
        .any(|grant| grant.grant_id == id)
    {
        return Err(error(StatusCode::NOT_FOUND, "Setup grant not found"));
    }
    let changed = store.revoke_grant(id).map_err(store_error)?;
    Ok(Json(
        serde_json::json!({"success": true, "changed": changed}),
    ))
}

async fn clients(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> ApiResult<Vec<ClientSummary>> {
    authorize(&state, &headers).await?;
    Ok(Json(state.relay.mcp_clients().await.map_err(store_error)?))
}

async fn revoke_client(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<serde_json::Value> {
    authorize(&state, &headers).await?;
    let id = parse_id(&id)?;
    let store = state.relay.enrollment_store().map_err(store_error)?;
    if !store
        .list_clients()
        .map_err(store_error)?
        .iter()
        .any(|client| client.installation_id == id)
    {
        return Err(error(StatusCode::NOT_FOUND, "MCP client not found"));
    }
    let changed = state
        .relay
        .revoke_mcp_client(id)
        .await
        .map_err(store_error)?;
    Ok(Json(
        serde_json::json!({"success": true, "changed": changed}),
    ))
}

async fn enroll(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Result<Json<RedeemRequest>, JsonRejection>,
) -> ApiResult<RedeemResponse> {
    // Use the actual peer, never attacker-controlled forwarded headers. Deployments behind a
    // proxy share its budget; configure additional public-facing limits at that trusted proxy.
    rate_limit(&state, peer.ip(), false).await?;
    let Json(request) = request.map_err(invalid_json)?;
    Ok(Json(
        state
            .relay
            .enrollment_store()
            .map_err(store_error)?
            .redeem(request, Utc::now())
            .map_err(store_error)?,
    ))
}

async fn rate_limit(
    state: &AdminState,
    peer: IpAddr,
    admin: bool,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if state
        .enrollment_limits
        .lock()
        .await
        .allow(peer, admin, Instant::now())
    {
        Ok(())
    } else {
        Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many enrollment requests; retry in one minute",
        ))
    }
}

fn parse_id(value: &str) -> Result<Uuid, (StatusCode, Json<ErrorResponse>)> {
    value
        .parse()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "Invalid identifier"))
}

// These owned callbacks are passed directly to Result::map_err.
#[allow(clippy::needless_pass_by_value)]
fn invalid_json(rejection: JsonRejection) -> (StatusCode, Json<ErrorResponse>) {
    let status = if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::BAD_REQUEST
    };
    error(status, "Invalid or oversized enrollment request")
}

#[allow(clippy::needless_pass_by_value)]
fn store_error(value: StoreError) -> (StatusCode, Json<ErrorResponse>) {
    match value {
        StoreError::InvalidInput(_) => error(
            StatusCode::BAD_REQUEST,
            "Invalid enrollment settings or request",
        ),
        StoreError::InvalidCredential | StoreError::Revoked => error(
            StatusCode::UNAUTHORIZED,
            "Enrollment credential is invalid or revoked",
        ),
        StoreError::Expired => error(
            StatusCode::GONE,
            "Setup grant has expired; ask the administrator for a new setup",
        ),
        StoreError::AlreadyRedeemed => error(
            StatusCode::CONFLICT,
            "Setup grant has already been used by another installation",
        ),
        StoreError::NotConfigured => error(
            StatusCode::CONFLICT,
            "Configure advertised MCP endpoints first",
        ),
        StoreError::Storage(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Enrollment storage is unavailable",
        ),
    }
}

fn error(status: StatusCode, message: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: message.to_owned(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_bounds_per_peer_and_resets_after_window() {
        let mut limits = EnrollmentLimits::default();
        let now = Instant::now();
        let ip = "127.0.0.1".parse().unwrap();
        for _ in 0..PER_PEER_LIMIT {
            assert!(limits.allow(ip, false, now));
        }
        assert!(!limits.allow(ip, false, now));
        assert!(
            limits.allow(ip, true, now),
            "admin and public budgets are independent"
        );
        assert!(limits.allow(ip, false, now + WINDOW));
    }

    #[test]
    fn limiter_caps_global_requests_across_peer_addresses() {
        let mut limits = EnrollmentLimits::default();
        let now = Instant::now();
        for index in 0..GLOBAL_LIMIT {
            let ip = IpAddr::V4(std::net::Ipv4Addr::from(index));
            assert!(limits.allow(ip, false, now));
        }
        assert!(!limits.allow("10.1.2.3".parse().unwrap(), false, now));
    }
}
