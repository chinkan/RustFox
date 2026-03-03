# Design: RustFox Web UI — Chat + Config + Google OAuth

**Date:** 2026-03-03
**Status:** Approved
**Supersedes:** `2026-03-03-web-ui.md` (extends it with config page and Google OAuth)

---

## Goal

Extend the RustFox web UI (chat page already planned) with:

1. A **config editor page** (`/config`) that replaces the `src/bin/setup.rs` setup binary — viewable and editable while the bot is running.
2. A **Google Workspace OAuth flow** embedded in the config page, using Device Code flow (no redirect URI registration required).
3. A **setup-only startup mode** when `config.toml` is missing or incomplete — only the web server starts, Telegram bot does not.

---

## Startup Modes

### Setup-only mode

Triggered when `config.toml` is missing or fails to parse as a valid `Config`.

- Only the Axum web server starts (always on port 8080, or `RUSTFOX_SETUP_PORT` env).
- Telegram bot, scheduler, MCP connections, and Agent do **not** start.
- User visits `http://localhost:8080/config`, fills the form, saves → `config.toml` written.
- User restarts `cargo run --bin rustfox` to start normally.
- Log line: `"No valid config found — starting setup server on :8080"`.

### Normal mode

Triggered when `config.toml` is valid.

- Telegram bot, scheduler, Agent all start as now.
- If `[web] enabled = true` in config: Axum web server starts on the configured port (default 8080).
- Both chat and config pages are accessible.

---

## Routes

| Method | Path | Handler | Mode |
|--------|------|---------|------|
| `GET` | `/` | Chat page (Askama template) | Normal only |
| `GET` | `/config` | Config editor page (Askama template) | Both |
| `POST` | `/chat/send` | Start LLM request → `session_id` | Normal only |
| `GET` | `/chat/stream/:id` | SSE token stream | Normal only |
| `GET` | `/api/load-config` | Returns parsed `config.toml` as JSON | Both |
| `POST` | `/api/save-config` | Writes `config.toml` | Both |
| `POST` | `/api/google-auth/start` | Calls Google device-code endpoint | Both |
| `GET` | `/api/google-auth/poll/:device_code` | SSE: polls Google → emits token event | Both |

In setup-only mode, chat routes return 503.

---

## Config Page UX (`/config`)

Single-page form (Askama + HTMX + Alpine.js) with sections:

- **Telegram** — bot token, allowed user IDs (comma-separated)
- **OpenRouter** — API key, model, max tokens, system prompt (textarea)
- **Sandbox** — allowed directory path
- **Memory** — SQLite database path
- **General** — optional location string
- **MCP Servers** — dynamic list; each server has: name, command, args (space-separated), env (key=value pairs, add/remove rows)
- **Google Workspace quick-connect** — within the MCP servers section, a collapsible "Connect Google Workspace" sub-panel:
  - Fields: Client ID, Client Secret
  - Button: **Connect Google** → `POST /api/google-auth/start`
  - On success: shows verification URL as a clickable link + user code in a highlighted box
  - Connects SSE at `GET /api/google-auth/poll/:device_code` (via EventSource)
  - SSE `token` event → auto-fills `GOOGLE_WORKSPACE_REFRESH_TOKEN` in the MCP env rows; shows success banner
  - SSE `error` event → shows error + retry button
- **Save** button → `POST /api/save-config` → success: "Saved. Restart the bot to apply changes."

Load-on-open: `GET /api/load-config` → pre-populates all fields from existing `config.toml`.

---

## Google OAuth — Device Code Flow

Uses the same Device Code flow as the existing `--google-auth` CLI, now surfaced in-browser.

**Scopes:** Drive, Gmail (modify), Calendar, Docs, Sheets, Presentations (same as existing).

**`POST /api/google-auth/start`**

Request body:
```json
{ "client_id": "...", "client_secret": "..." }
```

Response:
```json
{ "device_code": "...", "user_code": "XXXX-XXXX", "verification_url": "https://...", "expires_in": 1800, "interval": 5 }
```

**`GET /api/google-auth/poll/:device_code`**

SSE stream. Handler accepts `client_id` and `client_secret` as query params.

Polls `https://oauth2.googleapis.com/token` every `interval` seconds. Emits:
- `event: pending` — still waiting (every poll)
- `event: token\ndata: <refresh_token>` — success, stream closes
- `event: error\ndata: <message>` — denied / expired / unexpected error, stream closes

**State:** No server-side state stored. The `device_code` is passed directly to the SSE handler. Token is not persisted by the server — the browser JS writes it into the form field; user saves via `/api/save-config`.

---

## Code Structure

```
src/
  web/
    mod.rs           # build_router() for normal mode; build_setup_router() for setup-only
    chat.rs          # GET /, POST /chat/send, GET /chat/stream/:id
    config_page.rs   # GET /config, GET /api/load-config, POST /api/save-config
                     # (config parsing/formatting logic moved from src/bin/setup.rs)
    google_auth.rs   # POST /api/google-auth/start, GET /api/google-auth/poll/:device_code
  main.rs            # detect mode, spawn web server
  bin/setup.rs       # DELETED

templates/
  base.html          # shared nav + layout
  chat.html          # chat UI
  config.html        # config editor form
```

### WebState

```rust
pub struct WebState {
    pub agent: Option<Arc<Agent>>,   // None in setup-only mode
    pub config_path: PathBuf,
}
```

Chat handlers return 503 when `agent` is `None`.

### Agent

Gains `pub web_tx: Arc<broadcast::Sender<(String, String)>>` for token streaming (as per existing plan Task 4).

---

## Deletion

`src/bin/setup.rs` is deleted. Its reusable logic (`parse_existing_config`, `format_config`, related structs) moves to `src/web/config_page.rs`. Its test coverage is preserved and extended.

`Cargo.toml` bin entry for `setup` is removed.

---

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Config parse error on load | `/api/load-config` returns empty/default values with a parse error message |
| Save write failure | `POST /api/save-config` returns HTTP 500 + JSON `{ok: false, error: "..."}` |
| Google auth timeout | SSE emits `event: error\ndata: Authorization timed out.` |
| Google auth denied | SSE emits `event: error\ndata: Authorization was denied.` |
| Chat request in setup-only mode | HTTP 503 `{"error": "Bot not running — complete setup first"}` |

---

## Non-Goals (out of scope)

- Web authentication / password protection (localhost-only assumed)
- Live config hot-reload without restart (save triggers restart message only)
- True LLM token-by-token streaming (chunked simulation only, as per existing plan)
- Static asset self-hosting (CDN scripts used for HTMX/Alpine.js)
