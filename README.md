# gizmo

A self-hosted WebSocket message server with ed25519 identity, tag-based routing, channels, access control, and paginated history.

## Running

```
gizmo run
```

Data is stored in `~/.local/share/gizmo/` by default. On first start, a random token is generated and saved to `~/.local/share/gizmo/token`. The token is printed to stdout. On subsequent starts the saved token is reused.

| Flag | Default | Description |
|---|---|---|
| `--token` | *(auto-generated)* | Bearer token. Also via `GIZMO_TOKEN` env. Overrides the saved token. |
| `--port` | `10421` | Listen port |
| `--db` | `~/.local/share/gizmo/gizmo.db` | SQLite database path |
| `--max-history-bytes` | `10737418240` (10 GB) | Max stored history size; oldest messages evicted when exceeded |

HTTP only — use a reverse proxy for TLS.

## Channels

Messages belong to a **channel**. Channels are independent message streams — subscriptions, history, and last-modified queries are all scoped to a single channel.

- **Default channel:** When no channel is specified, messages go to the `"default"` channel.
- **Naming rules:** `[a-zA-Z0-9_-]`, 1–64 characters.
- **Storage:** All channels share the global 10 GB history limit. Oldest messages across all channels are evicted when the limit is exceeded.

## CLI Commands

### `gizmo keygen`

Generate a new ed25519 keypair. Prints `secret_key` and `public_key` as hex.

### `gizmo publish`

Publish a single message and exit.

| Flag | Env | Description |
|---|---|---|
| `--url` | `GIZMO_URL` | Server URL (default `ws://localhost:10421`) |
| `--token` | `GIZMO_TOKEN` | Bearer token |
| `--secret-key` | `GIZMO_SECRET_KEY` | Hex-encoded ed25519 secret key |
| `--channel` | | Channel name (default `"default"`) |
| `--tags` | | Comma-separated tags |
| `--body` | | Message body as JSON string |
| `--allow` | | Optional comma-separated allow list |
| `--disallow` | | Optional comma-separated disallow list |

### `gizmo history`

Fetch paginated message history over HTTP.

| Flag | Env | Description |
|---|---|---|
| `--url` | `GIZMO_URL` | Server URL (default `http://localhost:10421`) |
| `--token` | `GIZMO_TOKEN` | Bearer token |
| `--public-key` | `GIZMO_PUBLIC_KEY` | Optional hex pubkey for access control filtering |
| `--channel` | | Channel name (default `"default"`) |
| `--after` | | Return messages with id > this value |
| `--before` | | Return messages with id < this value |
| `--limit` | | Page size (default 50, max 200) |
| `--tags` | | Comma-separated tag filter |

## Authentication

Every request (WebSocket and HTTP) requires two headers:

| Header | Value |
|---|---|
| `Authorization` | `Bearer <token>` |
| `X-Ed25519-Pubkey` | Your ed25519 public key, hex-encoded (64 chars) |

The pubkey header is required for WebSocket connections and publish. For HTTP history/last-modified endpoints it is optional — if omitted, access-controlled messages are not filtered (you see nothing restricted).

## WebSocket API

Connect to `ws://<host>:<port>/ws` with the headers above.

All messages are JSON. Max message size: **16 KB**.

### Client → Server

#### Publish

```json
{
  "type": "publish",
  "channel": "my-channel",
  "tags": ["chat", "general"],
  "body": { "text": "hello world" },
  "signature": "<hex>",
  "allow": ["<pubkey>", "..."],
  "disallow": ["<pubkey>", "..."]
}
```

| Field | Required | Description |
|---|---|---|
| `channel` | no | Channel to publish to. Defaults to `"default"` if omitted. |
| `tags` | yes | At least one tag. Used for subscription filtering. |
| `body` | yes | Arbitrary JSON value — the message payload. |
| `signature` | yes | Hex-encoded ed25519 signature of the canonical payload (see below). |
| `allow` | no | If set, only these pubkeys (plus the sender) can see the message. |
| `disallow` | no | These pubkeys cannot see the message (overrides sender self-visibility). |

**Do not include an `ed25519` field.** The server adds it automatically from your connection identity. If you include it, the server rejects the message.

Server responds:

```json
{ "type": "published", "id": 42 }
```

#### Subscribe

```json
{
  "type": "subscribe",
  "sub_id": "my-sub-1",
  "channel": "my-channel",
  "tags": ["chat"]
}
```

| Field | Required | Description |
|---|---|---|
| `sub_id` | yes | Client-chosen identifier for this subscription. |
| `channel` | no | Channel to subscribe to. Defaults to `"default"` if omitted. |
| `tags` | no | If set, only messages matching at least one tag are delivered. If omitted, all visible messages in the channel are delivered. |

You can have multiple concurrent subscriptions with different `sub_id` values, different channels, and different filters on a single connection.

Server responds:

```json
{ "type": "subscribed", "sub_id": "my-sub-1" }
```

#### Unsubscribe

```json
{
  "type": "unsubscribe",
  "sub_id": "my-sub-1"
}
```

### Server → Client

#### Message delivery

```json
{
  "type": "message",
  "sub_id": "my-sub-1",
  "message": {
    "id": 42,
    "ed25519": "<sender-pubkey>",
    "channel": "my-channel",
    "tags": ["chat"],
    "body": { "text": "hello world" },
    "allow": ["..."],
    "disallow": ["..."],
    "signature": "<hex>",
    "created_at": "2025-01-15T12:34:56.789"
  }
}
```

A message is delivered to a subscription if:
1. It belongs to the subscription's channel.
2. It matches the subscription's tag filter (or the subscription has no filter).
3. It passes access control for your pubkey (see below).

The same message may be delivered to multiple subscriptions if their filters overlap.

#### Error

```json
{ "type": "error", "detail": "description of what went wrong" }
```

## HTTP API

### `GET /history`

Paginated message history. Returns messages in ascending `id` order.

**Query parameters:**

| Param | Description |
|---|---|
| `channel` | Channel to query. Default `"default"`. |
| `after` | Return messages with `id > after` (forward pagination). |
| `before` | Return messages with `id < before` (backward pagination). |
| `limit` | Page size. Default `50`, max `200`. |
| `tags` | Comma-separated tag filter. Messages matching any listed tag are returned. |

**Response:**

```json
{
  "messages": [ ... ],
  "has_more": true
}
```

**Pagination pattern — reading all history forward:**

```
GET /history?channel=my-channel&limit=100                    → messages, use last id as cursor
GET /history?channel=my-channel&after=<last_id>&limit=100    → next page
GET /history?channel=my-channel&after=<last_id>&limit=100    → repeat until has_more=false
```

Cursor-based pagination on monotonic IDs means you will never skip or duplicate messages, even while new messages are being written.

### `GET /last-modified`

**Query parameters:**

| Param | Description |
|---|---|
| `channel` | Channel to query. Default `"default"`. |
| `tags` | Optional comma-separated tag filter. |

**Response:**

```json
{
  "last_modified": "2025-01-15T12:34:56.789",
  "last_id": 42
}
```

Returns the most recent visible message's timestamp and id. Respects `allow`/`disallow` — if the most recent message is not visible to your pubkey, it is skipped. Both fields are `null` if no visible messages exist.

## Access Control

| `allow` | `disallow` | Behavior |
|---|---|---|
| omitted | omitted | Message visible to everyone. |
| set | omitted | Only listed pubkeys + sender can see it. |
| omitted | set | Everyone except listed pubkeys can see it. |
| set | set | Only `allow`-listed pubkeys can see it, minus anyone in `disallow`. |

The sender **implicitly sees their own messages** unless they are explicitly in the `disallow` list.

`disallow` takes priority over `allow` and over sender self-visibility.

Access control is enforced on: live subscription delivery, history pagination, and last-modified queries.

## Signing

Every published message must be signed. The canonical payload is the message serialized as compact JSON, **minus the `signature` field**. Optional fields that are omitted are excluded entirely (not set to `null`).

Key order must match the wire format. Fields appear in this order when present: `channel`, `tags`, `body`, `allow`, `disallow`.

**Examples:**

Minimal message (no channel, no allow/disallow):
```json
{"tags":["chat"],"body":{"text":"hello"}}
```

With channel and allow list:
```json
{"channel":"my-channel","tags":["chat"],"body":{"text":"hello"},"allow":["abcd1234..."]}
```

**Construction rules:**
- Start with the message object you intend to send.
- Remove the `signature` field (and `ed25519` if present).
- Serialize as compact JSON (no extra whitespace).
- Key order must be preserved (insertion order in JS, `preserve_order` feature in serde_json).
- Sign the resulting UTF-8 bytes with your ed25519 private key.
- Hex-encode the 64-byte signature.

**Pseudocode:**

```
msg = {tags: tags, body: body}
if channel: msg.channel = channel  // add before tags
if allow: msg.allow = allow
if disallow: msg.disallow = disallow
payload = json_serialize(msg)
signature = hex(ed25519_sign(private_key, payload.as_bytes()))
```
