#!/bin/sh
set -e

# --- Phase 1: Configure as root (agent can't see this) ---

# Configure gizmo
gizmo config --url "${GIZMO_URL:-https://gizmo.voltrevo.com}" --token "${GIZMO_TOKEN}"

# Import or generate identity
if [ -n "$GIZMO_PRIVATE_KEY" ]; then
  mkdir -p ~/.local/share/gizmo
  echo "$GIZMO_PRIVATE_KEY" > ~/.local/share/gizmo/${GIZMO_USER:-claude}.json
else
  gizmo keygen --user "${GIZMO_USER:-claude}"
fi

# Copy gizmo config + keys to agent user so gizmo CLI works
mkdir -p /home/agent/.local/share
cp -r /root/.local/share/gizmo /home/agent/.local/share/gizmo

# Copy claude credentials to agent user's home
if [ -f /root/.claude/.credentials.json ]; then
  mkdir -p /home/agent/.claude
  cp /root/.claude/.credentials.json /home/agent/.claude/.credentials.json
  chmod 600 /home/agent/.claude/.credentials.json
fi

# Lock down root credential files
chmod 600 /root/.claude/.credentials.json 2>/dev/null || true
chmod -R 700 /root/.local/share/gizmo 2>/dev/null || true

# Knowledge dir: init as bare repo + create agent worktree
# If KNOWLEDGE_BARE is set, use it as the bare repo path.
KNOWLEDGE_BARE="${KNOWLEDGE_BARE:-/knowledge-bare}"
KNOWLEDGE_WORKTREE="/knowledge"

if [ ! -d "$KNOWLEDGE_BARE" ]; then
  git init --bare "$KNOWLEDGE_BARE"
  echo "Initialized bare knowledge repo at $KNOWLEDGE_BARE"
fi

if [ ! -d "$KNOWLEDGE_WORKTREE/.git" ]; then
  # First-time: add the worktree
  git -C "$KNOWLEDGE_BARE" worktree add "$KNOWLEDGE_WORKTREE" --orphan -b main 2>/dev/null || \
  git -C "$KNOWLEDGE_BARE" worktree add "$KNOWLEDGE_WORKTREE" main
fi

# Ensure knowledge subdirs exist
mkdir -p "$KNOWLEDGE_WORKTREE/Wiki/people" \
         "$KNOWLEDGE_WORKTREE/Wiki/topics" \
         "$KNOWLEDGE_WORKTREE/_Config" \
         "$KNOWLEDGE_WORKTREE/_Temporal/Logs"

chown -R agent:agent "$KNOWLEDGE_BARE" "$KNOWLEDGE_WORKTREE"

# Copy coordinator script to a stable location
mkdir -p /opt/claude-knowledge
cp /coordinator.ts /opt/claude-knowledge/coordinator.ts
cp /prompt-router.md /opt/claude-knowledge/prompt-router.md
cp /prompt-worker.md /opt/claude-knowledge/prompt-worker.md

# Build router prompt from template
cp /prompt-router.md /tmp/prompt-router.md
sed -i "s/{{USER}}/${GIZMO_USER:-claude}/g" /tmp/prompt-router.md
sed -i "s/{{TAGS}}/${GIZMO_TAGS:-chat}/g" /tmp/prompt-router.md
sed -i "s/{{CHANNEL}}/${GIZMO_CHANNEL:-default}/g" /tmp/prompt-router.md
chown agent:agent /tmp/prompt-router.md

# Own agent's home
chown -R agent:agent /home/agent

# Capture config env vars (non-secret)
ROUTER_MODEL="${ROUTER_MODEL:-claude-haiku-4-5-20251001}"
WORKER_MODEL="${WORKER_MODEL:-claude-sonnet-4-6}"
MAX_WORKERS="${MAX_WORKERS:-3}"
MAX_BUDGET="${MAX_BUDGET:-}"

# Publish startup message before dropping privileges
gizmo publish --user "${GIZMO_USER:-claude}" --tags "${GIZMO_TAGS:-chat}" --body "starting..." 2>/dev/null || true

# --- Phase 2: Drop privileges, clear secrets, run router agent ---

unset ANTHROPIC_API_KEY GIZMO_TOKEN GIZMO_PRIVATE_KEY

echo "Starting router agent (model: $ROUTER_MODEL, max_workers: $MAX_WORKERS)..."
exec su -s /bin/sh agent -c "
  export MAX_WORKERS='$MAX_WORKERS'
  export WORKER_MODEL='$WORKER_MODEL'
  claude -p \"\$(cat /tmp/prompt-router.md)\" \
    --allowedTools 'Bash,Read,Write,Glob,Grep,WebFetch,WebSearch' \
    --model '$ROUTER_MODEL' \
    ${MAX_TURNS:+--max-turns $MAX_TURNS} \
    ${MAX_BUDGET:+--max-budget-usd $MAX_BUDGET} \
    --output-format stream-json \
    --verbose
" 2>&1
