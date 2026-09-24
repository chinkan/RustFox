# RustFox Web Portal — Implementation Context

> Living context document for the in-progress web portal work.
> Decisions live in `docs/adr/0004`–`0008`; this file records the agreed scope,
> source material, and domain language. Last updated: 2026-09-19.

## Goal
Turn the validated prototype at `web/` (React 19 + Vite + TanStack Router/Query, mock API) into a production portal served by RustFox itself: an opt-in embedded Axum server giving real chat (SSE), read-only agent/memory/task data, safe settings editing, and single-binary static hosting.

## Inputs
- Blueprint: Obsidian vault `ideas/incubating/rustfox-web-portal.md` (9 feature scopes, TanStack decision, 2026-09-18 stack update)
- Prototype: `web/` (vendored from `rustfox-portal/` @ 2026-09-18) — 10 routes, auth guard/RBAC demo, typed mock API contract (`src/api/types.ts`), 9 vitest tests green
- Backend anchors: `src/agent.rs` (`process_message` with `tool_event_tx`/`stream_token_tx`, `set_model`, `cancel_processing`), `src/memory/conversations.rs`, `src/scheduler/reminders.rs` (`ScheduledTaskStore`, `get_task_runs`), `src/setup/wizard.rs` (Axum pattern to reuse), `include_dir` already in deps

## Grill decisions (2026-09-19)
| Topic | Decision | ADR |
|---|---|---|
| MVP scope | read-only data pages + real chat streaming together; settings editing included; dashboard metrics minimal | — |
| Runtime topology | embedded Axum, opt-in `[portal] enabled` (default false), bind `127.0.0.1` | 0004 |
| Frontend | vendor prototype as-is into `web/`; Rust rewrite applies to backend only; `dist` embedded via `include_dir` | 0004 |
| Auth | static bearer token (`token_sha256`) now, Telegram WebView auto-login follow-up | 0006 |
| Sessions | web chat isolated: `platform="web"`, shared runtime/memory with Telegram; stateless signed cookie survives restart + SSE reconnect/reconcile | 0005, 0008 |
| Streaming | REST + SSE (`fetch`/ReadableStream); WebSocket deferred to live dashboard | 0005 |
| Settings | whitelisted field PATCH, masked secrets, `.bak` before write, `restart_required` flag; restart is manual (10.5a) but portal must reconnect after (0008) | 0006, 0008 |
| API contract | `docs/portal-api.md`, written before handlers | — |
| UI language | (8b) react-i18next scaffolding + English copy in MVP; zh-HK Cantonese locale file lands with M3 | — |
| PWA | keep manifest; offline service worker follow-up; access model: LAN bind + Tailscale remote + Telegram `/portal` link entry | 0007 |
| Testing | (10 = a+b both): axum in-memory handler tests + Playwright E2E on 5 critical journeys (Chromium desktop + mobile viewport) in CI; dogfooding gate per milestone | — |

## Domain language
- **Portal** — the whole web feature (`src/portal/`, `web/`, `/api/*`).
- **Portal session** — cookie/bearer identity for the admin UI; distinct from a **conversation** (agent chat thread keyed by `platform`+`user_id`).
- **Web session** — the chat identity `("web", <user_name>)`; one web user per deployment in MVP.
- **Projection** — the typed, masked, whitelisted view of `config.toml` returned by the API (never raw TOML).
- **Tray** — dashboard's compact task/system summary card (prototype concept, now backed by `ScheduledTaskStore`).

## Deferred (backlog, do not build yet)
Workspace switcher (multi-workspace model), memory/knowledge editing beyond search, tool sandbox testing, audit log, plugin system, multi-user RBAC/JWT, Telegram auto-login, live dashboard WebSocket, i18n (Cantonese UI), PWA offline, MCP server exposure of portal APIs.
