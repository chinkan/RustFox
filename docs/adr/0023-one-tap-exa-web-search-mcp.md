# ADR-0023: One-tap built-in Exa MCP for web search

- **Status:** Proposed (PO locks 2026-10-06; Product GO pending PO review of this ADR)
- **Date:** 2026-10-06
- **Issue:** none yet. Tracked in the Notion task "Setup: one-tap built-in MCP"; a GitHub
  issue will be opened for the implementation.
- **Branch:** `docs/adr-0023-exa-web-search` (decision); implementation in a follow-up PR
- **Related:** #119 (Google MCP one-tap, the built-in connector precedent), ADR-0004 / ADR-0007
  (portal bind, `/portal` command), ADR-0016 (API keys in SecretStore), ADR-0020 (mid-run
  queue; implementation of this ADR waits for it)
- **Source:** Exa MCP reference, <https://docs.exa.ai/reference/exa-mcp>

## Context

RustFox has no web search out of the box. Users who want it must hand-write an
`[[mcp_servers]]` row. `config.example.toml` shows one for Exa today, but it recommends a
Bearer token and, as "Option B", an API key inside the URL, which leaks into logs.

#119 added the first **built-in MCP**: Google (Gmail) behind one portal tap, never asked in
the setup wizard. Its shape: a fixed Streamable HTTP endpoint in code (`src/google_mcp.rs`),
the credential in SecretStore, one connection for the whole instance, the existing HTTP MCP
client (`McpManager::connect_http`, `src/mcp.rs`).

Exa publishes a hosted MCP server at `https://mcp.exa.ai/mcp` (Streamable HTTP). From the Exa
reference:

- **Keyless** mode: free and rate-limited, no sign-in, no key. A rate-limited call returns
  HTTP 429.
- **API key** mode: the `x-api-key` header carries the key. Usage goes to the key's plan.
- **OAuth** mode (`?login`): out of scope here.
- Tools: `web_search_exa` and `web_fetch_exa` are on by default. `agent_run` (paid, usage
  based) is also on by default **once a key or OAuth is present**. An explicit `tools` URL
  parameter **replaces** the default list.

Facts in the current code that shape the decision:

1. `McpServerConfig` has no header field. `connect_http` only sets `auth_header` (Bearer).
   rmcp 2.2 (`Cargo.lock`) has `StreamableHttpClientTransportConfig::custom_headers`, which
   can carry `x-api-key`.
2. `connect_http` logs the URL at `info`. Any secret in the URL would be logged.
3. MCP tools are exposed to the model as `mcp_{server}_{tool}`, so Exa tools would appear as
   `mcp_exa_web_search_exa` and `mcp_exa_web_fetch_exa`.
4. `McpManager` has `connect`, `connect_all` and `shutdown`, but no way to drop a single
   connection at runtime. A live "off" needs one.
5. Telegram already has `/portal`, which replies with the portal URL(s) for the current
   network (ADR-0007). The default portal bind is `127.0.0.1`.

## Decision

### 1. Scope

- v1 adds **one** built-in MCP: **Exa web search**. Google (#119) is not changed.
- The setup wizard **never asks about MCP**. It stays at its current four fields.
- Exa is **off by default** for every install, on every provider path. The Ollama setup path
  is not changed.
- Users turn it on after setup, by a **portal tap** or a **Telegram button** (Decision 6).

### 2. Connection

- Endpoint, fixed in code (not user-editable):
  `https://mcp.exa.ai/mcp?tools=web_search_exa,web_fetch_exa`.
  The `tools` pin is mandatory. It keeps the paid `agent_run` hidden from the model even when
  an API key is present.
- Transport: Streamable HTTP through the existing rmcp client.
- **One Exa connection for the whole instance** (all bots), like Google in #119.
- Server id `exa`, so the model sees `mcp_exa_web_search_exa` and `mcp_exa_web_fetch_exa`.
- Exa state does not live in a user-editable `[[mcp_servers]]` row (that would let a hand
  edit drop the `tools` pin or add a key to the URL). The built-in builds its
  `McpServerConfig` in code at connect time, as `google_mcp::runtime_config` does, from a
  small persisted state:
  - `enabled` (bool, default `false`)
  - `privacy_accepted_at` (timestamp or unset)
  The implementation PR picks the storage (config section or SQLite). It must survive a
  restart and be shared by the portal and every bot.

### 3. Keyless by default, optional API key

- **No shared key is baked into the binary.** Keyless mode is the default when enabled.
- The user may add their own Exa API key for more quota. It is stored in **SecretStore**
  (proposed name `exa.mcp.api_key`), following ADR-0016.
- The key is sent **only** as the `x-api-key` header via rmcp `custom_headers`.
  **Not** `Authorization: Bearer`, **not** a URL query parameter.
- The key never appears in `config.toml`, in the connection `url`, or in logs. Logs may name
  the header (`x-api-key set`) but never print its value.
- The key is entered **only in the portal** (Settings). It is never requested in Telegram,
  because a pasted key would stay in the chat history.

### 4. Privacy consent

- Next to the enable control, short copy says that search queries and fetched page content
  are sent to Exa.
- The first enable shows a one-time **"Enable and agree"** confirmation. Accepting sets
  `privacy_accepted_at`. Later enables (portal or Telegram) skip the confirmation.
- Without consent, Exa does not connect.

### 5. Shared on/off state

- Portal and Telegram toggle the **same** state. Off in one place is off everywhere, for all
  bots.
- **Enable:** connect to Exa and register its tools. Remove the `web_search` stub
  (Decision 7) so the model never sees two search tools.
- **Disable:** one tap. Drop the Exa connection and its tools at runtime (needs a new
  `McpManager` disconnect, Context fact 4). Re-register the stub. **Keep the API key.**
- **Delete key:** only in portal **advanced settings**. Deleting the key does not disable
  Exa; it falls back to keyless.

### 6. Entry points

- **Portal:** a Settings card next to Google. Toggle, privacy copy, optional API key field
  (write-only; shows "key saved" and never echoes the value), and the delete-key action under
  advanced settings.
- **Telegram:** an inline **callback** button ("Enable web search"). Telegram must **not**
  use a URL button that points to `localhost` / `127.0.0.1`: a phone cannot open it and
  Telegram may reject it. When the user needs the portal (for example to add a key), reply
  in plain text: "Open the portal on your computer → Settings → paste your Exa key", plus the
  portal address from the `/portal` logic. A direct URL button is allowed only once a remote
  portal or a Telegram Mini App exists.

### 7. `web_search` stub and the proactive offer (Telegram)

- While Exa is **off**, register a stub tool named `web_search` so the model can ask for
  search.
- When the model calls the stub:
  - the tool result tells the model that web search is not enabled, and the model answers
    without it;
  - the bot shows a short note with the **"Enable web search"** callback button and a
    **"No thanks"** button.
- Tapping enable runs the consent step (Decision 4) if needed, then enables Exa (Decision 5).
  It takes effect from the next turn.
- The offer appears **at most once per conversation**. If the user taps "No thanks", it
  never appears again in that conversation. Later stub calls in that conversation return the
  "not enabled" result with no button.
- When Exa is **on**, the stub is not registered.

### 8. Quota and rate limits (429)

- On an Exa 429 or quota error, the tool call returns a "web search was not available this
  time" result. **The turn does not fail.** The model still answers and says that search was
  not available this time.
- The bot adds a short text prompt to add an API key in the portal (Decision 6 wording). It
  does **not** open OAuth and does **not** ask for the key in Telegram.
- This prompt appears **at most once per conversation**.

### 9. Connect failure

- On a failed connect (startup or enable): **retry once**. If it still fails, show a friendly
  "Can't reach Exa for now" message (portal status, and in Telegram if the user just tapped
  enable).
- The bot must not crash or block startup. The model answers without web search.
- Do not repeat the failure message within the same turn.
- A failed connect does not flip `enabled` back to off. Exa connects again at the next
  enable or restart.

### 10. "Per conversation" state

- "Conversation" means the stored conversation for a bot + user, the one `/clear` resets.
- Flags kept per conversation: `exa_offer_shown`, `exa_offer_declined`, `exa_quota_prompted`.
  They should be persisted with the conversation so a restart does not show the same prompt
  again. `/clear` starts a new conversation, so the flags reset.

## Consequences

**Positive**

- Web search with no key and no config edit, one tap, opt-in.
- Paid `agent_run` cannot be reached by the model, even with a key.
- The API key stays out of config, URLs, logs and chat history.
- Search failures and quota limits degrade to a normal answer; they never break a turn.
- Same built-in pattern as Google (#119), so a third built-in can follow it.

**Negative / costs**

- Keyless quota is shared and rate-limited by Exa; heavy users will hit 429 and need a key.
- New code: header support in the HTTP MCP path, a runtime disconnect in `McpManager`, a stub
  tool, per-conversation flags, a Telegram callback handler, and a portal settings card.
- Search queries and fetched pages leave the machine and go to Exa. This is the reason for
  default off and the consent step.
- The `config.example.toml` Exa example (Bearer, key in URL) contradicts this ADR and must be
  rewritten in the implementation PR.

## Test plan (for the implementation PR)

- Built-in Exa config: URL is exactly
  `https://mcp.exa.ai/mcp?tools=web_search_exa,web_fetch_exa`; no key in the URL with or
  without a stored key; `x-api-key` set only when a key exists; no `Authorization` header.
- Logs: with a key stored, captured logs contain `x-api-key` at most as a name and never the
  value.
- Default state: fresh install and wizard output have Exa off and no Exa tools; the stub
  `web_search` is present.
- Enable/disable: enabling removes the stub and adds the two Exa tools; disabling restores the
  stub, drops the tools, keeps the key; deleting the key keeps Exa on in keyless mode.
- Shared state: toggle off from the portal, confirm Telegram bots lose the tools, and the
  reverse.
- Consent: first enable requires "Enable and agree"; second enable does not; no consent, no
  connect.
- Stub offer: first stub call shows the button; second call in the same conversation does
  not; "No thanks" suppresses it for the rest of that conversation; `/clear` resets.
- 429: mock Exa returning 429; the turn completes with an answer, the key prompt appears once
  per conversation, no OAuth, no request to paste a key in Telegram.
- Connect failure: unreachable endpoint; one retry; friendly message once; bot keeps running;
  the turn answers without search.
- Telegram buttons: no URL button with `localhost` or `127.0.0.1`.

## Open questions

1. **Per-bot `tools` allowlists.** Bots may set `tools = [...]` (the researcher example lists
   `"web_search"`). Proposed: an allowlist entry `web_search` also admits the Exa tools when
   Exa is on; bots without an allowlist get them by default. Needs TL confirmation.
2. **Hand-written `exa` row.** If `config.toml` already has `[[mcp_servers]] name = "exa"`,
   tool names collide. Proposed: the manual row wins, and the portal shows Exa as "configured
   in config.toml" with the toggle disabled. Needs TL confirmation.
3. **Ollama and the proactive offer.** Ollama users chose a local model. Should the stub
   offer still appear for bots on an Ollama provider, or should the stub be skipped there?
   Proposed: same behavior, since consent is still required. Needs PO confirmation.
4. **Portal chat.** The proactive offer is specified for Telegram. Proposed: the portal chat
   shows the same "not enabled" answer and links to Settings, with the same once-per-
   conversation rule. Needs PO confirmation.
5. **Storage for `enabled` / `privacy_accepted_at`.** Config section vs SQLite, decided in the
   implementation PR.

## Follow-ups

- GitHub issue for the implementation; link it here.
- Implementation PR. It starts after the ADR-0020 implementation lands and the TL assigns it.
- Rewrite the Exa example in `config.example.toml`; update `docs/portal-api.md` and the user
  guide; changelog entry.
