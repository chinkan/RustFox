# Settings

Settings is the authenticated admin surface for portal configuration views. A signed-in user opens `/settings`, can refresh without losing the SPA, and the embedded assets keep serving so the page is not a white screen.

## Sub-features

- `settings-deeplink` loads `/settings` with body content after login.
- `settings-refresh` keeps the app alive on browser reload of `/settings`.
- `settings-assets` serves hashed `/assets/*` with HTTP 200 from the embedded dist.

## How to get to it (user POV)

- After login, choose Settings in the portal nav.
- Deep-link `http://127.0.0.1:8123/settings` while holding a valid session cookie.

## Driving it with portal_preview harness

Preconditions:

- Doctor HEALTHY; session cookie or Bearer token valid.
- Preview binary built with a real `web/dist` (not stub).

- **Deep-link.** `page.goto(BASE + '/settings')` (see `e2e-verify.mjs` §3) or `curl -b jar "$BASE/settings"`. URL contains `/settings` and body text length > 50.
- **Refresh.** Reload the same URL; body still has content (SPA fallback `index.html` embedded).
- **Assets.** From the page, `performance.getEntriesByType('resource')` for `/assets/` all status 200, or `curl -sI` an asset URL from the HTML.
- **Auth me.** `GET /api/auth/me` (cookie or Bearer) includes authenticated web user.
- **Proof.** Screenshot `/tmp/pw-03-settings.png` from `e2e-verify.mjs` copied into `$EVIDENCE/settings/`, plus the `/api/auth/me` JSON.

## Gotchas

- Unauthenticated `/settings` redirects to login — obtain a session first.
- `web/e2e/smoke.mjs` targets port `5199` (vite dev) and is **not** the production embed path; use `e2e-verify.mjs` + `portal_preview` for ship-shaped proof.
- Settings writes that touch live config are richer on a full fox-home; preview still proves render + auth + SPA fallback.
