# ADR-0036: Live `current_model` for primary chat + `/models` copy

- **Status:** Accepted (PO Round 2 locked; TL assign)
- **Date:** 2026-10-19
- **Target release:** v1.0.7+ (does not block v1.0.6)
- **Depends on:** ADR-0035 (`/models` Active persist + `default_provider`; #233 / #235), ADR-0033 (sticky agent/bot/persona `model`; #223 / #225)
- **Related:** #217 (banner shows Active / `current_model`; closed), #237 (fallback UI; out of this slice)
- **Issue:** #236

## Context

Kan wanted clearer `/models` success copy after ADR-0035: success shows
`(saved)` but feels like `/restart` is required before the new Active takes
effect.

**QA tip `37d3086` FAIL (live):** `set_model` / portal PATCH update
`current_model`, disk, and settings UI (saved), but the next chat primary
`llm_call` still uses the **boot-frozen**
`registry.default_qualified_model()` when the main loop’s `LoopConfig.model`
is `None`. Telegram `/models` was not live-probed (allowlist); same
`Agent::set_model` path as portal.

**Researcher:** when `LoopConfig.model` is `None`, primary paths should inject
live `current_model`. When it is `Some(bots[]/persona)` the sticky model from
ADR-0033 must stay unchanged.

Round 1 Q1=A (copy-only) was **invalidated** by the QA live FAIL. Product
lock (PO on #236, 2026-10-19 HKT; Round 2 Q6–Q9=A; Q1=B) is authoritative for
the Decision below.

## Decision

1. **Must fix live:** when `LoopConfig.model` is `None`, primary `llm_call` /
   agent chat paths (Telegram + portal, **shared**) use live `current_model`,
   **not** boot-frozen `registry.default_qualified_model()`.

2. **`Some(bots[]/persona)` sticky unchanged (ADR-0033):** do **not** override
   an explicit sticky model with Active / `current_model`.

3. **Telegram success copy:** Active is switched **now** + saved; cold start
   follows. Do **not** require `/restart` for live chat to use the new Active.
   (Exact wording is an impl detail; must not imply restart is needed for the
   change to take effect.)

4. **Disk-fail:** live chat still follows `current_model`; Telegram must say
   **not saved**; cold start may revert (aligns with ADR-0035 Decision 5
   LiveOnly path).

5. **Portal copy is a non-goal** for this slice; the shared live fix covers
   portal chat.

6. **#237** fallback UI remains **out** of this slice.

7. **Docs-only this PR;** impl after merge + TL assign. Does **not** close
   #236 — leave open for impl. Target **v1.0.7+**. GitHub English only. Four
   gates on the **impl** PR.

## Non-goals

- Fallback Telegram view/edit UI (#237; separate issue).
- Portal success/settings copy polish (shared live fix covers portal chat).
- Changing ADR-0033 sticky `Some(bots[]/persona)` behaviour.
- Changing ADR-0035 boot/`default_provider` persist rules or shaping from
  ADR-0032 / ADR-0034.
- Reopening #217 (banner display-only).
- Blocking v1.0.6 tagging (Kan-owned). No tag from this issue.
- Implementing Rust in this PR.

## Acceptance criteria

- Docs state: when `LoopConfig.model` is `None`, primary chat uses live
  `current_model` (Telegram + portal shared); boot registry default is not
  the live path.
- Docs state: `Some(bots[]/persona)` remains sticky (ADR-0033); Active does
  not override sticky.
- Docs state: success Telegram copy says Active changed now + saved and does
  **not** require `/restart` for live chat; disk-fail still follows live +
  says not saved (cold may revert).
- Docs state: portal copy out; #237 out; docs-only this PR does **not** close
  #236.
- **Impl regression (note for later PR):** after `set_model`, next chat
  **without** `/restart` uses the new Active when there is no sticky model.
- CHANGELOG Unreleased note for this docs slice; shipping release note on
  impl.

## Consequences

- Docs-only first (this ADR + short cross-refs on ADR-0035 / ADR-0033). Tech
  lead assigns RustFox Rust Developer for impl after merge
  (`fix/236-live-current-model` or similar). Leave #236 open for impl — this
  PR is a docs slice only.
- Operators can `/models` switch Active and continue chatting immediately
  without restart; cold start still follows ADR-0035 `default_provider`
  persist when disk succeeded.
- Sticky persona/bot models stay predictable (ADR-0033).
