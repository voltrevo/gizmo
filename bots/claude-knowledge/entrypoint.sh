#!/bin/sh
set -e

# --- Phase 1: Configure as root (agent can't see this) ---

gizmo config --url "${GIZMO_URL:-https://gizmo.voltrevo.com}" --token "${GIZMO_TOKEN}"

if [ -n "$GIZMO_PRIVATE_KEY" ]; then
  mkdir -p ~/.local/share/gizmo
  echo "$GIZMO_PRIVATE_KEY" > ~/.local/share/gizmo/${GIZMO_USER:-claude}.json
else
  gizmo keygen --user "${GIZMO_USER:-claude}"
fi

mkdir -p /home/agent/.local/share
cp -r /root/.local/share/gizmo /home/agent/.local/share/gizmo

if [ -f /root/.claude/.credentials.json ]; then
  mkdir -p /home/agent/.claude
  cp /root/.claude/.credentials.json /home/agent/.claude/.credentials.json
  chmod 600 /home/agent/.claude/.credentials.json
fi

chmod 600 /root/.claude/.credentials.json 2>/dev/null || true
chmod -R 700 /root/.local/share/gizmo 2>/dev/null || true

# --- Brain repo setup ---
#
# /brain is a single mounted volume containing:
#   /brain/bare           — bare git repo (the "remote")
#   /brain/router         — router agent's clone
#   /brain/workers/slot-N — each worker's permanent clone
#
BRAIN="${BRAIN:-/brain}"
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

chown -R agent:agent "$BRAIN"

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
chown agent:agent /tmp/prompt-router.md

chown -R agent:agent /home/agent

ROUTER_MODEL="${ROUTER_MODEL:-claude-haiku-4-5-20251001}"
WORKER_MODEL="${WORKER_MODEL:-claude-sonnet-4-6}"
MAX_BUDGET="${MAX_BUDGET:-}"

gizmo publish --user "${GIZMO_USER:-claude}" --tags "${GIZMO_TAGS:-chat}" --body "starting..." 2>/dev/null || true

# --- Phase 2: Drop privileges, run router ---

unset ANTHROPIC_API_KEY GIZMO_TOKEN GIZMO_PRIVATE_KEY

echo "Starting coordinator daemon..."
bun /opt/claude-knowledge/coordinator.ts daemon 2>/var/log/coordinator.log &
echo "Starting router agent (model: $ROUTER_MODEL, max_workers: $MAX_WORKERS)..."
exec su -s /bin/sh agent -c "
  export MAX_WORKERS='$MAX_WORKERS'
  export WORKER_MODEL='$WORKER_MODEL'
  export BRAIN='$BRAIN'
  claude -p \"\$(cat /tmp/prompt-router.md)\" \
    --allowedTools 'Bash,Read,Write,Glob,Grep,WebFetch,WebSearch' \
    --model '$ROUTER_MODEL' \
    ${MAX_TURNS:+--max-turns $MAX_TURNS} \
    ${MAX_BUDGET:+--max-budget-usd $MAX_BUDGET} \
    --output-format stream-json \
    --verbose
" 2>&1
