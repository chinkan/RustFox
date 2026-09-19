# ADR-0009: LLM HTTP status retry policy (429/5xx)

- **Status:** Accepted
- **Date:** 2026-09-19
- **Branch:** `fix/llm-429-retry`
- **Context:** `journalctl` shows `openrouter API error (429 Too Many Requests) … qwen/qwen3.8-flash temporarily rate-limited upstream … limit_source: upstream_provider_shared_pool`, clustered at 12:00–12:50 HKT when the scheduled news-to-threads job and interactive messages hit the same shared-pool model concurrently.

## Problem

`chat_completion_with_retry` (provider.rs) only retried HTTP **200 with a missing `choices` field**. Any non-success status — including 429 — returned immediately (`"other errors (network, HTTP status, JSON parse) are returned immediately"`). No other layer covered it: `empty_response_retry_limit` handles empty replies, scheduler jobs default `retry_max: 0`, and the Telegram handler just surfaces the error. Result: a transient upstream throttle became a user-visible failure.

## Decision

1. **Classify by status** in the shared retry helper:
   - **429 + any 5xx** → retryable, budget `rate_limit_retry_limit` (new `[agent]` config key, default **3**, `0` disables → pre-fix fail-fast).
   - **Other 4xx (400/401/403/404…)** → fail fast, unchanged.
   - Missing `choices` → keeps `parse_retry_limit` (default 3), unchanged semantics.
2. **Independent budgets.** A 429 retry does **not** consume the parse-retry budget and vice versa (`rate_limit_attempts` tracked separately; backoff schedule for parse path computed from parse attempts only).
3. **Backoff:** honour `Retry-After` (delta-seconds **or** HTTP-date via rfc2822) when present; otherwise exponential `4s · 2^(n-1)`. Both paths cap at **60 s** and add **0–1 s jitter** so two RustFox callers never re-collide in lockstep.
4. **Observability:** each rate-limit retry logs `Transient error from {provider} (429) - retry n/max after T`. A user-facing 429 error now implies the full retry budget was actually spent.
5. `Retry-After` is extracted **before** consuming the body (`response.text()` moves `self`).

## Consequences

- ✅ Throttled-but-transient upstreams self-heal; worst-case added latency is bounded (3 retries × ≤60 s + jitter).
- ✅ Old `config.toml` files keep loading (serde default); opt-out is `rate_limit_retry_limit = 0`.
- ✅ Portal chat inherits the fix automatically (same `LlmClient` path).
- ⚠️ Retries mask persistent quota exhaustion at the request level. The systemic fix is **provider routing / fallback order** on OpenRouter (or BYO key) — tracked as follow-up, along with a **scheduler job-level retry backstop** (`retry_max > 0` for LLM-call failures). Those are deliberately out of scope here.
- ⚠️ Telegram-API-side 429s (different rate limiter) are not covered.

## Alternatives rejected

- **Dep-only solution (`tower` retry layer):** heavier refactor of provider trait plumbing for one call site; the loop already exists and needed only a status branch.
- **Retry on all non-2xx:** would hammer auth/config errors (401/400) that can never succeed on resend.
- **Ignore header, fixed exponential only:** OpenRouter sends `Retry-After`; ignoring it wastes time or collides on short windows.

## Tests

`src/provider.rs` wiremock matrix (tokio `start_paused`, request-count via scripted `AtomicUsize` responder): 429→200; always-429 exhausts budget (3 requests); `Retry-After: 1` honoured; 400 fails fast (1 request); 503→200; missing-choices path unchanged; `limit=0` fail-fast; mixed 429+missing-choices independence; `parse_retry_after` unit cases (seconds/date/garbage/negative); backoff cap at 60 s. Config default/override tests in `src/config.rs`.
