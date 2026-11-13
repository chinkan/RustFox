# ADR 0002: Three-Level Tool UI Mode

## Status
Accepted (amended 2026-11-13, #404)

## Date
2026-07-31

## Context
Users requested a middle ground between:
- Silent: no tool progress or tool-result messages on Telegram (no `⏳ Thinking...` placeholder, no Working / Running line, no completed tool bubble, including no post-then-delete)
- Verbose: full tool call details including args, live command output, results

The original binary `tool_ui_enabled` (true/false) key couldn't express this.

## Decision
Introduce three `ToolUiMode` variants stored as `tool_ui_mode_{user_id}`:
- **Silent**: no Tool Notifier, no command output, no cancel button, and no Thinking placeholder. The final assistant reply still posts. `send_chat_action` Typing may stay. On Telegram, when a user message is accepted and the turn starts, Silent sets `setMessageReaction` 👀 on that message — the same emoji as the ADR-0020 mid-run steer ack, and not a new text bubble. When the turn completes (final reply / `sendVoice`, or the turn ends by cancel or error), that 👀 is cleared, including 👀 on mid-run steer messages accepted into the same turn. A `setMessageReaction` failure is `warn!` and the turn continues, with no text fallback. `/verbose` confirmations and queue-full refusals stay text. Commands that do not start a model turn do not get 👀. Minimal and Verbose do not gain this start/clear.
- **Minimal**: Tool Notifier shows tool name + status (no args), command output hidden, cancel button available
- **Verbose**: Tool Notifier shows tool name + args + status, live command output + result, cancel button with live output

`/verbose` command cycles: Minimal → Verbose → Silent → Minimal

## Rationale
- Minimal gives users awareness of *what* tool runs without leaking args or command output
- Cancel button in Minimal lets users stop long-running commands without seeing output
- Silent mode sends no tool-feedback messages. It is the same Telegram quietness as `[[bots]].fully_silent` for tool UI. `fully_silent = true` still forces that quietness for the speaking bot even when the stored mode is Minimal or Verbose. `/verbose` may still cycle the stored mode and send the mode-change confirmation. When the flag is later turned off, the stored mode applies. The 👀 reaction is the identifier that Silent has read the message and is processing it. It is set when the turn starts and cleared when that turn is done.
- Backward compatible: old `tool_ui_enabled=true` → Verbose, `false` → Silent (old "false" = no tool UI at all), absent key → Minimal (new default)

## Consequences
- Migration writes new key only when old key exists (idempotent; absent key keeps the live default)
- `execute_command` in Minimal sends cancel-button message, deletes it on completion
- Silent mode sends no tool-feedback message; Tool Notifier also disabled. The final assistant reply still posts. Silent also sets 👀 on the accepted user message for the duration of the turn and clears it, including steer 👀 from that turn, when the turn ends.
- Default for new users: Minimal
- Portal Chat tool UI is unchanged. This Silent quietness is Telegram only.