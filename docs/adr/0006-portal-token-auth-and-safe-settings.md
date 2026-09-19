# ADR 0006: Portal Auth via Static Bearer Token; Settings via Whitelisted Field PATCH

## Status
Accepted

## Date
2026-09-19

## Context
The portal exposes powerful operations (chat as the agent, config changes) and must never be an open port. Multi-user RBAC/SSO is on the blueprint but out of MVP scope. Editing `config.toml` through the UI is dangerous: it holds secrets (OpenRouter key, Telegram bot token, Gmail refresh token) and a typo bricks the bot.

## Decision
**Auth (MVP)** — static token, both-mode ready:
- `[portal] token_sha256`: SHA-256 of the chosen token, stored in config (never plaintext). If `token` is omitted, the server generates one at startup and logs it once.
- Login flow: `POST /api/auth/login {token}` → validates against the hash → sets an `HttpOnly` session cookie (in-memory `HashSet<token>` session store, server restart invalidates all) or accepts `Authorization: Bearer` for scripted clients.
- **Follow-up**: Telegram WebView auto-login (`initData` HMAC verification against bot token) — same session format, different credential exchange. No schema change needed.

**Settings editor** — never expose raw TOML:
- `GET /api/settings` returns a whitelisted, typed projection of config (`model`, `default_autonomy_mode`, `[portal] port`, `general.location`, provider base URLs…), secrets rendered masked (`"sk-or-…abcd"`, read-only).
- `PATCH /api/settings` accepts only whitelisted JSON fields; each maps to a concrete setter (`Agent::set_model` for model, `toml::Value` table edits for the rest). The server writes the new file only after **backup** (`config.toml.bak` next to it) and re-serializes through `toml::Value` (never string surgery).
- Soul/markdown files (SOUL.md, USER.md, AGENTS.md) are edited through a separate `/api/soul` document endpoint with plain-text write + `.bak` — no secrets live there.

## Rationale
- A single long-lived bearer token behind a private network (bind 127.0.0.1 / Tailscale) is proportionate for a personal assistant; the heavy machinery of JWT/RBAC can be added later without invalidating this (login endpoint is the seam).
- Field-level PATCH makes an invalid write structurally impossible (no free-form TOML), while the backup covers the remaining footgun (valid-but-wrong values → restart required).
- Hashing the token means `config.toml` (world-readable in some setups, synced in backups) leaks nothing usable.

## Consequences
- Session cookie without TLS is plaintext on the wire — acceptable **only** because MVP is loopback/private-network; the docs must say so loudly.
- Generated-token-on-startup survives restarts only if it's persisted; chosen deployment: log it and tell operators to set `token_sha256` for stable auth.
- Whitelist grows endpoint by endpoint; adding a config knob requires touching both the projection and PATCH map (accepted maintenance cost for safety).
- `set_model` persists to `[[provider]]`/`[openrouter]` and takes effect immediately; other edits report `restart_required: true`.
