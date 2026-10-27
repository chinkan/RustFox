# ADR-0050: Portal pixel office (Agent seats)

- **Status:** Accepted
- **Date:** 2026-10-27
- **Related:** Portal `/agents`, `GET /api/agents`; follow-up ADR for Telegram multi-agent ToolNotifier roster (out of scope here); #313 Portal pixel office UI tip (game-office expand after Product visual NO)
- **Issue:** Refs #309; implementation PR(s) follow this docs lock. Game-office expand: this docs PR Refs #313 (do not close #313); UI iterates on the #313 tip after amend Soft N/A squash.

## Context

Users want a carton/pixel office on the Portal so each loaded agent has a desk (idle vs working). Today `/agents` is a table. `GET /api/agents` returns `idle` | `running` | `error`, but only the main agent reflects live `is_processing`; subagent definition rows are always idle. Glance truth beats vibe. Telegram multi-agent ToolNotifier roster is a separate follow-up ADR.

Product visual check on the #313 tip (static desk grid of fox seats) was **NO GO squash** (Kan 2026-10-27): expand to a game-office feel with idle roam, busy→desk, and deterministic fox variants — still fox sprites only, not humans. Seat←run honesty and reduce-motion / disable-anim locks stand.

This document records the product locks for the Portal pixel office. It does not implement Rust, portal UI, tests, or a release tag.

## Decision

1. **Primary view.** Upgrade `/agents` primary view to a low-res pixel office of Agent seats. Keep table/list as secondary toggle. No new nav item.

2. **Seat mapping.** Seat = one desk per Agent definition row from `listAgents` (main + loaded subagent definitions).

3. **v1 statuses / poses.** `idle` | `running` | `error` only (`idle` = lounging or roam behaviour; `running` = keyboard/mouse working pose at the assigned desk; `error` = distinct pose). Finer states are Phase 2. Roam is **idle** behaviour only; it does not add a new status.

4. **Honesty / seat←run.** A seat is `running` if and only if (a) it is the main agent and `is_processing` is true for web or Telegram, or (b) an in-flight `invoke_agent` / `spawn_agents` call is tagged to that agent name via a lightweight in-process runtime flag (not a full activity bus / SSE). Otherwise the seat is idle. Rows that have no live-signal capability show a definition-only / no live run affordance. Never invent busy animations without a signal. (`live_signal` / `definition_only` on the API stand; unchanged by the game-office expand.)

5. **Refresh.** TanStack Query poll every 2–5s while the page is visible.

6. **Opt-in / a11y.** Default office view with control to switch to table. Settings: Reduce motion / Disable office animations — both **stop roam** (and other office motion). Honor `prefers-reduced-motion` with static poses + text status badges. Screen-reader live regions are Phase 2.

7. **Performance.** Soft-cap visible desks (~12 or by viewport); overflow as “+K more” and/or force table.

8. **Ownership.** Portal UI owns sprites/CSS pixel art and office layout; backend/API work wires status accuracy and the seat←run flag. One pose loop per status. Split PRs allowed (API/signal first, office UI second).

9. **Release / Soft.** Independent of the v1.0.8 Soft/tag train. Soft = Portal web after the #313 UI tip squash (game-office iterate on that tip). No Telegram Soft for this ADR. No new release train. Tag only under normal Soft + ship rules when a release includes it.

## Decision — Game-office expand (#313)

Product locks after Kan visual **NO** on the #313 tip (static desk-grid feel). Amend in place; Decision 3 poses (`idle` / `running` / `error`) and Decision 4 seat←run honesty (`live_signal` / `definition_only`) are unchanged. This docs PR **Refs #313**; do **not** close #313. UI continues on the #313 tip after Soft N/A squash of this amend.

1. **Game-office feel.** The office floor must feel like a **game office** (space to move, ambient life), not only a static desk grid of seated foxes.

2. **Idle = roam.** Idle foxes (live-signal seats that are not busy) **may walk / roam** the floor. Layout must leave space for roam paths between desks.

3. **Busy → desk.** When a seat becomes busy / live-run (`running` per Decision 4), the fox **paths back to its assigned desk** and takes the working pose. Seat←run honesty is unchanged: never invent busy without `live_signal` / non-`definition_only` signal.

4. **Fox variants.** Slight visual differences **deterministic from the agent name** (palette / accessories / desk prop). **Fox sprites only — not humans** (Kan lock).

5. **A11y / motion off.** Existing Settings **Reduce motion** / **Disable office animations** stop roam (and other office motion), same as Decision 6.

6. **Soft / train.** Soft = Portal web after UI tip squash on #313. **Not** v1.0.8. UI stays on the #313 tip; no new release train; Soft N/A for this docs amend.

## Consequences

- Users can see a full office while only seats with live signals animate as running; definition-only labels prevent false busy.
- Idle roam and busy→desk pathing make the floor feel alive without new status enums; reduce-motion / disable-anim keep a11y.
- Fox variants stay name-deterministic carton foxes (never humanoid).
- The `error` pose may be unused until the API emits `error`.
- Shared lightweight invoke/spawn name flags may later feed a Telegram roster ADR without expanding this ADR’s Portal scope.
- #313 tip squash stays HOLD until this amend lands and UI re-gates the game-office expand; Soft Portal web follows tip squash, not this docs PR.

## Out of scope

- Telegram ToolNotifier Helpers roster (follow-up ADR).
- Write/Emit filler spam (separate issue/ADR).
- Thinking / tool / waiting-human statuses.
- Full SSE/WS activity bus.
- Human / humanoid seat sprites (Kan lock: foxes only).
- Closing #313 from this docs PR; runtime / UI code in this amend.
