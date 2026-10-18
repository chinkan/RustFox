# ADR-0031: Telegram `/logs` sends the bot log file

- **Status:** Accepted (grill Q1–Q10 locked 2026-10-18)
- **Date:** 2026-10-18
- **Target release:** v1.0.7+
- **Depends on:** none (new surface)
- **Amends:** macOS LaunchAgent plist log redirect (stop writing `Library/Logs/rustfox.log`)
- **Locks:** PO grill 2026-10-18 — Q1–Q10 as below

## Context

Kan / Product want a Telegram `/logs` command so an allowed user can download the latest bot log on a phone and forward it for debugging (e.g. Ollama / quota false messages).

Today the app does **not** write a log file itself: `tracing` goes to stdout. Linux systemd user unit has no file redirect (journald). macOS LaunchAgent plist redirects stdout/stderr to `{{RUSTFOX_HOME}}/Library/Logs/rustfox.log` (no rotation). Windows `sc create` also has no log file. There is no portal logs download API and no Telegram `/logs`.

Without an in-app file sink, "send the current log file" is undefined on Linux / foreground / Windows.

## Decision

1. **Surface (Q1 / Q5).** Telegram **`/logs` only** for v1. **No** portal Logs UI/API unless a later grill proves need.

2. **Payload (Q1).** Send the **current log file** as a Telegram **document** (one file).

3. **Size (Q2).** If the file exceeds **16 MiB**, send only the **last 16 MiB** (tail truncate), then send as document. (Telegram document limit ~50 MiB; 16 MiB is the product cap for forwardability.)

4. **Secrets (Q3).** v1 sends the file **as-is** (no redact). Auth is allowlist-only (same as other sensitive commands). Redaction is a **separate ADR** if needed.

5. **Command shape (Q4).** Bare **`/logs` only**. Any non-empty / unknown args → **reject** with a clear tip (same strict style as ADR-0030 `/selfupgrade`). No `tail` / `N` args in v1.

6. **Auth (Q6 lock item).** Only **allowed / admin** users (same gate as other sensitive Telegram commands).

7. **File sink (Q6=A, Q7, Q8).** Introduce an in-app tracing file sink writing to **`{resolved_home}/logs/rustfox.log`** on all platforms (same relative path under `resolved_home`). Size rotate: when the active file reaches **32 MiB**, rename to **`rustfox.log.1`** (keep at most one `.1`); then continue writing a new `rustfox.log`. `/logs` always targets the active `rustfox.log` path (after optional 16 MiB tail for send).

8. **Missing / unreadable file (Q9).** Return a **clear English** Telegram error (not silent), e.g. that no log file exists yet / path under `~/.rustfox/logs/` (worded for users; mention the configured home path).

9. **macOS plist (Q10=A).** **Stop** LaunchAgent plist file redirect to `Library/Logs/rustfox.log`. stdout follows launchd/console; the **only** durable log file is the in-app sink. Update `com.rustfox.bot.plist.template` and any `service.rs` install hints that print the old Library/Logs path. **Do not** migrate old Library/Logs files automatically; `/logs` only recognizes the new path. Windows service install must **not** add a separate log path.

10. **Release.** Target **v1.0.7+**. Must **not** block unsigned **v1.0.6** / Kan `release.yml`. GitHub English only.

## Non-goals

- Portal logs UI/API (v1).
- Redacting secrets from logs (separate ADR).
- journalctl export as the primary Linux path (v1 uses in-app sink).
- Zipping multiple rotated files for send (v1 sends the active file only, with 16 MiB tail).
- Auto-migrating existing `Library/Logs/rustfox.log` content.

## Acceptance criteria (implementation — later assign; not this docs PR)

- In-app sink creates `{resolved_home}/logs/rustfox.log` with 32 MiB → `.1` rotation (max one `.1`).
- Telegram: `/logs` (allowed user) sends the file as document; >16 MiB → last 16 MiB only.
- Telegram: unknown args rejected; missing file → clear English error.
- macOS plist no longer redirects to Library/Logs; docs/hints updated.
- No portal surface in the impl slice unless a new ADR says so.
- Unit tests for rotate + tail-truncate helpers; Telegram parse reject.
- Four gates before merge of the **impl** PR. This ADR PR is docs-only.

## Consequences

- Docs-only first (this ADR). Tech lead assigns RustFox Rust Developer for impl after merge.
- Cross-platform single log path for `/logs`.
- Existing macOS Library/Logs files become stale; users must run a build that has the sink before `/logs` works.
