# Changelog

All notable changes to RustFox are documented here.
This project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.0.5] — 2026-10-13

Release candidate on main after `v1.0.4`. Headline: sandbox / trusted mode
(ADR-0025). No tag in this commit.

### Added

- **Sandbox / trusted mode (ADR-0025, #186, #191).** `execute_command` runs
  confined to the workspace: bubblewrap on Linux, Seatbelt on macOS. Windows
  refuses sandbox mode and defaults to trusted. Choose the mode in the setup
  wizard or switch it in the portal. If the sandbox cannot start, the command
  fails closed instead of running unconfined.
- **Mid-run steering (ADR-0020, #170, #171).** Messages sent while a turn is
  running steer that turn; scheduled runs go through the same turn gate.
- **Scheduler run timeout (ADR-0024, #175).** Scheduled runs have a wall-clock
  timeout, so one hung run no longer blocks the scheduler. Stale `running`
  rows are reconciled.
- OpenRouter: type any model id in setup; the API key is sealed in the secret
  store (#140, #141).

### Changed

- **Schedule timezone (ADR-0022, #166).** Cron and naive one-shots evaluate in a
  per-task IANA timezone (default: system local, optional `[general] timezone`).
  Config crons (`summarize_cron`, `user_model_cron`, heartbeat) use the effective
  default timezone (e.g. nightly summarisation at 02:00 local, not UTC). Legacy
  recurring rows keep `UTC`; legacy one-shots keep the system zone at migration.
- Five-field cron expressions are accepted (seconds default to 0) (#167).
- LLM fallback applies to any failure; context compaction no longer trims
  skills (ADR-0019, #148, #150).
- Workspace version bumped to `1.0.5`.

### Fixed

- **File tools no longer follow symlinks out of the workspace (#188, #193).**
  `read_file`, `write_file`, `list_files` and `send_file` open with no-follow
  semantics; on Windows, symlinks and junctions (reparse points) are rejected.
  Subagent `tools_override` is intersected with the parent's tools (#188).
  Closes #189.
- Subagents declared with block-style `tools:` in `AGENT.md` now get their
  sandbox read tools (#173).
- Portal tasks and reminders reach the right bot and the owner's Telegram DM
  (#162, #164).
- Setup and secrets: Linux always uses the file vault, wizard secrets land in
  the vault the bot opens, `config.toml.bak` is owner-only, WSL opens the Windows
  browser (#137, #139, #142, #143, #157, #160).
- Cancelled and max-iterations replies are saved to history; `self_upgrade`
  errors are surfaced; duplicate tool names are deduped; skill catalog
  descriptions are capped; `patch_skill` defaults to append (#144, #146, #149,
  #151, #153).

### Dependencies

- `reqwest` 0.12 → 0.13.4 (#134), `toml` 0.8 → 1.1.6 (#131), `croner` 2 → 4
  (#132), `tokio-cron-scheduler` 0.13 → 0.15.1 (#135), `pdf-extract` 0.10 →
  0.12.1 (#129).
- Web: `typescript` 5.9.3 → 7.0.2 (#133) and a minor/patch group update (#130).
- `rustls` and `source-map-js` lockfile bumps to clear Dependabot alerts (#161).

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
