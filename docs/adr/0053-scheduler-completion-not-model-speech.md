# ADR-0053: Scheduled completion is not model speech

- **Status:** Accepted
- **Date:** 2026-10-31
- **Related:** [ADR-0013](./0013-dead-letter-rerun-queue.md) (manual gate, no silent replay), [ADR-0038](./0038-scheduler-human-gate-copy-inline.md) (gate skeleton), [ADR-0043](./0043-opencode-detect-and-peer.md) / [ADR-0045](./0045-register-time-tool-name-dedupe-and-empty-progress-clarify.md) (the three rewrites this ADR makes visible to the scheduler)
- **Issue:** Closes #330, Closes #333

## Context

`run_one_scheduled_job` treated every non-max-iterations `RunStop::FinalResponse` as `scheduled_task_runs.status = completed`. Guard rewrites in `loop_runner` replace the model text and still return `FinalResponse`, so a clarify sentence was stored as success: no retry, no dead-letter, no owner DM.

#330 covers the three rewrites that already exist (cold-intro after a failed regenerate, post–empty-progress lost-context, short meta-token filler). #333 covers no-ops that never touch a guard: the model itself asks the user to restate the task, or prints a capability menu, and the run is still `completed`. Those runs have no file (or post) to show for the task.

A fresh artifact is the reliable proof when the operator configured one. Speech length alone is not: a weather note is short and real. This ADR does not backfill the live task rows and does not call the Threads API.

## Decision

1. **Guard identity lives on the outcome, not on `RunStop`.** `RunStop` stays a `Copy` unit the turn gate compares. `LoopOutcome::FinalResponse` and `RunOutcome` gain `rewritten_by_guard: Option<GuardKind>` with exactly three variants: `ColdIntroClarify`, `LostContextClarify`, `MetaFillerAbort`. The regenerate `continue` does not set the flag. Early returns that are not one of those three (empty-progress force-clarify, "trouble processing", tool-loop stop) stay `None`. Interactive `process_message` still returns only the text, so chat UX is unchanged.

2. **The scheduler asks `judge_schedule_completion` before it writes `completed`.**

   | `expected_artifact` | fresh file | guard rewrite | short clarify/menu | result |
   | --- | --- | --- | --- | --- |
   | unset | — | yes | — | `failed` + manual gate |
   | unset | — | no | yes | `failed` + manual gate |
   | unset | — | no | no | `completed` (including a legitimately short reply) |
   | set | yes | — | — | `completed` (warn if a guard or the heuristic also fired) |
   | set | no | — | — | `failed` + manual gate; `fail_reason` names the glob |

   A fresh file is the final verdict. The guard flag and the heuristic do not override it. Length under 200 alone never fails a run. `expected_artifact` NULL (or blank) is the compat path for long substantive replies.

3. **Manual gate, not auto re-fire.** Failure uses `enqueue_manual` (first run) or `mark_awaiting_user` (existing `rerun_id`), then `HumanGateKind::UnverifiedResult` on the ADR-0038 skeleton ("result not verified", did **not** retry automatically). The model text is still delivered. The runner does not retire the one-shot, does not `mark_done`, and does not `supersede_live_for_task` (that would cancel the gate just opened). A no-op may already have partial side effects, so this is not the transient `enqueue()` path.

4. **Artifact check.** `scheduled_tasks.expected_artifact` (TEXT, nullable) and `artifact_grace_secs` (INTEGER, default 900). A hit is a regular file whose `mtime >= run_started_at - lookback`. `lookback` is `artifact_grace_secs` and is never tighter than 300s (`ARTIFACT_PRESTART_SKEW_SECS`), so a file from `run_started_at - 300s` always counts. The default 900s also accepts a file written in the few minutes before the run row was stamped. Operators set the column to 300 to use that window exactly. There is no upper bound of "grace after start": a long run's file is still fresh. The scanner floors stored values `1..=299` up to 300 and treats `<= 0` as 900. Create/update APIs reject grace below 300 so a new row cannot claim a tighter window.

   Relative globs resolve under the resolved RustFox home. Absolute globs need a literal directory prefix (`/*.jpg` is rejected). `..` and NUL are rejected. `**`, `*`, and `?` only. Symlinks are ignored. Directories are not hits. A missing directory is "no match", not a scan error. Home unset plus a relative glob fails closed.

5. **Heuristic is auxiliary.** When no fresh artifact is in play, a reply shorter than 200 characters that matches a clarify/menu phrase is `failed`. Phrases: `你再 send 一次`, `你想我做咩`, `想繼續之前邊件事`, `即係做咗未`, `未收到`, `冇收到`, `Let's continue from the prior thread`, and (ASCII, case-insensitive) `didn't receive`, `without progress`, `wasn't making progress`. A menu is at least three lines starting with `- ` plus `Fox 在線`, `Fox 喺度`, or the substring `skill`. Empty-progress force-clarify is not a `GuardKind`; the phrase `wasn't making progress` is what fails that sentence on the schedule path.

   **False-positive risk:** a real short task whose text contains one of those phrases (for example `未收到` inside a weather note) is failed until an `expected_artifact` glob matches. Do not use the heuristic as the only gate for tasks that are supposed to be short. Length under 200 without a phrase stays `completed`.

6. **Who can set the glob.** Optional on `schedule_task` (`expected_artifact`, `artifact_grace_secs`) and on portal create/update JSON (`expectedArtifact`, `artifactGraceSecs`). The portal task form is unchanged. Migration adds the columns and leaves `expected_artifact` NULL. This ADR does not seed globs onto existing rows.

   Operator examples, not applied by this change (date token written as `*`):

   | Task | Glob |
   | --- | --- |
   | arXiv briefing | `obsidian-vault/arxiv-briefings/arxiv-briefing-*.md` |
   | newspaper frontpage | `workspace/newsheet/frontpage-*.jpg` |
   | daily note | `obsidian-vault/daily/daily-*.md` |
   | inbox sweep | `_inbox/_sweep-report-*.md` |

7. **Non-goals.** Threads API read-back (`get_thread_replies` or a stored post id) for `debe2062`, `67d7bc76`, and `2d557a52` is parked. No version bump and no tag.

## Consequences

- A guard-rewritten clarify on a schedule with no fresh artifact is `failed` and shows up in the dead-letter queue. The same sentence in interactive chat is still shown.
- Tasks with `expected_artifact` NULL keep today's success path except for the guard flag and the short clarify/menu heuristic.
- A matching fresh file completes the run even when the reply was rewritten. The scheduler logs a warning.
- Setting a glob that the task does not actually write will dead-letter every run until a file matches.
