# gizmo

A self-hosted WebSocket message server with ed25519 identity, tag-based routing, access control, and paginated history.

## Running

```
gizmo run
```

On first start, a random token is generated and saved to `.gizmo_token` (next to the database file). The token is printed to stdout. On subsequent starts the saved token is reused.

| Flag | Default | Description |
|---|---|---|
| `--token` | *(auto-generated)* | Bearer token. Also via `GIZMO_TOKEN` env. Overrides the saved token. |
| `--port` | `10421` | Listen port |
| `--db` | `gizmo.db` | SQLite database path |
| `--max-history-bytes` | `10737418240` (10 GB) | Max stored history size; oldest messages evicted when exceeded |

HTTP only — use a reverse proxy for TLS.

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
  "tags": ["chat", "general"],
  "body": { "text": "hello world" },
  "signature": "<hex>",
  "allow": ["<pubkey>", "..."],
  "disallow": ["<pubkey>", "..."]
}
```

| Field | Required | Description |
|---|---|---|
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
  "tags": ["chat"]
}
```

| Field | Required | Description |
|---|---|---|
| `sub_id` | yes | Client-chosen identifier for this subscription. |
| `tags` | no | If set, only messages matching at least one tag are delivered. If omitted, all visible messages are delivered. |

You can have multiple concurrent subscriptions with different `sub_id` values and different filters on a single connection.

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
1. It matches the subscription's tag filter (or the subscription has no filter).
2. It passes access control for your pubkey (see below).

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
GET /history?limit=100                    → messages, use last id as cursor
GET /history?after=<last_id>&limit=100    → next page
GET /history?after=<last_id>&limit=100    → repeat until has_more=false
```

Cursor-based pagination on monotonic IDs means you will never skip or duplicate messages, even while new messages are being written.

### `GET /last-modified`

**Query parameters:**

| Param | Description |
|---|---|
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

Every published message must be signed. The canonical payload is a JSON string with exactly these keys in this order:

```json
{"tags":["chat"],"body":{"text":"hello"},"allow":null,"disallow":null}
```

Construction rules:
- Build a JSON object with keys `tags`, `body`, `allow`, `disallow` in that order.
- Omitted `allow`/`disallow` must be `null`.
- Serialize with no extra whitespace (compact JSON).
- Key order must be exactly: `tags`, `body`, `allow`, `disallow`.
- Sign the resulting UTF-8 bytes with your ed25519 private key.
- Hex-encode the 64-byte signature.

**Example (pseudocode):**

```
payload = json_serialize({"tags": tags, "body": body, "allow": allow_or_null, "disallow": disallow_or_null})
signature = hex(ed25519_sign(private_key, payload.as_bytes()))
```
