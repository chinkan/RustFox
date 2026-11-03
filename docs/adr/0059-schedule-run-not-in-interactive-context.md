# ADR-0059: Schedule runs are not main interactive LLM context

- **Status:** Accepted
- **Date:** 2026-11-03
- **Issue:** #369 (Must v1.0.9)
- **Amends:** [ADR-0052](./0052-interactive-lane-during-scheduled-run.md) Decision 6
- **Related:** [ADR-0048](./0048-telegram-inbound-reply-to-active.md) / #358 (reply-quote Opt A), [ADR-0054](./0054-scheduler-cross-run-progress.md) (checkpoint and daily progress), #338

## Context

ADR-0052 split the interactive lane from the scheduled lane so inbound chat is not a steer into a running job. Decision 6 left conversation rows on the owning chat: the job runner appended the schedule prompt and result, marked `[schedule:{id}]`, onto that chat (and, for a portal-origin task, onto the owner's Telegram thread as well). The next interactive turn loaded those turns in `messages[]`.

That is the wrong continuity. A schedule result in the main window showed up in a later chat trace (LangSmith `73a21568-21d6-4abf-b387-76e43a836a7a`) without the user asking for it. The scheduled turn also loaded prior interactive chat as its history window.

Product lock (2026-11-03): schedule prompt, tool traffic, and result do not enter later interactive `messages[]` unless the user reply-quotes the schedule Telegram message. The run itself does not use prior interactive chat as its continuity window. Telegram notify and the human-gate stay. Dual-lane and per-lane cancel stay. `scheduled_task_runs` stays the audit. No backfill that rewrites old rows. No tag.

## Decision

1. **Interactive load skips schedule segments.** A stored turn whose text starts with `[schedule:` is left out of the interactive LLM window (`ConversationManager` and `/btw`). Reply-quote text starts with `[Reply to` and is kept. Rows already in SQLite are not rewritten.

2. **The job runner does not append the segment to the owning chat.** Success, failure, cancel, timeout, and max-iterations still finish the run and still send Telegram. They do not copy the prompt or the result into the interactive conversation or the portal chat transcript. Portal → Telegram notify is delivery only.

3. **A scheduled turn loads a fresh conversation.** The key is platform `schedule`, not the owner's telegram/portal/web row, and a new id on every fire. Prior user turns are not in that window. The prompt on the turn still carries the schedule marker and, on a fresh same-day fire, the daily progress instruction (ADR-0054).

4. **Reply-quote is the pull path.** Quoting the schedule Telegram message composes that bubble into this turn's user text (ADR-0048 / #358) and nowhere else. A later turn with no reply does not receive the schedule segment. ADR-0048 still persists the composed user text of the turn that did reply.

5. **Checkpoint resume is unchanged.** Human Retry continues from the transcript checkpoint (ADR-0054), not from the interactive chat and not from the fresh schedule row. Daily progress files are unchanged.

6. **Lanes are unchanged.** Inbound during a schedule is still an interactive turn (ADR-0052). `Submit::Deferred` stays gone. Cancel stays per lane. `scheduled_incoming` keeps the owning bot and user for those keys.

## Consequences

- An interactive prompt that does not reply-quote a schedule message does not contain that run's `[schedule:{id}]` body or result.
- A schedule fire does not see unrelated prior user chat.
- Operators still get the Telegram result and the human-gate. Task run history in `scheduled_task_runs` and the portal task page is the durable record.
- Each fire opens a private `schedule` conversation row. Interactive load does not read it.
- Old segment rows remain in SQLite and stay out of the LLM window. They are not migrated.

## Rejected

- Cancelling the Telegram notify so the result never exists to quote.
- Putting the schedule back on `Submit::Deferred` or a shared cancel token.
- Backfilling or deleting old `[schedule:` rows.
- Treating reply-quote as a rewrite of Continuity (#279 / #282 / #285).

## Tests

- After a segment is recorded, the interactive chat row and the interactive prompt omit that run's body. A pre-existing `[schedule:` row is also omitted from the prompt.
- Reply-quote of the Telegram result is on the current user text only. The history window without that reply does not contain the result. A `[Reply to …]` turn is not dropped as a schedule segment.
- The schedule LLM key is not the interactive `(platform, bot, user)`, and that row does not contain prior user chat. A second fire gets a new id.
- `scheduled_incoming` still routes to the owning bot and user.
