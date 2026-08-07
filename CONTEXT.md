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
Truncated (60 chars), redacted JSON args shown in Tool Notifier. Only in Verbose mode.