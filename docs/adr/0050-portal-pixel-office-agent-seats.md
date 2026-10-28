# ADR-0050: Portal pixel office (Agent seats)

- **Status:** Accepted
- **Date:** 2026-10-27
- **Related:** Portal `/agents`, `GET /api/agents`; follow-up ADR for Telegram multi-agent ToolNotifier roster (out of scope here); #313 Portal pixel office UI tip (game-office expand after Product visual NO); optional WebGL visualize amend (2026-10-28): Product KEEP evidence on #321 — this docs PR does not merge #321 and does not close #321 unless Product closes after evidence; production UI is a separate PR after Accept
- **Issue:** Refs #309; implementation PR(s) follow this docs lock. Game-office expand: this docs PR Refs #313 (do not close #313); UI iterates on the #313 tip after amend Soft N/A squash. Optional WebGL visualize (2026-10-28): cites Product KEEP evidence on #321 (do not merge #321; do not close #321 unless Product closes after evidence); production UI is a separate PR after Accept. No UI in this docs PR.

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

## Decision — Optional WebGL visualize (amend 2026-10-28)

Product KEEP on the disposable R3F spike (#321). This amend records production locks for an **optional** WebGL office layer. It does not replace pixel/SVG ownership, seat←run honesty, fox-only sprites, the soft-cap, or the reduce-motion locks above. This docs PR does not implement UI. Disposable #321 is evidence only and is not merged into `main` as the production path.

1. **Optional visualize modes.** Portal `/agents` supports `office_visualize: svg | webgl` (Settings key **`office_visualize`**).
   - **`svg`** = current ADR-0050 pixel / SVG office (#313 path) — **default**.
   - **`webgl`** = optional isometric dollhouse layer.
   - UI copy: **`Office view: Pixel | 3D`** (Pixel ↔ `svg`, 3D ↔ `webgl`). The ADR uses the technical enum; the UI keeps the human labels.

2. **Default.** Unset / first visit / cleared settings → **`svg` (Pixel)**. Kan lock: SVG #313 remains the `/agents` default.

3. **Persistence.** Production **Settings persist** the last chosen `office_visualize` (survives refresh). Spike session-only behaviour is discarded for production.

4. **Stack (webgl).** **R3F (`@react-three/fiber`) + OrthographicCamera** (drei as needed). Vanilla Three only as an exception if R3F is unfit. This office visualize path does not use Spline, Unity, Babylon, Threlte, Pixi, or Phaser.

5. **Silhouette (webgl).** **Fox SVG billboards** (or textures derived from the existing fox SVG). **No** humanoids, blob mascots, or non-fox avatars (extends the fox-only lock).

6. **Honesty.** Decision 4 (Honesty / seat←run, above) is unchanged: busy/running poses **only** with `live_signal` / non-`definition_only`. Never invent busy animation in either visualize mode.

7. **webgl v1 motion scope.** **Fixed seats / desk poses only** under `webgl`. Idle **roam** remains an **`svg`**-mode behaviour (game-office expand). When a seat is busy per Decision 4, show the working pose at the assigned desk (no requirement for 3D pathing roam). Richer 3D roam is a later Product call, not this amend.

8. **Fallback locks (must).**
   1. Default / unset → **svg**.
   2. `prefers-reduced-motion: reduce` → **force svg** (no WebGL rAF; no static one-frame WebGL).
   3. Settings **Reduce motion** / **Disable office animations** → **force svg** (Decision 6 applies to webgl; no exception).
   4. WebGL unavailable / context lost / chunk load fail → **svg**; **never** a blank canvas.
   5. Table / list toggle (Decision 1) remains available and is orthogonal to `office_visualize`.

9. **Code-split / budget.** WebGL deps (**three / r3f / drei**) are **lazy-loaded only when `webgl` is active**. The Pixel / `svg` initial path ships **zero** three/r3f/drei. Soft target: WebGL JS chunk **≤ 350 KB gzip** (assets reported separately, lazy). Over target → Soft HOLD / revisit this amend; this is not a CI hard-fail kill switch.

10. **Soft-cap.** Decision 7 soft-cap (~12 desks) applies to **both** visualize modes.

11. **Entry.** In-page control on `/agents` only. A separate route (for example `/agents/office`) is **out of scope** (P2).

12. **Chrome out of this amend.** Approve/Decline workflows, day/night sky, speech bubbles, and orbit/bloom/LOD polish are not required by this amend (follow-up Product if wanted).

13. **Release / Soft.** Does **not** gate Soft / v1.0.8. This docs amend is Soft N/A (or an independent Portal docs Soft). Production UI PR Soft = Portal web after that PR’s squash — not by merging disposable #321. This docs PR does not merge #321 and does not close #321; Product may close #321 after KEEP evidence. Production is a **new** UI PR that follows this ADR.

14. **Ownership / sequencing.** Portal UI owns the R3F scene, the visualize toggle, and the fallbacks. Backend stays unchanged unless honesty API gaps appear. This docs PR records the amend only. After Accept, production UI is a separate PR. Disposable #321 stays KEEP evidence and is not the production branch.

## Consequences

- Users can see a full office while only seats with live signals animate as running; definition-only labels prevent false busy.
- Idle roam and busy→desk pathing make the floor feel alive without new status enums; reduce-motion / disable-anim keep a11y.
- Fox variants stay name-deterministic carton foxes (never humanoid).
- The `error` pose may be unused until the API emits `error`.
- Shared lightweight invoke/spawn name flags may later feed a Telegram roster ADR without expanding this ADR’s Portal scope.
- #313 tip squash stays HOLD until this amend lands and UI re-gates the game-office expand; Soft Portal web follows tip squash, not this docs PR.
- Users keep a crisp SVG default; WebGL is opt-in and remembered in Settings (`office_visualize`).
- Reduced-motion and animation-off users never enter a WebGL render loop.
- webgl v1 stays cheap (no 3D idle roam); svg keeps game-office roam.
- Bundle stays small on the default path; WebGL cost is isolated and Soft-visible via the 350 KB gzip soft target.
- Disposable spike #321 stays evidence-only; production code is reviewed against this amend, not the spike branch shape.

## Out of scope

- Telegram ToolNotifier Helpers roster (follow-up ADR).
- Write/Emit filler spam (separate issue/ADR).
- Thinking / tool / waiting-human statuses.
- Full SSE/WS activity bus.
- Human / humanoid seat sprites (Kan lock: foxes only).
- Closing #313 from this docs PR; runtime / UI code in this amend.
- Merging disposable #321 as production.
- A separate `/agents/office` route.
- Approve/Decline, day-night, and speech-bubble chrome.
- 3D idle roam / pathing under `webgl` (v1).
- A hard CI fail on chunk size.
- Changing seat←run rules, the fox-only lock, or soft-cap numbers.
