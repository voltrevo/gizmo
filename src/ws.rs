use crate::db::is_visible_to;
use crate::models::{resolve_channel, ClientEnvelope, IncomingMessage, ServerEnvelope};
use crate::server::AppState;
use axum::extract::ws::{Message, WebSocket};
use ed25519_dalek::{Signature, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

fn verify_sig(pubkey_hex: &str, canonical: &str, sig_hex: &str) -> Result<(), String> {
    let sig_bytes = hex::decode(sig_hex).map_err(|_| "signature must be hex-encoded".to_string())?;
    let signature = Signature::from_slice(&sig_bytes).map_err(|_| "invalid signature length".to_string())?;
    let pubkey_bytes = hex::decode(pubkey_hex).map_err(|_| "invalid pubkey hex".to_string())?;
    let verifying_key = VerifyingKey::from_bytes(
        pubkey_bytes.as_slice().try_into().map_err(|_| "pubkey must be 32 bytes".to_string())?,
    ).map_err(|_| "invalid ed25519 public key".to_string())?;
    use ed25519_dalek::Verifier;
    verifying_key.verify(canonical.as_bytes(), &signature)
        .map_err(|_| "signature verification failed".to_string())
}

const MAX_MSG_SIZE: usize = 16_384;

pub async fn handle_ws(socket: WebSocket, state: Arc<AppState>, client_pubkey: String) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Channel for sending messages back to the client.
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEnvelope>();

    // Register this connection in the whisper registry.
    {
        let mut clients = state.connected_clients.write().await;
        clients.entry(client_pubkey.clone()).or_default().push(tx.clone());
    }

    // Sender task: forwards envelopes to the websocket.
    let send_task = tokio::spawn(async move {
        while let Some(env) = rx.recv().await {
            let json = serde_json::to_string(&env).unwrap();
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    // Track subscriptions: sub_id → (channel, optional tag filter).
    type SubValue = (String, Option<Vec<String>>);
    let mut subs: HashMap<String, SubValue> = HashMap::new();

    // Subscribe to the broadcast channel.
    let mut broadcast_rx = state.broadcast_tx.subscribe();

    // Spawn a task that listens to broadcast and forwards matching messages.
    let tx_clone = tx.clone();
    let subs_shared: Arc<tokio::sync::RwLock<HashMap<String, SubValue>>> =
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
            for (sub_id, (channel, tag_filter)) in subs.iter() {
                if msg.channel != *channel {
                    continue;
                }
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
            ClientEnvelope::Subscribe { sub_id, channel, tags } => {
                let ch = match resolve_channel(channel.as_deref()) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(ServerEnvelope::Error { detail: e });
                        continue;
                    }
                };
                let val = (ch, tags);
                subs.insert(sub_id.clone(), val.clone());
                subs_shared.write().await.insert(sub_id.clone(), val);
                let _ = tx.send(ServerEnvelope::Subscribed { sub_id });
            }
            ClientEnvelope::Unsubscribe { sub_id } => {
                subs.remove(&sub_id);
                subs_shared.write().await.remove(&sub_id);
                let _ = tx.send(ServerEnvelope::Unsubscribed { sub_id });
            }
            ClientEnvelope::Whisper { to, body, signature } => {
                handle_whisper(&state, &tx, &client_pubkey, to, body, signature).await;
            }
        }
    }

    // Deregister this connection from the whisper registry.
    {
        let mut clients = state.connected_clients.write().await;
        if let Some(senders) = clients.get_mut(&client_pubkey) {
            senders.retain(|s| !s.is_closed());
            if senders.is_empty() {
                clients.remove(&client_pubkey);
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
    // Rate limit.
    if let Err(wait_secs) = state.rate_limiter.try_consume(client_pubkey) {
        return Err(format!(
            "rate limited: try again in {:.1}s",
            wait_secs
        ));
    }

    // Reject if client tried to set ed25519 field.
    if msg.ed25519.is_some() {
        return Err("ed25519 field must not be provided; it is set by the server".into());
    }

    // Validate tags.
    if msg.tags.is_empty() {
        return Err("at least one tag is required".into());
    }

    // Resolve and validate channel.
    let channel = resolve_channel(msg.channel.as_deref())?;

    // Verify signature.
    let canonical = canonical_payload(&msg);
    verify_sig(client_pubkey, &canonical, &msg.signature)?;

    // Store.
    let stored = state.db.insert_message(
        client_pubkey,
        &channel,
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

async fn handle_whisper(
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerEnvelope>,
    from_pubkey: &str,
    to: String,
    body: serde_json::Value,
    signature: String,
) {
    // Rate limit.
    if let Err(wait_secs) = state.rate_limiter.try_consume(from_pubkey) {
        let _ = tx.send(ServerEnvelope::Error {
            detail: format!("rate limited: try again in {:.1}s", wait_secs),
        });
        return;
    }

    // Validate recipient pubkey.
    if hex::decode(&to).map_or(true, |b| b.len() != 32) {
        let _ = tx.send(ServerEnvelope::Error {
            detail: "to must be a valid 32-byte ed25519 pubkey (64 hex chars)".into(),
        });
        return;
    }

    // Verify signature over canonical payload: JSON({to, body}).
    let canonical = serde_json::to_string(&serde_json::json!({ "to": to, "body": body })).unwrap();
    if let Err(e) = verify_sig(from_pubkey, &canonical, &signature) {
        let _ = tx.send(ServerEnvelope::Error { detail: e });
        return;
    }

    // Deliver to all connections for the recipient, pruning closed senders.
    let env = ServerEnvelope::Whisper { from: from_pubkey.to_string(), body, signature };
    let mut clients = state.connected_clients.write().await;
    if let Some(senders) = clients.get_mut(&to) {
        let mut delivered = false;
        senders.retain(|s| {
            let ok = s.send(env.clone()).is_ok();
            if ok { delivered = true; }
            ok
        });
        if senders.is_empty() { clients.remove(&to); }
        drop(clients);
        if delivered {
            let _ = tx.send(ServerEnvelope::Published { id: 0 });
        } else {
            let _ = tx.send(ServerEnvelope::Error { detail: "recipient not connected".into() });
        }
    } else {
        drop(clients);
        let _ = tx.send(ServerEnvelope::Error { detail: "recipient not connected".into() });
    }
}

/// Build the canonical string that clients must sign.
/// Serializes the incoming message as JSON, minus the `signature` and `ed25519` fields.
/// Key order is preserved by serde_json's `preserve_order` feature.
pub fn canonical_payload(msg: &IncomingMessage) -> String {
    let mut obj = serde_json::to_value(msg).unwrap();
    if let Some(map) = obj.as_object_mut() {
        map.remove("signature");
        map.remove("ed25519");
    }
    serde_json::to_string(&obj).unwrap()
}
