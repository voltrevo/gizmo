#!/bin/sh
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
IMAGE="gizmo-claude"
CONTAINER="gizmo-claude"

# Build from project root (needs bun/ for gizmo source)
PROJECT_ROOT="$SCRIPT_DIR/../.."
docker build -f "$SCRIPT_DIR/Dockerfile" -t "$IMAGE" "$PROJECT_ROOT"

# Stop existing container if running
docker rm -f "$CONTAINER" 2>/dev/null || true

# Auth: either ANTHROPIC_API_KEY for API, or mount ~/.claude for Max/OAuth
AUTH_ARGS=""
if [ -n "$ANTHROPIC_API_KEY" ]; then
  AUTH_ARGS="-e ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY"
elif [ -f "$HOME/.claude/.credentials.json" ]; then
  echo "No ANTHROPIC_API_KEY set — mounting credentials for OAuth/Max auth"
  AUTH_ARGS="-v $HOME/.claude/.credentials.json:/root/.claude/.credentials.json:ro"
else
  echo "Error: Set ANTHROPIC_API_KEY or log in with 'claude' first (for Max)" >&2
  exit 1
fi

# Gizmo token: use env, or prompt interactively
if [ -z "$GIZMO_TOKEN" ]; then
  printf "Gizmo token: "
  read -r GIZMO_TOKEN
  if [ -z "$GIZMO_TOKEN" ]; then
    echo "Error: Gizmo token is required" >&2
    exit 1
  fi
fi

KNOWLEDGE_DIR="${KNOWLEDGE_DIR:-$SCRIPT_DIR/knowledge}"
mkdir -p "$KNOWLEDGE_DIR"

docker run -d \
  --name "$CONTAINER" \
  --restart unless-stopped \
  $AUTH_ARGS \
  -v "$KNOWLEDGE_DIR:/knowledge" \
  -e GIZMO_TOKEN="$GIZMO_TOKEN" \
  -e GIZMO_USER="${GIZMO_USER:-claude}" \
  -e GIZMO_TAGS="${GIZMO_TAGS:-chat}" \
  -e GIZMO_CHANNEL="${GIZMO_CHANNEL:-default}" \
  ${MAX_TURNS:+-e MAX_TURNS="$MAX_TURNS"} \
  ${MAX_BUDGET:+-e MAX_BUDGET="$MAX_BUDGET"} \
  "$IMAGE"

echo "Started $CONTAINER (detached, restarts unless stopped)"
echo "  Logs:  docker logs -f $CONTAINER"
echo "  Stop:  docker stop $CONTAINER"
