import { test, expect, beforeAll, afterAll } from "bun:test";
import { GizmoClient, type StoredMessage } from "./client.ts";
import { Signer } from "./crypto.ts";
import { $ } from "bun";

const TOKEN = "test-token-" + Math.random().toString(36).slice(2);
const PORT = 10_500 + Math.floor(Math.random() * 1000);
const URL = `http://localhost:${PORT}`;
const DB = `/tmp/gizmo-test-${Date.now()}.db`;

let serverProc: Bun.Subprocess;

beforeAll(async () => {
  // Build and start the server
  await $`cargo build --quiet`.cwd(`${import.meta.dir}/..`);
  serverProc = Bun.spawn(
    [`${import.meta.dir}/../target/debug/gizmo`, "run", "--token", TOKEN, "--port", String(PORT), "--db", DB],
    { stdout: "ignore", stderr: "ignore" },
  );
  // Wait for server to be ready
  for (let i = 0; i < 50; i++) {
    try {
      await fetch(`${URL}/last-modified`, {
        headers: { Authorization: `Bearer ${TOKEN}` },
      });
      break;
    } catch {
      await Bun.sleep(100);
    }
  }
});

afterAll(() => {
  serverProc?.kill();
  try {
    require("fs").unlinkSync(DB);
    require("fs").unlinkSync(DB + "-wal");
    require("fs").unlinkSync(DB + "-shm");
  } catch {}
});

test("keygen produces valid keypair", () => {
  const signer = Signer.generate();
  expect(signer.secretKey).toHaveLength(64);
  expect(signer.publicKey).toHaveLength(64);
});

test("HTTP: last-modified with no messages", async () => {
  const signer = Signer.generate();
  const client = new GizmoClient({ url: URL, token: TOKEN, signer });
  const resp = await client.lastModified();
  expect(resp.last_modified).toBeNull();
  expect(resp.last_id).toBeNull();
});

test("WebSocket: publish and receive via subscription", async () => {
  const signer = Signer.generate();
  const client = new GizmoClient({ url: URL, token: TOKEN, signer });

  await client.connect();

  const received: unknown[] = [];
  await client.subscribe((msg) => received.push(msg), ["chat"]);

  const id = await client.publish({
    tags: ["chat"],
    body: { text: "hello" },
  });

  expect(id).toBeGreaterThan(0);

  // Give broadcast a moment
  await Bun.sleep(100);

  expect(received).toHaveLength(1);
  expect((received[0] as any).body).toEqual({ text: "hello" });
  expect((received[0] as any).ed25519).toBe(signer.publicKey);

  client.disconnect();
});

test("HTTP: history pagination", async () => {
  const signer = Signer.generate();
  const client = new GizmoClient({ url: URL, token: TOKEN, signer });

  await client.connect();

  // Publish 5 messages with a unique tag
  const tag = `pag-${Date.now()}`;
  for (let i = 0; i < 5; i++) {
    await client.publish({ tags: [tag], body: { i } });
  }

  client.disconnect();

  // Use historyAll to collect all messages, then verify count
  const all: StoredMessage[] = [];
  for await (const page of client.historyAll({ tags: [tag], limit: 2 })) {
    all.push(...page);
  }
  expect(all).toHaveLength(5);
  // Verify ordering
  for (let i = 1; i < all.length; i++) {
    expect(all[i]!.id).toBeGreaterThan(all[i - 1]!.id);
  }
});

test("HTTP: historyAll iterator", async () => {
  const signer = Signer.generate();
  const client = new GizmoClient({ url: URL, token: TOKEN, signer });

  let total = 0;
  for await (const page of client.historyAll({ limit: 2 })) {
    total += page.length;
  }
  // At least the messages from previous tests
  expect(total).toBeGreaterThan(0);
});

test("access control: allow list", async () => {
  const alice = Signer.generate();
  const bob = Signer.generate();
  const eve = Signer.generate();

  const aliceClient = new GizmoClient({ url: URL, token: TOKEN, signer: alice });
  await aliceClient.connect();

  // Alice publishes a message only Bob can see
  await aliceClient.publish({
    tags: ["secret"],
    body: { msg: "for bob only" },
    allow: [bob.publicKey],
  });

  aliceClient.disconnect();

  // Bob should see it
  const bobClient = new GizmoClient({ url: URL, token: TOKEN, signer: bob });
  const bobHistory = await bobClient.history({ tags: ["secret"] });
  const bobVisible = bobHistory.messages.filter(
    (m) => m.body && (m.body as any).msg === "for bob only",
  );
  expect(bobVisible.length).toBeGreaterThan(0);

  // Eve should not
  const eveClient = new GizmoClient({ url: URL, token: TOKEN, signer: eve });
  const eveHistory = await eveClient.history({ tags: ["secret"] });
  const eveVisible = eveHistory.messages.filter(
    (m) => m.body && (m.body as any).msg === "for bob only",
  );
  expect(eveVisible).toHaveLength(0);
});

test("multiple subscriptions on same connection", async () => {
  const signer = Signer.generate();
  const client = new GizmoClient({ url: URL, token: TOKEN, signer });

  await client.connect();

  const chatMsgs: unknown[] = [];
  const systemMsgs: unknown[] = [];

  await client.subscribe((msg) => chatMsgs.push(msg), ["multi-chat"]);
  await client.subscribe((msg) => systemMsgs.push(msg), ["multi-system"]);

  await client.publish({ tags: ["multi-chat"], body: { text: "chat msg" } });
  await client.publish({ tags: ["multi-system"], body: { text: "system msg" } });

  await Bun.sleep(100);

  expect(chatMsgs).toHaveLength(1);
  expect(systemMsgs).toHaveLength(1);
  expect((chatMsgs[0] as any).body.text).toBe("chat msg");
  expect((systemMsgs[0] as any).body.text).toBe("system msg");

  client.disconnect();
});
