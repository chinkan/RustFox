# Portal office visualize (production) — evidence

**Branch:** `cursor/portal-office-visualize-dc6e`  
**ADR:** [ADR-0050 optional WebGL amend](https://github.com/chinkan/RustFox/blob/main/docs/adr/0050-portal-pixel-office-agent-seats.md) (2026-10-28)  
**Spike reference (not merged):** PR #321 — visual target for Pixel vs 3D wide layouts.

## Production build (`web/`, `npm run build`)

| Asset | Raw | Gzip |
| --- | ---: | ---: |
| `office-3d-*.js` (Three + R3F + scene; **lazy**) | ~964 KB | **~259 KB** |
| Main `index-*.js` (no Three until 3D toggle) | ~503 KB | **~153 KB** |

Soft target ≤ 350 KB gzip for the WebGL chunk: **met**.

## Behaviour locks (P0)

- Settings key `office_visualize`: `svg` (default) \| `webgl`, persisted in `localStorage` (`rustfox.portal.office`).
- UI: **Office view: Pixel \| 3D** on `/agents` office view only.
- Fallbacks → Pixel/svg: reduced motion, disable office animations, WebGL probe fail, context loss, lazy chunk error (`Office3DErrorBoundary` + Suspense).
- webgl v1: fixed desk poses only (no idle roam in 3D); honesty via shared `seatView()`.

## Screenshots

Product-side captures from the disposable spike (#321) remain the visual baseline for wide Pixel vs 3D. Re-capture on this branch after merge with a live Portal + agents API:

- Wide: 1440×900 — Pixel and 3D toggles
- Narrow: 390×844 — legend stacks single-column (CSS)
