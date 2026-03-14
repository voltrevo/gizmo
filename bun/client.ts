import { Signer } from "./crypto.ts";

// ── Types ──────────────────────────────────────────────────────────────

export interface GizmoOptions {
  /** Server base URL, e.g. "http://localhost:10421" */
  url: string;
  /** Bearer token */
  token: string;
  /** Ed25519 signer (holds secret key, derives public key) */
  signer: Signer;
}

export interface StoredMessage {
  id: number;
  ed25519: string;
  channel: string;
  tags: string[];
  body: unknown;
  allow?: string[];
  disallow?: string[];
  signature: string;
  created_at: string;
}

export interface HistoryResponse {
  messages: StoredMessage[];
  has_more: boolean;
}

export interface HistoryQuery {
  channel?: string;
  after?: number;
  before?: number;
  limit?: number;
  tags?: string[];
}

export interface LastModifiedResponse {
  last_modified: string | null;
  last_id: number | null;
}

export interface Subscription {
  id: string;
  unsubscribe(): void;
}

export interface IncomingWhisper {
  from: string;
  body: unknown;
  signature: string;
}

type MessageHandler = (msg: StoredMessage, subId: string) => void;
type WhisperHandler = (whisper: IncomingWhisper) => void;
type ErrorHandler = (detail: string) => void;
type CloseHandler = (code: number, reason: string) => void;

// ── Client ─────────────────────────────────────────────────────────────

export class GizmoClient {
  private url: string;
  private wsUrl: string;
  private token: string;
  private signer: Signer;
  private ws: WebSocket | null = null;
  private messageHandlers = new Map<string, MessageHandler>();
  private globalMessageHandler: MessageHandler | null = null;
  private whisperHandler: WhisperHandler | null = null;
  private errorHandler: ErrorHandler | null = null;
  private closeHandler: CloseHandler | null = null;
  private pendingResolves = new Map<string, (value: unknown) => void>();
  private subCounter = 0;

  constructor(opts: GizmoOptions) {
    this.url = opts.url.replace(/\/$/, "");
    this.wsUrl = this.url.replace(/^http/, "ws");
    this.token = opts.token;
    this.signer = opts.signer;
  }

  // ── HTTP API ───────────────────────────────────────────────────────

  private headers(): Record<string, string> {
    return {
      Authorization: `Bearer ${this.token}`,
      "X-Ed25519-Pubkey": this.signer.publicKey,
    };
  }

  async history(query: HistoryQuery = {}): Promise<HistoryResponse> {
    const params = new URLSearchParams();
    if (query.channel) params.set("channel", query.channel);
    if (query.after != null) params.set("after", String(query.after));
    if (query.before != null) params.set("before", String(query.before));
    if (query.limit != null) params.set("limit", String(query.limit));
    if (query.tags?.length) params.set("tags", query.tags.join(","));

    const qs = params.toString();
    const resp = await fetch(`${this.url}/history${qs ? `?${qs}` : ""}`, {
      headers: this.headers(),
    });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}: ${await resp.text()}`);
    return resp.json() as Promise<HistoryResponse>;
  }

  /** Iterate through all history page by page. */
  async *historyAll(
    query: Omit<HistoryQuery, "after" | "before"> = {},
  ): AsyncGenerator<StoredMessage[], void, unknown> {
    let after: number | undefined;
    while (true) {
      const page = await this.history({ ...query, after });
      if (page.messages.length === 0) break;
      yield page.messages;
      after = page.messages[page.messages.length - 1]!.id;
      if (!page.has_more) break;
    }
  }

  async lastModified(tags?: string[], channel?: string): Promise<LastModifiedResponse> {
    const params = new URLSearchParams();
    if (channel) params.set("channel", channel);
    if (tags?.length) params.set("tags", tags.join(","));

    const qs = params.toString();
    const resp = await fetch(
      `${this.url}/last-modified${qs ? `?${qs}` : ""}`,
      { headers: this.headers() },
    );
    if (!resp.ok) throw new Error(`HTTP ${resp.status}: ${await resp.text()}`);
    return resp.json() as Promise<LastModifiedResponse>;
  }

  // ── WebSocket ──────────────────────────────────────────────────────

  connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(`${this.wsUrl}/ws`, {
        headers: this.headers(),
      } as unknown as string[]);

      ws.onopen = () => {
        this.ws = ws;
        resolve();
      };

      ws.onerror = (ev) => {
        reject(new Error(`WebSocket error: ${ev}`));
      };

      ws.onclose = (ev) => {
        this.ws = null;
        this.closeHandler?.(ev.code, ev.reason);
      };

      ws.onmessage = (ev) => {
        const data = JSON.parse(String(ev.data));
        switch (data.type) {
          case "message": {
            const handler = this.messageHandlers.get(data.sub_id);
            if (handler) handler(data.message, data.sub_id);
            this.globalMessageHandler?.(data.message, data.sub_id);
            break;
          }
          case "published": {
            // id=0 means ephemeral whisper ack, otherwise regular publish
            const key = data.id === 0 ? "whisper" : "publish";
            const r = this.pendingResolves.get(key);
            if (r) { this.pendingResolves.delete(key); r(data.id as number); }
            break;
          }
          case "subscribed": {
            const r = this.pendingResolves.get(`sub:${data.sub_id}`);
            if (r) {
              this.pendingResolves.delete(`sub:${data.sub_id}`);
              r(undefined);
            }
            break;
          }
          case "unsubscribed": {
            const r = this.pendingResolves.get(`unsub:${data.sub_id}`);
            if (r) {
              this.pendingResolves.delete(`unsub:${data.sub_id}`);
              r(undefined);
            }
            break;
          }
          case "whisper": {
            this.whisperHandler?.({ from: data.from, body: data.body, signature: data.signature });
            break;
          }
          case "error": {
            this.errorHandler?.(data.detail);
            // Reject any pending publish or whisper
            for (const key of ["publish", "whisper"]) {
              const r = this.pendingResolves.get(key);
              if (r) { this.pendingResolves.delete(key); r(-1); }
            }
            break;
          }
        }
      };
    });
  }

  disconnect(): void {
    this.ws?.close();
    this.ws = null;
  }

  onMessage(handler: MessageHandler): void {
    this.globalMessageHandler = handler;
  }

  onWhisper(handler: WhisperHandler): void {
    this.whisperHandler = handler;
  }

  onError(handler: ErrorHandler): void {
    this.errorHandler = handler;
  }

  onClose(handler: CloseHandler): void {
    this.closeHandler = handler;
  }

  private send(data: unknown): void {
    if (!this.ws) throw new Error("not connected");
    this.ws.send(JSON.stringify(data));
  }

  /** Publish a message. Returns the server-assigned message id. */
  async publish(opts: {
    channel?: string;
    tags: string[];
    body: unknown;
    allow?: string[];
    disallow?: string[];
  }): Promise<number> {
    // Build message fields in deterministic order (must match Rust's serde order).
    const msg: Record<string, unknown> = {};
    if (opts.channel) msg.channel = opts.channel;
    msg.tags = opts.tags;
    msg.body = opts.body;
    if (opts.allow) msg.allow = opts.allow;
    if (opts.disallow) msg.disallow = opts.disallow;

    const signature = this.signer.signPayload(msg);

    const promise = new Promise<number>((resolve) => {
      this.pendingResolves.set("publish", resolve as (v: unknown) => void);
    });

    this.send({
      type: "publish",
      ...msg,
      signature,
    });

    return promise;
  }

  /** Send an ephemeral whisper to a specific recipient (by pubkey). Not stored. */
  async whisper(to: string, body: unknown): Promise<void> {
    const msg = { to, body };
    const signature = this.signer.signPayload(msg);

    const promise = new Promise<void>((resolve) => {
      this.pendingResolves.set("whisper", resolve as (v: unknown) => void);
    });

    this.send({ type: "whisper", to, body, signature });
    await promise;
  }

  /** Subscribe to live messages. Returns a Subscription handle. */
  async subscribe(
    handler: MessageHandler,
    opts?: { tags?: string[]; channel?: string; subId?: string },
  ): Promise<Subscription> {
    const id = opts?.subId ?? `sub-${++this.subCounter}`;
    this.messageHandlers.set(id, handler);

    const promise = new Promise<void>((resolve) => {
      this.pendingResolves.set(`sub:${id}`, resolve as (v: unknown) => void);
    });

    this.send({
      type: "subscribe",
      sub_id: id,
      ...(opts?.channel ? { channel: opts.channel } : {}),
      ...(opts?.tags ? { tags: opts.tags } : {}),
    });

    await promise;

    return {
      id,
      unsubscribe: () => {
        this.messageHandlers.delete(id);
        const p = new Promise<void>((resolve) => {
          this.pendingResolves.set(
            `unsub:${id}`,
            resolve as (v: unknown) => void,
          );
        });
        this.send({ type: "unsubscribe", sub_id: id });
        return p;
      },
    };
  }
}
