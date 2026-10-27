# ADR-0045: Register-time tool name dedupe and empty-progress clarify

- **Status:** Accepted (#286 shipped A+B; #298 amends short meta-token FinalResponse filler class)
- **Date:** 2026-10-26
- **Related:** #231, ADR-0019 (send-time duplicate drop is a symptom shield, not the root fix), live tip `7b7bbaa`, #286 (A+B shipped), #298 (short meta-token filler amend)
- **Issue:** #286 Closed (A+B); #298 thin amend (this PR Closes #298)

## Context

On live tip `7b7bbaa`, with Active OpenCode / `deepseek-v4.1-flash`, a mid-task turn produced no user-visible FinalResponse. Logs showed repeated `Dropping duplicate tool function.name; keeping first occurrence tool_name=mcp_threads_schedule_post` and tight `llm_call` cycles (tools≈160). The hang class is empty-progress / duplicate-tool loop (related to historical #231): not the max-iterations sentence path, and not a process crash or delivery failure.

ADR-0019 already noted a follow-up track to dedupe tool names (keep first) as a separate PR from fallback-any-fail. The send-time drop in the LLM path is a belt / symptom shield; it does not stop the agentic loop from burning iterations without a FinalResponse when duplicate `function.name` values are registered (for example MCP tools that double-report the same name).

This document records the product locks for register-time dedupe and an empty-progress clarify safety net. It does not implement Rust, tests, CHANGELOG, or a release tag.

## Decision

Product locks are authoritative for the first implementable slice after this ADR is on `main` and the usual four gates pass:

1. **(A) primary — register-time first-wins:** At tool / MCP **register** time, keep the first `function.name` only; reject or skip later duplicates. Applies to **all** tools, not Threads-only. The `llm.rs` send-time “Dropping duplicate” path may remain as a belt; it is **not** the root fix. Why Threads double-reports upstream is an optional follow-up issue and does **not** block #286.

2. **(B) safety net (same track; does not replace A):** When loop_detection is enabled, after **N = `agent.loop_detection.threshold` (default 3)** consecutive iterations that are only empty-progress and/or duplicate-name drops (no useful tool progress / no user-visible FinalResponse) → **force a clarify FinalResponse** aligned with existing #231 / empty-progress copy. Do **not** mention MCP or duplicate internals to the user. Do **not** emit a silent empty FinalResponse.

3. **Scope:** shared agent loop, all Active peers. No token / Active / leave-Go / W2 / #194 / #202 changes.

4. **Sequencing:** this docs PR **Refs #286** → four gates squash → impl PR **Closes #286** (A+B) → soft QA/QA2 only. **No tag.** Park #194 / #202.

5. **Impl MUST tests** (acceptance for the later impl PR; not implemented in this docs PR):
   - (a) register the same name twice → one entry in schema / registry
   - (b) N× empty-progress / duplicate-drop → clarify FinalResponse (no unbounded `llm_call`)


## Decision — Short meta-token FinalResponse filler (#298)

Accepted delta after live tip `13714ff` (OpenCode Go + DeepSeek): short content-channel fillers such as `Output.` / `Emit.` / `Final.` / `Write.` / `Ok.` can pass empty-progress clarify (non-empty), LoopDetector, and cold-intro guards, and ship to Telegram as a normal FinalResponse. Class is **not** DSML (#307 / #308). Amend in place; keep (A) register-time dedupe and (B) empty-progress clarify unchanged.

1. **Shared FinalResponse path (all Active peers, not OpenCode-only):** before a text-only FinalResponse ships, detect short meta-token / filler replies.

2. **Allowlist (trim):** `{Output, Emit, Final, Write, Ok}` with optional trailing `.`. Matching is case-insensitive on the token. **`Done` omitted** — a legitimate FinalResponse of `"done"` / `"Done."` must not be treated as filler (false-positive on #298 QA `langsmith_child_runs`).

3. **Filler when:**
   - **single stub:** entire trimmed content is only one allowlist token + optional `.`; **or**
   - **≥3-line / alternating streak:** ≥3 non-empty lines, each an allowlist stub (tokens may differ / alternate).

4. **On filler:** **abort → clarify** FinalResponse (same family as (B) empty-progress clarify copy). Do **not** silent-drop. Do **not** mention MCP / DeepSeek / internals in user-visible copy.

5. **False-positive guard:** benign short substantive answers (not only an allowlist stub / not a ≥3-line stub streak) must still FinalResponse — including `"done"` / `"Done."` (hence `Done` is not on the allowlist).

6. **Unchanged:** (A) register-time first-wins; (B) empty-progress clarify (N = `agent.loop_detection.threshold`); no DSML strip/recover changes (#307 / #308); no leave-Go / Active peer / W2 / #194 / #202; parallel ADR-0050 / #309 left alone. Soft = fixtures + Soft TG on QA/QA2 later (QA); **no tag**; not in v1.0.8 Soft/tag train.

7. **Impl MUST tests:** filler / streak → clarify; benign short → FinalResponse; empty-progress path unchanged.

## Non-goals

- Restart / token / Active provider changes.
- W2 cold-intro matcher work (#283 / #285) — different symptom class.
- A release tag from this docs change or the later impl PR without owner.
- Threads upstream root cause (why the MCP schema double-reports); optional follow-up, does not block #286.
- Rust, tests, or CHANGELOG in this docs PR.
- #194 / #202.
- Replacing (A) with send-time drop alone; send-time drop remains belt only.
- Mentions of MCP / duplicate internals in user-visible clarify copy.

## Consequences

- Register-time first-wins stops drop-and-continue quota burn from duplicate `function.name` entries in the schema.
- Safety net (B) reuses existing `agent.loop_detection` knobs (`threshold`, default 3) rather than inventing a parallel counter.
- First-wins trade-off is acceptable: later duplicate registrations are skipped; callers that relied on a second same-name tool must rename.
- (B) may still fire if (A) is imperfect or another empty-progress class appears; that is intentional belt-and-suspenders, not a substitute for (A).
- Tracking for A+B closed on #286. #298 adds the short meta-token filler safety net on the shared FinalResponse path (abort→clarify; Soft QA/QA2; no tag; not v1.0.8). #194 / #202 remain parked.

## Related

- ADR-0019 — noted follow-up ② “dedupe tool names before the LLM call (keep first)”; send-time drop is a symptom shield. This ADR moves the root fix to **register** time and adds the empty-progress clarify safety net.
- #231 — historical empty-progress / duplicate-tool loop class; clarify copy alignment for (B).
- #286 — Closed: (A) register-time dedupe + (B) empty-progress clarify shipped.
- #298 — thin amend: short meta-token / filler FinalResponse abort→clarify + streak; Soft QA/QA2; no tag; not v1.0.8.
- #307 / #308 — DSML strip/recover (different class; out of scope for #298).
- Live tip `7b7bbaa` — incident base for #286 (Active OpenCode / `deepseek-v4.1-flash`; duplicate `mcp_threads_schedule_post`).
- Live tip `13714ff` — evidence base for #298 short meta-token FinalResponse filler (OpenCode Go + DeepSeek).
- #194 / #202 — parked; out of scope.
