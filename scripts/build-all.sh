#!/usr/bin/env bash
# Build the web portal dist FIRST, then the Rust binary — so the
# `include_dir!("web/dist")` embed always contains the fresh frontend.
# (Building cargo before web = portal ships stale/white-screen stub.)
#
# Usage:
#   ./scripts/build-all.sh              # npm ci (if needed) + vite build + cargo build --release
#   ./scripts/build-all.sh --skip-web   # cargo only (keeps existing web/dist)
#   ./scripts/build-all.sh --install    # ...then cargo install --path . (replaces ~/.cargo/bin/rustfox)
#   ./scripts/build-all.sh --profile dev  # debug build (faster compile, slower binary)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PROFILE="release"
SKIP_WEB=0
DO_INSTALL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --skip-web)  SKIP_WEB=1; shift ;;
    --install)   DO_INSTALL=1; shift ;;
    --profile)   PROFILE="$2"; shift 2 ;;
    -h|--help)   grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "Unknown flag: $1 (see --help)"; exit 1 ;;
  esac
done

CARGO_FLAGS=(--profile "$PROFILE")
[ "$PROFILE" = "dev" ] && CARGO_FLAGS=()

# ---- 1. Web ----------------------------------------------------------------
if [ "$SKIP_WEB" -eq 0 ]; then
  cd "$ROOT/web"
  if [ ! -d node_modules ] || [ package-lock.json -nt node_modules ]; then
    echo "npm ci ..."
    npm ci
  fi
  echo "vite build ..."
  npm run build
  # Sanity gate: fail loudly if dist looks empty/stub, otherwise cargo
  # happily embeds junk again (the original white-screen bug).
  if [ ! -s dist/index.html ]; then
    echo "ERROR: web/dist/index.html missing or empty — refusing to build binary."
    exit 1
  fi
  echo "OK: web/dist ready ($(find dist -type f | wc -l) files)"
  # include_dir! has NO rerun-if-changed fingerprint: editing web/dist
  # content does not re-trigger cargo, so the binary can keep embedding a
  # STALE dist (the white-screen trap). Touch the module that embeds it to
  # force a recompile with the fresh dist.
  touch "$ROOT/src/portal/static_serve.rs"
else
  echo "SKIP: --skip-web, embedding existing web/dist as-is"
fi

# ---- 2. Rust ---------------------------------------------------------------
cd "$ROOT"
if [ "$DO_INSTALL" -eq 1 ]; then
  echo "cargo install --path . ($PROFILE) ..."
  cargo install --path . --locked ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"}
else
  echo "cargo build ($PROFILE) ..."
  cargo build ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"}
fi

BIN="target/$([ "$PROFILE" = dev ] && echo debug || echo release)/rustfox"
echo "DONE: $([ "$DO_INSTALL" = 1 ] && echo "installed to ~/.cargo/bin/rustfox" || echo "$BIN")"
