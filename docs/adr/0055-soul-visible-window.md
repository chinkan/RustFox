# ADR-0055: Soul prompt window keeps the recent tail

- **Status:** Accepted
- **Date:** 2026-11-02
- **Issue:** Closes #347
- **Does not block:** v1.0.8 tag. No version bump. No tag.

## Context

`build_system_context` injected each soul file with `truncate_to(_, 8_000)`, a head slice. On a live home (2026-11-02) that kept 100% of a short `SOUL.md`, 20.8% of `USER.md`, and 1.87% of `AGENTS.md`. Every `##` lesson after 2026-06-23 was invisible. `update_soul_file` append only limits a single call to 100KB, so new lessons landed past the cut with no feedback. The model kept re-learning the same mistakes and appending them again.

Product (2026-11-02) accepts the window below and adds three notes that do not change the cut:

- SOUL is usually short. Prefer full inclusion: a file that fits is copied whole. Apply the window only when the file is over budget.
- USER and AGENTS are recent-tail primary. The preamble exists to keep identity and standing preferences, and it stays at most 25% of the budget.
- Do not raise the 8,000-character quota to make today's files fit. Do not replace this window with same-role nested summarization.

## Decision

1. **Budget stays 8,000 characters** (`SOUL_PROMPT_CHAR_BUDGET`). One budget for `SOUL.md`, `AGENTS.md`, and `USER.md`. Do not raise it. Do not summarize older sections into the preamble on inject or on write.

2. **Short files are injected whole.** Character length `<=` budget returns the file unchanged and sets no omission marker. That is how a typical `SOUL.md` stays complete. There is no separate SOUL quota and no SOUL-only code path.

3. **Sectioned files over budget.** A section starts at a line-start `## ` (hash, hash, space). `###` is not a boundary. The preamble is everything before the first section. Keep `min(preamble chars, floor(budget * 0.25))` from the start of the preamble (identity and standing preferences live at the top). Give every remaining character to sections, chosen from the end of the file toward the start. Assemble preamble, then the chosen sections from older to newer. Never reverse-dump sections.

4. **One section that does not fit.** Keep its head and hard-truncate its tail on a char boundary. Insert a truncated marker at the cut. Stop. Do not take older sections after that.

5. **No `## ` headings.** The window is the first 25% of the budget and the last 75% of the budget (head and tail of the file), with a marker between them.

6. **Markers describe the window.** They say preamble plus the most recent sections (or the 25/75 head-tail when there are no sections) and point at `read_soul_file` / consolidate. They do not consume the 8,000-character budget. The old "File truncated" line is gone.

7. **Write path is a soft warn.** `update_soul_file` still writes. The 100KB per-call content limit stays a hard reject. Hard-rejecting large files is out of scope. After a successful write the tool result warns, and tells the agent to consolidate or use `mode=replace`, when any of these is true:
   - the newly written span is not fully inside the visible window (append: the appended text; replace or a new file: the whole result), or
   - the file is longer than 200,000 characters (`SOUL_FILE_WARN_CHARS`), or
   - the file has more than 200 `## ` sections (`SOUL_FILE_WARN_SECTIONS`).

   A visible append under those thresholds stays quiet, including when older text is already outside the window.

8. **Persona locks are unchanged.** `resolve_persona_soul_files` still binds the persona `SOUL.md` and the shared home `AGENTS.md` and `USER.md`. This ADR only changes the injection window.

## Consequences

- Recent AGENTS lessons and recent USER preferences reach the prompt. A short SOUL file is unchanged.
- An append that lands in the truncated tail of a huge section no longer succeeds silently.
- Files may still grow without a hard cap. The warn is not a quota. Middle history stays on disk for `read_soul_file`.
- No extra model call was added to compress soul files.

## Rejected

- Raising 8,000 so a 400KB `AGENTS.md` fits in the prompt.
- Same-role nested summarization (fold older `##` sections into a summary in the preamble on each inject or each write).
- Head-only truncation plus a hint to call `read_soul_file`.
- Hard-rejecting writes that exceed the warn thresholds.
