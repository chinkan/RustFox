# ADR-0032: Primary Ollama + tools (message shaping and routing)

- **Status:** Accepted
- **Date:** 2026-10-18
- **Target release:** v1.0.7+ (does not block v1.0.6)
- **Depends on:** ADR-0029 (fallback-only Ollama tool-loop safety; #203 / #206)
- **Amends:** ADR-0029 non-goal “Making Ollama the primary production model” — primary Ollama is in scope for **local/private** use
- **Issue:** #213

## Context

ADR-0029 / #206 cover **fallback** onto Ollama: mid-tool-loop (or user-less) payloads are rewritten to `system` + plain `user` and tools may be omitted. That path does **not** cover Ollama as **primary**.

QA (2026-10-18) with primary `ollama/qwen3.8:27b` + MCP tools:

- Portal / Telegram tool turns fail with `ollama API error (500): "no user query found in messages"` after a long wait (~3 min), then a fake quota-style message.
- Journal showed **no** `Rewriting mid-tool-loop…` warn — so this is **not** a #206 mid-loop rewrite regression.
- Today’s gate (`is_mid_tool_loop || !has_plain_user_turn`) leaves **first turn with a plain user** on the raw tools path even when the provider is Ollama.
- Direct Ollama calls with a comparable tools schema can succeed; the failure is in RustFox shaping / routing.
- Separately: when primary is already Ollama, the loop still attempts unprefixed OpenRouter-style ids (e.g. `deepseek/…`). `ProviderRegistry::resolve_model` treats an unknown first path segment as “fall through to default provider with the full string”, so those ids hit Ollama and 404.

Product: **primary Ollama + tools** is P1 for local/private use. Not Exa / ADR-0023. Adjacent to ADR-0029 / #206, not a reopen of ADR-0019.

## Decision

1. **Primary Ollama is in scope** for local/private deployments. ADR-0029 remains the rule for **fallback** onto Ollama (including omit/degrade tools on rewrite). This ADR locks **primary** behaviour.

2. **Message shaping (primary Ollama / tool-weak primary):**
   - Every chat request sent to primary Ollama must include at least one plain `role:user` turn with non-empty text after serialization.
   - This applies to **first turn with tools schema** (today’s gap: has user → raw tools, no rewrite warn) **and** to later tool / rewrite turns.
   - Prefer **keeping tools** on primary so MCP tool bubbles can complete (unlike ADR-0029’s fallback rewrite, which may omit tools). Mid-tool-loop on primary may still rewrite/fold if the target cannot consume raw `tool` / `assistant+tool_calls` roles; choosing rewrite vs skip vs a tools-preserving shape is an implementation detail as long as Ollama never receives a user-less / tool-only transcript and acceptance below holds.
   - If a valid payload cannot be built, fail with a **clear RustFox error** mapped to the same plain user-facing sentence as ADR-0029 §3 — never surface Ollama’s 500 JSON.

3. **Routing (primary Ollama):**
   - When the call’s resolved provider is Ollama (including default-provider fallthrough), do **not** attempt OpenRouter-style model ids whose first `/` segment is **not** a registered provider name and which are not bare Ollama library model names (example: `deepseek/deepseek-v4-flash` must not be sent to Ollama).
   - Skip or reject with a clear error / continue the fallback chain only for **qualified** `provider/model` entries that resolve to a known non-wrong provider. Do not 404-loop on Ollama with foreign ids.

4. **Tests:** unit/integration coverage for (a) primary Ollama + tools with a plain user (first turn) never sends a user-less payload; (b) unknown-prefix OpenRouter-style id with Ollama as default does not call Ollama with that model string; (c) ADR-0029 fallback rewrite behaviour unchanged for OpenRouter→Ollama mid-loop.

5. **Release:** target v1.0.7+. Must not block v1.0.6. GitHub English only. Four gates on the **impl** PR; this ADR PR is docs-only.

## Non-goals

- Replacing or weakening ADR-0029 fallback rewrite / omit-tools for chain walks onto Ollama.
- Reverting ADR-0019 cloud→cloud mid-tool-loop fallback.
- Exa one-tap / ADR-0023.
- Portal UI redesign.
- Requiring operators to raise Ollama `num_ctx` as the primary fix (same stance as ADR-0029 §4).


## Cross-reference (#218)

Primary **keep-tools** shaping in this ADR applies to **primary Ollama only**.
Fallback onto tool-weak Ollama (including first turn with tools + plain user)
remains ADR-0029 rewrite + **omit tools** — do not reuse `shape_primary_ollama_payload`
on the fallback walk.

## Acceptance criteria (implementation — later assign; not this docs PR)

- With Ollama as **primary**, a tool-calling turn completes (tool bubble + useful reply) without `no user query found in messages`.
- Message list sent to Ollama always includes a valid user query after tool / rewrite turns (or a clear RustFox error, not a malformed Ollama 500).
- When primary is Ollama, do not route to unprefixed OpenRouter-style ids (e.g. `deepseek/…`) that 404 on Ollama.
- Regression test and/or documented manual QA: primary Ollama + MCP tool call.
- CHANGELOG entry under the shipping release.

## Consequences

- Docs-only first (this ADR). Tech lead assigns RustFox Rust Developer for impl after merge (`fix/213-ollama-primary-tools`).
- Primary Ollama becomes a supported local path; fallback Ollama stays last-resort with ADR-0029 shaping.
- `resolve_model` / fallback walk gain a guard against foreign ids when default is Ollama.
