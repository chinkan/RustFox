---
name: verify-rustfox
description: Verify RustFox via its embedded web portal (primary agent drive surface). Use when proving portal chat, settings, memory, tasks, or agents against an isolated portal_preview instance — never against Kan's live fox-home or live Telegram.
---

# Verify RustFox

RustFox surfaces: **Telegram bot**, **embedded web portal**, and **CLI**. For agents, the **primary drive surface is the web portal**. Prefer the isolated `portal_preview` example (no Telegram, no LLM) over a full bot start.

Isolation rules (non-negotiable):

- Use an absolute disposable home when touching the real binary (`RUSTFOX_HOME=/tmp/...`). Exclusive flock per home (ADR-0046).
- **Never** set `RUSTFOX_HOME` to `/home/kan/.rustfox` or drive live Telegram.
- `portal_preview` already uses its own `/tmp/rustfox-preview-<pid>` config dir; leave `RUSTFOX_HOME` unset for preview runs.
- Never kill by process name. Only tear down PIDs recorded by Launch.

## Launch

From the repo root (after a fresh checkout, build once so `web/dist` is embedded):

```bash
# One-time (or after frontend changes): embeds portal into the binary
./scripts/build-all.sh
cargo build --release --example portal_preview

# Start an isolated preview instance + evidence dir
.cursor/skills/verify-rustfox/helpers/launch.sh
# → prints LAUNCHED pid=… url=http://127.0.0.1:8123/ evidence=/tmp/verify-rustfox-<run>/ …
# Ready signal: stdout line `PREVIEW_URL=http://127.0.0.1:<port>/` (also accepted: HTTP 200 on `/`)
```

Env overrides:

| Var | Default | Purpose |
|---|---|---|
| `PORTAL_PREVIEW_PORT` | `8123` | Bind port (`127.0.0.1` only) |
| `PORTAL_PREVIEW_TOKEN` | `preview-token` | Login / Bearer token |
| `VERIFY_RUN_ID` | timestamp-pid | Evidence/state suffix |
| `VERIFY_EVIDENCE_DIR` | `/tmp/verify-rustfox-<run>` | Proof artifacts (survive cleanup) |
| `VERIFY_STATE_DIR` | `/tmp/verify-rustfox-state-<run>` | pid/log/meta (removed on cleanup) |

Manual equivalent:

```bash
PORTAL_PREVIEW_PORT=8123 PORTAL_PREVIEW_TOKEN=preview-token \
  ./target/release/examples/portal_preview
# wait until log contains PREVIEW_URL=http://127.0.0.1:8123/
```

Symlinks written for later helpers: `/tmp/verify-rustfox-last-state` → state dir, `/tmp/verify-rustfox-last-evidence` → evidence dir.

## Doctor

```bash
.cursor/skills/verify-rustfox/helpers/doctor.sh
```

Checks (read-only): recorded PID alive, port listening, `GET /` → 200 with SPA/HTML body, Bearer or cookie auth via `/api/auth/me`. Exit `0` = healthy to drive; non-zero = do not drive.

## Drive

Feature recipes live in [`features/`](features/README.md). Always doctor first. Prefer stable routes and API paths the SPA uses (`/dashboard`, `/chat`, `/settings`, `/memory`, `/tasks`, `/agents`) and Playwright handles already used by root `e2e-verify.mjs`.

Drive one mapped feature (portal home/chat) via the helper:

```bash
# curl path — login → history → POST /api/chat SSE → history again
.cursor/skills/verify-rustfox/helpers/drive-portal-home-chat.sh curl

# or full Playwright gate (needs web/node_modules with playwright)
.cursor/skills/verify-rustfox/helpers/drive-portal-home-chat.sh playwright
```

Direct harnesses already in-repo:

```bash
BASE=http://127.0.0.1:8123 TOKEN=preview-token node e2e-verify.mjs
# screenshots land in /tmp/pw-*.png — copy into the evidence dir
```

API clients may use `Authorization: Bearer <token>` or cookie from `POST /api/auth/login` (see `docs/portal-api.md`).

## Evidence

- Default path: `/tmp/verify-rustfox-<run>/` (also `VERIFY_EVIDENCE_DIR`).
- Optional in-skill mirror: `.cursor/skills/verify-rustfox/evidence/` (gitignored contents; keep `.gitignore`).
- Capture: launch meta, HTTP/SSE bodies, Playwright screenshots, `PROOF.txt` naming `feature_id` + entry point.
- Proof standards: exercise the real user path (SPA or the same `/api/*` the SPA calls); capture action + resulting state; for chat, history after reload/re-fetch must contain the new messages. Mocks only where production already isolates (portal_preview's fake AgentOps / in-memory SQLite — that *is* the safe path for UI/SSE without TG/LLM).

## Cleanup

```bash
.cursor/skills/verify-rustfox/helpers/cleanup.sh
```

Sends SIGTERM (then SIGKILL) **only** to `PORTAL_PREVIEW_PID` from the state meta. Removes the state dir. **Does not delete evidence.** After cleanup, confirm `$VERIFY_EVIDENCE_DIR` (or `/tmp/verify-rustfox-last-evidence`) still exists.

## Helpers

| Script | Role |
|---|---|
| `helpers/launch.sh` | Build-if-needed, start portal_preview, wait for ready, write meta + evidence stub |
| `helpers/doctor.sh` | Read-only health |
| `helpers/drive-portal-home-chat.sh` | Drive `portal-home-chat` (`curl` \| `playwright`) |
| `helpers/cleanup.sh` | Kill recorded PID only; keep evidence |

All helpers are executable. Invoke from repo root as shown above (paths relative to root).

## Maintain

When portal routes, e2e-verify checks, or preview fixtures change, update the matching file under `features/` and re-prove one journey. Use `/maintain-verification-skill` for the maintenance loop.
