#!/usr/bin/env bash
# Read-only health check for a verify-rustfox instance.
set -euo pipefail

STATE_DIR="${VERIFY_STATE_DIR:-/tmp/verify-rustfox-last-state}"
if [[ -f "$STATE_DIR/meta.env" ]]; then
  # shellcheck disable=SC1091
  source "$STATE_DIR/meta.env"
fi

PORT="${PORTAL_PREVIEW_PORT:-8123}"
TOKEN="${PORTAL_PREVIEW_TOKEN:-preview-token}"
URL="${PORTAL_PREVIEW_URL:-http://127.0.0.1:${PORT}/}"
PID="${PORTAL_PREVIEW_PID:-}"
FAIL=0

ok() { echo "OK   $*"; }
bad() { echo "FAIL $*"; FAIL=1; }

echo "doctor: url=$URL port=$PORT"

if [[ -n "$PID" ]]; then
  if kill -0 "$PID" 2>/dev/null; then
    ok "process alive pid=$PID"
  else
    bad "process dead pid=$PID"
  fi
else
  bad "no PORTAL_PREVIEW_PID in state (set VERIFY_STATE_DIR or re-launch)"
fi

# Port owned by our PID when possible
if [[ -n "$PID" ]] && command -v ss >/dev/null 2>&1; then
  if ss -ltnp 2>/dev/null | grep -E "[:.]${PORT}\b" | grep -q "pid=${PID},"; then
    ok "port ${PORT} owned by pid=${PID}"
  elif ss -ltn 2>/dev/null | awk '{print $4}' | grep -qE "[:.]${PORT}\$"; then
    ok "port ${PORT} is listening (pid ownership unchecked)"
  else
    bad "port ${PORT} not listening"
  fi
fi

HTTP_CODE="$(curl -s -o /tmp/verify-rustfox-doctor-body.html -w '%{http_code}' "$URL" || echo 000)"
if [[ "$HTTP_CODE" == "200" ]]; then
  ok "GET / → 200"
else
  bad "GET / → $HTTP_CODE"
fi

BODY="$(cat /tmp/verify-rustfox-doctor-body.html 2>/dev/null || true)"
if echo "$BODY" | grep -qi 'RustFox\|root\|login\|<!DOCTYPE'; then
  ok "SPA/HTML body present"
else
  bad "body looks empty/stub (did you build web/dist before cargo?)"
fi

# Auth path
ME="$(curl -s -H "Authorization: Bearer ${TOKEN}" "http://127.0.0.1:${PORT}/api/auth/me" || true)"
if echo "$ME" | grep -q '"authenticated"[[:space:]]*:[[:space:]]*true'; then
  ok "Bearer auth /api/auth/me authenticated"
elif echo "$ME" | grep -q 'authenticated'; then
  # try login then me with cookie jar
  JAR="/tmp/verify-rustfox-doctor.jar"
  LOGIN="$(curl -s -c "$JAR" -H 'Content-Type: application/json' \
    -d "{\"token\":\"${TOKEN}\"}" "http://127.0.0.1:${PORT}/api/auth/login" || true)"
  ME2="$(curl -s -b "$JAR" "http://127.0.0.1:${PORT}/api/auth/me" || true)"
  if echo "$ME2" | grep -q '"authenticated"[[:space:]]*:[[:space:]]*true'; then
    ok "cookie auth /api/auth/me authenticated (login ok)"
  else
    bad "auth failed login=$LOGIN me=$ME me2=$ME2"
  fi
else
  bad "auth/me unreachable: $ME"
fi

# Isolation reminder
if [[ -d /home/kan/.rustfox ]]; then
  ok "isolation: /home/kan/.rustfox exists but was not used by portal_preview"
fi

if [[ "$FAIL" -ne 0 ]]; then
  echo "doctor: UNHEALTHY — do not drive"
  exit 1
fi
echo "doctor: HEALTHY"
