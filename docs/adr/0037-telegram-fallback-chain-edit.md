# ADR-0037: Telegram `/fallback` chain list / add / remove

- **Status:** Accepted
- **Date:** 2026-10-21
- **Target release:** v1.0.7+ (does not block v1.0.6)
- **Depends on:** ADR-0012 (request-layer fallback chain), ADR-0019 (walk on any primary failure), ADR-0035 / ADR-0036 (persist + live memory pattern for operator edits)
- **Related:** #233 / #236 (Active persist + live; #237 was deferred non-goal there), `config_edit` bak / atomic write path
- **Issue:** #237

## Context

Kan Should (2026-10-19), deferred from #233 / ADR-0035 non-goals and restated
as out-of-slice in ADR-0036: there is **no** Telegram (or portal) UI today to
**view / add / remove** `[fallback].chain` entries. Operators must hand-edit
`config.toml` and restart to change the ordered backup list.

Fallback **walk semantics** already exist and stay locked elsewhere:

- ADR-0012 — request-layer ordered chain of fully-qualified `provider/model`
  entries resolved through the registry.
- ADR-0019 — walk the chain on any primary failure except hard auth exclusions.

Runtime already caps the in-memory chain at **`MAX_FALLBACK_CHAIN = 5`**
(`LlmClient` attach path). Boot / attach may still truncate oversized config
for safety; this ADR locks the **Telegram edit** path to **reject** an add
that would exceed the cap (no silent truncate on operator add).

Product lock (PO grill on #237, 2026-10-21 HKT; Round 1 Q1–Q5=A; Round 2
Q6–Q8=A, Q9=C) is authoritative for the Decision below.

## Decision

1. **Telegram-first surface (Q1 A / Q5 A):** add allowlisted Telegram
   **`/fallback`** with **list + add + remove**. Portal fallback editor is
   **out** of this slice (unless a later grill expands).

2. **No reorder this slice (Q2 A):** operators cannot reorder entries via
   Telegram in v1 of this command. Order remains config / append order.
   Reorder (if wanted) is a later grill.

3. **Persist + live immediately (Q3 A):** every successful add/remove writes
   disk `[fallback].chain` **and** updates live in-memory chain **now**,
   same operator-edit pattern as `set_model` / ADR-0035 / ADR-0036. Do **not**
   require `/restart` for the next chat turn to use the updated chain.
   Prefer extending the existing `config_edit` validate → `.bak` → atomic
   write → restore-on-fail helper family (or a sibling helper for the
   `[fallback] chain` array).

4. **Add validation (Q4 A):** `add` must take a fully-qualified
   `provider/model` that **registry-resolves** (known provider + usable
   model id shape for that provider). Otherwise **reject** with a clear
   English Telegram error (no silent skip / no half-applied disk).

5. **Empty chain allowed (Q6 A):** list may show an empty chain; remove may
   leave the chain empty. Empty chain = ADR-0012 / ADR-0019 behaviour with
   no backups (primary-only). Do **not** invent a mandatory sentinel entry.

6. **Reject duplicates (Q7 A):** `add` of an entry already present
   (exact `provider/model` match) is **rejected** with a clear message.
   Do **not** silently no-op or append a second copy.

7. **Cap on add (Q8 A):** when the live/disk chain already has
   **`MAX_FALLBACK_CHAIN` (5)** entries, **reject** further `add` with a
   clear cap message. Do **not** silently truncate on the Telegram add path.

8. **Remove addressing (Q9 C):** `remove` accepts **either**:
   - a **1-based index** as shown in the list reply, **or**
   - the **exact** `provider/model` string.
   Ambiguous / out-of-range / unknown → reject with a clear tip (show usage).

9. **List UX:** bare `/fallback` (or explicit list subcommand if impl prefers
   one verb shape) replies with the current ordered chain (1-based indices +
   `provider/model`), empty-state copy when none, and short usage for
   add/remove. Exact wording is an impl detail; must stay English on GitHub
   surfaces.

10. **Walk semantics unchanged:** this ADR does **not** change ADR-0012 /
    ADR-0019 trigger, hard exclusions, per-entry ADR-0009 budgets, or
    rewrite-of-answered-model behaviour. It only locks operator edit of the
    configured chain via Telegram.

11. **Release / process:** target **v1.0.7+**. Impl lands in `feat/237-telegram-fallback`
    (**Closes #237**). GitHub English only. Four gates on the impl PR. No tag
    from this issue.

## Non-goals

- Portal fallback editor / Settings UI (Q5 A).
- Reorder / move / swap of chain entries (Q2 A).
- Changing ADR-0012 / ADR-0019 walk rules, hard exclusions, or shaping from
  ADR-0029 / ADR-0032 / ADR-0033 / ADR-0034.
- Changing Active `/models` persist or live `current_model` locks
  (ADR-0035 / ADR-0036).
- Silent truncate on Telegram `add` when at cap (reject instead).
- Blocking v1.0.6 tagging (Kan-owned). No tag from this issue.
- Implementing Rust in this PR.

## Acceptance criteria

- Docs state: Telegram `/fallback` supports list, add, and remove; portal
  editor out; no reorder this slice.
- Docs state: successful edits persist `[fallback].chain` on disk **and**
  update live memory immediately (no `/restart` required for the next turn).
- Docs state: add requires registry-resolved fully-qualified `provider/model`
  or reject; duplicates rejected; empty chain allowed; at cap 5 reject add
  with clear message (no silent truncate on add).
- Docs state: remove accepts 1-based index **or** exact `provider/model`.
- Docs state: ADR-0012 / ADR-0019 walk semantics unchanged.
- CHANGELOG Unreleased note for the shipping feature (impl).

## Consequences

- Impl on `feat/237-telegram-fallback` after ADR merge. Closes #237 via PR.
- Operators can inspect and edit the backup chain from Telegram without
  hand-editing TOML or restarting, matching the persist+live trust model of
  `/models`.
- Cap and duplicate rejects keep the chain predictable and bounded; empty
  chain remains a valid “primary only” config.

## Related

- ADR-0012 / ADR-0019 — chain walk semantics (unchanged by this ADR).
- ADR-0035 / ADR-0036 — persist + live operator-edit pattern to mirror.
- #237 — tracking issue (stays open through impl).
