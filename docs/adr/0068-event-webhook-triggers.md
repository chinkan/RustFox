# ADR-0068: Portal HMAC webhook triggers

- **Status:** Accepted
- **Date:** 2026-11-16
- **Issue:** #89 (v1.0.10 Should). Product Soft / contract ACCEPT is comment 6910869106. The contract source is comment 6910766242.
- **Does not amend:** [ADR-0007](./0007-portal-lan-access-tailscale-telegram-entry.md), [ADR-0065](./0065-portal-tailscale-settings-serve.md). Funnel and any public tunnel stay off.
- **Reuses:** [ADR-0059](./0059-schedule-run-not-in-interactive-context.md) schedule conversation isolation, the scheduled-run iteration cap (`max_iterations`, default 25) and wall-clock cap (`scheduler.run_timeout_secs`, default 900) from [ADR-0024](./0024-scheduler-run-timeout.md) / [ADR-0054](./0054-scheduler-cross-run-progress.md). [ADR-0067](./0067-scheduled-auto-resume.md) is not given a second budget.

## Context

RustFox already fires cron and one-shot work through `build_fire_closure` → `ScheduledJobRequest`, isolated from the interactive conversation. It cannot start that same path when an external event arrives.

The 2026-09-29 issue asked for webhook, poll, and file-watch, a `TriggerEngine`, and a hash-chain ledger. That remains the long-term direction. This decision is only the locked v1.0.10 slice: one trigger source, on the portal the operator already runs.

## Decision

1. **Source A only.** An HMAC webhook on the existing portal Axum bind (`POST /api/hooks/{id}`). It is reachable only where the portal already is (loopback, LAN, or tailnet Serve). Outbound poll and a provider push webhook plus public ingress are residual. This ADR does not change portal bind or Tailscale Serve policy.

2. **Registry.** A new `triggers` table. Cron and one-shot `scheduled_tasks.trigger_type` stay `recurring` | `one_shot`. There is no third trigger type and no `TriggerEngine` trait.

3. **Dispatch.** An accepted event builds a `ScheduledJobRequest` the same way a fire closure does (`schedule_id` set, fresh schedule conversation, run row in `scheduled_task_runs`). The run is not written into the interactive conversation. `scheduled_task_runs.task_id` for these rows is the trigger id. The foreign key that required a `scheduled_tasks` parent is dropped so that row can exist without inventing a scheduled task. A new `origin` column is `schedule` (default, every existing insert) or `event`. Cron and one-shot arming, overlap, and completion are unchanged.

4. **Caps.** At most one `running` event row per trigger. At most three `running` event rows in the process. At most 30 accepted events per 10 minutes per trigger. Over the rate cap or either concurrency cap, the webhook returns `429` with `Retry-After` and does not queue the body. Raw body larger than 16 KiB returns `413`. A fresh event run uses the configured scheduled-run iteration cap and `scheduler.run_timeout_secs` (defaults 25 and 900). It does not get a second budget. Auto-resume still belongs to scheduled tasks: an event run is not a `scheduled_tasks` row, so the existing auto-resume dispatch is not started for it and no parallel budget is added.

5. **Untrusted payload.** The model sees the body only inside a fixed user-turn template marked `UNTRUSTED EVENT PAYLOAD`. At most 8 KiB of payload text is included. A longer body is cut on a UTF-8 boundary and followed by `[truncated]`. The operator prompt sits outside that block. The payload is not copied into the system prompt, and retrieval is not run on it. No tool reads a stored event blob.

6. **Authentication.** The webhook does not use the portal session. It checks `X-RustFox-Signature` as hex HMAC-SHA256 of the raw body, optionally prefixed with `sha256=`. The key lives in SecretStore under `trigger.{id}.hmac`. A missing or wrong signature is `401`. A disabled trigger with a valid signature is `403`.

7. **Dedup.** `Idempotency-Key` wins when it is non-empty; otherwise `X-Idempotency-Key`; otherwise `sha256` of the trigger id bytes concatenated with the raw body. The same key for that trigger within 24 hours returns `200` with `deduped: true` and does not start a second run. A replay does not consume the 30-per-10-minutes budget.

8. **Portal.** Authenticated list, create, enable, disable, webhook path, and HMAC secret set/rotate through SecretStore. `bot_id` is stamped at create from the request when the operator sends one, or from the only configured bot when they do not. Several bots and no `botId` is `400`. The portal does not invent a shim bot for events and does not retarget the row later. There is no bot-picker component on this tree; the section lists configured bot ids when more than one exists.

9. **No automatic send, publish, or post.** While an event run is in progress, a tool whose name has a `send`, `publish`, or `post` segment is not executed. The tool result is a draft of the arguments and tells the model a person must confirm later. No per-tool confirm button exists for those tools, so this train is draft-only. The existing schedule reply to the owner is unchanged: it is the run result, not an external post. Nested tool calls in the same run are covered by the same gate.

## Consequences

- A producer on loopback or the tailnet can POST a signed body and start one isolated agent run.
- Cron and one-shot tasks keep their current trigger types, arming, and overlap rules.
- A GitHub or Notion push webhook still cannot reach this bind unless a later decision accepts public ingress.
- Event runs that hit the iteration or timeout cap are recorded on `scheduled_task_runs` and are not given a separate auto-resume budget.

## Residual

File-watch, outbound poll, provider push plus public ingress, other product sources, a hash-chain ledger, a `TriggerEngine` extraction, Funnel, and a full Automations UI are out of this decision.
