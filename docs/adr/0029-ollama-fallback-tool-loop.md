# ADR-0029: Safe fallback when the next provider is Ollama (or tool-weak)

- **Status:** Proposed
- **Date:** 2026-10-18
- **Target release:** v1.0.7 (or earlier hotfix if QA confirms severity)
- **Depends on:** ADR-0012, ADR-0019 (amends behaviour of the walk; does not reopen “walk on any fail”)
- **Issue:** #203

## Context

With `[fallback] chain` ending on Ollama (Kan: primary OpenRouter → Ollama
`qwen3.8:27b`), a mid-agent-loop failure (429 / no credit) that walks the
chain almost always surfaces:

```text
ollama API error (500): {"error":{"message":"no user query found in messages",...}}
```

Facts locked so far:

- `LlmClient` retries the **same** `messages` + tools on each fallback
  candidate (no Ollama-specific rewrite).
- Ollama returns this 500 from its own router when the payload has no plain
  `role:user` turn (common after `assistant`+`tool_calls` / `tool` messages).
  Upstream reports also cite context prune (`num_ctx`, often 4096) dropping
  the user turn; `qwen3.8` renderers are especially strict.
- Product: almost every tool-using turn hits this after OpenRouter→Ollama
  fallback (stable repro rate). Non-technical users see a dead bot.
- Does not block #201 / v1.0.6.

## Decision

1. **Keep ADR-0019** for cloud→cloud walks (e.g. OpenRouter→OpenRouter):
   mid-tool-loop fallback remains allowed when the candidate can consume the
   same OpenAI-style tool message shape.
2. **When the next candidate is Ollama** (provider type `ollama`), or a
   provider we mark as tool-weak / no-tools in config:
   - **Do not** send the raw mid-tool-loop history unchanged.
   - **Do not** send a first-turn (or any) payload that still carries a tools
     schema to tool-weak Ollama without rewrite (#218): rewrite + **omit
     tools** whenever tools are present on the fallback walk.
   - Either:
     - **(preferred)** rewrite a fallback payload that always includes at
       least one non-tool `user` turn (keep the latest real user text;
       fold recent tool results into that user content or a short
       assistant summary), and omit tools; or
     - **skip** that candidate and continue the chain / fail the turn.
   - Choosing rewrite vs skip is an implementation detail as long as Ollama
     never receives a tool-only / user-less transcript **or** a raw tools
     schema that 500s with `no user query found`.
3. **User-visible errors** on Telegram and the portal must be one plain
   sentence (quota / temporarily unavailable). Do not leak Ollama’s 500 JSON
   or internal stack text to the user.
4. **Secondary hardening (same release if cheap):** log when Ollama returns
   `no user query found`; optional note in docs that low `num_ctx` can prune
   user turns. Do not require operators to raise `num_ctx` as the primary fix.
5. **Tests:** unit/integration coverage for (a) mid-tool-loop messages →
   Ollama path never sends a payload without a plain `user` role;
   (b) user-facing error mapping hides the Ollama envelope.


## Amendment (#218) — 2026-10-18

**Product lock:** when walking onto tool-weak Ollama with **tools present**
(including **first turn + plain user**), always run the ADR-0029 rewrite and
**omit tools**. Do **not** send a raw tools schema on first-turn fallback
(that path 500s with `no user query found` on qwen3.8 / similar). Do **not**
reuse primary keep-tools shaping from ADR-0032 on the fallback walk —
ADR-0032 remains **primary-only**. Mid-loop rewrite+omit behaviour unchanged.
Log text must only say “after rewrite” when rewrite actually ran.

## Amendment (#223) — 2026-10-19

**Product lock:** rewrite + omit-tools applies to **every** fallback → tool-weak
walk, including **agent/subagent invoke** (first-turn or mid-loop), the same as
main chat (#218 / #220). If tip ≥ `01db8f6` still lacks the rewrite warn on the
agent-invoke path, treat as a code gap and fix under the #223 impl PR. Agent /
Active / `[openrouter].model` resolution is locked in **ADR-0033** — this amend
only extends the fallback shaping scope.

## Amendment (#226) — 2026-10-19

**Cross-ref only:** when **primary** Ollama returns `no user query found` after
ADR-0032 keep-tools, **ADR-0034** locks rewrite + omit tools and retry **once**
on the **same primary** (journal WARN), then this ADR’s fallback walk if that
retry fails. Fallback rewrite+omit behaviour in this document is unchanged;
primary first-pass keep-tools remains ADR-0032.

## Non-goals

- Removing `[fallback] chain` or reverting ADR-0019 for cloud providers.
- Making Ollama the primary production model for cloud-first deployments (local/private **primary** Ollama + tools: see ADR-0032 / #213).
- Portal UI redesign.
- Blocking v1.0.6 / #201.

## Acceptance criteria

- With OpenRouter forced to fail (429 / no credit) and Ollama
  `qwen3.8:27b` (or CI mock) next in the chain, a turn that already has
  tool calls does **not** surface `no user query found in messages` to the
  user.
- QA repro on the maintainer environment: high repro rate before the fix
  becomes zero (or clearly documented residual only when Ollama itself is
  down).
- Logs may keep the raw provider body; Telegram/portal text does not.
- CHANGELOG entry under the shipping release.

## Consequences

- Fallback remains useful for cloud→cloud; Ollama stays a last resort
  without breaking tool turns.
- Slightly more logic in the fallback walk (detect Ollama / tool-weak,
  rewrite or skip).
- Assign Rust Developer after this ADR merges; QA verifies on maintainer
  Windows (or test bot) with `qwen3.8:27b`.
