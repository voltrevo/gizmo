# gizmo-client (TypeScript/Bun)

TypeScript client for the [gizmo](../README.md) WebSocket message server.

## Install

```sh
bun add ../bun
```

Or import directly:

```ts
import { GizmoClient, Signer, generateKeypair } from "../bun";
```

## Quick Start

```ts
import { GizmoClient, Signer } from "../bun";

const signer = Signer.generate();
// or from an existing secret key:
// const signer = new Signer("abcd1234...");

const client = new GizmoClient({
  url: "http://localhost:10421",
  token: "your-bearer-token",
  signer,
});

// Subscribe to live messages
await client.connect();
const sub = await client.subscribe((msg, subId) => {
  console.log(`[${subId}]`, msg.body);
}, { tags: ["chat"], channel: "my-channel" });

// Publish a message
const id = await client.publish({
  channel: "my-channel",
  tags: ["chat"],
  body: { text: "hello world" },
});
console.log("published:", id);

// Unsubscribe and disconnect
await sub.unsubscribe();
client.disconnect();
```

## API

### `generateKeypair(): Keypair`

Returns `{ secretKey, publicKey }` as hex strings.

### `new Signer(secretKeyHex: string)`

Holds an ed25519 keypair. Derives the public key from the secret key.

| Method | Description |
|---|---|
| `Signer.generate()` | Create a signer with a random keypair |
| `signer.publicKey` | Hex-encoded public key |
| `signer.sign(message)` | Sign a UTF-8 string, returns hex signature |
| `signer.signPayload(msg)` | Remove `signature` from `msg`, JSON-stringify the rest, and sign it |

### `new GizmoClient(opts: GizmoOptions)`

```ts
interface GizmoOptions {
  url: string;    // e.g. "http://localhost:10421"
  token: string;  // Bearer token
  signer: Signer; // Ed25519 signer
}
```

#### HTTP Methods

| Method | Description |
|---|---|
| `client.history(query?)` | Fetch paginated history. Returns `{ messages, has_more }` |
| `client.historyAll(query?)` | Async generator that yields all pages of history |
| `client.lastModified(tags?, channel?)` | Get the most recent message timestamp and id |

`HistoryQuery` fields: `channel`, `after`, `before`, `limit`, `tags`.

#### WebSocket Methods

| Method | Description |
|---|---|
| `client.connect()` | Open WebSocket connection |
| `client.disconnect()` | Close WebSocket connection |
| `client.publish({ channel?, tags, body, allow?, disallow? })` | Publish a message, returns the assigned id |
| `client.subscribe(handler, { tags?, channel?, subId? }?)` | Subscribe to live messages, returns a `Subscription` |
| `client.onMessage(handler)` | Global handler for all subscription messages |
| `client.onError(handler)` | Handle server error messages |
| `client.onClose(handler)` | Handle connection close |

A `Subscription` has an `id` and an `unsubscribe()` method.

All `channel` parameters default to `"default"` when omitted.

## Dependencies

- [`@noble/ed25519`](https://github.com/paulmillr/noble-ed25519) — ed25519 signatures
- [`@noble/hashes`](https://github.com/paulmillr/noble-hashes) — sha512 for ed25519
