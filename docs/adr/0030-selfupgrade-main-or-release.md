# ADR-0030: `/selfupgrade` chooses main (source) or release (binary)

- **Status:** Accepted (grill Q1–Q5 locked 2026-10-18; **Accepted delta 2026-10-27 — stash-path + kill-all before re-register / #303**)
- **Date:** 2026-10-18
- **Target release:** v1.0.7
- **Depends on:** ADR-0018 (error surfacing / optional GitHub token), ADR-0046 (fox-home flock; kill-all belt)
- **Amends:** Telegram `/selfupgrade` UX only; reuses existing `learning::self_upgrade` / `UpgradeMode`
- **Locks:** PO lock 2026-10-18 — Telegram arg allowlist **A** (reject other); Product GO Option A 2026-10-27 — stash exe path + kill-all before re-register (#303)
- **Issue:** #303 (Accepted delta impl)

## Context

`learning::self_upgrade(branch, mode, …)` already supports:

- `mode = "source" | "release" | "auto"` (anything else → auto)
- `UpgradeMode::Source` — git fetch/checkout/pull + `cargo build --release` (and optional `cargo install` when running as a service); `branch` selects the git branch
- `UpgradeMode::Release` — download GitHub latest release binary via `self_update`; **`branch` is ignored**
- `auto` — `detect_deployment_mode()`: walk up from the exe for `Cargo.toml` → Source, else Release

Today Telegram `/selfupgrade [arg]` always passes `mode="auto"` and treats `arg` as a **git branch** (default `main`). There is no way for a user to say “download the latest release” vs “rebuild from git main” when they mean deployment path, not branch name.

Kan / Product asked for an explicit **main | release** choice on Telegram (and portal/CLI where an entry exists), without rewriting the upgrade engine.

## Decision

1. **Semantics (Q1 = A).** User-facing labels map to existing modes:
   - **`main`** → `UpgradeMode::Source` with git branch **`main`** (git pull + cargo build).
   - **`release`** → `UpgradeMode::Release` (GitHub latest release binary; overrides auto).
   - These are **deployment paths**, not arbitrary git branch names in the Telegram UX.

2. **Bare `/selfupgrade` (Q3 = A / Q5 = A).** No argument keeps **`mode=auto`**: if a source tree is detected (`Cargo.toml` near the exe), follow git (current source behaviour); otherwise take the latest release binary. Explicit `main` / `release` always override auto. UI may mark **release** as “recommended” for non-technical users without changing the bare default.

3. **Release install asking for main (Q2 = A).** If the deployment is Release (no source tree) and the user asks for **`main`** / source, **refuse** with a clear message that a source clone is required. Do **not** silently `git clone` a repo.

4. **Surfaces (Q4).** Same semantics everywhere an upgrade entry exists:
   - **Telegram:** `/selfupgrade main|release` (optional buttons). Bare = auto.
   - **Portal / CLI:** if an upgrade control exists (or is added), use the same `main` / `release` / bare-auto meanings. Portal visual work is owned by RustFox UI after this ADR is accepted and implementation is assigned.
   - **Built-in tool `self_upgrade`:** already has `mode` + `branch`; align docs and any Telegram parsing so `main`/`release` tokens are not mistaken for random branch names when that is the product intent.

5. **Telegram arg allowlist (PO lock A, 2026-10-18).** Telegram `/selfupgrade <arg>` accepts only:
   - bare (empty) → auto (per item 2);
   - `main` → source + branch `main` (per item 1);
   - `release` → release binary (per item 1).
   Any other arg (e.g. `develop`) is **rejected** with a clear tip that only `main` | `release` | bare are accepted. Do **not** silently treat unknown args as git branch names. Other git branches remain allowed only via the built-in tool with `mode=source` + `branch=…` (advanced), not via Telegram UX.

6. **Out of scope for this ADR’s implementation slice:** changing release asset naming; baking GitHub credentials; auto-cloning; rewriting `self_update` internals (beyond the refuse-on-no-source-tree path for `main`).


## Accepted delta 2026-10-27 (stash-path + kill-all before re-register)

Product GO **Option A** for [#303](https://github.com/chinkan/RustFox/issues/303) (release-channel `/selfupgrade` after `self_replace`):

1. **Stash absolute exe before replace.** Before any release binary `self_replace`, record the absolute path of the running executable.
2. **Reuse the stashed path after replace.** Service re-register (`rustfox install`) and foreground restart **must** spawn that stashed path. Do **not** call `current_exe()` again after replace for those steps — on Linux, `current_exe()` often points at a deleted inode and yields `ENOENT` (`Failed to run rustfox install: … error 2`) even when the replace itself succeeded.
3. **Stop + kill-all before re-register.** Before install / service re-register, stop the user service when doing so would not terminate this process, then kill leftover processes that share the same fox-home (`RUSTFOX_HOME` / `~/.rustfox`). This reuses the ADR-0046 ops belt (flock is not a replacement for stop + kill-all).
4. **Soft.** Soft must **not** treat this ENOENT / Telegram “Upgrade failed” path as pure soft-env benign. Fail Soft if Upgrade failed still appears for this cause after replace.
5. **No tag** from this delta alone; Soft required after merge; do not open v1.0.8 unless Product/TL separately decide.

Status stays **Accepted**. Do **not** open a new ADR for this delta.

## Non-goals

- Inventing a second upgrade engine.
- Changing ADR-0018 token / error-surfacing behaviour.
- Blocking #201 / v1.0.6.

## Acceptance criteria (implementation — later assign)

- Telegram: `/selfupgrade` → auto; `/selfupgrade main` → source+branch main (or refuse if no source tree); `/selfupgrade release` → GitHub latest binary.
- Telegram: unknown arg (not empty / `main` / `release`) → reject with a clear tip that only `main` | `release` | bare are accepted; no silent treat-as-branch.
- Optional Telegram buttons for main / release with the same semantics.
- Portal/CLI upgrade entry (if present) matches Telegram semantics; UI bot owns portal chrome.
- No silent clone when release binary users pick `main`.
- Regression tests cover parse mapping, unknown-arg reject, and the refuse path.
- English on GitHub; four gates before merge of the impl PR.

## Consequences

- Docs-only first (this ADR). Tech lead assigns Rust (and RustFox UI for portal) after merge.
- Source deployers who rely on bare `/selfupgrade` keep auto→git behaviour.
- Release-binary users get an explicit path to stay on GitHub releases without learning `mode=`.
- After the 2026-10-27 Accepted delta: release self-upgrade no longer re-resolves the exe via post-replace `current_exe()` for install/restart; Soft treats residual ENOENT Upgrade-failed as a real fail, not soft-env noise.

## Related

- ADR-0046 — fox-home single-instance flock; kill-all before upgrade remains required.
- #303 — post-`self_replace` stale `current_exe` → install ENOENT; Option A stash-path + kill-all.
