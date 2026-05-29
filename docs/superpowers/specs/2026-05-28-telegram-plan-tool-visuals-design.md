# Telegram Plan and Tool Visuals Design

## Goal

Improve RustFox's Telegram verbose-mode experience for agent planning and tool execution.

When verbose mode is enabled, users should see one clear live progress message while the agent works, then a persistent audit card after completion. Planning tools should render as a readable checklist instead of generic tool-call text. Other tools should still appear in the same progress surface as a compact activity log.

This design also fixes the current duplicate tool-message bug where tool-call status appears in a separate streamed assistant message and remains after the final response.

## Decision

Use a B-style persistent audit card for verbose mode.

Behavior:

- During execution, the bot sends one live progress message.
- The live message is edited as planning and tool events arrive.
- `plan_create`, `plan_update`, and `plan_view` update a structured plan checklist area.
- Other tool calls update a compact tool activity area.
- The final assistant answer is sent separately through the existing answer stream.
- On completion, the live progress message is edited into a completed audit summary and kept in the chat.
- Non-verbose mode stays clean and should not show planning/tool audit cards.

The audit card should be produced only by the tool notifier path. Tool-status text must not be sent into the normal assistant response stream.

## Evidence

Telegram behavior supports this direction:

- `editMessageText` is the right primitive for reducing chat clutter while progress changes.
- `sendChatAction` is ephemeral and useful only as a lightweight wait signal, not as a multi-step progress surface.
- `deleteMessage` has message-age and permission limitations, so persistent final summaries are less brittle than delete-dependent cleanup.
- `sendMessageDraft` exists for temporary 30-second previews, but final user-visible output still needs normal messages. It is a possible future enhancement, not needed for this implementation.

UX guidance supports showing real steps for long-running work:

- Immediate feedback matters for waits longer than a short moment.
- Multi-step work should show current step and completed work rather than a static loading message.
- Unknown-length agent runs should avoid fake percentages and instead show actual work observed from events.

## Current Problem

Verbose mode currently has two independent progress paths:

1. `src/agent.rs` emits `ToolEvent::Started` and `ToolEvent::Completed` into `ToolCallNotifier`.
2. `src/agent.rs` also formats a status line with `format_tool_status_line()` and sends it into the normal response stream.

The Telegram platform then displays both:

- `ToolCallNotifier` edits a live `Working...` message and currently deletes it at finish.
- The response stream treats tool-status lines as assistant output, creating a separate message that remains in chat.

The root cause is dual emission, not a Telegram deletion failure.

## Desired Message Model

Verbose-mode messages should be:

1. One progress/audit message owned by `ToolCallNotifier`.
2. One or more final answer messages owned by the existing answer streamer.

No tool-status line should enter the final answer stream.

Non-verbose-mode messages should remain:

1. Existing transient thinking placeholder.
2. Final answer messages.

## Audit Card Shape

The live card should favor compact, scan-friendly text that works inside Telegram message constraints.

During execution:

```text
Working on your request

Plan
[>] 1. Gather context
[ ] 2. Update implementation
[ ] 3. Verify behavior

Current
Running: read_file

Recent tools
- plan_create: started
- read_file: started
```

After completion:

```text
Completed

Plan
[x] 1. Gather context
[x] 2. Update implementation
[x] 3. Verify behavior

Tool activity
- plan_create: completed
- read_file: completed
- cargo test: completed

Result
Final answer sent below.
```

If no planning tools are used, omit the plan section and show only tool activity.

If no tools are used, the notifier does not need to create an audit card.

## Plan Rendering

Planning tools are first-class in the notifier.

### plan_create

On `ToolEvent::Started`, parse arguments if possible:

- `title`
- `steps[]`

Render a plan checklist immediately, with all steps initially pending.

On `ToolEvent::Completed`, if the tool result contains a richer serialized plan, prefer the result. If not, keep the parsed arguments as the display model.

### plan_update

On `ToolEvent::Started`, parse arguments:

- `step_id`
- `status`
- optional `notes`

Optimistically update that step in the notifier display so the user sees the current step quickly.

On `ToolEvent::Completed`:

- Keep the optimistic update if the tool succeeded.
- If the tool failed, mark the attempted step as failed in the audit display and include a short failure note.

### plan_view

Use as a refresh opportunity. If the result includes checklist text, parse it conservatively or show it as a compact plan snapshot. Do not let parsing failure break notifier rendering.

## Tool Activity Rendering

For non-planning tools, use the existing friendly naming direction but keep the log compact.

Rules:

- Show the current active tool at the top when available.
- Keep a recent activity list with a bounded number of entries, for example last 8 to 12.
- Collapse repeated tool calls of the same name when useful, for example `read_file x4`.
- Redact or omit long argument previews.
- Never show secrets or raw config values.
- Indicate success or failure after completion.

## Component Changes

### `src/platform/tool_notifier.rs`

Extend `ToolEvent` or add notifier-side parsing so the notifier can maintain a display state:

```rust
struct ToolDisplayState {
    plan: Option<PlanDisplay>,
    active_tool: Option<ToolActivity>,
    recent_tools: Vec<ToolActivity>,
    mode: ToolDisplayMode,
}
```

`ToolDisplayMode` should support at least:

- `Live`
- `CompletedPersistent`

The notifier should format messages from this display state rather than appending plain text lines.

Change `finish()` behavior in verbose mode:

- If at least one tool or plan event occurred, edit the existing progress message into the completed audit card.
- Do not delete the message by default for verbose mode.
- If no events occurred, delete or clean up as current behavior allows.

Keep edit failures non-fatal. If the final edit fails because Telegram cannot edit the message, log the error and leave the last live state visible.

### `src/agent.rs`

Remove or gate the stream-status emission that currently calls `format_tool_status_line()` and sends the result to the response stream.

The agent should continue to emit structured tool events:

- before tool execution: `ToolEvent::Started`
- after tool execution: `ToolEvent::Completed`

The normal response stream should contain only final assistant content tokens.

### `src/platform/telegram.rs`

Keep the existing verbose-mode notifier wiring:

- create tool-event channel when verbose mode is enabled
- spawn `ToolCallNotifier`
- pass `tool_event_tx` into `agent.process_message()`

Make sure the final answer streaming task and the notifier task remain separate. The final answer should not need to know whether the audit card is persistent.

If the notifier returns an error, the assistant answer should still be sent.

### `src/tools.rs`

No behavioral change is required for the planning tools initially. The notifier should parse existing tool-call arguments and results.

Optional future improvement: return a stable structured JSON result from `plan_create`, `plan_update`, and `plan_view` so display parsing does not depend on human-readable output.

## Failure Handling

- If `plan_create` arguments cannot be parsed, show the tool in the generic tool activity log.
- If `plan_update` references an unknown step, show the attempted update in recent tool activity and preserve the prior plan display.
- If message editing is rate-limited or fails, retry only through existing notifier cadence; do not block tool execution.
- If the final answer fails, keep the audit card as diagnostic context and let existing error handling report the answer failure.
- If the agent exceeds iteration limits, the audit card should remain and show the latest known plan/tool state.

## Security and Privacy

Verbose mode may expose tool names and argument summaries in Telegram. Keep existing verbose opt-in behavior and avoid increasing exposure in non-verbose mode.

Notifier rendering must:

- avoid showing full file contents
- avoid showing raw config values
- avoid showing long command arguments verbatim
- truncate argument previews
- preserve any existing redaction helpers if present

## Test Plan

Add focused unit tests before implementation.

Suggested tests:

1. Formatting a `plan_create` started event renders a checklist.
2. Formatting a `plan_update` event changes the target step status.
3. Formatting a failed `plan_update` marks or reports failure without panicking.
4. Generic tool events render in recent activity when no plan exists.
5. Completed verbose notifier output is persistent-summary text, not empty or delete-only behavior.
6. `src/agent.rs` no longer sends `format_tool_status_line()` output through the answer stream when verbose tool events are enabled.
7. Non-verbose processing does not create a notifier/audit card.

Run at minimum:

```bash
cargo fmt --all -- --check
cargo test
cargo clippy -- -D warnings
```

## Non-Goals

- Do not build a separate Telegram web app or image renderer for this iteration.
- Do not use `sendMessageDraft` yet.
- Do not rewrite the planning tool storage format unless required by tests.
- Do not make audit cards visible in non-verbose mode.
- Do not change final-answer Markdown rendering.

## Future Enhancements

- Add a setting to choose between ephemeral verbose progress and persistent verbose audit cards.
- Return structured JSON from planning tools for more robust UI rendering.
- Use Telegram drafts for short-lived progress previews if library support and product behavior are mature enough.
- Add richer plan summaries, such as elapsed time, failed steps, and verification evidence.
- Consider adding `.superpowers/` to `.gitignore` if local brainstorming artifacts should never be committed.
