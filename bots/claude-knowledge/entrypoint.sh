#!/bin/sh
set -e

gizmo config --url "${GIZMO_URL:-https://gizmo.voltrevo.com}" --token "${GIZMO_TOKEN}"

if [ -n "$GIZMO_PRIVATE_KEY" ]; then
  mkdir -p ~/.local/share/gizmo
  echo "$GIZMO_PRIVATE_KEY" > ~/.local/share/gizmo/${GIZMO_USER:-claude}.json
else
  gizmo keygen --user "${GIZMO_USER:-claude}"
fi

# --- Brain repo setup ---
#
# ~/brain contains:
#   brain/bare           — bare git repo (the "remote")
#   brain/router         — router agent's clone
#   brain/workers/slot-N — each worker's permanent clone
#
BRAIN="${BRAIN:-/root/brain}"
BRAIN_BARE="$BRAIN/bare"
ROUTER_CLONE="$BRAIN/router"
MAX_WORKERS="${MAX_WORKERS:-3}"

mkdir -p "$BRAIN_BARE" "$ROUTER_CLONE" "$BRAIN/workers"

# Init bare repo if needed
if [ ! -f "$BRAIN_BARE/HEAD" ]; then
  git init --bare "$BRAIN_BARE"
  TMPINIT=$(mktemp -d)
  git -C "$TMPINIT" init
  git -C "$TMPINIT" config user.email "wren@gizmo"
  git -C "$TMPINIT" config user.name "Wren"
  git -C "$TMPINIT" commit --allow-empty -m "init"
  git -C "$TMPINIT" remote add origin "$BRAIN_BARE"
  git -C "$TMPINIT" push origin HEAD:main
  rm -rf "$TMPINIT"
  echo "Initialized bare brain repo at $BRAIN_BARE"
fi

# Router clone
if [ ! -d "$ROUTER_CLONE/.git" ]; then
  git clone "$BRAIN_BARE" "$ROUTER_CLONE"
  git -C "$ROUTER_CLONE" config user.email "wren-router@gizmo"
  git -C "$ROUTER_CLONE" config user.name "Wren Router"
fi

# Ensure router knowledge subdirs exist
mkdir -p "$ROUTER_CLONE/Wiki/people" \
         "$ROUTER_CLONE/Wiki/topics" \
         "$ROUTER_CLONE/_Config" \
         "$ROUTER_CLONE/_Temporal/Logs"

# Worker clones (permanent)
i=1
while [ "$i" -le "$MAX_WORKERS" ]; do
  WORKER_CLONE="$BRAIN/workers/slot-$i"
  if [ ! -d "$WORKER_CLONE/.git" ]; then
    mkdir -p "$WORKER_CLONE"
    git clone "$BRAIN_BARE" "$WORKER_CLONE"
    git -C "$WORKER_CLONE" config user.email "wren-worker-$i@gizmo"
    git -C "$WORKER_CLONE" config user.name "Wren Worker $i"
    echo "Created worker clone: $WORKER_CLONE"
  fi
  i=$((i + 1))
done

# --- Copy coordinator and prompts ---
mkdir -p /opt/claude-knowledge
cp /coordinator.ts /opt/claude-knowledge/coordinator.ts
cp /prompt-worker.md /opt/claude-knowledge/prompt-worker.md

# Build router prompt from template
cp /prompt-router.md /tmp/prompt-router.md
sed -i "s/{{USER}}/${GIZMO_USER:-claude}/g" /tmp/prompt-router.md
sed -i "s/{{TAGS}}/${GIZMO_TAGS:-chat}/g" /tmp/prompt-router.md
sed -i "s/{{CHANNEL}}/${GIZMO_CHANNEL:-default}/g" /tmp/prompt-router.md
sed -i "s|{{BRAIN}}|$BRAIN|g" /tmp/prompt-router.md

ROUTER_MODEL="${ROUTER_MODEL:-claude-haiku-4-5-20251001}"
WORKER_MODEL="${WORKER_MODEL:-claude-sonnet-4-6}"
MAX_BUDGET="${MAX_BUDGET:-}"

gizmo publish --user "${GIZMO_USER:-claude}" --tags "${GIZMO_TAGS:-chat}" --body "starting..." 2>/dev/null || true

# Derive router's ed25519 pubkey so coordinator can filter out self-messages
ROUTER_PUBKEY=$(gizmo users 2>/dev/null | awk -v u="${GIZMO_USER:-claude}" 'index($0,u)==1{print $NF}')

echo "Starting coordinator daemon..."
GIZMO_SECRET_KEY=$(cat ~/.local/share/gizmo/users/${GIZMO_USER:-claude}/secret-key 2>/dev/null || true)
ROUTER_PUBKEY="$ROUTER_PUBKEY" \
  GIZMO_TOKEN="$GIZMO_TOKEN" \
  GIZMO_URL="${GIZMO_URL:-https://gizmo.voltrevo.com}" \
  GIZMO_CHANNEL="${GIZMO_CHANNEL:-default}" \
  GIZMO_SECRET_KEY="$GIZMO_SECRET_KEY" \
  bun /opt/claude-knowledge/coordinator.ts daemon 2>/var/log/coordinator.log &

# Wait for coordinator socket before starting router
until [ -S /tmp/coordinator.sock ]; do sleep 0.2; done

echo "Starting router agent (model: $ROUTER_MODEL, max_workers: $MAX_WORKERS)..."
export MAX_WORKERS WORKER_MODEL BRAIN
while true; do
  claude -p "$(cat /tmp/prompt-router.md)" \
    --allowedTools 'Bash,Read,Write,Glob,Grep,WebFetch,WebSearch' \
    --model "$ROUTER_MODEL" \
    --max-turns ${MAX_TURNS:-1000} \
    ${MAX_BUDGET:+--max-budget-usd $MAX_BUDGET} \
    --output-format stream-json \
    --verbose
  echo 'Router exited, restarting in 3s...' >&2
  sleep 3
done
