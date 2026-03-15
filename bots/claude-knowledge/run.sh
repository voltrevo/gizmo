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

# Persistent root home: gizmo keys, brain, claude credentials all live here
MOUNT_DIR="${MOUNT_DIR:-$SCRIPT_DIR/mount}"
ROOT_HOME="${ROOT_HOME:-$MOUNT_DIR/root}"
mkdir -p "$ROOT_HOME"
ROOT_HOME="$(cd "$ROOT_HOME" && pwd)"

# Auth: either ANTHROPIC_API_KEY for API, or credentials in root home for Max/OAuth
AUTH_ARGS=""
if [ -n "$ANTHROPIC_API_KEY" ]; then
  AUTH_ARGS="-e ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY"
elif [ -f "$ROOT_HOME/.claude/.credentials.json" ]; then
  echo "No ANTHROPIC_API_KEY set — using credentials from root home mount"
elif [ -f "$HOME/.claude/.credentials.json" ]; then
  echo "No ANTHROPIC_API_KEY set — seeding credentials from host ~/.claude"
  mkdir -p "$ROOT_HOME/.claude"
  cp "$HOME/.claude/.credentials.json" "$ROOT_HOME/.claude/.credentials.json"
  chmod 600 "$ROOT_HOME/.claude/.credentials.json"
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

ROUTER_MODEL="${ROUTER_MODEL:-claude-haiku-4-5-20251001}"
WORKER_MODEL="${WORKER_MODEL:-claude-sonnet-4-6}"
MAX_WORKERS="${MAX_WORKERS:-3}"

docker run -d \
  --name "$CONTAINER" \
  --restart unless-stopped \
  $AUTH_ARGS \
  -v "$ROOT_HOME:/root" \
  -e GIZMO_TOKEN="$GIZMO_TOKEN" \
  -e GIZMO_URL="${GIZMO_URL:-https://gizmo.voltrevo.com}" \
  -e GIZMO_USER="${GIZMO_USER:-claude}" \
  -e GIZMO_TAGS="${GIZMO_TAGS:-chat}" \
  -e GIZMO_CHANNEL="${GIZMO_CHANNEL:-default}" \
  ${GIZMO_PRIVATE_KEY:+-e GIZMO_PRIVATE_KEY="$GIZMO_PRIVATE_KEY"} \
  -e ROUTER_MODEL="$ROUTER_MODEL" \
  -e WORKER_MODEL="$WORKER_MODEL" \
  -e MAX_WORKERS="$MAX_WORKERS" \
  ${MAX_TURNS:+-e MAX_TURNS="$MAX_TURNS"} \
  ${MAX_BUDGET:+-e MAX_BUDGET="$MAX_BUDGET"} \
  "$IMAGE"

echo "Started $CONTAINER (detached, restarts unless stopped)"
echo "  Logs:      docker logs -f $CONTAINER"
echo "  Stop:      docker stop $CONTAINER"
echo "  Home:      $ROOT_HOME"
echo "  Brain:     $ROOT_HOME/brain"
echo "  Router:    $ROUTER_MODEL"
echo "  Workers:   $WORKER_MODEL (max $MAX_WORKERS concurrent)"
