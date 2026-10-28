#!/usr/bin/env bash
# Tear down ONLY the portal_preview PID this run started.
# NEVER deletes evidence. NEVER kills by process name.
set -euo pipefail

STATE_DIR="${VERIFY_STATE_DIR:-/tmp/verify-rustfox-last-state}"
if [[ -f "$STATE_DIR/meta.env" ]]; then
  # shellcheck disable=SC1091
  source "$STATE_DIR/meta.env"
fi

PID="${PORTAL_PREVIEW_PID:-}"
if [[ -z "$PID" ]]; then
  echo "cleanup: no PORTAL_PREVIEW_PID — nothing to kill (evidence left intact)"
  exit 0
fi

if kill -0 "$PID" 2>/dev/null; then
  echo "cleanup: sending SIGTERM to pid=$PID"
  kill "$PID" || true
  for _ in $(seq 1 20); do
    kill -0 "$PID" 2>/dev/null || break
    sleep 0.25
  done
  if kill -0 "$PID" 2>/dev/null; then
    echo "cleanup: SIGKILL pid=$PID"
    kill -9 "$PID" || true
  fi
  wait "$PID" 2>/dev/null || true
  echo "cleanup: process stopped"
else
  echo "cleanup: pid=$PID already gone"
fi

# Remove scratch state only — keep evidence
if [[ -n "${VERIFY_STATE_DIR:-}" && -d "${VERIFY_STATE_DIR}" ]]; then
  rm -rf "${VERIFY_STATE_DIR}"
  echo "cleanup: removed state dir ${VERIFY_STATE_DIR}"
fi

if [[ -L /tmp/verify-rustfox-last-state ]]; then
  rm -f /tmp/verify-rustfox-last-state
fi

EVIDENCE="${VERIFY_EVIDENCE_DIR:-/tmp/verify-rustfox-last-evidence}"
if [[ -d "$EVIDENCE" ]] || [[ -L "$EVIDENCE" ]]; then
  echo "cleanup: evidence retained at $EVIDENCE"
fi
