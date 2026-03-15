#!/usr/bin/env bun
/**
 * Wren Coordinator — daemon + CLI for multi-agent task management.
 *
 * Usage:
 *   bun coordinator.ts daemon       — start the daemon (listens on SOCKET_PATH)
 *   bun coordinator.ts enqueue      — add a task (--prompt, --priority, [--id])
 *   bun coordinator.ts wait         — block until next batch event (--after <msg-id>)
 *   bun coordinator.ts status       — print current state as JSON
 *   bun coordinator.ts cancel <id>  — cancel a queued or running task
 *   bun coordinator.ts pause <id>   — pause a running task (preserves clone)
 *   bun coordinator.ts reprioritize <id> --priority <n>
 *   bun coordinator.ts stop         — gracefully shut down the daemon
 *
 * Clone states:
 *   active  — has a running worker process
 *   paused  — worker killed, clone preserved (partial work intact, task re-queued)
 *   free    — available for new task
 *
 * wait event format (returned as JSON line):
 *   { type: "batch", chat: StoredMessage[], last_chat_id: number, worker_events: WorkerEvent[] }
 */

import { createServer, createConnection, type Socket } from "net";
import { spawn, type ChildProcess } from "child_process";
import {
  mkdirSync,
  existsSync,
  readFileSync,
  rmSync,
  readdirSync,
} from "fs";
import { randomUUID } from "crypto";

// ── Config ────────────────────────────────────────────────────────────────

const SOCKET_PATH = process.env.COORDINATOR_SOCKET ?? "/tmp/coordinator.sock";
const MAX_WORKERS = Number(process.env.MAX_WORKERS ?? 3);
const WORKER_MODEL = process.env.WORKER_MODEL ?? "claude-sonnet-4-6";
const WORKER_MAX_TURNS = Number(process.env.WORKER_MAX_TURNS ?? 30);
const BRAIN = process.env.BRAIN ?? "/brain";
const WORKERS_BASE = `${BRAIN}/workers`;
// Router's pubkey — filter out self-messages from gizmo subscription
const ROUTER_PUBKEY = process.env.ROUTER_PUBKEY ?? "";

// ── Types ─────────────────────────────────────────────────────────────────

interface Task {
  id: string;
  prompt: string;
  priority: number;
  enqueuedAt: number;
  cloneDir?: string; // assigned clone (persists through pause/resume)
}

type CloneState = "free" | "active" | "paused";

interface Clone {
  dir: string;
  state: CloneState;
  taskId?: string;
  lastUsedAt: number;
}

interface ActiveWorker {
  task: Task;
  clone: Clone;
  proc: ChildProcess;
  startedAt: number;
  taskDir: string;
}

interface WorkerEvent {
  type: "done" | "failed";
  id: string;
  cloneDir: string;
  result?: string;
  error?: string;
}

interface StoredMessage {
  id: number;
  ed25519: string;
  channel: string;
  tags: string[];
  body: unknown;
  created_at: string;
}

// ── State ─────────────────────────────────────────────────────────────────

const queue: Task[] = [];
const activeWorkers = new Map<string, ActiveWorker>();
const clones: Clone[] = [];
let lastChatId = 0;
const pendingChatMessages: StoredMessage[] = [];
const pendingWorkerEvents: WorkerEvent[] = [];
const waiters: Array<() => void> = [];

// ── Clone management ──────────────────────────────────────────────────────

function loadExistingClones() {
  if (!existsSync(WORKERS_BASE)) return;
  for (const name of readdirSync(WORKERS_BASE)) {
    const dir = `${WORKERS_BASE}/${name}`;
    if (existsSync(`${dir}/.git`)) {
      clones.push({ dir, state: "free", lastUsedAt: 0 });
    }
  }
}

function createClone(name: string): Clone {
  const dir = `${WORKERS_BASE}/${name}`;
  mkdirSync(dir, { recursive: true });
  const barePath = `${BRAIN}/bare`;
  const r = Bun.spawnSync(["git", "clone", barePath, dir]);
  if (r.exitCode !== 0) throw new Error(`git clone failed: ${r.stderr}`);
  Bun.spawnSync(["git", "-C", dir, "config", "user.email", `wren-${name}@gizmo`]);
  Bun.spawnSync(["git", "-C", dir, "config", "user.name", `Wren ${name}`]);
  const clone: Clone = { dir, state: "free", lastUsedAt: 0 };
  clones.push(clone);
  return clone;
}

function getFreeClone(): Clone {
  const free = clones.find((c) => c.state === "free");
  if (free) return free;
  return createClone(`slot-${clones.length + 1}`);
}

function activeCount(): number {
  return clones.filter((c) => c.state === "active").length;
}

function pruneIdleClones() {
  if (activeCount() > 0 || queue.length > 0) return;
  // Sort: keep most recently used; drop excess beyond MAX_WORKERS
  const candidates = clones
    .filter((c) => c.state !== "active")
    .sort((a, b) => b.lastUsedAt - a.lastUsedAt)
    .slice(MAX_WORKERS);
  for (const c of candidates) {
    rmSync(c.dir, { recursive: true, force: true });
    const idx = clones.indexOf(c);
    if (idx >= 0) clones.splice(idx, 1);
  }
}

// ── Worker management ─────────────────────────────────────────────────────

function startWorker(task: Task, clone: Clone, isResume = false) {
  clone.state = "active";
  clone.taskId = task.id;
  clone.lastUsedAt = Date.now();
  task.cloneDir = clone.dir;

  const taskDir = `/tmp/wren-task-${task.id}`;
  rmSync(taskDir, { recursive: true, force: true });
  mkdirSync(taskDir, { recursive: true });

  const resultPath = `${taskDir}/result.txt`;

  const resumeNote = isResume
    ? `NOTE: This task was previously started and paused. Your brain clone (${clone.dir}) may contain partial work from the previous run — pull and review what's already there before continuing. Do not start over.\n\n`
    : "";

  const workerPrompt = `${resumeNote}${task.prompt}

---
Task ID: ${task.id}
Brain clone: ${clone.dir}
  - Sync before reading: cd ${clone.dir} && git pull
  - If you make brain updates, commit and push incrementally (so progress is preserved if this task is paused):
    cd ${clone.dir} && git add -A && git commit -m "<desc>" && git push origin HEAD
  - On push conflict: git fetch origin && git merge origin/HEAD, resolve, then push
Write result to: ${resultPath}
`;

  const proc = spawn(
    "claude",
    [
      "-p", workerPrompt,
      "--allowedTools", "Bash,Read,Write,Glob,Grep,WebFetch,WebSearch",
      "--max-turns", String(WORKER_MAX_TURNS),
      "--model", WORKER_MODEL,
    ],
    { stdio: ["ignore", "pipe", "pipe"] }
  );

  const worker: ActiveWorker = { task, clone, proc, startedAt: Date.now(), taskDir };
  activeWorkers.set(task.id, worker);

  proc.on("close", (code) => {
    activeWorkers.delete(task.id);
    clone.state = "free";
    clone.taskId = undefined;

    const result = existsSync(resultPath)
      ? readFileSync(resultPath, "utf-8").trim()
      : "(no result written)";

    pendingWorkerEvents.push(
      code === 0
        ? { type: "done", id: task.id, cloneDir: clone.dir, result }
        : { type: "failed", id: task.id, cloneDir: clone.dir, result, error: `exit ${code}` }
    );

    rmSync(taskDir, { recursive: true, force: true });
    notifyWaiters();
    drainQueue();
    pruneIdleClones();
  });

  proc.stderr?.on("data", (d) =>
    process.stderr.write(`[worker:${task.id.slice(0, 8)}] ${d}`)
  );
}

function drainQueue() {
  while (activeCount() < MAX_WORKERS && queue.length > 0) {
    queue.sort((a, b) => b.priority - a.priority || a.enqueuedAt - b.enqueuedAt);
    const task = queue.shift()!;
    // Resume from existing clone if paused, otherwise get a free one
    const resumeClone = task.cloneDir
      ? clones.find((c) => c.dir === task.cloneDir && c.state === "paused")
      : undefined;
    startWorker(task, resumeClone ?? getFreeClone(), !!resumeClone);
  }
}

function pauseWorker(taskId: string) {
  const worker = activeWorkers.get(taskId);
  if (!worker) return false;
  worker.proc.kill("SIGTERM");
  worker.clone.state = "paused";
  worker.clone.taskId = taskId;
  activeWorkers.delete(taskId);
  queue.push(worker.task); // re-queue so it can be resumed
  return true;
}

// ── Gizmo WebSocket watcher ───────────────────────────────────────────────

const GIZMO_URL = process.env.GIZMO_URL ?? "https://gizmo.voltrevo.com";
const GIZMO_TOKEN = process.env.GIZMO_TOKEN ?? "";
const GIZMO_CHANNEL = process.env.GIZMO_CHANNEL ?? "default";

function ingestMessages(msgs: StoredMessage[]) {
  const newMsgs = msgs.filter((m) => !ROUTER_PUBKEY || m.ed25519 !== ROUTER_PUBKEY);
  if (newMsgs.length === 0) return;
  lastChatId = Math.max(lastChatId, ...newMsgs.map((m) => m.id));
  pendingChatMessages.push(...newMsgs);
  notifyWaiters();
}

async function catchUpHistory() {
  try {
    const wsBase = GIZMO_URL.replace(/^http/, "ws");
    const url = new URL(`${GIZMO_URL}/history`);
    url.searchParams.set("after", String(lastChatId));
    url.searchParams.set("limit", "200");
    url.searchParams.set("channel", GIZMO_CHANNEL);
    const resp = await fetch(url.toString(), {
      headers: { Authorization: `Bearer ${GIZMO_TOKEN}`, "X-Ed25519-Pubkey": ROUTER_PUBKEY || "0".repeat(64) },
    });
    if (resp.ok) {
      const data = await resp.json() as { messages: StoredMessage[] };
      ingestMessages(data.messages);
    }
  } catch { /* ignore transient errors */ }
}

function startGizmoWatcher() {
  const wsBase = GIZMO_URL.replace(/^http/, "ws");
  const wsUrl = `${wsBase}/ws?token=${encodeURIComponent(GIZMO_TOKEN)}&pubkey=${encodeURIComponent(ROUTER_PUBKEY || "0".repeat(64))}`;

  const connect = () => {
    const ws = new WebSocket(wsUrl);

    ws.onopen = async () => {
      process.stderr.write("coordinator: gizmo WebSocket connected\n");
      // Catch up on any messages missed before this connection was established.
      await catchUpHistory();
      ws.send(JSON.stringify({ type: "subscribe", sub_id: "coordinator", channel: GIZMO_CHANNEL }));
    };

    ws.onmessage = (ev) => {
      try {
        const data = JSON.parse(ev.data as string);
        if (data.type === "message" && data.sub_id === "coordinator") {
          ingestMessages([data.message as StoredMessage]);
        }
      } catch { /* ignore parse errors */ }
    };

    ws.onclose = () => {
      process.stderr.write("coordinator: gizmo WebSocket closed, reconnecting in 2s\n");
      setTimeout(connect, 2000);
    };

    ws.onerror = () => {
      process.stderr.write("coordinator: gizmo WebSocket error\n");
    };
  };

  connect();
}

// ── Wait/notify ───────────────────────────────────────────────────────────

function hasPending(): boolean {
  return pendingChatMessages.length > 0 || pendingWorkerEvents.length > 0;
}

function notifyWaiters() {
  if (!hasPending()) return;
  for (const resolve of waiters.splice(0)) resolve();
}

function buildBatch(): object {
  return {
    type: "batch",
    chat: pendingChatMessages.splice(0),
    last_chat_id: lastChatId,
    worker_events: pendingWorkerEvents.splice(0),
  };
}

// ── IPC server ────────────────────────────────────────────────────────────

function handleConnection(socket: Socket) {
  let buf = "";
  socket.on("data", (d) => {
    buf += d.toString();
    const lines = buf.split("\n");
    buf = lines.pop() ?? "";
    for (const line of lines) {
      if (!line.trim()) continue;
      try {
        handleMessage(socket, JSON.parse(line));
      } catch {
        socket.write(JSON.stringify({ error: "invalid json" }) + "\n");
      }
    }
  });
}

function handleMessage(socket: Socket, msg: Record<string, unknown>) {
  switch (msg.type) {
    case "enqueue": {
      const task: Task = {
        id: (msg.id as string) || randomUUID(),
        prompt: msg.prompt as string,
        priority: Number(msg.priority ?? 5),
        enqueuedAt: Date.now(),
      };
      if (activeCount() < MAX_WORKERS) {
        startWorker(task, getFreeClone());
      } else {
        queue.push(task);
      }
      socket.write(JSON.stringify({ type: "enqueued", id: task.id }) + "\n");
      socket.end();
      break;
    }
    case "cancel": {
      const id = msg.id as string;
      const qi = queue.findIndex((t) => t.id === id);
      if (qi >= 0) {
        queue.splice(qi, 1);
        socket.write(JSON.stringify({ type: "cancelled", id }) + "\n");
      } else if (activeWorkers.has(id)) {
        const w = activeWorkers.get(id)!;
        w.proc.kill("SIGTERM");
        activeWorkers.delete(id);
        w.clone.state = "free";
        drainQueue();
        socket.write(JSON.stringify({ type: "cancelled", id }) + "\n");
      } else {
        socket.write(JSON.stringify({ error: "not found", id }) + "\n");
      }
      socket.end();
      break;
    }
    case "pause": {
      const id = msg.id as string;
      const ok = pauseWorker(id);
      socket.write(JSON.stringify(ok ? { type: "paused", id } : { error: "not running", id }) + "\n");
      socket.end();
      break;
    }
    case "reprioritize": {
      const id = msg.id as string;
      const t = queue.find((x) => x.id === id);
      if (t) {
        t.priority = Number(msg.priority);
        socket.write(JSON.stringify({ type: "reprioritized", id, priority: t.priority }) + "\n");
      } else {
        socket.write(JSON.stringify({ error: "not in queue", id }) + "\n");
      }
      socket.end();
      break;
    }
    case "status": {
      socket.write(
        JSON.stringify({
          type: "status",
          active: Array.from(activeWorkers.values()).map((w) => ({
            id: w.task.id,
            priority: w.task.priority,
            cloneDir: w.clone.dir,
            runningMs: Date.now() - w.startedAt,
          })),
          queue: queue.map((t) => ({
            id: t.id,
            priority: t.priority,
            waitMs: Date.now() - t.enqueuedAt,
            cloneDir: t.cloneDir,
          })),
          clones: clones.map((c) => ({ dir: c.dir, state: c.state, taskId: c.taskId })),
          last_chat_id: lastChatId,
        }) + "\n"
      );
      socket.end();
      break;
    }
    case "wait": {
      const afterId = Number(msg.after ?? 0);
      if (hasPending() || lastChatId > afterId) {
        socket.write(JSON.stringify(buildBatch()) + "\n");
        socket.end();
      } else {
        waiters.push(() => {
          socket.write(JSON.stringify(buildBatch()) + "\n");
          socket.end();
        });
      }
      break;
    }
    case "stop": {
      socket.write(JSON.stringify({ type: "stopping" }) + "\n");
      socket.end();
      process.exit(0);
    }
    default:
      socket.write(JSON.stringify({ error: `unknown: ${msg.type}` }) + "\n");
      socket.end();
  }
}

// ── CLI client ────────────────────────────────────────────────────────────

async function sendToSocket(msg: object): Promise<string> {
  return new Promise((resolve, reject) => {
    const socket = createConnection(SOCKET_PATH);
    let buf = "";
    socket.on("data", (d) => { buf += d.toString(); });
    socket.on("end", () => resolve(buf.trim()));
    socket.on("error", reject);
    socket.write(JSON.stringify(msg) + "\n");
  });
}

const args = process.argv.slice(2);
const command = args[0];
function flag(name: string) {
  const i = args.indexOf(`--${name}`);
  return i >= 0 && i + 1 < args.length ? args[i + 1] : undefined;
}

async function main() {
  if (!command || command === "daemon") {
    if (existsSync(SOCKET_PATH)) { try { rmSync(SOCKET_PATH); } catch { /* */ } }
    loadExistingClones();
    startGizmoWatcher();
    const server = createServer(handleConnection);
    server.listen(SOCKET_PATH, () => {
      process.stderr.write(`coordinator: ready (socket=${SOCKET_PATH} max_workers=${MAX_WORKERS})\n`);
    });
    return;
  }

  switch (command) {
    case "enqueue": {
      const prompt = flag("prompt");
      if (!prompt) { console.error("--prompt required"); process.exit(1); }
      console.log(await sendToSocket({ type: "enqueue", prompt, priority: Number(flag("priority") ?? 5), id: flag("id") }));
      break;
    }
    case "cancel":
      console.log(await sendToSocket({ type: "cancel", id: args[1] })); break;
    case "pause":
      console.log(await sendToSocket({ type: "pause", id: args[1] })); break;
    case "reprioritize":
      console.log(await sendToSocket({ type: "reprioritize", id: args[1], priority: Number(flag("priority")) })); break;
    case "status":
      console.log(await sendToSocket({ type: "status" })); break;
    case "wait":
      console.log(await sendToSocket({ type: "wait", after: Number(flag("after") ?? 0) })); break;
    case "stop":
      console.log(await sendToSocket({ type: "stop" })); break;
    default:
      console.error(`unknown: ${command}\nusage: daemon|enqueue|cancel|pause|reprioritize|status|wait|stop`);
      process.exit(1);
  }
}

main().catch((e) => { console.error(e.message ?? e); process.exit(1); });
