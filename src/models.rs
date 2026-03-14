use serde::{Deserialize, Serialize};

pub const DEFAULT_CHANNEL: &str = "default";

/// Resolve an optional channel to a validated channel name.
pub fn resolve_channel(channel: Option<&str>) -> Result<String, String> {
    let ch = channel.unwrap_or(DEFAULT_CHANNEL);
    validate_channel(ch)?;
    Ok(ch.to_string())
}

fn validate_channel(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("channel name must be 1-64 characters".into());
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err("channel name must contain only [a-zA-Z0-9_-]".into());
    }
    Ok(())
}

/// Wire format for an incoming message from a client.
#[derive(Debug, Serialize, Deserialize)]
pub struct IncomingMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    pub tags: Vec<String>,
    pub body: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disallow: Option<Vec<String>>,
    /// Signature of the canonical payload (hex-encoded).
    pub signature: String,
    /// Must NOT be present — server fills this in. We deserialize it to detect clashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ed25519: Option<String>,
}

fn default_channel() -> String {
    "default".into()
}

/// Stored / outgoing message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: i64,
    pub ed25519: String,
    #[serde(default = "default_channel")]
    pub channel: String,
    pub tags: Vec<String>,
    pub body: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disallow: Option<Vec<String>>,
    pub signature: String,
    pub created_at: String,
}

/// Client→server envelope over WebSocket.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientEnvelope {
    #[serde(rename = "publish")]
    Publish(IncomingMessage),
    #[serde(rename = "subscribe")]
    Subscribe {
        /// Client-chosen subscription id.
        sub_id: String,
        #[serde(default)]
        channel: Option<String>,
        #[serde(default)]
        tags: Option<Vec<String>>,
    },
    #[serde(rename = "unsubscribe")]
    Unsubscribe { sub_id: String },
}

/// Server→client envelope over WebSocket.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum ServerEnvelope {
    #[serde(rename = "message")]
    Message {
        sub_id: String,
        message: StoredMessage,
    },
    #[serde(rename = "published")]
    Published { id: i64 },
    #[serde(rename = "subscribed")]
    Subscribed { sub_id: String },
    #[serde(rename = "unsubscribed")]
    Unsubscribed { sub_id: String },
    #[serde(rename = "error")]
    Error { detail: String },
}

/// Query params for WebSocket auth (browser fallback).
#[derive(Debug, Deserialize, Default)]
pub struct WsQuery {
    pub token: Option<String>,
    pub pubkey: Option<String>,
}

/// Query params for the HTTP history endpoint.
#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    /// Channel (defaults to "default").
    pub channel: Option<String>,
    /// Cursor: return messages with id > after.
    pub after: Option<i64>,
    /// Cursor: return messages with id < before.
    pub before: Option<i64>,
    /// Page size (default 50, max 200).
    pub limit: Option<i64>,
    /// Comma-separated tag filter.
    pub tags: Option<String>,
}

/// Response for the history endpoint.
#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub messages: Vec<StoredMessage>,
    pub has_more: bool,
}

/// Response for the last-modified endpoint.
#[derive(Debug, Serialize)]
pub struct LastModifiedResponse {
    pub last_modified: Option<String>,
    pub last_id: Option<i64>,
}
