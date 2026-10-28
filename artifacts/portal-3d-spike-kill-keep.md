# Portal 3D office spike — kill / keep memo (2026-10-28)

**Branch:** `cursor/portal-3d-office-spike-9a25`  
**AC source:** `rustfox-portal-3d-spike-ac-2026-10-28.md` (grill R1+R2 locked)  
**Default:** `/agents` stays SVG Pixel (#313 / ADR-0050). Toggle is **session-only**.

## What shipped (disposable spike)

| AC | Status | Notes |
| --- | --- | --- |
| 1 Iso room ≤12 seats, fixed ortho camera | Done | `Office3DScene` + `isoDeskSlots`, `OFFICE_DESK_CAP` reused |
| 2 R3F + fox SVG billboards | Done | `foxBillboardSvg` → `TextureLoader` on billboards |
| 3 Honesty (`live_signal` / `definition_only`) | Done | `seatView()` shared with Pixel; 3D legend uses `SeatStatus` |
| 4 Toggle `Office view: Pixel \| 3D` session-only | Done | `officeViewMode.tsx` → `sessionStorage` |
| 5 Fallbacks (reduced motion, no WebGL, Pixel) | Done | `motion` gate + `probeWebGL()` + Suspense → Pixel; context loss → Pixel |
| 6 Budget report (no hard fail) | Below | Optional lazy chunk `office-3d` |
| 7 Evidence | Screenshots in `artifacts/portal-3d-spike/` | Side-by-side Pixel vs 3D (wide + narrow) |
| 8 No ADR in PR | Done | ADR-0050 amend deferred until Product KEEP |

## Honesty wiring

- **No API changes.** 3D reads the same `Agent[]` from `/api/agents` as Pixel.
- **Busy/running pose** only when `seatView()` returns non–definition-only and `pose === 'running'` (requires `live_signal` / not `definition_only` per ADR-0050).
- **Definition-only** seats: pose forced to idle (or error), badges in HTML legend match Pixel `SeatStatus`.

## Fallback behavior

| Condition | UI |
| --- | --- |
| Toggle **Pixel** | SVG `#313` `PixelOffice` |
| `prefers-reduced-motion: reduce` or Settings office motion off | 3D not mounted; Pixel even if toggle says 3D |
| WebGL probe fails or context lost | Pixel |
| Lazy 3D chunk loading | Pixel via `Suspense` fallback |
| Empty agent list | Copy in legend; no blank canvas |

## Budget (gzip, production build `npm run build`)

Measured on spike tip (Vite 8 / Rolldown):

| Asset | Raw | Gzip |
| --- | ---: | ---: |
| `office-3d-*.js` (Three + R3F + scene; **lazy**) | ~964 KB | **~259 KB** |
| Main `index-*.js` (without 3D until toggle) | ~503 KB | **~153 KB** |
| `index-*.css` | ~19 KB | **~5 KB** |

**Optional-load pattern:** `React.lazy(() => import('./office3d/Office3D'))` + Rollup `manualChunks` → `office-3d`. Three loads only after user picks **3D** (and motion/WebGL allow).

**Mobile smoke:** 390×844 viewport screenshots in evidence pack; legend stacks single-column via CSS.

## KILL vs KEEP (Product AC)

**KILL if any:**

- [ ] Fake busy without live signal — **not observed** (shared `seatView`)
- [ ] Missing fallback / blank canvas — **not observed** (Pixel fallbacks)
- [ ] Narrow viewport unusable — **smoke OK** (canvas + legend); polish TBD if KEEP

**KEEP if all:**

- [x] Iso room + ≤12 seats + fox SVG billboards
- [x] Live honesty + fallbacks + budget memo

## Recommendation (engineering)

Lean **KEEP** for a **thin ADR-0050 amend** (optional `svg | webgl`, persist policy, gzip budget target). Spike is intentionally disposable; do **not** merge without Product sign-off.

**Out of scope (unchanged):** Approve/Decline chrome, orbit polish, `/agents/office` route, Settings persist for toggle, Soft / v1.0.8 gate (`SOFT_BLOCKS_v1.0.8=NO`).
