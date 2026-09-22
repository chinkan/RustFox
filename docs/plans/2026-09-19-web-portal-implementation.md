# Web Portal Implementation Plan

Date: 2026-09-19 · Branch: `feat/web-portal-impl` · ADRs: 0004–0008 · Contract: `docs/portal-api.md`

## Milestones

### M1 — Vendored frontend compiles against real API shape (this PR)
- [x] Copy `rustfox-portal/` → `web/` (no node_modules/dist), commit
- [ ] `web/src/api/client.ts`: replace mock `delay()` with `fetch` against `/api/*`; same exported `api` object + types so **zero component changes**
- [ ] `useChatStream.ts`: `fetch` POST + ReadableStream SSE parser (token/tool/done/error frames); drop `useWebSocket.ts` from chat
- [ ] SSE reconnect state machine (ADR 0008B): bootId watch + banner + history reconcile; Vitest with dropped-stream mock
- [ ] Login screen posts token → signed cookie (ADR 0008A); `authStore` backed by `/api/auth/me`
- [ ] i18n scaffolding (grill 8b): react-i18next wired, all copy into `en.json`; components unchanged beyond `t()` swap — zh-HK lands in M3
- [ ] CI: `web` job runs `npm run verify` (tsc + build + vitest)

### M2 — Backend `src/portal/` (this PR) ✅ complete 2026-09-22
- [x] `config.rs`: `PortalConfig` (`enabled=false`, `port=8090`, `bind="127.0.0.1"`, `token`, `token_sha256`, `user_name="web"`)
- [x] `portal/mod.rs`: `serve(Arc<PortalState>)` — Axum Router mirroring wizard pattern + graceful shutdown
- [x] `auth.rs`: sha256 compare (constant-time), **stateless HMAC session cookie + `portal_secret.key` + SQLite `kv` gen counter** (ADR 0008A), middleware extractor
- [x] `chat.rs`: SSE via `tokio::sync::mpsc` bridged from `process_message` channels; 409 busy guard; cancel endpoint
- [x] `data.rs`: agents/skills/memory/tasks/health/stats read-only handlers; **`GET /api/health` public with bootId** (ADR 0008B)
- [x] `settings.rs`: GET projection (masked secrets) + PATCH whitelist + `.bak`; `/api/soul` GET/PUT; `restart_required` + sticky banner support
- [x] `static_serve.rs`: `include_dir!("web/dist")` + SPA fallback
- [x] `main.rs`: spawn when `config.portal.enabled`, wire into shutdown select
- [x] Telegram `/portal` command → reply LAN + tailnet (best-effort `tailscale ip -4`) URLs (ADR 0007)
- [x] `docs/portal-access.md` runbook: LAN bind, Tailscale setup, WebView tips (ADR 0007)
- [x] Tests: in-memory `tower::ServiceExt` per router group (auth gate 401s, cookie sign/verify + gen-bump invalidation, settings PATCH whitelist, chat 409/cancel, tasks mapping)

### M3 — Access + polish (separate PRs, ordered)
- [ ] **Telegram WebView auto-login** (initData HMAC) — promoted from backlog: daily out-and-about friction (ADR 0007)
- [ ] `zh-HK` Cantonese locale file + language toggle (grill 8b)
- [ ] Playwright E2E suite in CI: 5 journeys × (Chromium desktop + Pixel mobile viewport) — login → chat stream → dashboard real data → settings PATCH + restart banner → logout (grill 10a)
- [ ] WebSocket live dashboard push; memory/knowledge editing; PWA service worker; token rotation UI

### M4 — Decision layer (parked, see vault idea notes)
- NanoJev-style local router spike (RustFox telemetry → labels → Qwen3-0.6B heads) — only after portal ships; portal gets a "Context Health" demo screen as its natural seat.

## Verification gates (grill 10: a+b both)
1. `cargo test -p rustfox` green (portal handlers with real temp-dir config + in-memory `MemoryStore::open_in_memory`)
2. `cd web && npm run verify` green
3. Playwright: 5 journeys green on desktop + mobile viewport
4. **Dogfooding gate**: each milestone only "done" when Kan has used it from a phone (LAN + out-of-home via Tailscale) and had one real chat + one settings change round-trip, restart included
5. Post-restart: browser tab reconnects to login-free session within one poll cycle (ADR 0008 end-to-end check)
