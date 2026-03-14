use crate::db::is_visible_to;
use crate::models::*;
use crate::server::AppState;
use axum::extract::ws::{Message, WebSocket};
use ed25519_dalek::{Signature, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

const MAX_MSG_SIZE: usize = 16_384;

pub async fn handle_ws(socket: WebSocket, state: Arc<AppState>, client_pubkey: String) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Channel for sending messages back to the client.
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEnvelope>();

    // Sender task: forwards envelopes to the websocket.
    let send_task = tokio::spawn(async move {
        while let Some(env) = rx.recv().await {
            let json = serde_json::to_string(&env).unwrap();
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    // Track subscriptions: sub_id → optional tag filter.
    let mut subs: HashMap<String, Option<Vec<String>>> = HashMap::new();

    // Subscribe to the broadcast channel.
    let mut broadcast_rx = state.broadcast_tx.subscribe();

    // Spawn a task that listens to broadcast and forwards matching messages.
    let tx_clone = tx.clone();
    let subs_shared: Arc<tokio::sync::RwLock<HashMap<String, Option<Vec<String>>>>> =
        Arc::new(tokio::sync::RwLock::new(HashMap::new()));
    let subs_reader = subs_shared.clone();
    let pubkey_for_broadcast = client_pubkey.clone();

    let broadcast_task = tokio::spawn(async move {
        while let Ok(msg) = broadcast_rx.recv().await {
            // Check visibility.
            if !is_visible_to(&msg, &pubkey_for_broadcast) {
                continue;
            }
            let subs = subs_reader.read().await;
            for (sub_id, tag_filter) in subs.iter() {
                let matches = match tag_filter {
                    Some(tags) => tags.iter().any(|t| msg.tags.contains(t)),
                    None => true,
                };
                if matches {
                    let env = ServerEnvelope::Message {
                        sub_id: sub_id.clone(),
                        message: msg.clone(),
                    };
                    if tx_clone.send(env).is_err() {
                        return;
                    }
                }
            }
        }
    });

    // Main receive loop.
    while let Some(Ok(msg)) = ws_rx.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };

        if text.len() > MAX_MSG_SIZE {
            let _ = tx.send(ServerEnvelope::Error {
                detail: format!("message exceeds {MAX_MSG_SIZE} byte limit"),
            });
            continue;
        }

        let envelope: ClientEnvelope = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(e) => {
                let _ = tx.send(ServerEnvelope::Error {
                    detail: format!("invalid json: {e}"),
                });
                continue;
            }
        };

        match envelope {
            ClientEnvelope::Publish(incoming) => {
                if let Err(e) = handle_publish(&state, &tx, &client_pubkey, incoming) {
                    let _ = tx.send(ServerEnvelope::Error { detail: e });
                }
            }
            ClientEnvelope::Subscribe { sub_id, tags } => {
                subs.insert(sub_id.clone(), tags.clone());
                subs_shared.write().await.insert(sub_id.clone(), tags);
                let _ = tx.send(ServerEnvelope::Subscribed { sub_id });
            }
            ClientEnvelope::Unsubscribe { sub_id } => {
                subs.remove(&sub_id);
                subs_shared.write().await.remove(&sub_id);
                let _ = tx.send(ServerEnvelope::Unsubscribed { sub_id });
            }
        }
    }

    broadcast_task.abort();
    send_task.abort();
}

fn handle_publish(
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerEnvelope>,
    client_pubkey: &str,
    msg: IncomingMessage,
) -> Result<(), String> {
    // Reject if client tried to set ed25519 field.
    if msg.ed25519.is_some() {
        return Err("ed25519 field must not be provided; it is set by the server".into());
    }

    // Validate tags.
    if msg.tags.is_empty() {
        return Err("at least one tag is required".into());
    }

    // Verify signature.
    // The canonical payload to sign is: tags + body + allow + disallow (JSON, sorted keys).
    let canonical = canonical_payload(&msg.tags, &msg.body, &msg.allow, &msg.disallow);
    let sig_bytes =
        hex::decode(&msg.signature).map_err(|_| "signature must be hex-encoded".to_string())?;
    let signature =
        Signature::from_slice(&sig_bytes).map_err(|_| "invalid signature length".to_string())?;
    let pubkey_bytes =
        hex::decode(client_pubkey).map_err(|_| "invalid pubkey hex".to_string())?;
    let verifying_key = VerifyingKey::from_bytes(
        pubkey_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "pubkey must be 32 bytes".to_string())?,
    )
    .map_err(|_| "invalid ed25519 public key".to_string())?;

    use ed25519_dalek::Verifier;
    verifying_key
        .verify(canonical.as_bytes(), &signature)
        .map_err(|_| "signature verification failed".to_string())?;

    // Store.
    let stored = state.db.insert_message(
        client_pubkey,
        &msg.tags,
        &msg.body,
        &msg.allow,
        &msg.disallow,
        &msg.signature,
    );

    let id = stored.id;

    // Broadcast.
    let _ = state.broadcast_tx.send(stored);

    // Enforce size limit in background.
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || db.enforce_size_limit());

    let _ = tx.send(ServerEnvelope::Published { id });
    Ok(())
}

/// Build the canonical string that clients must sign.
pub fn canonical_payload(
    tags: &[String],
    body: &serde_json::Value,
    allow: &Option<Vec<String>>,
    disallow: &Option<Vec<String>>,
) -> String {
    let obj = serde_json::json!({
        "tags": tags,
        "body": body,
        "allow": allow,
        "disallow": disallow,
    });
    // Use serde_json default which sorts nothing — but the client must produce
    // the exact same JSON. We document the canonical form.
    serde_json::to_string(&obj).unwrap()
}
