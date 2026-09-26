# CONTEXT: 429 resilience — backup model + dead-letter queue + watchdog

- **Date:** 2026-09-26 · **Branch:** `fix/429-backup-model` · **Base:** main @ fd42438 (#59 merged)
- **Follows:** ADR-0009 (request-level 429/5xx retry, shipped via PR #56)

## Problem (observed, not hypothetical)

11 scheduled-task runs in 7 days died on `429 … qwen/qwen3.8-flash … upstream_provider_shared_pool`.
ADR-0009's retry budget (3 × ≤60s ≈ 28s worst case) is the wrong order of magnitude: upstream
congestion windows run **tens of minutes**. Same-model resend = same dead shared pool.

## Grill resolutions (Kan, 2026-09-26)

| Q | Decision |
|---|----------|
| 1 | **(a)** Chat: on transient failure, switch to backup model immediately (own retry budget), then give up. Scheduled tasks: dead-letter **queue**. |
| 2 | Ordered **fallback chain**, fully-qualified `provider/model`, may live on **another provider**; chain = existing-but-unwired `[fallback] chain` config key (was dead config). |
| 3 | **Two-strike semantics:** run dies → queue → watchdog auto re-fires **once** (attempts=1). If the re-fire **also dies** → DM Kan asking whether to retry (human gate, `task_reruns` tool). No blind repeated re-fires: LLM state is not resumable mid-run, and re-fire replays the whole agent loop → side-effect risk (duplicate publishing). |
| 4 | Watchdog polls **hourly**. Re-fire success output is **indistinguishable** from a normal run (no "♻️ delayed" line; surfacing into the morning report is a skill-level concern, not code). |
| 5 | Watchdog = **tokio interval job in main** (system-level, not a user scheduled_task row). New table `pending_reruns(task_id, original_run_id, fail_reason, attempts, next_eligible_at, status)`. |
| 6 | Merged #59 first (it touched `config.rs`/`main.rs`); this branch cuts from main tip. |

## Architecture (seams found during recon)

- `provider.rs::chat_completion_with_retry` — budgets spent → `Err` bubbles; error carries
  `"{provider} API error ({status}): {body}"`. Now a typed `LlmHttpError` so classification
  downcasts instead of string-matching.
- `llm.rs::chat_completion_with_model` — the single choke point every caller uses (agent loop,
  conversation compaction, summarizer, portal chat) → request-layer fallback lives here.
- `main.rs` background runner loop — persists run rows (`running`→`completed`/`failed`), DMs
  errors → queue enqueue/complete/await transitions live here, keyed by new
  `ScheduledJobRequest::rerun_id: Option<String>`.
- `agent.rs::build_fire_closure` — constructs `ScheduledJobRequest` → rerun dispatch mirrors it.
- `scheduler/reminders.rs::ScheduledTaskStore` — has the `Arc<Mutex<Connection>>`; RerunQueue
  shares it.

## Explicitly rejected

- OpenRouter-native `models:[…]+routing:fallback` — hides which model answered, zero control at
  task layer, doesn't solve queueing. (Documented in ADR-0012.)
- Reusing `scheduled_task_runs.status` for queue state — conflates audit history with control
  state; a dead run row must stay `failed` as evidence.
- Auto-retry >1 without asking — violates two-strike; duplicate side effects (Threads history).

## Verification bar

Wiremock matrix (fallback chain), in-memory SQLite queue tests, crash-recovery reset test, plus
existing suite green (`cargo test`, `clippy -D warnings`, `fmt`).
