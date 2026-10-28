#!/usr/bin/env bash
# Drive the portal-home-chat feature via real user path (Playwright e2e-verify
# subset OR curl chat API). Captures evidence; does not clean up.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
STATE_DIR="${VERIFY_STATE_DIR:-/tmp/verify-rustfox-last-state}"
if [[ -f "$STATE_DIR/meta.env" ]]; then
  # shellcheck disable=SC1091
  source "$STATE_DIR/meta.env"
fi

PORT="${PORTAL_PREVIEW_PORT:-8123}"
TOKEN="${PORTAL_PREVIEW_TOKEN:-preview-token}"
BASE="${PORTAL_PREVIEW_URL:-http://127.0.0.1:${PORT}/}"
BASE="${BASE%/}"
EVIDENCE="${VERIFY_EVIDENCE_DIR:-/tmp/verify-rustfox-last-evidence}"
FEATURE_DIR="$EVIDENCE/portal-home-chat"
mkdir -p "$FEATURE_DIR"

MODE="${1:-curl}"  # curl | playwright

echo "drive portal-home-chat mode=$MODE base=$BASE evidence=$FEATURE_DIR"

if [[ "$MODE" == "playwright" ]]; then
  if [[ ! -d "$ROOT/web/node_modules/playwright" ]]; then
    (cd "$ROOT/web" && npm ci)
  fi
  # Prefer full e2e-verify.mjs — it exercises login→dashboard→chat SSE.
  # Redirect its /tmp/pw-*.png into evidence after run.
  export BASE TOKEN
  set +e
  (cd "$ROOT" && node e2e-verify.mjs) | tee "$FEATURE_DIR/e2e-verify.log"
  RC=${PIPESTATUS[0]}
  set -e
  for f in /tmp/pw-*.png; do
    [[ -f "$f" ]] || continue
    cp "$f" "$FEATURE_DIR/"
  done
  echo "feature_id=portal-home-chat" >"$FEATURE_DIR/PROOF.txt"
  echo "entry=e2e-verify.mjs (login→dashboard→chat SSE+reload)" >>"$FEATURE_DIR/PROOF.txt"
  echo "exit=$RC" >>"$FEATURE_DIR/PROOF.txt"
  echo "base=$BASE" >>"$FEATURE_DIR/PROOF.txt"
  exit "$RC"
fi

# curl path: login → history → POST chat SSE → history again
JAR="$FEATURE_DIR/cookies.jar"
curl -sS -c "$JAR" -H 'Content-Type: application/json' \
  -d "{\"token\":\"${TOKEN}\"}" "$BASE/api/auth/login" | tee "$FEATURE_DIR/01-login.json"
echo

curl -sS -b "$JAR" "$BASE/api/auth/me" | tee "$FEATURE_DIR/02-me.json"
echo

curl -sS -b "$JAR" "$BASE/api/chat/history" | tee "$FEATURE_DIR/03-history-before.json"
echo

curl -sS -b "$JAR" "$BASE/api/chat/threads" | tee "$FEATURE_DIR/04-threads.json"
echo

# Real user path: POST /api/chat (SSE) — same bridge the SPA uses
curl -sS -N -b "$JAR" -H 'Content-Type: application/json' \
  -d '{"text":"verify-rustfox hello stream"}' \
  "$BASE/api/chat" | tee "$FEATURE_DIR/05-chat-sse.txt"
echo

sleep 1
curl -sS -b "$JAR" "$BASE/api/chat/history" | tee "$FEATURE_DIR/06-history-after.json"
echo

# Observable proof: history after must contain the user text and preview reply
if grep -q 'verify-rustfox hello stream' "$FEATURE_DIR/06-history-after.json" \
  && grep -q 'Preview reply to: verify-rustfox hello stream' "$FEATURE_DIR/06-history-after.json"; then
  echo "PASS chat persistence + stream reply in history" | tee "$FEATURE_DIR/PROOF.txt"
  echo "feature_id=portal-home-chat" >>"$FEATURE_DIR/PROOF.txt"
  echo "entry=curl POST /api/auth/login → GET history → POST /api/chat SSE → GET history" >>"$FEATURE_DIR/PROOF.txt"
  echo "base=$BASE" >>"$FEATURE_DIR/PROOF.txt"
  exit 0
fi

echo "FAIL history did not contain user text + preview reply" | tee "$FEATURE_DIR/PROOF.txt"
exit 1
