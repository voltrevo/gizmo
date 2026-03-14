use crate::db::Db;
use crate::models::*;
use crate::models::resolve_channel;
use crate::ws;
use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{broadcast, RwLock, mpsc};
use tower_http::cors::CorsLayer;

/// Token bucket rate limiter for a single identity.
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

/// Per-identity publish rate limiter.
/// Capacity 5 tokens, refill rate 10/min (1 token per 6 seconds).
pub struct RateLimiter {
    buckets: Mutex<HashMap<String, TokenBucket>>,
    capacity: f64,
    refill_interval_secs: f64,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            capacity: 5.0,
            refill_interval_secs: 6.0, // 1 token per 6s = 10/min
        }
    }

    /// Try to consume one token. Returns Ok(()) or Err(seconds until next token).
    pub fn try_consume(&self, pubkey: &str) -> Result<(), f64> {
        let mut buckets = self.buckets.lock().unwrap();
        let now = Instant::now();

        let bucket = buckets.entry(pubkey.to_string()).or_insert(TokenBucket {
            tokens: self.capacity,
            last_refill: now,
        });

        // Refill tokens based on elapsed time.
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        let refilled = elapsed / self.refill_interval_secs;
        bucket.tokens = (bucket.tokens + refilled).min(self.capacity);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let deficit = 1.0 - bucket.tokens;
            let wait_secs = deficit * self.refill_interval_secs;
            Err(wait_secs)
        }
    }
}

pub struct AppState {
    pub db: Arc<Db>,
    pub token: String,
    pub broadcast_tx: broadcast::Sender<StoredMessage>,
    pub rate_limiter: RateLimiter,
    /// Registry of connected WebSocket clients: pubkey → list of MPSC senders.
    pub connected_clients: Arc<RwLock<HashMap<String, Vec<mpsc::UnboundedSender<ServerEnvelope>>>>>,
}

impl AppState {
    pub fn new(db_path: &str, token: String, max_bytes: u64) -> Self {
        let (broadcast_tx, _) = broadcast::channel(4096);
        Self {
            db: Arc::new(Db::new(db_path, max_bytes)),
            token,
            broadcast_tx,
            rate_limiter: RateLimiter::new(),
            connected_clients: Arc::new(RwLock::new(HashMap::new())),
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
    Query(ws_query): Query<WsQuery>,
) -> impl IntoResponse {
    // Try headers first, fall back to query params (for browser WebSocket).
    let bearer_ok = extract_bearer(&headers, &state.token).is_ok()
        || ws_query.token.as_deref() == Some(&state.token);
    if !bearer_ok {
        return (StatusCode::UNAUTHORIZED, "invalid or missing token").into_response();
    }

    let pubkey = extract_pubkey(&headers).ok().or(ws_query.pubkey);
    let pubkey = match pubkey {
        Some(pk) => pk,
        None => return (StatusCode::BAD_REQUEST, "missing pubkey").into_response(),
    };

    // Validate pubkey format.
    let pk_bytes = match hex::decode(&pubkey) {
        Ok(b) if b.len() == 32 => b,
        _ => return (StatusCode::BAD_REQUEST, "pubkey must be 64 hex chars").into_response(),
    };
    drop(pk_bytes);

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

    let channel = match resolve_channel(query.channel.as_deref()) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e}))).into_response(),
    };

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
            .query_messages(&channel, query.after, query.before, limit, &tag_filter, viewer.as_deref());

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

    let channel = match resolve_channel(query.channel.as_deref()) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e}))).into_response(),
    };

    let tag_filter = query.tags.as_ref().map(|t| {
        t.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    });

    let (last_modified, last_id) = state.db.last_modified(&channel, &tag_filter, viewer.as_deref());

    Json(LastModifiedResponse {
        last_modified,
        last_id,
    })
    .into_response()
}

