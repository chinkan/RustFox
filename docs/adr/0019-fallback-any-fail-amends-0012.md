# ADR 0019: Walk `[fallback] chain` on any primary failure (amends ADR-0012)

- **Status:** Accepted
- **Date:** 2026-10-05
- **Amends:** ADR-0012 (request-layer model fallback), ADR-0009 (per-model 429/5xx retry)
- **Notion:** https://app.notion.com/p/3f095d6dd45e810893e5cac373f2405a

## Context

Kan (WSL, v1.0.3) configured DeepSeek → Qwen flash → Ollama, but after OpenRouter
failures the chain never ran. Observed outer **HTTP 400** with `previous_errors`
containing upstream **429** (DeepSeek rate-limit). ADR-0012 only walks the chain when
`LlmHttpError::is_transient()` (429/5xx on the **top-level** status). Top-level 400
therefore hits `fallback_400_never_switches_models` and never reaches local Ollama.

Separately seen (out of slice ①): Wafer `duplicate_tool_name` at `tools[155]`, Relace
empty/too-long content — product wants those as later PRs, not blockers for fallback.

Kan product lock (2026-10-05, overrides the earlier “only unwrap 429-in-envelope”
narrow fix): **any** primary failure must walk `[fallback] chain` so a local
`ollama/…` entry can still answer. Cloud hang / 400 envelopes must not dead-end the turn.

## Decision

1. **Trigger (amends ADR-0012 §2):** after the primary’s own provider call returns
   `Err`, walk `[fallback] chain` in order for **any** failure **except** the hard
   exclusions in (2). Do not require top-level 429/5xx. Keep rewriting
   `ChatCompletion.model` to the model that actually answered.

2. **Hard exclusions (fail-fast, do not walk):**
   - HTTP **401** / **403** (auth / permission — permanent for this key).
   - Failures that never issued HTTP (missing provider/API key, invalid client
     config that aborts before request).
   - **Not** excluded: **402** (credits) — still walk so local Ollama can catch
     (OpenRouter docs treat 402 as “add credits”; local personal use wants offline
     rescue). Also not excluded: top-level **400**, timeouts, DNS/connect errors,
     body/JSON parse errors.

3. **Envelope parse (required, observability + classification helpers):** when the
   error body is OpenRouter-shaped, parse top-level / `previous_errors[*]` /
   `metadata` for **numeric 429/5xx** or documented `error_type` (e.g.
   `rate_limit_exceeded`). Use for logs and for recognizing upstream transient
   inside a 400 envelope. **Do not** string-guess “rate limit”. Trigger itself is
   still “any non-excluded Err” — parse does not gate the walk.

4. **429 / ADR-0009 interaction (Q5):** unchanged structure from ADR-0012 — **each**
   model (primary and every fallback entry) gets its **own** ADR-0009 retry budget
   before the next chain entry. No parallel double-fire of primary + backup.

5. **Scope of this ADR vs follow-up PRs:**
   - **①** (this ADR’s first impl PR): fallback-any-fail + envelope parse + tests
     (replace `fallback_400_never_switches_models` with “400 walks”; add 401/403
     never walk; 402 walks; timeout/network walks).
   - **②** separate PR: dedupe tool names before the LLM call (keep first).
   - **③** later optional PR: shorten / trim skills when context blows up.

6. **Still out of scope (unchanged from ADR-0012):** streaming, embeddings,
   image-gen; OpenRouter-native `models:[]` routing.

## Consequences

- ✅ Local Ollama (or any later chain entry) can rescue cloud 400/402/timeouts.
- ✅ Auth misconfig fails fast instead of hammering local with a bad cloud key story.
- ⚠️ More chain walks on “real” client 400s (bad schema) — acceptable for personal
  local reliability; bounded by `MAX_FALLBACK_CHAIN`.
- ⚠️ `fallback_400_never_switches_models` must be rewritten; ADR-0012 prose about
  “non-transient → return immediately” is superseded by this ADR for the chat
  choke point.

## Tests (impl PR ①)

Wiremock (or equivalent): primary 400 → fallback 200 (model rewritten); primary
401/403 → zero fallback hits; primary 402 → fallback tried; primary timeout /
connect fail → fallback tried; primary 429 exhausts ADR-0009 then fallback;
envelope body with outer 400 + inner 429 still walks (and logs inner code).
