# ADR-0060: Skill catalog is a short trigger plus one Load with

- **Status:** Accepted
- **Date:** 2026-11-03
- **Issue:** #332
- **Product lock:** [comment 6594946602](https://github.com/chinkan/RustFox/issues/332#issuecomment-6594946602) (2026-11-03, recorded at `530d079`). This implementation is a tip on `4667697` (includes #368).
- **Does not block:** no version bump. No tag.

## Context

Every main-loop call was carrying the full skill catalog in the system prompt. Issue #332 measured about 15,002 bytes of parseable `description` text inside a ~52KB prompt, and proposed option A: keep only the name and a one-line trigger of at most 80 bytes (~6.8KB for 83 skills), and load `SKILL.md` with `read_skill_file` when needed.

That 6.8KB figure is an optimistic floor. A catalog line the model will actually follow has to name the tool. `Load with: read_skill_file(skill_name="…", relative_path="SKILL.md")` is already on the order of 80 bytes before the trigger prose or the skill name. Putting a long `Load with:` inside every description (some were already ~265 bytes) can make the catalog larger, not smaller. The corrected target is an `# Available Skills` section of about 12KB, with the tool name kept.

The full skill body must stay on demand. `read_skill_file` already returns it (with offset/limit from #337). Stuffing `SKILL.md` back into the system prompt is out. ADR-0059 is used by #369, so this decision is ADR-0060.

## Decision

**A1, not A2.** Optional frontmatter `trigger:` supplies the catalog prose. When it is absent or blank, the catalog truncates `description` and leaves the stored description alone.

1. **`# Available Skills` is name + short trigger + one load clause.** Instruction skills (no `model`, empty `tools`) are the only entries. The line is `- **{name}**: {prose} Load with: read_skill_file(skill_name="{name}", relative_path="SKILL.md")`. The tool name is required.

2. **Prose cap is 24 UTF-8 bytes** (`CATALOG_TRIGGER_MAX_BYTES`), cut on a char boundary, with `…` only when the text was cut. 24 is the largest cap at which an 83-skill section still fits in 12 KiB: heading, the existing preamble, realistic names, every prose field at the cap plus the ellipsis, and one synthetic load clause. The fixture measures **12,260 bytes** (budget `12 * 1024` = 12,288). That replaces the 6.8KB / 80-byte floor, which did not count the load clause.

3. **`trigger:` wins when it is non-empty.** It is a single-line scalar, same parser as `description`. The portal and any other reader keep `Skill.description` unchanged. The portal list endpoint's existing 160-character trim is unchanged.

4. **One `Load with:` per line.** If the chosen source (the trigger, otherwise the description) already contains `Load with:`, do not append another. When that clause names `read_skill_file`, keep it, including a non-`SKILL.md` path, and cap the clause at 180 bytes. When the marker is present but does not name the tool, emit the synthetic clause instead so the tool name is still there, still once. A trigger that does not itself contain `Load with:` gets the synthetic clause even if the hidden description has one — the description text is not copied onto the line.

5. **The skill body is not part of the system prompt.** `build_context` / `available_skills_section` never copy `Skill.content`. The agent loads it with `read_skill_file`.

6. **Subagent skills and `agents/` lines are unchanged.** They stay on `invoke_agent` with the existing 200-character description cap. They are not the `# Available Skills` section.

7. **Out of this decision:** option B (`INDEX.md` line tables), tool whitelist narrowing, duplicate-tool fail-fast, the 52KB breakdown, and #336. A residual `System prompt is large` warning is acceptable; skills were about 29% of that prompt.

## Why not A2

A2 is the same 24-byte cap and the same single synthetic `Load with:`, without a `trigger:` field. Every catalog line would be a chopped description. A1 gives authors a deliberate short line and leaves the portal description intact. The byte budget does not improve under A2, so A1 is the better catalog at the same size.

## Consequences

- Instruction-skill catalog lines shrink from up to 200 characters of description plus a second `Load with:` toward ≤24 bytes of prose plus one clause.
- Skills that do not set `trigger:` show a truncated description in the prompt only.
- Authors who already wrote `Load with:` into a description keep that clause once, instead of seeing it doubled.
- `creating-skills` documents the optional field. No tag.

## Rejected

- Injecting `SKILL.md` bodies into the system prompt.
- Closing #332 with an INDEX-only change.
- Dropping the tool name to chase the 6.8KB floor.
- Appending a second `Load with:` after one already in the catalog text.
