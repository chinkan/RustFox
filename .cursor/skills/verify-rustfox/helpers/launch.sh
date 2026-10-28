#!/usr/bin/env bash
# Launch isolated portal_preview for verify-rustfox.
# Never touches /home/kan/.rustfox or live Telegram.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_ID="${VERIFY_RUN_ID:-$(date +%Y%m%d-%H%M%S)-$$}"
EVIDENCE="${VERIFY_EVIDENCE_DIR:-/tmp/verify-rustfox-${RUN_ID}}"
PORT="${PORTAL_PREVIEW_PORT:-8123}"
TOKEN="${PORTAL_PREVIEW_TOKEN:-preview-token}"
STATE_DIR="${VERIFY_STATE_DIR:-/tmp/verify-rustfox-state-${RUN_ID}}"

mkdir -p "$EVIDENCE" "$STATE_DIR"

# Refuse to collide with a live Kan fox-home.
if [[ "${RUSTFOX_HOME:-}" == "/home/kan/.rustfox" ]] || [[ "${RUSTFOX_HOME:-}" == "$HOME/.rustfox" && -d /home/kan/.rustfox ]]; then
  echo "REFUSE: RUSTFOX_HOME points at a live fox-home. Unset it; portal_preview uses its own temp dir." >&2
  exit 2
fi

BIN="$ROOT/target/release/examples/portal_preview"
if [[ ! -x "$BIN" ]]; then
  echo "Building portal_preview (needs web/dist embedded)…" >&2
  (cd "$ROOT" && ./scripts/build-all.sh)
  (cd "$ROOT" && cargo build --release --example portal_preview)
fi

# Port already in use?
if ss -ltn 2>/dev/null | awk '{print $4}' | grep -qE "[:.]${PORT}\$"; then
  echo "REFUSE: port ${PORT} already bound. Pick another PORTAL_PREVIEW_PORT or stop the owner you started." >&2
  exit 3
fi

LOG="$STATE_DIR/portal_preview.log"
PID_FILE="$STATE_DIR/portal_preview.pid"
META="$STATE_DIR/meta.env"

(
  cd "$ROOT"
  env PORTAL_PREVIEW_PORT="$PORT" PORTAL_PREVIEW_TOKEN="$TOKEN" \
    "$BIN" >"$LOG" 2>&1 &
  echo $! >"$PID_FILE"
)

PID="$(cat "$PID_FILE")"

# Ready signal: stdout line PREVIEW_URL=http://127.0.0.1:<port>/
READY=0
for _ in $(seq 1 90); do
  if ! kill -0 "$PID" 2>/dev/null; then
    echo "portal_preview exited early. Log:" >&2
    tail -50 "$LOG" >&2
    exit 4
  fi
  if grep -q "^PREVIEW_URL=" "$LOG" 2>/dev/null; then
    READY=1
    break
  fi
  # Also accept HTTP answering on the bind
  if curl -sf -o /dev/null "http://127.0.0.1:${PORT}/"; then
    READY=1
    break
  fi
  sleep 0.5
done

if [[ "$READY" -ne 1 ]]; then
  echo "Timed out waiting for PREVIEW_URL / HTTP on :${PORT}. Log:" >&2
  tail -50 "$LOG" >&2
  kill "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
  exit 5
fi

cat >"$META" <<META
VERIFY_RUN_ID=$RUN_ID
VERIFY_EVIDENCE_DIR=$EVIDENCE
VERIFY_STATE_DIR=$STATE_DIR
PORTAL_PREVIEW_PORT=$PORT
PORTAL_PREVIEW_TOKEN=$TOKEN
PORTAL_PREVIEW_PID=$PID
PORTAL_PREVIEW_URL=http://127.0.0.1:${PORT}/
PORTAL_PREVIEW_LOG=$LOG
META

# Convenience symlink for doctor/cleanup defaults
ln -sfn "$STATE_DIR" /tmp/verify-rustfox-last-state
ln -sfn "$EVIDENCE" /tmp/verify-rustfox-last-evidence

cp "$META" "$EVIDENCE/launch-meta.env"
echo "LAUNCHED pid=$PID url=http://127.0.0.1:${PORT}/ evidence=$EVIDENCE state=$STATE_DIR"
