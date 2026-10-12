# Changelog

All notable changes to RustFox are documented here.
This project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- **Schedule timezone (ADR-0022 / #166).** Cron and naive one-shots evaluate in a
  per-task IANA timezone (default: system local, optional `[general] timezone`).
  Config crons (`summarize_cron`, `user_model_cron`, heartbeat) now use the
  effective default timezone (e.g. nightly summarisation at 02:00 local, not UTC).
  Legacy recurring rows keep `UTC`; legacy one-shots keep the system zone at migration.

## [1.0.4] — 2026-10-04

Release candidate on main after `v1.0.3`. Bumps the workspace version users see
(`CARGO_PKG_VERSION`) ahead of the tag. No tag in this commit.

### Added

- **Per-bot schedules.** Every schedule row has a required `bot_id`. Runs use
  that bot's prompt and tools; portal and list views stay on the current bot.
- **Schedule results in chat.** Prompt and result (including failure, cancel,
  and max-iterations) are written with a `[schedule:<id>]` marker. Portal-origin
  runs also land in the owning bot's Telegram conversation.
- **Thin setup.** First-run asks only four fields; Ollama is the local option.
- **One-tap Google MCP.** Settings Google control for Gmail readonly+compose
  (desktop OAuth client id baked at release; source builds hide or use Advanced).
- **Fully silent tool calls.** Opt-in `fully_silent` on `[[bots]]` so that bot's
  Telegram turn posts no tool UI; the final reply still sends.
- **PDF page vision and OCR.** Vision only on retrieved PDF pages; image OCR
  pinned to PP-OCRv4 chinese_cht.
- **Portal memory browse.** Conversations browse with fact vs knowledge split.

### Fixed

- Multi-bot tool-call UI routes to the owning bot.
- LangSmith child `tool` and `llm_call` runs are recorded on the same
  `rustfox_request` chain (no client swap).
- Scheduler listing, `next_run_at`, and related owner-scope fixes from the
  post-1.0.3 patch set.

### Changed

- Workspace version bumped to `1.0.4`.

## [1.0.3] — 2026-09-29

Security hardening release. Clears all open Dependabot and CodeQL
code-scanning alerts, and upgrades the MCP client to a patched major.

### Security

- **MCP client (`rmcp`) upgraded `0.15` → `2.2`.** Resolves all four open
  Dependabot alerts (GHSA-9pj6-vhgr-3mwh, GHSA-33f5-2c5q-wgwj,
  GHSA-89vp-x53w-74fx, GHSA-9g45-5xwm-f3wc). Adapted the two call sites in
  `src/mcp.rs` to the 2.x API: `CallToolRequestParams` is now
  `#[non_exhaustive]` (built via `::new(..).with_arguments(..)`), and tool
  result content is matched through the `ContentBlock` enum instead of the
  removed `raw` field.
- **SecretStore file backend.** Key and nonce are now drawn directly from the
  OS CSPRNG (`rand::rngs::OsRng.gen()`) instead of filling a zero-initialised
  buffer, and the on-disk key is converted with `try_from` without
  intermediate constant-initialised arrays. No constant is ever used as key
  material. (`rust/hard-coded-cryptographic-value`)
- **Cleartext-logging hardening.** Removed sensitive identifiers and ambiguous
  credential wording from log/format sinks in the setup wizard, conversation
  tests, and secret-store notify tests. (`rust/cleartext-logging`)
- **CI least privilege.** Added explicit `permissions: contents: read` to the
  `ci`, `check-compile`, and `release` (build job) workflows.
  (`actions/missing-workflow-permissions`)

### Changed

- Workspace version bumped to `1.0.3`.

### Fixed

- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and the full test
  suite (792 tests across 18 suites) pass with the upgraded dependency set.

## [1.0.2] — 2026-09-28

- Web portal Control Plane: skills/agents CRUD, GitHub installer, task CRUD.
- 429 resilience: model fallback chain and dead-letter re-run queue.

[1.0.4]: https://github.com/chinkan/RustFox/releases/tag/v1.0.4
[1.0.3]: https://github.com/chinkan/RustFox/releases/tag/v1.0.3
[1.0.2]: https://github.com/chinkan/RustFox/releases/tag/v1.0.2
