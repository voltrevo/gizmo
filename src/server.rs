use crate::db::Db;
use crate::models::*;
use crate::ws;
use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

pub struct AppState {
    pub db: Arc<Db>,
    pub token: String,
    pub broadcast_tx: broadcast::Sender<StoredMessage>,
}

impl AppState {
    pub fn new(db_path: &str, token: String, max_bytes: u64) -> Self {
        let (broadcast_tx, _) = broadcast::channel(4096);
        Self {
            db: Arc::new(Db::new(db_path, max_bytes)),
            token,
            broadcast_tx,
        }
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/ws", get(ws_handler))
        .route("/history", get(history_handler))
        .route("/last-modified", get(last_modified_handler))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Extract and validate bearer token from headers.
fn extract_bearer(headers: &HeaderMap, expected: &str) -> Result<(), (StatusCode, String)> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::UNAUTHORIZED, "missing authorization header".into()))?;

    let token = auth
        .strip_prefix("Bearer ")
        .ok_or((StatusCode::UNAUTHORIZED, "expected Bearer token".into()))?;

    if token != expected {
        return Err((StatusCode::UNAUTHORIZED, "invalid token".into()));
    }

    Ok(())
}

/// Extract ed25519 public key from headers (hex-encoded).
fn extract_pubkey(headers: &HeaderMap) -> Result<String, (StatusCode, String)> {
    let pubkey = headers
        .get("x-ed25519-pubkey")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::BAD_REQUEST, "missing x-ed25519-pubkey header".into()))?;

    // Validate it's valid hex and 32 bytes.
    let bytes = hex::decode(pubkey)
        .map_err(|_| (StatusCode::BAD_REQUEST, "pubkey must be hex-encoded".into()))?;
    if bytes.len() != 32 {
        return Err((StatusCode::BAD_REQUEST, "pubkey must be 32 bytes (64 hex chars)".into()));
    }

    Ok(pubkey.to_string())
}

async fn health_handler() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err((status, msg)) = extract_bearer(&headers, &state.token) {
        return (status, msg).into_response();
    }

    let pubkey = match extract_pubkey(&headers) {
        Ok(pk) => pk,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    ws.max_message_size(16_384)
        .on_upgrade(move |socket| ws::handle_ws(socket, state, pubkey))
}

async fn history_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> impl IntoResponse {
    if let Err((status, msg)) = extract_bearer(&headers, &state.token) {
        return (status, Json(serde_json::json!({"error": msg}))).into_response();
    }

    let viewer = extract_pubkey(&headers).ok();

    let limit = query.limit.unwrap_or(50).min(200).max(1);
    let tag_filter = query.tags.as_ref().map(|t| {
        t.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    });

    let (messages, has_more) =
        state
            .db
            .query_messages(query.after, query.before, limit, &tag_filter, viewer.as_deref());

    Json(HistoryResponse { messages, has_more }).into_response()
}

async fn last_modified_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> impl IntoResponse {
    if let Err((status, msg)) = extract_bearer(&headers, &state.token) {
        return (status, Json(serde_json::json!({"error": msg}))).into_response();
    }

    let viewer = extract_pubkey(&headers).ok();

    let tag_filter = query.tags.as_ref().map(|t| {
        t.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    });

    let (last_modified, last_id) = state.db.last_modified(&tag_filter, viewer.as_deref());

    Json(LastModifiedResponse {
        last_modified,
        last_id,
    })
    .into_response()
}
