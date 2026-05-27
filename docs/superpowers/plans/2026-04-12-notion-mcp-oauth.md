# Notion MCP `invalid_token` / AuthRequired Fix

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate misleading configuration and implement **OAuth 2.0 Authorization Code + PKCE** so users obtain a valid **Notion MCP access token** during setup and RustFox can connect to `https://mcp.notion.com/mcp` without `AuthRequired` / `invalid_token` failures.

**Architecture:** Notion’s hosted server requires OAuth (not an internal integration secret) per [Connecting to Notion MCP](https://developers.notion.com/guides/mcp/get-started-with-mcp) and [Integrating your own MCP client](https://developers.notion.com/guides/mcp/build-mcp-client). RustFox already embeds the setup SPA from [`setup/index.html`](../../../setup/index.html) via [`src/bin/setup.rs`](../../../src/bin/setup.rs) (Axum on a local port, default **8719**). The **primary delivery path** is: extend that setup binary with OAuth discovery + PKCE endpoints; update the **Notion modal and Step 5 UI** so the user clicks **Sign in with Notion**, completes OAuth in a browser window, and the callback delivers the **access token** (and optionally **refresh token**) into the form so generated `config.toml` contains `url = "https://mcp.notion.com/mcp"` and `auth_token = "<access_token>"` (raw string, no `Bearer ` prefix). The main bot’s [`src/mcp.rs`](../../../src/mcp.rs) continues to pass that value to `StreamableHttpClientTransportConfig::auth_header` ([rmcp docs](https://docs.rs/rmcp/latest/rmcp/transport/streamable_http_client/struct.StreamableHttpClientTransportConfig.html)). A **follow-up** task adds **token refresh** at runtime so expired access tokens do not break the bot without re-running setup.

**Tech Stack:** Rust (`src/bin/setup.rs`, `src/mcp.rs`), Axum, `oauth2` + PKCE (`PkceCodeChallenge`), optional `reqwest` for discovery HTTP GETs, vanilla JS in `setup/index.html`, Notion MCP docs.

---

## Root cause (from logs and docs)

Log excerpt:

- `AuthRequired` / `www_authenticate_header: "Bearer realm=\"OAuth\", error=\"invalid_token\""` during `initialize` on `https://mcp.notion.com/mcp`.

Interpretation:

1. **Notion MCP** requires user OAuth; custom clients must implement discovery + PKCE + refresh per [build-mcp-client](https://developers.notion.com/guides/mcp/build-mcp-client).
2. A **Notion internal integration secret** (`ntn_` / `secret_`) is the **wrong credential type** for this hosted MCP endpoint unless Notion explicitly documents otherwise for your integration type.
3. **rmcp `auth_header`** expects the **raw bearer token without the `Bearer ` prefix**.

**Current setup wizard gap:** The Notion modal in `setup/index.html` still describes **internal integration + paste secret** (`#notion-modal`, lines ~489–540), and `MCP_CATALOG` uses `authTokenVar:'NOTION_TOKEN'` with placeholder “Notion integration token”—which matches the old mental model, not OAuth. [`src/bin/setup.rs`](../../../src/bin/setup.rs) parses `url` / `auth_token` from TOML for forward compatibility but **does not expose them** in `ExistingMcpServer` / `GET /api/load-config`, so HTTP MCP entries are dropped when reloading an existing config.

---

### Task 1: Align `config.example.toml` and internal docs with Notion’s auth model

**Files:**

- Modify: `config.example.toml` (Notion HTTP MCP example block)
- Optional: `README.md` setup section if it still says “integration token” for hosted Notion MCP

- [ ] **Step 1: Replace the misleading Notion example**

- State that `auth_token` must be an **OAuth access token** from the setup wizard’s **Sign in with Notion** flow (once Task 3 ships), or from another OAuth-capable client that implements Notion’s MCP OAuth—**not** an internal integration secret.
- Link: [get-started-with-mcp](https://developers.notion.com/guides/mcp/get-started-with-mcp), [build-mcp-client](https://developers.notion.com/guides/mcp/build-mcp-client).

- [ ] **Step 2: Add “Alternatives” in the same comment block**

- Legacy **open-source** [`notion-mcp-server`](https://github.com/makenotion/notion-mcp-server) with an API token (deprecated).
- **stdio bridge:** `npx -y mcp-remote https://mcp.notion.com/mcp` ([Notion troubleshooting](https://developers.notion.com/guides/mcp/get-started-with-mcp))—still requires OAuth for first connection.

- [ ] **Step 3: Commit**

```bash
git add config.example.toml
git commit -m "docs(mcp): clarify Notion hosted MCP OAuth vs integration secret"
```

---

### Task 2: Normalize `auth_token` for HTTP MCP in the bot

**Files:**

- Modify: `src/mcp.rs` (`connect_http`)

- [ ] **Step 1: Strip accidental `Bearer ` prefix**

```rust
fn normalize_bearer_token(raw: Option<String>) -> String {
    let Some(s) = raw else {
        return String::new();
    };
    let t = s.trim();
    t.strip_prefix("Bearer ")
        .or_else(|| t.strip_prefix("bearer "))
        .map(str::trim)
        .unwrap_or(t)
        .to_string()
}
```

Use `normalize_bearer_token(config.auth_token.clone())` where `auth_header` is set.

- [ ] **Step 2: Run checks**

```bash
cargo fmt
cargo clippy -- -D warnings
cargo test
```

- [ ] **Step 3: Commit**

```bash
git add src/mcp.rs
git commit -m "fix(mcp): strip Bearer prefix from HTTP MCP auth_token"
```

---

### Task 3: OAuth2 PKCE for Notion — setup wizard UI + `setup.rs` backend

**Primary references:**

- [Integrating your own MCP client](https://developers.notion.com/guides/mcp/build-mcp-client) — discovery (RFC 9470 → RFC 8414), PKCE, token endpoint, scopes.
- [get-started-with-mcp](https://developers.notion.com/guides/mcp/get-started-with-mcp) — endpoint URL `https://mcp.notion.com/mcp`.

**Files:**

- Modify: [`setup/index.html`](../../../setup/index.html) — Notion modal, catalog labels, JS for OAuth popup / `postMessage` handler.
- Modify: [`src/bin/setup.rs`](../../../src/bin/setup.rs) — new routes, PKCE + token exchange, in-memory session state.
- Modify: `Cargo.toml` — dependencies (`oauth2`, and any HTTP client already available via workspace if shared with main crate; `setup` binary may need `reqwest` for discovery GETs).

#### 3.1 Load-config: preserve HTTP MCP servers

- [ ] **Extend `ExistingMcpServer`** with optional `url: String` and `auth_token: String` (default empty).
- [ ] **Update `RawMcpServer`** — remove `#[allow(dead_code)]` on `url` / `auth_token`; map them in `parse_existing_config` when building `ExistingMcpServer`.
- [ ] **Update `setup/index.html`** `loadExistingConfig` / merge logic so Step 5 pre-checks Notion and fills the token field from `auth_token` (not from `env` for HTTP entries).

#### 3.2 OAuth discovery (setup binary)

- [ ] **Implement `discover_oauth_metadata(mcp_url: &str) -> ...`** per Notion’s TypeScript sketch in [build-mcp-client](https://developers.notion.com/guides/mcp/build-mcp-client):  
  - GET `/.well-known/oauth-protected-resource` relative to the MCP origin.  
  - Follow `authorization_servers[0]` to fetch `/.well-known/oauth-authorization-server` (or equivalent) for `authorization_endpoint`, `token_endpoint`, `registration_endpoint` (if dynamic registration is required).

#### 3.3 PKCE session state (in-memory)

- [ ] **Struct** `PendingNotionOAuth { pkce_verifier: String, created: Instant }` stored in `Arc<Mutex<HashMap<String, PendingNotionOAuth>>>` keyed by **cryptographically random `state`** (CSRF).
- [ ] **TTL:** Reject exchanges older than e.g. 15 minutes; prune on access.

#### 3.4 HTTP routes (Axum)

Register on the same `Router` as `/` and `/api/load-config`:

- [ ] **`GET /api/notion/oauth/start`**  
  - Input (query): optional `redirect` same-origin check.  
  - Run discovery against `https://mcp.notion.com/mcp` (or configurable base).  
  - Generate PKCE (`PkceCodeChallenge::new(S256)` via `oauth2` crate).  
  - If Notion requires **dynamic client registration** (see Notion doc § registration), POST to `registration_endpoint` and cache `client_id` (and `client_secret` if issued) in memory for the token exchange.  
  - Build authorization URL with scopes from Notion metadata (`scopes_supported` intersection with Notion MCP required scopes—read Notion doc for exact list).  
  - **`redirect_uri`:** must be fixed and documented, e.g. `http://127.0.0.1:{port}/api/notion/oauth/callback` where `port` is the bound setup server port (use `127.0.0.1` consistently in Notion app settings if pre-registered).  
  - Return JSON: `{ "authorization_url": "..." }` for the SPA to `window.open`.

- [ ] **`GET /api/notion/oauth/callback?code=...&state=...`**  
  - Validate `state`, load PKCE verifier, exchange `code` at `token_endpoint`.  
  - Return small **HTML** document that:  
    - Calls `window.opener.postMessage({ type: 'rustfox-notion-oauth', ok: true, access_token: '...', refresh_token: '...', expires_in: N }, window.location.origin)` (or `'*'` if origin matching is awkward during dev—prefer origin).  
    - `window.close()` after short delay; show error HTML if exchange fails.

- [ ] **Security notes in code comments:** Setup server is local-only; tokens cross from callback page to SPA via `postMessage`—validate `event.origin` in the parent listener.

#### 3.5 `setup/index.html` UX

- [ ] **Replace `#notion-modal` body:** Remove “internal integration / paste secret” steps. Replace with:  
  - Short explanation: hosted MCP requires **Sign in with Notion** (OAuth).  
  - Button **Connect Notion** → `fetch('/api/notion/oauth/start')` → `window.open(data.authorization_url, 'notion-oauth', 'width=...')`.  
  - Listener: `window.addEventListener('message', ...)` → on success, set `state.mcp_selections.notion` env map entry for `NOTION_TOKEN` to `access_token` (or introduce dedicated `state.notion_oauth_token` and teach `buildMcpToml` / checkbox sync to use it).  
  - Show **Connected** state and masked token preview; **Disconnect** clears stored token.

- [ ] **Update `MCP_CATALOG` notion row:** Change `authTokenVar` label in UI from “integration token” to **OAuth access token**; keep key name `NOTION_TOKEN` only if it is still used as the form field id—or rename to `NOTION_OAUTH_ACCESS_TOKEN` for clarity (requires JS + save path consistency).

- [ ] **Placeholder text** for the Notion token input: e.g. “Click Connect Notion or paste access token”.

- [ ] **Link** [get-started-with-mcp](https://developers.notion.com/guides/mcp/get-started-with-mcp) in the modal subtitle.

#### 3.6 Tests and manual verification

- [ ] **Unit tests** in `setup.rs`: discovery parsing (mock JSON fixtures), PKCE state round-trip without network if feasible; or integration test with `curl` against local server (optional).

- [ ] **Manual:** `cargo run --bin setup` → Step 5 → Connect Notion → complete OAuth → save config → inspect `config.toml` for `[[mcp_servers]]` with `url` + `auth_token`. Run `cargo run --bin rustfox` and confirm MCP connects (see prior log line `Connected to MCP server 'notion'`).

- [ ] **Commit** (split if large): `feat(setup): Notion MCP OAuth PKCE in setup wizard`, `fix(setup): load-config returns url/auth_token for HTTP MCP`.

---

### Task 4 (follow-up): Runtime refresh for Notion access token

**Files:**

- Modify: `src/config.rs` — optional `notion_refresh_token` or generic `mcp_server` OAuth fields (design with YAGNI: only what Notion needs).
- Modify: `src/mcp.rs` — before `serve(transport)`, if token expired or API returns 401, refresh using `refresh_token` and update on-disk config or a sidecar file (document security: file permissions).

- [ ] **Step 1:** Persist `refresh_token` from Task 3 callback into `config.toml` (or `~/.config/rustfox/notion-mcp.json`) when user saves from setup wizard.
- [ ] **Step 2:** Implement refresh using `token_endpoint` + `oauth2` `refresh_token` grant (reuse discovery or cache endpoints in config).
- [ ] **Step 3:** Document token rotation in `config.example.toml`.

---

## Self-review (writing-plans checklist)

| Spec / requirement | Covered by |
|--------------------|------------|
| OAuth2 for Notion MCP | Task 3 |
| **setup/index.html** retrieves access token via OAuth | Task 3.5 + 3.4 |
| Explain `invalid_token` / wrong token type | Root cause |
| rmcp `auth_header` format | Task 2 + root cause |
| Load existing HTTP MCP + token in wizard | Task 3.1 |
| Refresh / long-running bot | Task 4 |
| Notion official docs | Links throughout |

**Placeholder scan:** Registration details (exact scopes, whether dynamic registration is mandatory) depend on Notion’s current API—Task 3.2/3.4 explicitly call for reading [build-mcp-client](https://developers.notion.com/guides/mcp/build-mcp-client) at implementation time and fixing constants accordingly.

---

## Execution handoff

**Plan updated and saved to `docs/superpowers/plans/2026-04-12-notion-mcp-oauth.md`. Two execution options:**

1. **Subagent-Driven (recommended)** — Dispatch a fresh subagent per task; review between tasks. **REQUIRED SUB-SKILL:** superpowers:subagent-driven-development.

2. **Inline Execution** — Execute tasks in this session using checkpoints. **REQUIRED SUB-SKILL:** superpowers:executing-plans.

**Which approach?**
