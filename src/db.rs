use crate::models::StoredMessage;
use rusqlite::{params, Connection};
use std::sync::Mutex;

pub struct Db {
    conn: Mutex<Connection>,
    max_bytes: u64,
}

impl Db {
    pub fn new(path: &str, max_bytes: u64) -> Self {
        let conn = Connection::open(path).expect("failed to open sqlite db");
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;

            CREATE TABLE IF NOT EXISTS messages (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                ed25519     TEXT NOT NULL,
                tags        TEXT NOT NULL,  -- JSON array
                body        TEXT NOT NULL,  -- JSON
                allow_list  TEXT,           -- JSON array or NULL
                disallow    TEXT,           -- JSON array or NULL
                signature   TEXT NOT NULL,
                created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f','now')),
                size_bytes  INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_messages_tags ON messages(tags);
            CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at);
            ",
        )
        .expect("failed to init schema");

        Self {
            conn: Mutex::new(conn),
            max_bytes,
        }
    }

    pub fn insert_message(
        &self,
        ed25519: &str,
        tags: &[String],
        body: &serde_json::Value,
        allow: &Option<Vec<String>>,
        disallow: &Option<Vec<String>>,
        signature: &str,
    ) -> StoredMessage {
        let conn = self.conn.lock().unwrap();
        let tags_json = serde_json::to_string(tags).unwrap();
        let body_json = serde_json::to_string(body).unwrap();
        let allow_json = allow.as_ref().map(|a| serde_json::to_string(a).unwrap());
        let disallow_json = disallow.as_ref().map(|d| serde_json::to_string(d).unwrap());

        let size = tags_json.len() + body_json.len() + allow_json.as_ref().map_or(0, |s| s.len())
            + disallow_json.as_ref().map_or(0, |s| s.len())
            + signature.len()
            + ed25519.len();

        conn.execute(
            "INSERT INTO messages (ed25519, tags, body, allow_list, disallow, signature, size_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                ed25519,
                tags_json,
                body_json,
                allow_json,
                disallow_json,
                signature,
                size as i64
            ],
        )
        .expect("insert failed");

        let id = conn.last_insert_rowid();
        let created_at: String = conn
            .query_row("SELECT created_at FROM messages WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();

        StoredMessage {
            id,
            ed25519: ed25519.to_string(),
            tags: tags.to_vec(),
            body: body.clone(),
            allow: allow.clone(),
            disallow: disallow.clone(),
            signature: signature.to_string(),
            created_at,
        }
    }

    /// Enforce the max history size by deleting oldest messages.
    pub fn enforce_size_limit(&self) {
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn
            .query_row("SELECT COALESCE(SUM(size_bytes), 0) FROM messages", [], |r| {
                r.get(0)
            })
            .unwrap();

        if total as u64 > self.max_bytes {
            let excess = total as u64 - self.max_bytes;
            // Delete oldest messages until we're under the limit.
            conn.execute(
                "DELETE FROM messages WHERE id IN (
                    SELECT id FROM messages ORDER BY id ASC LIMIT (
                        SELECT COUNT(*) FROM (
                            SELECT id, SUM(size_bytes) OVER (ORDER BY id ASC) AS running
                            FROM messages
                        ) WHERE running <= ?1
                    ) + 1
                )",
                [excess as i64],
            )
            .unwrap();
        }
    }

    /// Query messages for history pagination.
    /// Forward pagination: after=Some(id), before=None → id > after ORDER BY id ASC
    /// Backward pagination: before=Some(id), after=None → id < before ORDER BY id DESC
    pub fn query_messages(
        &self,
        after: Option<i64>,
        before: Option<i64>,
        limit: i64,
        tag_filter: &Option<Vec<String>>,
        viewer_pubkey: Option<&str>,
    ) -> (Vec<StoredMessage>, bool) {
        let conn = self.conn.lock().unwrap();

        let mut conditions = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(a) = after {
            conditions.push("id > ?".to_string());
            param_values.push(Box::new(a));
        }
        if let Some(b) = before {
            conditions.push("id < ?".to_string());
            param_values.push(Box::new(b));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let order = if before.is_some() && after.is_none() {
            "DESC"
        } else {
            "ASC"
        };

        let sql = format!(
            "SELECT id, ed25519, tags, body, allow_list, disallow, signature, created_at
             FROM messages {where_clause} ORDER BY id {order} LIMIT ?",
        );
        param_values.push(Box::new(limit + 1)); // fetch one extra to detect has_more

        let params_ref: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql).unwrap();
        let rows = stmt
            .query_map(params_ref.as_slice(), |row| {
                let tags_str: String = row.get(2)?;
                let body_str: String = row.get(3)?;
                let allow_str: Option<String> = row.get(4)?;
                let disallow_str: Option<String> = row.get(5)?;
                Ok(StoredMessage {
                    id: row.get(0)?,
                    ed25519: row.get(1)?,
                    tags: serde_json::from_str(&tags_str).unwrap_or_default(),
                    body: serde_json::from_str(&body_str).unwrap_or_default(),
                    allow: allow_str.map(|s| serde_json::from_str(&s).unwrap_or_default()),
                    disallow: disallow_str.map(|s| serde_json::from_str(&s).unwrap_or_default()),
                    signature: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })
            .unwrap();

        let mut messages: Vec<StoredMessage> = rows.filter_map(|r| r.ok()).collect();
        let has_more = messages.len() > limit as usize;
        messages.truncate(limit as usize);

        // Reverse if we fetched in DESC order so output is always ascending.
        if order == "DESC" {
            messages.reverse();
        }

        // Apply tag filter in Rust (SQLite JSON ops are clunky).
        if let Some(filter_tags) = tag_filter {
            messages.retain(|m| filter_tags.iter().any(|t| m.tags.contains(t)));
        }

        // Apply access control filter.
        if let Some(viewer) = viewer_pubkey {
            messages.retain(|m| is_visible_to(m, viewer));
        }

        (messages, has_more)
    }

    pub fn last_modified(
        &self,
        tag_filter: &Option<Vec<String>>,
        viewer_pubkey: Option<&str>,
    ) -> (Option<String>, Option<i64>) {
        let conn = self.conn.lock().unwrap();
        // Get recent messages and filter in Rust for consistency.
        let mut stmt = conn
            .prepare(
                "SELECT id, ed25519, tags, body, allow_list, disallow, signature, created_at
                 FROM messages ORDER BY id DESC LIMIT 100",
            )
            .unwrap();

        let rows = stmt
            .query_map([], |row| {
                let tags_str: String = row.get(2)?;
                let body_str: String = row.get(3)?;
                let allow_str: Option<String> = row.get(4)?;
                let disallow_str: Option<String> = row.get(5)?;
                Ok(StoredMessage {
                    id: row.get(0)?,
                    ed25519: row.get(1)?,
                    tags: serde_json::from_str(&tags_str).unwrap_or_default(),
                    body: serde_json::from_str(&body_str).unwrap_or_default(),
                    allow: allow_str.map(|s| serde_json::from_str(&s).unwrap_or_default()),
                    disallow: disallow_str.map(|s| serde_json::from_str(&s).unwrap_or_default()),
                    signature: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })
            .unwrap();

        let messages: Vec<StoredMessage> = rows.filter_map(|r| r.ok()).collect();

        for m in &messages {
            if let Some(filter_tags) = tag_filter {
                if !filter_tags.iter().any(|t| m.tags.contains(t)) {
                    continue;
                }
            }
            if let Some(viewer) = viewer_pubkey {
                if !is_visible_to(m, viewer) {
                    continue;
                }
            }
            return (Some(m.created_at.clone()), Some(m.id));
        }

        (None, None)
    }
}

/// Check if a message is visible to the given viewer public key.
pub fn is_visible_to(msg: &StoredMessage, viewer_pubkey: &str) -> bool {
    // Sender can always see own messages unless explicitly disallowed.
    let is_sender = msg.ed25519 == viewer_pubkey;

    if let Some(ref disallow) = msg.disallow {
        if disallow.contains(&viewer_pubkey.to_string()) {
            return false;
        }
    }

    if is_sender {
        return true;
    }

    if let Some(ref allow) = msg.allow {
        return allow.contains(&viewer_pubkey.to_string());
    }

    true
}
