#!/usr/bin/env bash
# PreToolUse hook: block destructive git commands.
# Exit 2 = block; exit 0 = allow.
set -euo pipefail

INPUT=$(cat)

extract_command() {
  if command -v jq >/dev/null 2>&1; then
    printf '%s' "$1" | jq -r '.tool_input.command // empty'
  elif command -v python3 >/dev/null 2>&1; then
    printf '%s' "$1" | python3 -c 'import sys,json
try:
 d=json.load(sys.stdin)
 print((d.get("tool_input") or {}).get("command") or "")
except Exception:
 print("")'
  else
    printf '%s\n' "BLOCKED: jq or python3 required for git guardrails (fail closed)." >&2
    exit 2
  fi
}

COMMAND=$(extract_command "$INPUT")

# No command field → nothing to judge (allow).
if [ -z "$COMMAND" ] || [ "$COMMAND" = "null" ]; then
  exit 0
fi

# Collapse whitespace for simpler matching; keep original for message.
NORMALIZED=$(printf '%s' "$COMMAND" | tr '\n' ' ' | sed 's/[[:space:]]\+/ /g')

blocked() {
  local reason=$1
  printf '%s\n' "BLOCKED: '$COMMAND' — $reason. The user has prevented you from doing this." >&2
  exit 2
}

# Match a git subcommand invocation (not e.g. `git log --grep='git push'`).
# Requires `git` as a command word followed by the subcommand as next token.
git_cmd() {
  local sub=$1
  printf '%s' "$NORMALIZED" | grep -qE "(^|[;|&]|&&|\|\|) *git +${sub}( |$)"
}

# --- always block ---
if git_cmd push; then
  blocked "git push is not allowed"
fi
if printf '%s' "$NORMALIZED" | grep -qE "(^|[;|&]|&&|\|\|) *git +reset +(--hard|-h\b)"; then
  blocked "git reset --hard is not allowed"
fi
if git_cmd 'reset' && printf '%s' "$NORMALIZED" | grep -qE -- '--hard'; then
  blocked "git reset --hard is not allowed"
fi

# git clean with force (-f / -ff) in any flag cluster
if git_cmd clean && printf '%s' "$NORMALIZED" | grep -qE -- '(^| )-[a-zA-Z]*f'; then
  blocked "git clean -f is not allowed"
fi

# branch force-delete: -D or --delete --force / -d --force
if git_cmd branch; then
  if printf '%s' "$NORMALIZED" | grep -qE -- '(^| )-D( |$)' \
    || printf '%s' "$NORMALIZED" | grep -qE -- '--delete +--force|--force +--delete|-d +--force|--force +-d'; then
    blocked "force-deleting branches is not allowed"
  fi
fi

# Discard working tree: checkout/restore of "." only (not paths like .github/...)
# Matches: git checkout . | git checkout -- . | git restore . | git restore --source=HEAD .
if git_cmd checkout || git_cmd restore; then
  if printf '%s' "$NORMALIZED" | grep -qE '(checkout|restore)( +--[a-zA-Z0-9_=-]+)* +(\.|\-\- +\.)( |$)'; then
    blocked "discarding the whole working tree (checkout/restore .) is not allowed"
  fi
fi

exit 0
