#!/usr/bin/env bash
# Verification gate (ADR 0011 plan): prove control-plane tests have TEETH.
# Each mutation changes a gate's BEHAVIOUR (not removing call sites, which
# would trip crate-wide #![deny(dead_code)] and mask the point). For each,
# the named test MUST go red; we then restore from git.
set -u
cd "$(dirname "$0")/.."
C=src/portal/control.rs
fail=0
snap=$(mktemp)
cp "$C" "$snap"

check() {
  local label="$1" needle="$2" repl="$3" test="$4"
  python3 - "$needle" "$repl" <<PY
import sys
p="$C"
s=open(p).read()
n,r=sys.argv[1],sys.argv[2]
assert n in s, f"NEEDLE MISSING for: $label"
open(p,"w").write(s.replace(n,r,1))
PY
  if [ $? -ne 0 ]; then echo "  ?? could not apply: $label"; cp "$snap" "$C"; fail=1; return; fi
  out=$(cargo test --test portal_api "$test" 2>&1)
  if echo "$out" | grep -q "test result: ok\."; then
    echo "  ✗ $label — test '$test' PASSED on mutated code (NO TEETH)"; fail=1
  else
    echo "  ✓ $label — caught by '$test'"
  fi
  cp "$snap" "$C"
}

echo "Mut-check control-plane gates:"

# 1) optimistic lock always passes -> stale-etag write would land
check "optimistic-lock (baseHash)" \
'    if expect.is_empty() {
        return Ok(());
    }' \
'    if expect.is_empty() {
        return Ok(());
    }
    if true { return Ok(()); }' \
'put_base_hash'

# 2) duplicate-create guard disabled
check "duplicate-create (409)" \
'    if primary_path(state, kind, &body.name).is_some() {' \
'    if false && primary_path(state, kind, &body.name).is_some() {' \
'post_creates_skill'

# 3) PUT update-only gate disabled -> PUT could create
check "PUT update-only (404)" \
'    if primary_path(state, kind, name).is_none() {' \
'    if false && primary_path(state, kind, name).is_none() {' \
'put_update_only'

# 4) tool-whitelist gate on create path disabled
check "create tool-whitelist (400)" \
'        if !missing_tools.is_empty() && !allow_missing {
            return Err(PortalError::bad_request(
                "unknown_tools",
                format!(
                    "tools not available at runtime: {} — configure the server or \
                     POST with ?allowMissing=1 to keep them anyway",' \
'        if false && !missing_tools.is_empty() && !allow_missing {
            return Err(PortalError::bad_request(
                "unknown_tools",
                format!(
                    "tools not available at runtime: {} — configure the server or \
                     POST with ?allowMissing=1 to keep them anyway",' \
'post_creates_agent'

rm -f "$snap"
if [ "$fail" -eq 0 ]; then echo "ALL MUT-CHECKS CAUGHT (tests have teeth)"; else echo "SOME MUT-CHECKS NOT CAUGHT"; exit 1; fi
