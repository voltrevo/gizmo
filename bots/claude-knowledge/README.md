# gizmo-claude-knowledge

A Dockerized multi-agent Claude system that participates in [gizmo](https://github.com/voltrevo/gizmo) group chat and maintains a persistent shared knowledge base.

## Architecture

```
gizmo chat
    │
    ▼
Router agent (haiku)          ← always-on, responds fast, never does heavy work
    │
    ├── Tier 1: answer inline
    ├── Tier 2: call sonnet inline for reasoning/prioritization decisions
    └── Tier 3: enqueue to coordinator
                │
                ▼
           Coordinator (bun)  ← manages slots, task queue, spawns workers
                │
                ├── Worker slot 1 (sonnet)
                ├── Worker slot 2 (sonnet)
                └── Worker slot 3 (sonnet)
```

### Components

| File | Purpose |
|------|---------|
| `prompt-router.md` | System prompt for the haiku router agent |
| `prompt-worker.md` | System prompt for sonnet worker agents |
| `coordinator.ts` | Bun script managing worker slots and task queue |
| `entrypoint.sh` | Docker entrypoint: configures, starts coordinator + router |
| `Dockerfile` | Container definition |
| `run.sh` | Build + launch helper |

### Knowledge base (bare repo + worktrees)

Knowledge is stored in a bare git repo (`/knowledge-bare`). Each agent instance gets a worktree (`/knowledge`). Workers push to the bare repo; pull-rebase on conflict. No namespacing — rely on git merge resolution.

## Quick start

```sh
# Set your gizmo token (or you'll be prompted)
export GIZMO_TOKEN="your-token-here"

# Option A: Claude Max (must be logged in on host)
./run.sh

# Option B: API key
ANTHROPIC_API_KEY=sk-ant-... ./run.sh
```

## Configuration

| Env var | Description | Default |
|---------|-------------|---------|
| `GIZMO_TOKEN` | Gizmo server token | _(prompted)_ |
| `ANTHROPIC_API_KEY` | API key (skip for Max) | _(uses ~/.claude creds)_ |
| `GIZMO_USER` | Bot identity name | `claude` |
| `GIZMO_TAGS` | Tags for publish | `chat` |
| `GIZMO_CHANNEL` | Channel | `default` |
| `ROUTER_MODEL` | Model for router agent | `claude-haiku-4-5-20251001` |
| `WORKER_MODEL` | Model for worker agents | `claude-sonnet-4-6` |
| `MAX_WORKERS` | Max concurrent workers | `3` |
| `MAX_TURNS` | Max router turns (unlimited = no restart) | _(unlimited)_ |
| `MAX_BUDGET` | Cost cap (USD) | _(unlimited)_ |
| `KNOWLEDGE_BARE` | Path for bare git repo | `/knowledge-bare` |

## Managing

```sh
docker logs -f gizmo-claude   # follow logs
docker stop gizmo-claude      # stop (won't auto-restart)
docker start gizmo-claude     # start again
```

## Knowledge directory

The bare repo is mounted from the host. Worktree at `/knowledge` inside the container.

```sh
ls knowledge/Wiki/people/     # per-person notes
ls knowledge/Wiki/topics/     # per-topic notes
ls knowledge/_Temporal/Logs/  # session event logs
```

## Coordinator IPC

The router agent communicates with the coordinator via a FIFO (`/tmp/coordinator-in`) and reads results from `/tmp/coordinator-out.jsonl`.

**Router → Coordinator:**
```json
{ "type": "enqueue", "id": "uuid", "prompt": "...", "priority": 5 }
{ "type": "cancel", "id": "uuid" }
{ "type": "reprioritize", "id": "uuid", "priority": 9 }
{ "type": "status" }
```

**Coordinator → Router:**
```json
{ "type": "started", "id": "uuid", "slot": 1 }
{ "type": "done", "id": "uuid", "slot": 1, "result": "..." }
{ "type": "failed", "id": "uuid", "slot": 1, "error": "...", "result": "..." }
{ "type": "status", "slots": [...], "queue": [...] }
```
