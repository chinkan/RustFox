# ADR 0018: Self-upgrade must surface causes; GitHub API auth optional

## Status
Accepted

## Date
2026-10-05

## Context
Kan’s v1.0.3 → v1.0.4 `/selfupgrade` (release path) failed with Telegram text only `self_update failed`. QA Linux repro (2026-10-05) showed the real cause: anonymous `GET …/releases/latest` returned **HTTP 403** with `x-ratelimit-remaining: 0`. Assets exist; authenticated `gh release download` of the 1.0.4 linux tar.gz worked. The Telegram handler formats the error with `{}` after `.context("self_update failed")`, so the chain is easy to miss. Windows zip deflate support remains unconfirmed (Kan OS TBD) but is a separate hardening.

## Decision
1. **Surface the full error** on the Telegram `/selfupgrade` path (and keep the tool path consistent): show `{:#}` / a formatted chain so Network/Io/Zip causes are visible in chat.
2. **Optional GitHub auth for release lookups**: if `GITHUB_TOKEN` or `GH_TOKEN` is set in the process environment, pass it to `self_update` via `.auth_token(…)`. Do not require a token for public releases when the anonymous quota is fine. Document in GUIDE: rate-limit 403 → wait or set a token (classic PAT with `public_repo` / fine-grained read releases is enough).
3. **Friendlier 403/rate-limit copy** when the cause is recognizable: tell the user it is GitHub API rate limit, not a missing asset, and point at the token/wait options. Do not invent a permanent baked-in token.
4. **Windows hardening (same PR or immediate follow-up)**: enable `self_update` feature `compression-zip-deflate` so PowerShell `Compress-Archive` zips extract. Does not block Linux fix.
5. No new tag required for this fix alone.

## Consequences
Upgrades on shared/CI IPs become reliable when the user exports a token; anonymous users still work under quota and get actionable errors when not. SecretStore for a GitHub token is out of scope unless product later wants wizard storage.

## Out of scope
- Changing release asset naming
- Source-mode (`cargo build`) upgrade path
- Baking credentials into the binary
