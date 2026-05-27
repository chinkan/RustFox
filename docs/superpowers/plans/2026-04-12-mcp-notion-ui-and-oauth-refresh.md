# Notion MCP UI + OAuth Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Streamline the setup wizard Notion MCP UX (OAuth-only token, no redundant guide modal) and implement **persistent refresh-token rotation** with **automatic access-token refresh** so the bot can run indefinitely without manual re-auth, aligned with [Notion MCP: Step 8 — Handle token refresh](https://developers.notion.com/guides/mcp/build-mcp-client).

**Architecture:** Extend `McpServerConfig` with optional OAuth fields used only for HTTP MCP servers that completed Notion’s DCR + authorization code flow. The setup wizard persists `auth_token`, `oauth_refresh_token`, `oauth_client_id`, `oauth_client_secret` (if any), and `oauth_access_expires_at` (derived from `expires_in` at callback time). Extract shared **discovery + token refresh** HTTP logic into `src/notion_oauth.rs` and include it from both `main` and `setup` binaries via `#[path = "../notion_oauth.rs"] mod notion_oauth;` in `setup.rs` and `mod notion_oauth;` in the primary binary root. `McpManager` resolves a fresh access token before connecting (refresh when expired or within a short skew window), then writes updated secrets back to `config.toml` using **`toml_edit`** so comments and unrelated sections are preserved. Optional: lightweight periodic refresh task if the process stays up past the access-token lifetime.

**Tech Stack:** Rust (`src/config.rs`, `src/mcp.rs`, `src/bin/setup.rs`, `setup/index.html`), `reqwest`, `toml` / `toml_edit`, Notion OAuth (RFC 9470 + 8414 + PKCE + refresh grant).

**Design decisions (brainstorming summary):**

| Topic | Options | Recommendation |
|-------|---------|------------------|
| Token field UX | Readonly vs editable for power users | **Readonly** — user asked OAuth-only; document that advanced users may edit `config.toml` by hand if needed. |
| Remove guide | Modal + guide button vs one-line doc link | Remove **modal** and **“Notion Setup Guide”** button; keep a **single** short line + link to official docs on the card (not a separate wizard). |
| Persist OAuth | Sidecar JSON vs extra TOML keys | **Extra optional keys** on `[[mcp_servers]]` for the `notion` entry — single file, matches “save to config”, easier backup story. |
| Config rewrite | Full `Serialize` of `Config` vs surgical edit | **`toml_edit`** surgical update of the relevant `[[mcp_servers]]` row — avoids deriving `Serialize` for the entire config and preserves user comments/order where possible. |
| Shared OAuth code | Duplicate vs `lib.rs` vs `#[path]` module | **`src/notion_oauth.rs` + `#[path]` in setup binary** — minimal churn; no full crate lib split required. |
| When to refresh | Only at startup vs on 401 vs periodic | **At MCP connect time** if `oauth_refresh_token` present and access token missing/expired/near expiry; **optional** `tokio::interval` (e.g. 15–30 min) to refresh proactively for long uptime. Retry **once** after refresh on `invalid_token` if feasible without deep `rmcp` changes (else document “restart bot” as fallback). |

---

## References

- [Integrating your own MCP client](https://developers.notion.com/guides/mcp/build-mcp-client) — Steps 6–8 (tokens, Bearer, refresh, rotation).
- Prior notes: `docs/superpowers/plans/2026-04-12-notion-mcp-invalid-token-root-cause-and-fix.md` (Task 4).
- Internal OAuth details: `docs/superpowers/plans/2026-04-12-notion-mcp-oauth.md` (deduplicate overlapping work).

---

### Task 1: Config schema — OAuth fields on `McpServerConfig`

**Files:**

- Modify: [`src/config.rs`](../../../src/config.rs) — `McpServerConfig`
- Modify: [`config.example.toml`](../../../config.example.toml) — commented Notion example

- [ ] **Step 1:** Add optional fields (all `Option<String>` or appropriate types), `#[serde(default)]`:

  - `oauth_refresh_token` — refresh token from token endpoint (rotate when refreshed).
  - `oauth_client_id` — from dynamic client registration (required for refresh grant).
  - `oauth_client_secret` — optional; include when registration returned a secret.
  - `oauth_access_expires_at` — store as **Unix timestamp** (`i64` or `u64`) when access token expires; compute at OAuth callback as `now + expires_in` when `expires_in` present, else `None` (force refresh-on-next-connect or conservative behaviour).

- [ ] **Step 2:** Add unit test in `config.rs` `#[cfg(test)]` parsing a TOML snippet with `[[mcp_servers]]` `name = "notion"` including the new keys.

- [ ] **Step 3:** Run `cargo test` and `cargo clippy -- -D warnings`.

---

### Task 2: Shared module `src/notion_oauth.rs` (discovery + refresh)

**Files:**

- Create: [`src/notion_oauth.rs`](../../../src/notion_oauth.rs)
- Modify: [`src/main.rs`](../../../src/main.rs) — `mod notion_oauth;`

- [ ] **Step 1:** Move or reimplement (without behaviour change) from [`src/bin/setup.rs`](../../../src/bin/setup.rs):

  - HTTP discovery: protected resource metadata → authorization server metadata (`authorization_endpoint`, `token_endpoint`, `registration_endpoint`, `scopes_supported`).
  - `refresh_access_token(http, token_endpoint, client_id, client_secret, refresh_token) -> Result<TokenRefreshResponse>` where `TokenRefreshResponse` contains at least `access_token`, `refresh_token` (optional — use prior if omitted per RFC), `expires_in` (optional).

- [ ] **Step 2:** Map Notion Step 8: `grant_type=refresh_token`, form body fields per official doc; on `invalid_grant` return a distinct error so callers can log “re-run Connect Notion”.

- [ ] **Step 3:** Unit tests with `mockito` or `wiremock` **if** already in tree; else `#[cfg(test)]` with a stub — if no HTTP mock dep, test pure URL parsing/helpers only; **do not** add heavy deps without justification.

- [ ] **Step 4:** `cargo test` / `clippy`.

---

### Task 3: Wire `notion_oauth` into setup binary + persist full OAuth row

**Files:**

- Modify: [`src/bin/setup.rs`](../../../src/bin/setup.rs) — top: `#[path = "../notion_oauth.rs"] mod notion_oauth;` then delete duplicated discovery structs/functions now living in `notion_oauth.rs` (keep setup-specific `PendingNotionOAuth`, Axum handlers, HTML callback).

- [ ] **Step 1:** Callback handler: after successful token exchange, pass `client_id`, `client_secret`, `access_token`, `refresh_token`, `expires_in` to the HTML/JSON payload for the wizard. Extend `oauth_callback_html_ok` JSON payload to include `client_id`, `client_secret` (if any), `expires_in` (already partially there).

- [ ] **Step 2:** [`setup/index.html`](../../../setup/index.html) — extend `postMessage` handler to stash `oauth_client_id`, `oauth_client_secret`, `oauth_refresh_token`, and compute/store expiry in `state` (e.g. `state.mcp_selections.notion.oauthExpiresAt` as ms timestamp).

- [ ] **Step 3:** [`generateToml()`](../../../setup/index.html) — for catalog tool `notion`, emit new TOML keys under `[[mcp_servers]]` when values exist (escape with existing `esc()`).

- [ ] **Step 4:** [`loadExistingConfig`](../../../setup/index.html) / [`parse_existing_config`](../../../src/bin/setup.rs) — round-trip `ExistingMcpServer` in setup.rs to include new fields; merge into wizard state on load.

- [ ] **Step 5:** Setup tests in `setup.rs` for parse TOML containing new fields.

---

### Task 4: Notion MCP wizard UI — simplify + readonly token

**Files:**

- Modify: [`setup/index.html`](../../../setup/index.html)

- [ ] **Step 1:** Remove the **Notion MCP Setup Modal** block (`#notion-modal` and related CSS if unused).

- [ ] **Step 2:** Remove `openNotionModal`, `closeNotionModal`, `__NOTION_GUIDE_BUTTON__` branch, and any `onclick` that opened the modal.

- [ ] **Step 3:** On the Notion card: one concise line, e.g. “Sign in with Notion via OAuth — [Get started ↗](https://developers.notion.com/guides/mcp/get-started-with-mcp) · [Client integration ↗](https://developers.notion.com/guides/mcp/build-mcp-client)”. No separate guide modal.

- [ ] **Step 4:** Token `<input>`: set `readonly` attribute for `NOTION_TOKEN`; remove `oninput` that called `setEnv` for manual typing — only `setEnv` from OAuth `postMessage` and `disconnectNotionOAuth` updates state. Style readonly (e.g. muted background) to match UX.

- [ ] **Step 5:** Keep **Connect Notion** / **Disconnect**; ensure previous fix remains: **do not** use `noopener` on `window.open` for OAuth popup.

- [ ] **Step 6:** Adjust `looksLikeNotionIntegrationSecret` / validation: still useful when **loading** old configs; readonly prevents new pastes.

- [ ] **Step 7:** Manual browser check: load wizard, Connect Notion, verify field fills + TOML preview contains OAuth fields.

---

### Task 5: `toml_edit` — update Notion MCP block after refresh

**Files:**

- Modify: [`Cargo.toml`](../../../Cargo.toml) — add `toml_edit = "0.22"` (or current compatible).

- Create or modify: e.g. [`src/config_toml_patch.rs`](../../../src/config_toml_patch.rs) or `src/config.rs` — `pub fn update_mcp_server_oauth_in_file(path: &Path, server_name: &str, patch: &OauthTokenPatch) -> Result<()>`.

- [ ] **Step 1:** Implement read file → parse as `toml_edit::DocumentMut` → locate `[[mcp_servers]]` where `name == "notion"` (or passed `server_name`) → set `auth_token`, `oauth_refresh_token`, `oauth_client_id`, `oauth_client_secret`, `oauth_access_expires_at`.

- [ ] **Step 2:** Write atomically: temp file in same dir + `rename` (pattern already used elsewhere if present; else standard `fs::write` to temp + rename).

- [ ] **Step 3:** Unit test with a string constant TOML fixture in `config.rs` or new test module.

---

### Task 6: `McpManager` — refresh then connect + persist

**Files:**

- Modify: [`src/mcp.rs`](../../../src/mcp.rs)
- Modify: [`src/main.rs`](../../../src/main.rs) — pass `config_path` into MCP layer

- [ ] **Step 1:** Extend `McpManager::new` or `connect_all` to accept `config_path: PathBuf` (or `Option<PathBuf>` — `None` skips persist for tests).

- [ ] **Step 2:** For each `McpServerConfig` with `url` set and `oauth_refresh_token` + `oauth_client_id` present, and access token expired or near expiry (compare `oauth_access_expires_at` with `SystemTime` + 5 min skew), call `notion_oauth::refresh_access_token`, build updated `auth_token`, then patch TOML via Task 5 helper, then connect with new bearer.

- [ ] **Step 3:** If no refresh fields (legacy config with only `auth_token`), connect as today.

- [ ] **Step 4:** Log success/failure at `info!`/`error!`; on refresh failure with `invalid_grant`, log clear “run setup Connect Notion again”.

- [ ] **Step 5:** `cargo test`, `clippy`.

---

### Task 7 (optional but recommended): Proactive refresh while bot runs

**Files:**

- Modify: [`src/main.rs`](../../../src/main.rs) or [`src/mcp.rs`](../../../src/mcp.rs)

- [ ] **Step 1:** If `oauth_refresh_token` present, spawn `tokio::spawn` interval (e.g. every 20–30 minutes): if token expires within next 15 minutes, refresh and patch TOML (same as Task 6).

- [ ] **Step 2:** Use `tokio::sync::Mutex` or single-flight guard so concurrent refresh never runs twice.

---

## Self-review (writing-plans)

| Requirement | Task |
|-------------|------|
| Better Notion setup UI, remove guide modal, readonly token | Task 4 |
| Refresh token + persist + auto refresh “for lifetime” | Tasks 1–3, 5–7 |
| Align with Notion build-mcp-client Step 8 | Task 2 (refresh grant), Task 6–7 |
| No placeholder-only steps | Checked — file paths and behaviours explicit |

---

## Execution handoff

**Plan saved to:** `docs/superpowers/plans/2026-04-12-mcp-notion-ui-and-oauth-refresh.md`

**Two execution options:**

1. **Subagent-driven (recommended)** — One subagent per task; review between tasks. **Skill:** `superpowers:subagent-driven-development`.

2. **Inline** — Same session, checkpoints. **Skill:** `superpowers:executing-plans`.

**Which approach?**
