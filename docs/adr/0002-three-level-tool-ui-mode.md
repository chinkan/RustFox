# ADR 0002: Three-Level Tool UI Mode

## Status
Accepted

## Date
2026-07-31

## Context
Users requested a middle ground between:
- Silent: no tool progress messages (only "⏳ Thinking..." placeholder)
- Verbose: full tool call details including args, live command output, results

The original binary `tool_ui_enabled` (true/false) key couldn't express this.

## Decision
Introduce three `ToolUiMode` variants stored as `tool_ui_mode_{user_id}`:
- **Silent**: no Tool Notifier, no command output, no cancel button
- **Minimal**: Tool Notifier shows tool name + status (no args), command output hidden, cancel button available
- **Verbose**: Tool Notifier shows tool name + args + status, live command output + result, cancel button with live output

`/verbose` command cycles: Minimal → Verbose → Silent → Minimal

## Rationale
- Minimal gives users awareness of *what* tool runs without leaking args or command output
- Cancel button in Minimal lets users stop long-running commands without seeing output
- Silent mode retains original "placeholder only" behavior
- Backward compatible: old `tool_ui_enabled=true` → Verbose, `false` → Silent (old "false" = no tool UI at all), absent key → Minimal (new default)

## Consequences
- Migration writes new key only when old key exists (idempotent; absent key keeps the live default)
- `execute_command` in Minimal sends cancel-button message, deletes it on completion
- Silent mode sends nothing; Tool Notifier also disabled
- Default for new users: Minimal