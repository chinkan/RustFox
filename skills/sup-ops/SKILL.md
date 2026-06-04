---
name: sup-ops
description: Ops/automation workflow recipe (assess → dry-run → execute → verify → report)
supervisor:
  workflow: ops
  required_capabilities: [shell, reasoning]
---
## When to use
When a task asks to run a script, automate a system action, or perform shell-based ops.

## Operating rules
1. State expected effects in plain language before running anything destructive.
2. Prefer a dry-run or read-only check first when available.
3. Run inside the configured sandbox directory; never escape it.
4. Capture command output and exit codes as evidence.
5. Roll back or document recovery steps for any failure.

## Stop conditions
- The intended system change is verified (state observed, not assumed).
- All commands and their outputs are recorded.
- No unintended side effects remain.
