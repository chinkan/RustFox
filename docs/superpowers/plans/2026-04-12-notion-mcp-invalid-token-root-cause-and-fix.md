# Notion MCP `invalid_token` — Root Cause and Fix Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure RustFox connects to `https://mcp.notion.com/mcp` with a credential Notion accepts: an **OAuth 2.0 access token** obtained via Authorization Code + PKCE, sent as `Authorization: Bearer <access_token>`, per [Integrating your own MCP client](https://developers.notion.com/guides/mcp/build-mcp-client).

**Architecture:** The bot already uses `rmcp` streamable HTTP with `StreamableHttpClientTransportConfig::auth_header` ([`src/mcp.rs`](../../../src/mcp.rs)). The setup wizard ([`setup/index.html`](../../../setup/index.html), [`src/bin/setup.rs`](../../../src/bin/setup.rs)) implements discovery (RFC 9470 → RFC 8414), dynamic registration, PKCE, and token exchange. The gap is not “missing OAuth code” in the abstract—it is **operational**: users (or stale config) can still persist the **wrong token type**, which produces exactly the log you see.

**Tech Stack:** Rust (`src/mcp.rs`, `src/bin/setup.rs`), Axum + `oauth2`, Notion MCP docs.

---

## Systematic debugging — Phase 1: root cause (confirmed)

### Error (from `run-2026-04-12.log`)

```text
AuthRequired(AuthRequiredError { www_authenticate_header: "Bearer realm=\"OAuth\", error=\"invalid_token\"" })
Failed to connect to MCP server 'notion': ... Auth required, when send initialize request
```

The server is rejecting the `Authorization` bearer value during MCP `initialize`.

### Evidence: wrong credential type in `config.toml`

The workspace `config.toml` (user-local, not committed) contained:

```toml
[[mcp_servers]]
name = "notion"
url = "https://mcp.notion.com/mcp"
auth_token = "ntn_…"   # prefix indicates Notion *internal integration* token style
```

**Interpretation:** Tokens prefixed with `ntn_` are **internal integration secrets** (API access for the Integrations API), not OAuth access tokens for the **hosted MCP** resource. Notion’s guide states that connecting to `https://mcp.notion.com/mcp` requires the full OAuth flow (discovery, PKCE, token endpoint) and then passing the **returned `access_token`** on MCP requests ([build-mcp-client](https://developers.notion.com/guides/mcp/build-mcp-client), Step 7).

Therefore:

| Symptom | Root cause |
|--------|------------|
| OAuth browser flow “succeeds” but bot still fails | Config was never updated with the OAuth `access_token`, or an old/manual `ntn_` value was saved instead. |
| `invalid_token` on initialize | Bearer value is missing, expired, or **not an OAuth access token** (e.g. integration secret). |

### What the official guide requires (checklist)

From [Integrating your own MCP client](https://developers.notion.com/guides/mcp/build-mcp-client):

1. **Discovery:** Protected Resource Metadata → Authorization Server Metadata (`authorization_endpoint`, `token_endpoint`, optional `registration_endpoint`).
2. **PKCE:** `code_verifier` / `code_challenge` (S256).
3. **Dynamic client registration** when `registration_endpoint` is present.
4. **Token exchange:** `grant_type=authorization_code` with `code_verifier`, `client_id`, `redirect_uri`.
5. **MCP connect:** `Authorization: Bearer <access_token>` on streamable HTTP to `https://mcp.notion.com/mcp` (SSE fallback optional).
6. **Refresh:** Access tokens expire (~1 hour per Notion); refresh token rotation must persist the latest refresh token.

RustFox **implements 1–5 in setup**; the **runtime bot** only sends `auth_token` from config—it does not refresh (see Task 4).

---

## Brainstorming — approaches (3 options)

| Approach | Pros | Cons |
|----------|------|------|
| **A. User fixes config** | Immediate: re-run setup, **Connect Notion**, verify `auth_token` is **not** `ntn_…`, save, restart bot. | Does not prevent recurrence. |
| **B. Guardrails in UI + startup** | Reject or warn on `ntn_` / `secret_` in the Notion token field; log a clear error at MCP connect. | Small code change; does not replace OAuth. |
| **C. Persist refresh token + refresh in bot** | Matches Notion’s rotation semantics; bot stays up >1h. | Config/schema + token endpoint calls in `mcp` or a small OAuth helper. |

**Recommendation:** Do **A** immediately to unblock; ship **B** in the same milestone to stop confusion; schedule **C** as follow-up for long-running deployments.

---

## Tasks

### Task 0: Immediate verification (no code)

**Files:** User’s `config.toml` (local).

- [ ] **Step 1:** Open `config.toml` and inspect `[[mcp_servers]]` for `name = "notion"`.
- [ ] **Step 2:** If `auth_token` starts with `ntn_` or looks like an integration secret, **delete that value** and obtain a token via the wizard only: `cargo run --bin setup` → enable Notion → **Connect Notion** → complete browser login → confirm the token field updates → **Save** → restart `cargo run --bin rustfox`.
- [ ] **Step 3:** Expected log: connection succeeds (no `invalid_token`). If it still fails, capture **redacted** first 20 chars of `auth_token` and whether `expires_in` was shown in OAuth callback (for support).

---

### Task 1: Setup wizard — block or warn on integration-token shape

**Files:**

- Modify: [`setup/index.html`](../../../setup/index.html) — Notion token input / `setEnv` / `postMessage` handler.

- [ ] **Step 1:** After setting `NOTION_TOKEN` from OAuth **or** paste, if the value matches `^ntn_` or `^secret_`, show inline error: “This looks like a Notion **integration** secret. Hosted MCP at mcp.notion.com requires an **OAuth access token** — use **Connect Notion**.”

- [ ] **Step 2:** Optionally disable **Save** on Step 5 when Notion is selected and the token matches those prefixes.

- [ ] **Step 3:** Manual test: paste fake `ntn_test` → warning appears; OAuth flow still fills a different-shaped token → save → config valid.

---

### Task 2: Bot — clearer MCP connect error for bad Notion token

**Files:**

- Modify: [`src/mcp.rs`](../../../src/mcp.rs) — `connect_http` error path (map `AuthRequired` / body if available).

- [ ] **Step 1:** When HTTP MCP connection fails for server name `notion` and error mentions auth / `invalid_token`, log one line: suggest checking that `auth_token` is the OAuth access token from setup **Connect Notion**, not an integration secret (`ntn_…`).

- [ ] **Step 2:** `cargo clippy -- -D warnings` && `cargo test`.

---

### Task 3: Confirm load-config round-trip for HTTP MCP

**Files:**

- Modify (if not already complete): [`src/bin/setup.rs`](../../../src/bin/setup.rs) — `ExistingMcpServer` / `parse_existing_config`.
- Modify: [`setup/index.html`](../../../setup/index.html) — `loadExistingConfig` merge.

- [ ] **Step 1:** Verify `GET /api/load-config` returns `url` and `auth_token` for HTTP entries so editing does not drop the bearer token.
- [ ] **Step 2:** Add or extend unit test in `setup.rs` for TOML containing `url` + `auth_token` for notion.

*(See also the broader checklist in [`2026-04-12-notion-mcp-oauth.md`](./2026-04-12-notion-mcp-oauth.md); deduplicate any overlapping work.)*

---

### Task 4 (follow-up): Refresh token persistence and runtime refresh

**Files:**

- Modify: [`src/config.rs`](../../../src/config.rs) — optional fields for Notion OAuth refresh (YAGNI: only what’s needed).
- Modify: [`src/mcp.rs`](../../../src/mcp.rs) or new small module — refresh before connect or on 401.

Per [build-mcp-client](https://developers.notion.com/guides/mcp/build-mcp-client) Step 8 and Notion’s refresh-token rotation note:

- [ ] **Step 1:** Persist `refresh_token` from setup callback into config or a sidecar JSON next to config (document permissions).
- [ ] **Step 2:** Store `client_id` (and `client_secret` if any) required for refresh — same as registration result from setup.
- [ ] **Step 3:** On startup or on `invalid_token` when expiry is plausible, call token endpoint with `grant_type=refresh_token`; atomically update stored tokens.

---

## Self-review (writing-plans)

| Requirement | Task |
|-------------|------|
| Explain `invalid_token` vs wrong token type | Root cause section |
| Align with Notion official OAuth + MCP HTTP | References + Task 4 |
| User-unblocking path | Task 0 |
| Prevent repeat mistakes | Task 1–2 |
| Long-running bot | Task 4 |

---

## Execution handoff

**Plan saved to:** `docs/superpowers/plans/2026-04-12-notion-mcp-invalid-token-root-cause-and-fix.md`

**Two execution options:**

1. **Subagent-driven (recommended)** — One subagent per task; review between tasks. **Skill:** `superpowers:subagent-driven-development`.

2. **Inline** — Same session, checkpoints. **Skill:** `superpowers:executing-plans`.

---

## References

- [Integrating your own MCP client](https://developers.notion.com/guides/mcp/build-mcp-client) — discovery, PKCE, Bearer on streamable HTTP, refresh.
- [Get started with Notion MCP](https://developers.notion.com/guides/mcp/get-started-with-mcp) — endpoint URL.
- Related internal plan: [`2026-04-12-notion-mcp-oauth.md`](./2026-04-12-notion-mcp-oauth.md).
