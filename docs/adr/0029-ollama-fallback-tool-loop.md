# ADR-0029: Safe fallback when the next provider is Ollama (or tool-weak)

- **Status:** Proposed. Amendment (#438) is the Product-accepted lock (2026-11-25, Asia/Hong_Kong)
- **Date:** 2026-10-18
- **Amended:** 2026-11-25 (#438, Asia/Hong_Kong) — full work parity; supersedes #218/#220 omit-tools-first
- **Target release:** v1.0.7 (or earlier hotfix if QA confirms severity). Amendment (#438) is P1 / v1.0.10 Must
- **Depends on:** ADR-0012, ADR-0019 (amends behaviour of the walk; does not reopen “walk on any fail”)
- **Issue:** #203. Amendment (#438) is related to #438 (not Closes)

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
   **Amended 2026-11-25 (#438):** try the same work first (keep tools;
   guarantee a plain user message). The omit-tools bullets below are the
   historical #218 lock and are superseded by Amendment (#438).
   - **Do not** send the raw mid-tool-loop history unchanged.
   - **Do not** send a first-turn (or any) payload that still carries a tools
     schema to tool-weak Ollama without rewrite (#218): rewrite + **omit
     tools** whenever tools are present on the fallback walk.
     **Superseded 2026-11-25 (#438)** — omit-tools-first is not the rule.
     See Amendment (#438).
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

**Superseded for omit-tools-first by Amendment (#438) — 2026-11-25.** Historical lock:

**Product lock:** when walking onto tool-weak Ollama with **tools present**
(including **first turn + plain user**), always run the ADR-0029 rewrite and
**omit tools**. Do **not** send a raw tools schema on first-turn fallback
(that path 500s with `no user query found` on qwen3.8 / similar). Do **not**
reuse primary keep-tools shaping from ADR-0032 on the fallback walk —
ADR-0032 remains **primary-only**. Mid-loop rewrite+omit behaviour unchanged.
Log text must only say “after rewrite” when rewrite actually ran.

## Amendment (#223) — 2026-10-19

**Superseded for omit-tools-first by Amendment (#438) — 2026-11-25.** Historical lock. Agent/subagent invoke stays in scope; the shaping rule is Amendment (#438), not omit-first.

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

**Superseded for the fallback walk by Amendment (#438) — 2026-11-25.** Primary keep-tools and the primary one-shot omit-retry stay. Fallback is keep-tools first.

## Amendment (#438) — 2026-11-25

**Product lock (2026-11-25, Asia/Hong_Kong): fallback-model full work parity is P1 / v1.0.10 Must.** Related to #438. This docs amend does not implement runtime, tests, or config behavior. No merge or tag from this amend.

This **supersedes** the #218 / #220 omit-tools-first fallback lock: Decision 2’s omit-tools bullets, Amendment (#218), Amendment (#223), and the same omit-first reading in ADR-0032 and ADR-0033 Decision 5. It does **not** reopen 401/403 hopping.

Primary Ollama keep-tools (ADR-0032) and the primary one-shot omit-retry (ADR-0034) stay. Amendment (#226) still points a primary `no user query found` at ADR-0034. The fallback walk is no longer omit-first. Verified gaps on tip `af0a8161` are in ADR-0043’s residual section (same amend).

1. **Full work parity** between main and fallback for a turn: same tools (MCP, schedule, sandbox, spawn), same system/soul, same memory/history, same tool-loop, first turn and mid-loop, main chat and agent/subagent invoke.

2. **OpenCode→OpenRouter** (and any other configured chain entry) must be reachable when that backup is configured: the provider must be registered; `secret:` / Portal-saved keys resolve on both `[openrouter]` and `[[provider]] type=openrouter`; a successful leave-Go / 402 / quota walk sends tools, not text-only. If the backup is missing, fail in an operator-visible way that names it; no silent skip. A literal `secret:` value must never be sent as Bearer. Close secret resolve for every `[[provider]]` type that documents `secret:` keys, not only xAI and the legacy `[openrouter]` table. Optional or empty `[openrouter]` must not silently skip a configured `openrouter/…` chain entry at call time (the #433 residual, ADR-0043).

3. **Local/Ollama fallback:** try the same work first (keep tools; guarantee a plain user message). Rewrite+omit is recovery only, after keep-tools fails with no user query found (or equivalent), or the model has no tool interface. After that keep-tools 500: one ADR-0034-style omit-retry on the same fallback model, then the next chain entry.

4. **Plain OpenCode 429:** keep leave-Go (stay on Go, ADR-0009 retry). Walk only on 402 or quota-class 429 (`GoUsageLimitError` / policy+budget).

5. **Vision/audio parts the fallback cannot accept:** skip that chain entry; if none can, fail clearly. Do not silently strip and answer as text.

**Out of scope**

- Models that cannot call tools pretending to
- Equal quality, speed, context, or `max_tokens` (use each provider’s cap; do not shrink tools to fit)
- Voice STT/TTS cloud fallback (#349)
- Embeddings, image-gen, or OpenRouter `models:[]` routing
- 401/403 hop on a correctly resolved bad key
- Changing Silent / tool-UI policy
- mDNS, tag, or flash

**Regression:** Detect-only OpenCode / misconfigured xAI still do not silent-hop.

**Tests** (acceptance for the implementation issue; this docs PR does not write them):

- Replace the `fallback_ollama_first_turn_with_tools_rewrites_and_omits_tools` omit-first lock.
- Add leave-Go → registered OpenRouter with tools.
- Add secret-ref resolve on `type=openrouter`.

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
