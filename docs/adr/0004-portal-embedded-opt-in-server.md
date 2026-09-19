# ADR 0004: Web Portal as an Opt-In Embedded Axum Server

## Status
Accepted

## Date
2026-09-19

## Context
RustFox needs a web portal (chat + agent admin UI). The runtime is a single long-running process (Telegram bot + scheduler + supervisor); Axum 0.8 is already a dependency (setup wizard). Deployment target stays "one binary, one SQLite" on a home VPS, primarily reached via a Tailscale-style trusted network. Three options were considered:

- (a) embedded Axum server in the main runtime, always on
- (b) separate portal process + IPC (Unix socket / SQLite queue)
- (c) embedded Axum server, opt-in via `[portal] enabled`

## Decision
Go with **(c)**: an embedded Axum router (`src/portal/`) spawned from `main.rs` only when `[portal] enabled = true` (default `false`). Handlers share the process with the `Agent`, `MemoryStore`, and `ScheduledTaskStore` via `Arc` — no IPC layer.

The built React SPA (`web/dist`) is embedded into the binary with `include_dir` and served by the same router, preserving the single-binary deploy: `cargo build --release && rustfox` serves Telegram, scheduler, and portal together.

## Rationale
- Chat portal answers must reuse the full `Agent::process_message` pipeline (memory, tools, MCP, skills, conversation persistence). Only an in-process embedding gives that without duplicating the runtime.
- A portal crash cannot realistically take down the bot: Axum handlers are isolated per-connection tasks; the shared state is already `Arc`+`Mutex`/`RwLock` and is exercised by Telegram/scheduler paths anyway.
- Separate-process (b) doubles complexity (protocol, auth handoff, lifecycle) for a single-user home deployment.
- Opt-in default-off keeps existing installs, CI, and headless bots unaffected.

## Consequences
- `Config` gains a `#[serde(default)] PortalConfig { enabled, port, bind, token_sha256, user_name }` section; absent key = disabled.
- The portal is only as network-exposed as the operator makes it; bind defaults to `127.0.0.1`. Public exposure (TLS/reverse proxy) is explicitly out of MVP scope.
- `web/dist` must exist at compile time (placeholder `.gitkeep` committed); a binary built without the frontend logs a warning and serves API-only.
- Setup wizard Axum usage and portal Axum usage can coexist (wizard runs before the main runtime).
