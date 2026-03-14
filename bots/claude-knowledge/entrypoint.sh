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

# Knowledge dir owned by agent
mkdir -p /knowledge/people /knowledge/topics
touch /knowledge/log.md
chown -R agent:agent /knowledge

# Own agent's home
chown -R agent:agent /home/agent

# Build prompt from template
cp /prompt.md /tmp/prompt.md
sed -i "s/{{USER}}/${GIZMO_USER:-claude}/g" /tmp/prompt.md
sed -i "s/{{TAGS}}/${GIZMO_TAGS:-chat}/g" /tmp/prompt.md
sed -i "s/{{CHANNEL}}/${GIZMO_CHANNEL:-default}/g" /tmp/prompt.md
chown agent:agent /tmp/prompt.md

# Capture non-secret env vars
MAX_BUDGET="${MAX_BUDGET:-}"

# --- Phase 2: Drop privileges, clear secrets, run claude ---

# unset all secrets so they're not in the agent's environment
unset ANTHROPIC_API_KEY GIZMO_TOKEN GIZMO_PRIVATE_KEY

echo "Starting claude agent..."
exec su -s /bin/sh agent -c "
  claude -p \"\$(cat /tmp/prompt.md)\" \
    --allowedTools 'Bash,Read,Write,Glob,Grep,WebFetch,WebSearch' \
    ${MAX_TURNS:+--max-turns $MAX_TURNS} \
    ${MAX_BUDGET:+--max-budget-usd $MAX_BUDGET} \
    --output-format stream-json \
    --verbose
" 2>&1
