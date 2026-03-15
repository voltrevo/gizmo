# gizmo-claude-knowledge

## Upgrading from the single-agent version

**Breaking change:** the persistent volume was renamed from `/knowledge` to `/brain`, and the format changed from a flat file directory to a git-based repository.

If you have an existing container with data in `bots/claude-knowledge/knowledge/`, it will not be migrated automatically. To preserve it:

```sh
# Copy old flat files into the new brain repo structure before first run
mkdir -p brain/router/Wiki
cp -r bots/claude-knowledge/knowledge/people  brain/router/Wiki/people
cp -r bots/claude-knowledge/knowledge/topics  brain/router/Wiki/topics
cp    bots/claude-knowledge/knowledge/log.md  brain/router/Wiki/log.md 2>/dev/null || true
# Then let run.sh init the bare repo normally on first start
```

Or start fresh — the agent will rebuild its knowledge base from chat history.

A Dockerized multi-agent Claude system that participates in [gizmo](https://github.com/voltrevo/gizmo) group chat and maintains a persistent shared brain (knowledge base).

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
| `entrypoint.sh` | Docker entrypoint: configures, init brain repo, starts router |
| `Dockerfile` | Container definition |
| `run.sh` | Build + launch helper |

### Brain repo structure (single `/brain` volume)

```
brain/
  bare/           ← bare git repo (the "remote")
  router/         ← router agent's clone
  workers/
    slot-1/       ← worker 1's permanent clone
    slot-2/       ← worker 2's permanent clone
    slot-3/       ← worker 3's permanent clone
```

All clones treat `brain/bare` as `origin`. Sync via standard `git pull` / `git push`. Merge on conflict (not rebase) — parallel history is preserved.

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
| `MAX_TURNS` | Max router turns | _(unlimited)_ |
| `MAX_BUDGET` | Cost cap (USD) | _(unlimited)_ |
| `BRAIN_DIR` | Host path for brain volume | `./brain` |

## Managing

```sh
docker logs -f gizmo-claude   # follow logs
docker stop gizmo-claude      # stop (won't auto-restart)
docker start gizmo-claude     # start again
```

## Brain directory (on host)

```sh
ls brain/bare/           # bare git repo
ls brain/router/         # router's working copy
ls brain/workers/slot-1/ # worker 1's working copy
git -C brain/bare log    # full history from all agents
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
