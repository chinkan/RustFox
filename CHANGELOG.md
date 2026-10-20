# Changelog

All notable changes to RustFox are documented here.
This project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- **Scheduler timeout transcript continue (ADR-0039, #240 Slice B):** on wall-clock timeout human-gate only, persist transcript checkpoint (messages + tool results + iteration + dangling tool ids) in a new SQLite table keyed by run/queue id; same Retry button → continue if checkpoint present (OpenClaw-style; dangling interrupted ≠ success / do not re-run), else full replay (ADR-0013 remains default until B lands). Not summary-only; not full Rust loop/future serialize. Iced: Temporal/LangGraph node checkpoint; mid-tool hard resume; timeout auto-retry; idempotent-tool registry (Should later). Slice A copy+inline is ADR-0038/#242 (separate). Docs-only; impl follows. Target **v1.0.7+**.
- **Scheduler human-gate copy + inline retry (ADR-0038, #240 Slice A):** clearer Telegram stop skeleton (readable duration e.g. 900s→15 minutes; did **not** retry automatically; side effects; Reply `/retry`/`/cancel`; `(id: …)`); same family for re-run timeout / iteration stall / failed twice; inline buttons primary (`rerun:retry:<uuid>` / `rerun:cancel:<uuid>`, ≤64 bytes) + text backup `/retry <id>` `/cancel <id>` only (no LLM matching); callback answers + edits message (remove buttons + Retried/Cancelled/Already done); idempotent Already done; keep human-gate (no timeout auto-retry); Retry/Cancel = full replay (ADR-0013). Transient hiccup / gate expired / Slice B resume / portal out. Target **v1.0.7+**.
- **Telegram `/fallback` chain edit (ADR-0037, #237):** allowlisted `/fallback` list + add + remove for `[fallback].chain`; persist disk + live memory immediately (no `/restart`); add must registry-resolve fully-qualified `provider/model` else reject; empty chain allowed; duplicates rejected; at `MAX_FALLBACK_CHAIN=5` reject add with clear cap message (no silent truncate on add); remove accepts 1-based index or exact `provider/model`; portal editor / reorder out; ADR-0012/0019 walk unchanged. Target **v1.0.7+**.
- **Live `current_model` + `/models` copy (ADR-0036, #236):** when `LoopConfig.model` is `None`, primary Telegram/portal chat injects live `current_model` into the loop (not boot-frozen `registry.default_qualified_model()`); sticky `Some(bots[]/persona)` unchanged (ADR-0033); Telegram success = Active now + saved, cold follows (no `/restart` for live); disk-fail live + not saved, cold may revert. Portal copy / #237 out. Target **v1.0.7+**.
- **`/models` Active persist + `default_provider` (ADR-0035, #233):** optional top-level `default_provider`; boot uses key when valid, missing → `providers[0]` compat, unknown name → fail boot; `/models` / `set_model` persist writes that provider `model` + `default_provider` via `persist_model_edit`; success Telegram `✅ … (saved)`; disk-fail still applies live + `⚠️ … live, but not saved: …`. Target **v1.0.7+**.
- **Primary Ollama no-user-query retry omit (ADR-0034, #226):** after ADR-0032 keep-tools, if primary Ollama returns `no user query found`, rewrite+omit tools and retry **once** on the same primary (journal WARN); then existing fallback (ADR-0029). First-pass keep-tools unchanged. Target **v1.0.7+**; does not block v1.0.6.
- **Agent model vs Active (ADR-0033, #223):** explicit agent/subagent `model` is sticky; shipped examples inherit Active; no-agent-model invoke follows Active (`current_model` / banner), not `[openrouter].model`. Agent primary Ollama uses shared ADR-0032 keep-tools; fallback → tool-weak uses shared ADR-0029 rewrite+omit. Target **v1.0.7+**; does not block v1.0.6.
- **Primary Ollama + tools (ADR-0032, #213):** shape primary Ollama chat payloads so a plain non-empty `user` turn is always present (keep tools on primary; mid-loop may rewrite). Reject unknown-prefix OpenRouter-style model ids (e.g. `deepseek/…`) when the resolved provider is Ollama — no 404-loop. ADR-0029 fallback rewrite/omit-tools unchanged.
- **Telegram `/logs` + in-app log file sink (ADR-0031):** bot writes `{resolved_home}/logs/rustfox.log` (32 MiB → `.1` rotation); allowlisted `/logs` sends the file (last 16 MiB if larger). macOS LaunchAgent no longer redirects to Library/Logs.
- **Telegram `/selfupgrade` labels (ADR-0030):** bare → auto; `main` → source+branch main (refuse clearly if no source tree); `release` → GitHub latest binary. Unknown args are rejected with a tip (not treated as git branches).
- **Portal Settings self-upgrade (ADR-0030):** API `POST /api/settings/self-upgrade` + Settings UI for `auto` | `main` | `release` (same mapping as Telegram); sync POST then restart; no SSE / branch picker.

### Changed
- **CLI service verbs (ADR-0028):** `rustfox install|uninstall|status|start|stop|restart` and `rustfox run` (foreground alias). `--service …` and `remove` still work with a deprecation hint (removed next minor). macOS `stop` unloads the agent so KeepAlive cannot respawn it; `restart` uses `launchctl kickstart -k`. Portal / wizard / `install.sh` teach the new verbs (no new portal control buttons).
- Explicit `mode=source` / Telegram `main` no longer falls back to `current_dir` when no `Cargo.toml` is near the executable (ADR-0030 Q2); auto mode behaviour unchanged.

### Fixed
- **Primary chat uses live Active after `/models` (ADR-0036, #236):** `LoopConfig.model = None` no longer falls through to boot registry via `llm.chat()`; injects live `current_model`. Sticky persona/bot models unchanged. Telegram `/models` copy clarifies now+saved / not-saved+cold-may-revert.
- **Portal hang under keep-tools + large toolset (#231):** re-wire existing `LoopDetector` into `AgenticLoop` (exact tool+args repetition → recovery nudge, then clear stop). Secondary: configurable LLM HTTP request timeout (`[agent] llm_request_timeout_secs`, default 180) + `llm_call start/done/timeout` journal markers. Soft: weather/large-toolset must finish or fail cleanly (no unbounded SSE-only / systemd restart). Does not reopen #229/#230; no ADR-0034 omit probe.
- **ADR-0034 soft-omit on openai-compat Ollama (#226):** tool-weak detection now includes `openai_compatible` Active named `ollama` or listening on `:11434` (live tip `25dc563` skipped omit-retry WARN). Deterministic `no user query found` 500s fail fast in ADR-0009 (no same-payload Rate/transient storm) so rewrite+omit can run. Does not block v1.0.6 (v1.0.7+).
- **Primary Ollama keep-tools 500 recovery (ADR-0034, #226):** when primary Ollama returns `no user query found` after ADR-0032 keep-tools, rewrite+omit tools and retry once on the same primary (WARN), then existing fallback. Does not block v1.0.6 (v1.0.7+).
- **Agent invoke follows Active model (ADR-0033, #223):** `run_subagent` no longer defaults to `[openrouter].model` when the agent has no `model` override — uses `current_model` (banner Active). Explicit agent/override `model` stays sticky. Does not block v1.0.6 (v1.0.7+).
- **Ollama fallback first turn + tools (ADR-0029 amend, #218):** walking onto tool-weak Ollama with tools present (including first turn + plain user) always rewrites and omits tools — never raw tools that 500 with `no user query found`. Log says “after rewrite” only when rewrite ran. Primary keep-tools (ADR-0032) unchanged.
- **Telegram online banner `Model:` (#217):** startup/online notify now shows `current_model` (same as `/models` Active), not `config.openrouter.model`.

## [1.0.6] — 2026-10-14

Bug-fix release on main after `v1.0.5`. No tag in this commit.

### Fixed

- **Windows plans with `:` in the title (ADR-0026, #197).** Plan files now use
  safe, encoded file names, so titles containing `:` work on Windows. Plans
  that earlier versions had hidden this way are recovered automatically the
  first time a plan tool runs. If a recovered plan has the same name as an
  existing plan, it is kept as "title (recovered)" and the existing plan is
  left untouched.
- **Case-only title clashes (ADR-0026, #197).** Plans whose titles differ only
  in upper or lower case no longer overwrite each other.
- **macOS sandbox mode (ADR-0025, #192, #196).** Sandbox mode now starts
  correctly on macOS.

### Known limitations

- Plans that were already overwritten by a case-only title clash in v1.0.4 or
  v1.0.5 cannot be recovered. This release only prevents it from happening
  again.
- Windows binaries in this release are not code-signed yet. On Windows with
  Smart App Control turned on, the unsigned `rustfox.exe` cannot be opened and
  there is no "Run anyway" option. Please wait for a signed release (#198); we
  do not recommend turning Smart App Control off. Advanced users can build
  RustFox themselves on a machine without Smart App Control.
- If Windows SmartScreen shows "Windows protected your PC", click "More info",
  then "Run anyway".

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
