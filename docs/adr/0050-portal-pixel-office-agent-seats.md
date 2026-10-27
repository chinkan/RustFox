# ADR-0050: Portal pixel office (Agent seats)

- **Status:** Accepted
- **Date:** 2026-10-27
- **Related:** Portal `/agents`, `GET /api/agents`; follow-up ADR for Telegram multi-agent ToolNotifier roster (out of scope here)
- **Issue:** Refs #309; implementation PR(s) follow this docs lock

## Context

Users want a carton/pixel office on the Portal so each loaded agent has a desk (idle vs working). Today `/agents` is a table. `GET /api/agents` returns `idle` | `running` | `error`, but only the main agent reflects live `is_processing`; subagent definition rows are always idle. Glance truth beats vibe. Telegram multi-agent ToolNotifier roster is a separate follow-up ADR.

This document records the product locks for the Portal pixel office. It does not implement Rust, portal UI, tests, or a release tag.

## Decision

1. **Primary view.** Upgrade `/agents` primary view to a low-res pixel office of Agent seats. Keep table/list as secondary toggle. No new nav item.

2. **Seat mapping.** Seat = one desk per Agent definition row from `listAgents` (main + loaded subagent definitions).

3. **v1 statuses.** `idle` | `running` | `error` only (`idle` = lounging; `running` = keyboard/mouse loop; `error` = distinct pose). Finer states are Phase 2.

4. **Honesty / seat←run.** A seat is `running` if and only if (a) it is the main agent and `is_processing` is true for web or Telegram, or (b) an in-flight `invoke_agent` / `spawn_agents` call is tagged to that agent name via a lightweight in-process runtime flag (not a full activity bus / SSE). Otherwise the seat is idle. Rows that have no live-signal capability show a definition-only / no live run affordance. Never invent busy animations without a signal.

5. **Refresh.** TanStack Query poll every 2–5s while the page is visible.

6. **Opt-in / a11y.** Default office view with control to switch to table. Settings: Reduce motion / Disable office animations. Honor `prefers-reduced-motion` with static poses + text status badges. Screen-reader live regions are Phase 2.

7. **Performance.** Soft-cap visible desks (~12 or by viewport); overflow as “+K more” and/or force table.

8. **Ownership.** Portal UI owns sprites/CSS pixel art and office layout; backend/API work wires status accuracy and the seat←run flag. One pose loop per status. Split PRs allowed (API/signal first, office UI second).

9. **Release.** Independent of the v1.0.8 Soft/tag train. Portal Soft only (web e2e/manual). No Telegram Soft for this ADR. Tag only under normal Soft + ship rules when a release includes it.

## Consequences

- Users can see a full office while only seats with live signals animate as running; definition-only labels prevent false busy.
- The `error` pose may be unused until the API emits `error`.
- Shared lightweight invoke/spawn name flags may later feed a Telegram roster ADR without expanding this ADR’s Portal scope.

## Out of scope

- Telegram ToolNotifier Helpers roster (follow-up ADR).
- Write/Emit filler spam (separate issue/ADR).
- Thinking / tool / waiting-human statuses.
- Full SSE/WS activity bus.
