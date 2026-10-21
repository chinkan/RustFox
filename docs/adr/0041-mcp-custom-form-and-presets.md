# ADR-0041: Portal MCP presets and Custom HTTP form

- **Status:** Accepted. **Amended by ADR-0042** (wizard Exa-only exception).
- **Date:** 2026-10-21
- **Amends:** ADR-0023 (one-tap Exa). Exa tools pin, consent, `web_search` stub, and soft 429 stay on the Exa preset. Preset storage and the Custom entry point follow this ADR.
- **Depends on:** ADR-0016 (secrets as `secret:NAME` in SecretStore), ADR-0004 / ADR-0006 (portal Settings)
- **Related:** #172 (tracking; impl PR Closes), #240 Slice B (impl starts after that slice), #119 (Google MCP unchanged), ADR-0042 / #256 (wizard optional Exa)
- **Issue:** #172

## Context

#172 asks how to send an HTTP MCP Bearer token, including a Jina key taken from an environment variable name, without putting the secret in `config.toml`. OAuth fields exist on `McpServerConfig`. Explicit non-Bearer headers do not.

ADR-0023 already requires an Exa `x-api-key` header, a tools pin, portal key entry, and a runtime disconnect. That work and #172 are the same HTTP-header problem. Product locks (PO grill on #172, 2026-10-21; Q1–Q12 = A) are authoritative for the Decision below.

- **Q1** presets + Custom, Exa included
- **Q2** Exa tools pin, consent, stub, and soft 429 stay on the Exa preset
- **Q3** portal Settings only; no wizard MCP step (**amended by ADR-0042:** optional Exa-only keyless+consent page in the web wizard; Custom/Jina/Google stay out)
- **Q4** no Telegram full form
- **Q5** secrets never in chat
- **Q6** Custom fields: name, HTTP URL, Bearer, header map
- **Q7** SecretStore `secret:NAME` and/or `env:NAME`
- **Q8** no plaintext secrets in `config.toml` values
- **Q9** presets v1 are Exa and Jina (Jina: `https://mcp.jina.ai/v1`, Streamable HTTP, Bearer, tools `search_web` and `read_url`)
- **Q10** persist `[[mcp_servers]]` + secret refs; Exa consent state per ADR-0023
- **Q11** hot reload connect/disconnect without a full restart
- **Q12** this ADR amends ADR-0023; the implementation PR Closes #172 after #240 Slice B

Facts in the current code:

1. `McpServerConfig` has `url` and `auth_token`. `connect_http` sends `auth_token` only as `Authorization: Bearer` (`McpManager::resolve_bearer`). There is no header map. rmcp `StreamableHttpClientTransportConfig::custom_headers` can carry other headers (ADR-0023).
2. `resolve_bearer` already resolves `secret:NAME` (ADR-0016). A plaintext `auth_token` is still sent as-is. `env:NAME` is not resolved on the HTTP auth path.
3. `connect_http` logs the URL at `info`. A secret in the URL would be logged.
4. `McpManager` can `connect`, `connect_all`, and `shutdown`. It cannot drop one server at runtime (ADR-0023). `mcp.<name>.enabled` still tells the operator a restart is required.
5. The setup wizard does not ask about MCP (ADR-0023), except the optional Exa-only page in ADR-0042. Portal Settings is the credential surface (ADR-0006).

Jina’s hosted server is Streamable HTTP at `https://mcp.jina.ai/v1`, with `Authorization: Bearer`. Tool registration is filtered with the `include_tools` query parameter before the client sees the list (`search_web`, `read_url`, and others). Source: <https://github.com/jina-ai/MCP>.

## Decision

### 1. One model: presets + Custom (Q1 A)

HTTP MCP that RustFox turns on for the operator is either a **preset** or **Custom**.

ADR-0023 Exa converges into this model. It is the Exa preset, not a second built-in beside the form. Google (#119) is unchanged. Stdio `[[mcp_servers]]` rows are unchanged.

### 2. Exa preset keeps ADR-0023 behavior (Q2 A)

On the Exa preset only:

- Tools pin stays `https://mcp.exa.ai/mcp?tools=web_search_exa,web_fetch_exa`.
- Privacy consent and `privacy_accepted_at` stay as ADR-0023. With no consent, Exa does not connect.
- The `web_search` stub, the once-per-conversation offer, and removal of the stub while Exa is on stay as ADR-0023.
- An Exa 429 or quota error stays a soft tool result. The turn still answers. The key prompt stays portal-only and at most once per conversation (ADR-0023 §§7–8).
- Key transport stays ADR-0023: optional key in SecretStore (`exa.mcp.api_key`), sent only as `x-api-key`, never as Bearer and never in the URL. Keyless remains the default when the preset is on and no key is stored.

Jina and Custom do not gain the stub, the consent record, or the soft-429 copy.

### 3. Portal Settings only (Q3 A)

The form lives on portal **Settings**. Settings stays the full MCP form (presets + Custom).

**Amended by ADR-0042:** the web setup wizard may offer an **optional, skippable Exa-only** page (keyless + one-time consent). Custom, Jina, and Google stay out of the wizard. There is still no full MCP form in the wizard.

### 4. Telegram is not a form (Q4 / Q5 A)

Telegram has no full MCP form (no name, URL, Bearer, or header editor). Secrets are never requested, pasted, or echoed in Telegram chat.

The Exa preset may still use the ADR-0023 one-tap enable control and stub offer. That path collects no secret. Adding or rotating a key stays in portal Settings.

### 5. Custom fields (Q6 A)

Custom asks for:

- **name** — unique among `[[mcp_servers]]`. `exa` and `jina` are reserved for those presets.
- **HTTP URL** — Streamable HTTP endpoint. Query pins such as `tools` or `include_tools` are part of the URL. The URL is not a place to put a credential.
- **Bearer** — optional `Authorization: Bearer` value.
- **header map** — optional extra headers (name → value). Exa’s `x-api-key` is this map on the Exa preset.

OAuth client fields already on `McpServerConfig` stay for existing OAuth servers. Custom v1 does not add an OAuth builder.

### 6. Secret refs only (Q7 / Q8 A)

Bearer and any header value that is a credential are stored as one of:

- `secret:NAME` — SecretStore (ADR-0016), or
- `env:NAME` — process environment variable `NAME`, read at connect time.

Both forms are allowed on the same server (Bearer one way, a header the other). `config.toml` holds the ref, never the secret value.

Portal writes seal into SecretStore and store `secret:NAME`. A hand-written row may use `env:NAME` so one environment holds the key (the #172 Jina case).

A plaintext Bearer or credential header found on load is sealed into SecretStore and the file is rewritten to `secret:NAME` (same migrate-and-scrub idea as ADR-0016). Non-secret literal header values are allowed when they are not credentials.

Resolved values stay out of logs, Telegram chat, and `config.toml`. Logs may name a header (`Authorization set`, `x-api-key set`) and must not print the value. The connect log may include the URL only when that URL has no credential.

### 7. Presets v1: Exa and Jina (Q9 A)

v1 ships two presets.

| Preset | Endpoint | Auth | Tool pin |
|--------|----------|------|----------|
| Exa | `https://mcp.exa.ai/mcp?tools=web_search_exa,web_fetch_exa` | `x-api-key` from SecretStore when a key exists; keyless otherwise | `web_search_exa`, `web_fetch_exa` |
| Jina | `https://mcp.jina.ai/v1?include_tools=search_web,read_url` | `Authorization: Bearer` via `secret:NAME` and/or `env:NAME` | `search_web`, `read_url` |

Jina transport is Streamable HTTP. The `include_tools` query is the pin: the client registers only those two tools. Further presets wait for a later decision.

### 8. Persistence (Q10 A)

A preset or Custom server is saved as a `[[mcp_servers]]` row plus secret refs. Example shape (values are refs):

```toml
[[mcp_servers]]
name = "jina"
url = "https://mcp.jina.ai/v1?include_tools=search_web,read_url"
auth_token = "env:JINA_API_KEY"

[[mcp_servers]]
name = "exa"
url = "https://mcp.exa.ai/mcp?tools=web_search_exa,web_fetch_exa"
enabled = true
```

Exa consent state stays ADR-0023 (`enabled`, `privacy_accepted_at`). Those fields are not secrets. The implementation PR chooses config versus SQLite, shared by portal and Telegram, and surviving restart.

A hand-written `[[mcp_servers]]` row with `name = "exa"` still wins (ADR-0023 open question 2): the portal shows it as configured in `config.toml` and does not overwrite it. Preset pin, consent, and stub apply to the preset-managed Exa row.

### 9. Hot reload (Q11 A)

Saving, enabling, or disabling one server connects or disconnects **that** server without a full process restart. Disable drops that connection and its tools and keeps stored secrets. This is the per-server disconnect ADR-0023 already required, applied to every saved HTTP server, including `mcp.<name>.enabled`.

### 10. Sequencing (Q12 A)

This ADR amends ADR-0023. The implementation PR **Closes #172**. That work starts after #240 Slice B. This document does not implement the form and does not tag a release.

### Soft checks (implementation PR)

- Custom header map is sent on the HTTP connection, including a non-Bearer header.
- Exa preset: tools pin, consent gate, stub on/off, soft 429.
- Jina preset: Bearer from `secret:NAME` and from `env:NAME`, tools limited to `search_web` and `read_url`.
- Resolved secrets are absent from logs, Telegram chat, and `config.toml` values.

## Non-goals

- A full MCP form (name, URL, Bearer, header map) or Jina / Custom / Google in the setup wizard. ADR-0042 allows an optional Exa-only keyless+consent page in the web wizard; that exception is not a Non-goal.
- A Telegram form for name, URL, Bearer, or headers.
- Changing Google MCP (#119).
- Stdio server launch changes.
- A new OAuth flow.
- Presets beyond Exa and Jina.
- Jina or Custom copies of the Exa stub, consent record, or soft-429 prompt.
- Implementation or a release tag in the docs change that adds this ADR.

## Consequences

- One Settings form covers Jina Bearer, Exa, and any other Streamable HTTP server.
- Exa’s pin, consent, stub, and soft 429 survive the move onto a preset row.
- Operators can point at `env:NAME` and keep the key out of the config file.
- `McpServerConfig` gains a header map; HTTP auth learns `env:NAME`; plaintext credential fields get migrate-and-scrub; `McpManager` learns a one-server disconnect.
- `config.example.toml` still shows Exa Bearer and a key in the URL. The implementation PR rewrites that example to refs and the pinned URL.

## Related

- ADR-0023 — Exa pin, consent, stub, soft 429, `x-api-key`. Amended here for storage and for presets + Custom.
- ADR-0016 — `secret:NAME` and SecretStore.
- ADR-0042 / #256 — optional skippable Exa one-tap in the web setup wizard (Exa-only exception to §3 / Non-goals).
- #172 — tracking. The implementation PR Closes it, after #240 Slice B.
- #119 — Google MCP, unchanged.
