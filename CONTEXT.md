# CONTEXT.md — Domain Glossary

**Single-context repo.** All terms defined here.

---

## Tool UI Mode

Three levels of tool execution visibility for the user.

| Mode | Tool Notifier | Command Output | Cancel Button |
|------|---------------|----------------|---------------|
| Silent | Off (placeholder only) | Hidden | Hidden |
| Minimal | Tool name + status, no args | Hidden | Visible (simple text, no live output) |
| Verbose | Tool name + args + status | Live stream + result | Visible (with live output) |

### Tool Notifier
Telegram message that live-edits to show agent tool activity. Created per-message when mode ≠ Silent.

### Command Tool Output
Separate Telegram message from `execute_command` tool showing shell command + stdout/stderr. Suppressed in Silent/Minimal.

### Cancel Button
Inline keyboard button on command message allowing user to SIGKILL the running command. Available in Verbose + Minimal.

### Tool Activity
Entry in Tool Notifier showing: friendly tool name, optional args preview, status label (⏳/✓/✗).

### Friendly Tool Name
Human-readable label for built-in tools (e.g., "💻 Running a command" for `execute_command`).

### Args Preview
Truncated (60 chars), redacted JSON args shown in Verbose mode.

---

## Memory

### Knowledge
Current key-value snapshot the agent stores under `(category, key) → value`. One live value per pair.
_Avoid_: fact (when meaning KV), memory entry, note

### Knowledge History
Append-only archive of prior Knowledge values, written automatically on overwrite or delete.
_Avoid_: audit log, version log (generic)

### Fact
Time-bounded triple `(entity, relation, value)` with `valid_from` / `valid_to`. At most one active value per `(entity, relation)` (`valid_to` null = still active).
_Avoid_: subject/object_value, triple, statement, knowledge (KV sense)

### valid_from / valid_to
Inclusive start and exclusive-or-end bound of a Fact's validity window. `valid_to` null means still active. Stored as SQLite `datetime` TEXT.
_Avoid_: valid_until, effective_from, learned_at (provenance, not validity)
