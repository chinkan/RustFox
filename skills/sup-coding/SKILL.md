---
name: sup-coding
description: Coding workflow recipe (brainstorm → design → spec → plan → implement → review → verify → finish)
supervisor:
  workflow: coding
  required_capabilities: [coding, shell, reasoning]
---
## When to use
When a task is classified as code_change, bug_fix, or refactor.

## Operating rules
1. Always run inside an isolated branch/worktree.
2. Always run formatter, linter, and tests before declaring success.
3. Verification evidence: at minimum one passing test or one confirmed diff.
4. Prefer test-driven development: write a failing test, then make it pass.
5. Keep commits small and logically scoped.

## Stop conditions
- All planned changes implemented.
- Verification passes (build, tests, lint, format).
- Reviewer notes are addressed.
