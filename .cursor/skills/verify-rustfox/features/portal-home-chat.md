# Portal home & chat

Login, dashboard landing, and the web chat surface let a user authenticate with the portal token, see the home dashboard, open the active web thread, send a message, watch the streamed assistant reply (and tool notes), and confirm the exchange survives reload because SQLite is the source of truth.

## Sub-features

- `login-render` shows the login card (not a white screen) with title containing RustFox.
- `login-submit` accepts the portal token and redirects to `/dashboard`.
- `dashboard-content` renders meaningful body text after login.
- `chat-redirect` sends `/chat` to the active thread URL `/chat/<id>`.
- `chat-history` loads seeded messages (Rothfuss / Portia fixtures).
- `chat-send-sse` streams token/tool/done frames for a user message.
- `chat-persist` reloads the thread and still shows the new user + assistant text.

## How to get to it (user POV)

- Open `http://127.0.0.1:8123/`, enter the portal token, submit login.
- From the nav, open Dashboard or Chat.
- Deep-link `/chat` (redirects to active thread) or `/chat/<conversationId>`.
- Scripted: `POST /api/auth/login` then `POST /api/chat` (SSE) as the SPA does.

## Driving it with portal_preview harness

Preconditions:

- `helpers/launch.sh` completed; doctor reports HEALTHY.
- Token is `preview-token` (or the `PORTAL_PREVIEW_TOKEN` you launched with).
- Evidence dir exists (`VERIFY_EVIDENCE_DIR` or `/tmp/verify-rustfox-last-evidence`).

- **Login UI.** Open `/`. Run Playwright navigation as in `e2e-verify.mjs` section 1–2, or `curl -sS "$BASE/"`. Login card / form is visible; title includes `RustFox`.
- **Sign in.** Fill token and submit. Run `helpers/drive-portal-home-chat.sh curl` (login step) or Playwright fill `#token` / password input + `button[type=submit]`. URL becomes `**/dashboard`.
- **Chat redirect.** Go to `/chat`. URL matches `**/chat/*` within a few seconds.
- **Seeded history.** Body / `GET /api/chat/history` includes `The Winds of Winter` or `Death of Portia`.
- **Send + SSE.** Fill `.chat-input input` with a unique string and click `Send`, or `POST /api/chat` with `{"text":"…"}`. SSE / UI shows `Preview reply to: <text>` and a tool note for `read_file`.
- **Persist.** Reload the page or re-`GET /api/chat/history`. The unique user text and preview reply remain.
- **Proof.** Prefer `helpers/drive-portal-home-chat.sh curl` (writes `01-login.json` … `06-history-after.json` + `PROOF.txt`) or `… playwright` (copies `/tmp/pw-*.png` + `e2e-verify.log`).

## Gotchas

- Empty `web/dist` at **compile** time embeds a stub SPA — rebuild with `./scripts/build-all.sh` then `cargo build --release --example portal_preview` before blaming the harness.
- Default token is `preview-token`; a mismatched `TOKEN` env in `e2e-verify.mjs` fails login silently-looking stuck on `/`.
- Chat SSE is scripted (~5 words × 30ms + tool events); wait ~2.5s in UI before asserting the reply.
- Concurrent `POST /api/chat` → `409 chat_in_progress`; finish or cancel before another send.
- Never point this journey at a live fox-home portal (port 8090 / Kan's install).
