# ADR-0046: Register start-time single-instance flock and safe `--version`

- **Status:** Proposed
- **Date:** 2026-10-27
- **Related:** ADR-0030 (self-upgrade out of scope), ADR-0028 (CLI service verbs; must stay unlock-friendly), live tip `7ddd3db`
- **Issue:** this docs PR **Refs #289**; later impl PR **Closes #289**

## Context

After a WSL upgrade/restart, multiple `rustfox` processes can share the same fox-home (`RUSTFOX_HOME` / `~/.rustfox`) and each run an in-process scheduler (triple schedulers observed). There is no exclusive lock on bot start today. `systemctl --user restart rustfox` only restarts the user unit and does not clear stray bare/foreground instances.

Ops notes already product-locked (2026-10-27): before install / `cargo install` / upgrade, stop the service **and** kill all strays sharing the same home. Do not use bare `rustfox` or `rustfox --version` as a probe today: `--version` is not implemented; unknown flags fall through to full bot+scheduler start.

Self-upgrade-from-main already exists (ADR-0030 source mode). This issue is **not** about changing self-upgrade. Multi-bot-in-one-process via `[[bots]]` remains supported and is out of scope.

This document records the product locks for a start-time exclusive flock under the fox-home and a safe `--version` that never starts the bot. It does not implement Rust, tests, or a release tag.

## Decision

Product locks are authoritative for the first implementable slice after this ADR is on `main` and the usual four gates pass:

1. **Exclusive flock on normal bot start:** On bare / `run` / systemd `ExecStart` (normal bot start), acquire an exclusive **flock** on a lock file under `RUSTFOX_HOME` (e.g. `rustfox.lock`). Prefer flock over a pidfile — pidfiles have a stale-pid footgun. Hold the lock for the life of the bot process.

2. **Fail-fast on second start (same home):** If the lock is already held, the second start with the same fox-home **fails fast** with a clear error (no silent second scheduler).

3. **Different homes OK:** Different `RUSTFOX_HOME` values → both processes allowed (one lock per home).

4. **Service / setup verbs unlock-friendly:** `status` | `restart` | `stop` | `install` and `--setup` must **not** take (or must only briefly take) the bot lock so they still work while a bot is running or restarting.

5. **Safe `--version`:** Add `--version` that prints `CARGO_PKG_VERSION` and exits **0** with **no** bot/scheduler start, so probes never spawn a second instance via mistyped flags.

6. **Ops belt, not replacement:** Document that kill-all strays before upgrade remains required. Flock is a **belt**, not a replacement for stop + kill-all before install/upgrade.

7. **Sequencing:** this docs PR **Refs #289** → four gates squash → impl PR **Closes #289** → soft QA/QA2 only. **No tag.** Park #194 / #202.

## Non-goals

- Self-upgrade / ADR-0030 rewrite (`/selfupgrade main|release|bare-auto`).
- `[[bots]]` multi-bot-in-one-process (already supported; unchanged).
- Auto-clone for release binaries that want tip-of-main.
- Any Rust / tests / runtime change in this docs PR.
- A release tag from this docs change or the later impl PR without owner.
- #194 / #202.

## Consequences

- One fox-home → at most one live bot+scheduler process under normal start paths; dual-scheduler after WSL restart is fail-fast instead of silent.
- Operators can still run multiple fox-homes side by side.
- Service verbs and `--setup` remain usable without fighting the bot lock.
- `--version` becomes a safe probe; unknown-flag fall-through to full start is a separate hygiene concern for the impl to keep in mind when wiring early CLI exits.
- Kill-all-before-upgrade stays documented; flock reduces accidental dual start but does not replace stop+kill discipline.
- Tracking stays on #289: this PR Refs; later impl PR Closes after four gates. Soft QA/QA2 only after impl; no tag. #194 / #202 remain parked.

## Related

- ADR-0028 — CLI service verbs (`status` / `restart` / `stop` / `install` / `run`); must remain unlock-friendly relative to the bot flock.
- ADR-0030 — self-upgrade; explicitly out of scope for #289.
- #289 — tracking. This docs PR Refs; later impl PR Closes #289 after four gates. Soft QA/QA2 only; no tag.
- Live tip `7ddd3db` — product-lock GO base (TL + Researcher aligned 2026-10-27).
- #194 / #202 — parked; out of scope.
