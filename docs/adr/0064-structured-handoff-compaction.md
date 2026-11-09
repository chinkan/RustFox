# ADR-0064: Structured handoff compaction

- **Status:** Accepted
- **Date:** 2026-11-09
- **Amends:** [ADR-0003](./0003-conversation-compaction-redesign.md) (Q2 summary shape and how the extended document is stored). Q1–Q9 are not reopened.
- **Related:** [ADR-0051](./0051-compaction-ownership-option-a-and-harness-target.md) (Option A — RustFox always owns compact), #326 / #327 (protected tail), #376 (`PROMPT_HARD_CAP_PCT` 0.90)
- **Issue:** #384

## Context

ADR-0003 Q2 keeps a running summary and extends it on each Tier-3 compact. The live prompt in `conversation.rs::summarize_with_llm` asked for a blind extend of at most 300 words and formatted each message as `role: content` only. Assistants that are tool-calls-only therefore contributed an empty line (#326 P2). `apply_summary_layer` concatenated every new layer onto the previous text, and reload joined every `[SUMMARY]` row the same way, so the document grew without a bound. `build_compact_summary_prompt` and the empty-response recovery nudge still described a `## STATE` YAML block that the live summarizer does not produce.

Product lock (2026-11-09, including the supplement on tip `57146ed`) replaces that shape with a Hermes/OpenClaw-style structured handoff. Ownership stays ADR-0051 Option A. Nightly `memory/summarizer.rs` is out of scope.

## Decision

1. **Seven-section handoff.** Every new or extended compact summary uses these level-2 headings, in order. Empty sections stay, with the placeholder `None`.
   1. Goal
   2. Constraints & Preferences
   3. Progress — buckets Done, In Progress, and Blocked
   4. Key Decisions
   5. Relevant Files
   6. Next Steps
   7. Critical Context

2. **Update in place.** `summarize_with_llm` sends this schema and tells the model to update each section in place. The ≤300-word blind extend prompt is removed. Target length is about 1500–3000 tokens. The model is instructed to **MUST PRESERVE** verbatim: file paths, URLs, issue/PR numbers, commit SHAs, conversation / tool_call / message ids, and stated decisions, constraints, and task status strings. If the model drops an opaque id that was in the previous handoff or the unpruned range, that id is copied into Critical Context.

3. **Compact order (Tier 3 / turn boundary).** On the to-be-summarized range only (the protected tail from Q4 / #326 / #327 stays verbatim):
   1. Prune tool-result bodies longer than 160 characters to an 80-character head plus `[tool result pruned]`.
   2. Extend the previous summary into the seven-section handoff.
   Mid-loop Tier 1/2 masks in `prepare_messages_for_llm` are unchanged and are not this step.

4. **Summarizer input.** The transcript includes `tool_calls[].function` name and arguments, the tool_call id, and the (pruned) tool result. A tool_calls-only assistant is not an empty line.

5. **One stored document.** `apply_summary_layer` replaces the running summary with the extended handoff. The previous `[SUMMARY]` rows for that conversation are deleted and one new row is written. Reload uses the latest structured handoff when one exists, so older layers are not concatenated under the new budget. Legacy rows that are not a seven-section handoff still join, so a conversation that has never taken this path keeps its previous summaries.

6. **Same schema in the old prompt helpers.** `build_compact_summary_prompt` returns these instructions. The recovery nudge points at the structured handoff and no longer mentions `## STATE`. The compact boundary marker names the structured handoff.

7. **Unchanged pipeline.** Cadence (Q1), system-role injection, token trigger (Q3), protected tail (Q4), USER.md flush (Q5/Q6), defer-on-summary-fail with no truncation (Q7), persistence key (Q8), and `compaction_model` (Q9) stay as in ADR-0003. The hard cap remains `PROMPT_HARD_CAP_PCT` (0.90) × `context_window` (#376). ADR-0051 Option A is unchanged. No Portal or Telegram UI. No tag.

## Consequences

- A resumed session reads one stable handoff instead of a stack of prose layers.
- Opaque ids that the model paraphrases away remain in the document.
- Tool names and arguments from tool_calls-only turns reach the summarizer.
- Bulky tool output is pruned only on the summarized range, before the extend call.
- Nightly `memory/summarizer.rs` is unchanged. After a structured handoff exists, load prefers that handoff over a later non-handoff `[SUMMARY]` row.
