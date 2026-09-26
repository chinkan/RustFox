# ADR-0012: Request-layer model fallback chain (chat path)

- **Status:** Accepted
- **Date:** 2026-09-26
- **Branch:** `fix/429-backup-model`
- **Depends on:** ADR-0009 (status classification + retry budgets)

## Context

ADR-0009 made a single model's 429 self-heal for *seconds*-long blips. The real failures are
*ten-minute* upstream shared-pool congestions on `qwen/qwen3.8-flash`. Retrying the same model
against the same pool cannot help. Kan's grill answer Q1/Q2: interactive chat should **switch
model immediately** (own budget), backups may be a different model, possibly a different provider;
chain order is user-configured.

`[fallback] chain` already existed in `config.rs` (`FallbackConfig`) and `build_providers()` even
returns it — but **nothing consumed it** (dead config). This ADR wires it.

## Decision

1. **Location: `LlmClient::chat_completion_with_model`** — the single choke point used by the
   agent loop, compaction, summarizer, and portal chat. Providers stay dumb.
2. **Trigger:** the primary fails with a *transient* error (429/5xx via typed `LlmHttpError`,
   downcast — replaces ADR-0009's log-only classification). Non-transient (400/401, network
   parse) → return immediately, unchanged.
3. **Order:** primary model, then each entry of `fallback_chain` in configured order. Entries are
   fully-qualified `provider/model` resolved through the same registry. Each fallback gets its
   own provider-level ADR-0009 budget (one pass through `chat_completion_with_retry`).
4. **Cap:** chain is truncated to `MAX_FALLBACK_CHAIN` (config-validated, default 3 entries) —
   unbounded chains multiply worst-case latency.
5. **Observability:** every switch logs `WARN fallback: {primary} → {model} after {err}`; the
   returned `ChatCompletion.model` is rewritten to the model that **actually answered** — no
   silent lies in history/UI. LangSmith metadata records the chain position.
6. **Scope guard:** fallback applies to chat completions only (tools included). Streaming
   (`stream_text`), embeddings, and image-gen paths are out of scope this PR.
7. **Safety:** a fallback model lacking tool-calling can technically degrade agent behaviour;
   config load **warns** (does not reject) when a chain entry's provider/model isn't in the
   registry (typo = silent no-op at runtime).

## Consequences

- ✅ Upstream congestion on one shared pool becomes a sub-minute, user-invisible event when a
  healthy backup exists (DeepSeek V4 Flash, GLM-5.3, or local Ollama).
- ✅ Zero migration: no chain configured (default empty) → behaviour identical to ADR-0009.
- ⚠️ Two models in one conversation turn may produce stylistic drift — acceptable; logged.
- ⚠️ Cost/latency: worst case primary budget + N fallback budgets. Bounded by chain cap.

## Tests (TDD, wiremock, `start_paused`)

Primary 200 → chain never touched (request counts). Primary always-429 → fallback #1 200 with
rewritten `model`. Fallback #1 5xx → fallback #2 tried in order. Primary 400 → **no** fallback.
Empty/missing chain → exactly ADR-0009 behaviour. Unknown chain entry → warn + skip, primary
error preserved. `LlmHttpError` downcast carries status.
