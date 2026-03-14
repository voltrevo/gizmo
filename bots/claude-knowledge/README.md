# gizmo-claude-knowledge

A Dockerized Claude Code agent that participates in [gizmo](https://github.com/voltrevo/gizmo) group chat and builds a persistent knowledge base from its interactions.

## Quick start

```sh
# Set your gizmo token (or you'll be prompted)
export GIZMO_TOKEN="your-token-here"

# Option A: Claude Max (must be logged in on host)
./run.sh

# Option B: API key
ANTHROPIC_API_KEY=sk-ant-... ./run.sh
```

## How it works

- Claude Code runs non-interactively (`claude -p`) inside Docker
- It uses the gizmo CLI to read and send chat messages in a wait/publish loop
- It maintains a `/knowledge/` directory with notes on people, topics, and a log
- The knowledge dir is mounted from the host so you can inspect it
- Runs as a non-root user; credentials are cleared from the environment before Claude starts

## Configuration

| Env var | Description | Default |
|---------|-------------|---------|
| `GIZMO_TOKEN` | Gizmo server token | _(prompted)_ |
| `ANTHROPIC_API_KEY` | API key (skip for Max) | _(uses ~/.claude creds)_ |
| `GIZMO_USER` | Bot identity name | `claude` |
| `GIZMO_TAGS` | Tags for publish | `chat` |
| `GIZMO_CHANNEL` | Channel | `default` |
| `MAX_TURNS` | Max agentic turns | _(unlimited)_ |
| `MAX_BUDGET` | Cost cap (USD) | _(unlimited)_ |
| `KNOWLEDGE_DIR` | Host path for knowledge | `./knowledge/` |

## Managing

```sh
docker logs -f gizmo-claude   # follow logs
docker stop gizmo-claude      # stop (won't auto-restart)
docker start gizmo-claude     # start again
```

## Knowledge directory

Browse what the bot has learned:

```sh
ls knowledge/people/    # per-person notes
ls knowledge/topics/    # per-topic notes
cat knowledge/log.md    # event log
```
