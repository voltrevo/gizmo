#!/bin/bash
set -euo pipefail

# concurrent-wait: Block until a gizmo message arrives OR a background agent finishes.
# Prints a JSON event to stdout describing what happened.

AFTER_ID=""
AGENT_DIR=""

while [[ $# -gt 0 ]]; do
  case $1 in
    --after)    AFTER_ID="$2"; shift 2;;
    --agent-dir) AGENT_DIR="$2"; shift 2;;
    *) echo "Unknown arg: $1" >&2; exit 1;;
  esac
done

if [[ -z "$AFTER_ID" ]]; then
  echo "Usage: concurrent-wait --after <id> [--agent-dir <dir>]" >&2
  exit 1
fi

TMPFILE=$(mktemp /tmp/gizmo-wait.XXXXXX)

cleanup() {
  kill "$WAIT_PID" 2>/dev/null || true
  wait "$WAIT_PID" 2>/dev/null || true
  rm -f "$TMPFILE"
}
trap cleanup EXIT

# Launch gizmo wait in background
gizmo wait --after "$AFTER_ID" > "$TMPFILE" 2>/dev/null &
WAIT_PID=$!

while true; do
  sleep 1

  MSG_ARRIVED=false
  AGENT_DONE=false

  # Check if gizmo wait exited (message arrived)
  if ! kill -0 "$WAIT_PID" 2>/dev/null; then
    wait "$WAIT_PID" 2>/dev/null || true
    if [[ -s "$TMPFILE" ]]; then
      MSG_ARRIVED=true
    fi
  fi

  # Check if agent finished
  if [[ -n "$AGENT_DIR" && -f "$AGENT_DIR/done" ]]; then
    AGENT_DONE=true
  fi

  if $MSG_ARRIVED && $AGENT_DONE; then
    MSGS=$(jq -s '.' < "$TMPFILE")
    echo "{\"event\":\"both\",\"messages\":$MSGS,\"result_file\":\"$AGENT_DIR/result.txt\"}"
    exit 0
  elif $MSG_ARRIVED; then
    MSGS=$(jq -s '.' < "$TMPFILE")
    echo "{\"event\":\"message\",\"messages\":$MSGS}"
    exit 0
  elif $AGENT_DONE; then
    echo "{\"event\":\"agent_done\",\"result_file\":\"$AGENT_DIR/result.txt\"}"
    exit 0
  fi
done
