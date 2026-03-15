#!/usr/bin/env bun
/**
 * Wren coordinator — manages worker slots and routes tasks from the router agent.
 *
 * Architecture:
 *   - Router (haiku) writes tasks to the coordinator via IPC (stdin JSON-lines)
 *   - Coordinator spawns up to MAX_WORKERS concurrent claude worker processes
 *   - Workers write results to their slot dir; coordinator signals router on completion
 *   - Router can send priority-override or cancel commands via IPC
 *
 * IPC protocol (newline-delimited JSON on stdin):
 *   Router → Coordinator:
 *     { "type": "enqueue", "id": "uuid", "prompt": "...", "priority": 1-10 }
 *     { "type": "cancel", "id": "uuid" }
 *     { "type": "reprioritize", "id": "uuid", "priority": N }
 *     { "type": "status" }     — coordinator prints current slot state to stdout
 *
 *   Coordinator → Router (stdout JSON-lines):
 *     { "type": "started",  "id": "uuid", "slot": N }
 *     { "type": "done",     "id": "uuid", "slot": N, "result": "..." }
 *     { "type": "failed",   "id": "uuid", "slot": N, "error": "..." }
 *     { "type": "status",   "slots": [...], "queue": [...] }
 *
 * Worker slots: /knowledge-workers/slot-{N}/ — permanent clone of knowledge bare repo.
 * Each worker also gets a /tmp/wren-task-{N}/ for task-specific scratch space (cleaned per task).
 */

import { spawn } from "child_process";
import { mkdirSync, existsSync, readFileSync, rmSync } from "fs";
import { randomUUID } from "crypto";
import * as readline from "readline";

const MAX_WORKERS = Number(process.env.MAX_WORKERS ?? 3);
const WORKER_MODEL = process.env.WORKER_MODEL ?? "claude-sonnet-4-6";
const KNOWLEDGE_WORKERS_BASE = process.env.KNOWLEDGE_WORKERS_BASE ?? "/knowledge-workers";
const WORKER_MAX_TURNS = Number(process.env.WORKER_MAX_TURNS ?? 30);

interface Task {
  id: string;
  prompt: string;
  priority: number; // 1 (low) to 10 (high)
  enqueuedAt: number;
}

interface Slot {
  slotId: number;
  task: Task;
  proc: ReturnType<typeof spawn>;
  dir: string;          // per-task scratch dir
  knowledgeDir: string;  // permanent knowledge clone
  startedAt: number;
}

const queue: Task[] = [];
const slots = new Map<number, Slot>();

function freeSlots(): number[] {
  const used = new Set(slots.keys());
  return Array.from({ length: MAX_WORKERS }, (_, i) => i + 1).filter(
    (n) => !used.has(n)
  );
}

function pickNextTask(): Task | undefined {
  if (queue.length === 0) return undefined;
  // Sort by priority desc, then by enqueue time asc
  queue.sort((a, b) => b.priority - a.priority || a.enqueuedAt - b.enqueuedAt);
  return queue.shift();
}

function startWorker(slot: number, task: Task) {
  const knowledgeDir = `${KNOWLEDGE_WORKERS_BASE}/slot-${slot}`;
  const taskDir = `/tmp/wren-task-${slot}`;
  rmSync(taskDir, { recursive: true, force: true });
  mkdirSync(taskDir, { recursive: true });

  const resultPath = `${taskDir}/result.txt`;

  const workerPrompt = `${task.prompt}

---
Worker slot: ${slot}. Task ID: ${task.id}.
Your knowledge clone: ${knowledgeDir} (pull before reading, push after updating)
Write your result to: ${resultPath}
`;

  const proc = spawn(
    "claude",
    [
      "-p",
      workerPrompt,
      "--allowedTools",
      "Bash,Read,Write,Glob,Grep,WebFetch,WebSearch",
      "--max-turns",
      String(WORKER_MAX_TURNS),
      "--model",
      WORKER_MODEL,
    ],
    { stdio: ["ignore", "pipe", "pipe"] }
  );

  const slotEntry: Slot = { slotId: slot, task, proc, dir: taskDir, knowledgeDir, startedAt: Date.now() };
  slots.set(slot, slotEntry);

  emit({ type: "started", id: task.id, slot });

  proc.on("close", (code) => {
    const { dir: taskDir2 } = slotEntry; // for clarity
    const resultPath = `${taskDir2}/result.txt`;
    const result = existsSync(resultPath)
      ? readFileSync(resultPath, "utf-8").trim()
      : "(no result.txt written)";

    if (code === 0) {
      emit({ type: "done", id: task.id, slot, result });
    } else {
      emit({ type: "failed", id: task.id, slot, error: `exit code ${code}`, result });
    }

    slots.delete(slot);
    rmSync(taskDir2, { recursive: true, force: true });
    drainQueue();
  });

  // Forward worker stderr to our stderr for debugging
  proc.stderr?.on("data", (d) =>
    process.stderr.write(`[slot-${slot}] ${d}`)
  );
}

function drainQueue() {
  for (const slot of freeSlots()) {
    const task = pickNextTask();
    if (!task) break;
    startWorker(slot, task);
  }
}

function emit(obj: unknown) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

// IPC: read commands from router via stdin
const rl = readline.createInterface({ input: process.stdin });

rl.on("line", (line) => {
  let msg: { type: string; [k: string]: unknown };
  try {
    msg = JSON.parse(line);
  } catch {
    process.stderr.write(`coordinator: invalid JSON from router: ${line}\n`);
    return;
  }

  switch (msg.type) {
    case "enqueue": {
      const task: Task = {
        id: (msg.id as string) || randomUUID(),
        prompt: msg.prompt as string,
        priority: Number(msg.priority ?? 5),
        enqueuedAt: Date.now(),
      };
      const free = freeSlots();
      if (free.length > 0) {
        startWorker(free[0]!, task);
      } else {
        queue.push(task);
      }
      break;
    }

    case "cancel": {
      const id = msg.id as string;
      // Remove from queue if not started
      const idx = queue.findIndex((t) => t.id === id);
      if (idx >= 0) {
        queue.splice(idx, 1);
        emit({ type: "cancelled", id });
      } else {
        // Kill running slot
        for (const [slot, s] of slots) {
          if (s.task.id === id) {
            s.proc.kill("SIGTERM");
            emit({ type: "cancelled", id, slot });
            break;
          }
        }
      }
      break;
    }

    case "reprioritize": {
      const id = msg.id as string;
      const newPriority = Number(msg.priority);
      const task = queue.find((t) => t.id === id);
      if (task) {
        task.priority = newPriority;
        emit({ type: "reprioritized", id, priority: newPriority });
      }
      break;
    }

    case "status": {
      emit({
        type: "status",
        slots: Array.from(slots.values()).map((s) => ({
          slot: s.slotId,
          id: s.task.id,
          priority: s.task.priority,
          runningMs: Date.now() - s.startedAt,
        })),
        queue: queue.map((t) => ({
          id: t.id,
          priority: t.priority,
          waitMs: Date.now() - t.enqueuedAt,
        })),
      });
      break;
    }

    default:
      process.stderr.write(`coordinator: unknown message type: ${msg.type}\n`);
  }
});

rl.on("close", () => {
  process.stderr.write("coordinator: router stdin closed, shutting down\n");
  for (const s of slots.values()) s.proc.kill("SIGTERM");
  process.exit(0);
});

process.stderr.write(
  `coordinator: ready (max_workers=${MAX_WORKERS}, model=${WORKER_MODEL})\n`
);
