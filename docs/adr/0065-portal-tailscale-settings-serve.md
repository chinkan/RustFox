# ADR-0065: Portal Tailscale Settings + Serve (v1.0.10)

- **Status:** Product Soft Soft lock Soft ACCEPT Soft — Soft Soft Soft accepted Path A (2026-11-09; pending Soft tip Final). **§7 amend:** Product contract ACCEPT 2026-11-10 (Path A lock dated 2026-11-09 unchanged).
- **Date:** 2026-11-09
- **Milestone:** **v1.0.10 Soft parallel** (does **not** block v1.0.9 Soft tip Final Soft/QA / tag)
- **Issue:** this docs tip **Closes #390**. Impl issues open separately after Soft tip Final Soft/QA + tip squash. No tag. No runtime in this tip.
- **Amends:** [ADR-0007](./0007-portal-lan-access-tailscale-telegram-entry.md) — productize Settings enroll + Serve wrap; keep “no public tunnel / no Funnel”
- **Related:** [ADR-0004](./0004-portal-embedded-opt-in-server.md) (loopback bind), [ADR-0006](./0006-portal-token-auth-and-safe-settings.md) (whitelist / SecretStore), [ADR-0056](./0056-device-websocket-channel.md) (shared portal bind + device_ws), [ADR-0057](./0057-switchbot-open-api-connector.md) (Settings Enable + secrets pattern), runbook `docs/portal-access.md`
- **Research brief:** `/workspace/research/rustfox-soft-dig-portal-tailscale-2026-11-09.md` (Path A)

## Context

ADR-0007 locked remote Portal access to **OS Tailscale** and said MVP would **not** integrate the Tailscale API — operators install Tailscale by hand and use `docs/portal-access.md`. Tip already best-effort probes `tailscale ip -4` / `tailscaled.sock` for `/portal` (`src/portal/url.rs`). Portal default bind stays `127.0.0.1` (ADR-0004). Kan’s working temp (WSL userspace Tailscale + `tailscale serve` → loopback Portal) matches that path with no UI.

Kan via Product asked for Portal **Settings**-driven enroll so the fox can join a personal tailnet and expose Portal (and later other hosts) via MagicDNS Serve HTTPS — UX-aligned with Grok Bot’s Connect → Tailscale row, **without** copying Cursor-exclusive company posture / multi-slot / hosted-node internals.

Options:

- **(A)** Settings wrap of OS `tailscaled` + **Serve** HTTPS → `127.0.0.1:<portal.port>` (Funnel never)
- **(B)** In-process embed via `libtailscale` / tsnet (needs Go at build; community Rust binding)
- **(C)** Funnel public URL
- **(D)** Full VPN / ACL / posture editor in Portal

This document records that lock. It does not implement Rust, the Settings card, or a release tag.

## Decision

**Path A Must** for v1.0.10 Soft parallel:

1. **Surface.** Portal **Settings → Tailscale** card only (SwitchBot-style ADR-0057). Not the setup wizard.
2. **Controls.** Enable toggle; **auth key** write-only (masked when set); Connect/Disconnect (or Enable implies up); status: `offline` | `needs_login` | `connected` + hostname + **MagicDNS Serve HTTPS URL** (e.g. `https://<node>.<tailnet>.ts.net/`).
3. **Secrets.** Vault name `tailscale.auth_key`. `config.toml` keeps `secret:` ref + `enabled` (+ optional hostname) only — never plaintext. Settings GET/PATCH whitelist (ADR-0006). Prefer live up/down like SwitchBot; `restartRequired` only if impl cannot hot-apply.
4. **Exposure.** Portal **`bind` stays default loopback**. When enabled + node up, configure **`tailscale serve` HTTPS** (CLI or LocalAPI Serve config — **impl choice**) proxying to `http://127.0.0.1:<portal.port>`. **Serve only — Funnel never auto-enabled.**
5. **Auth UX.** Primary = paste **reusable auth key** (pre-approved when device approval is on; non-ephemeral for a long-lived home node). Secondary / Should: interactive **login URL** when no key.
6. **Soft fail.** Missing `tailscale` / `tailscaled` or Serve failure: bot + Portal keep running; Settings shows clear error + link to `docs/portal-access.md`. No process crash.
7. **URL surfaces.** When the operator runs `/portal` (existing command; also `/portal@bot` and `/portal` plus any trailing tokens including `tailscale`; no new subcommand): if Tailscale is usable (CLI/daemon present and the node can serve), start `tailscale serve` HTTPS to `127.0.0.1:<portal.port>` if it is not already up (Funnel never; soft-fail — bot + Portal must not crash; hard cap so `/portal` does not hang the bot), then reply with one primary tappable Telegram link to the MagicDNS Serve HTTPS URL (e.g. `https://<node>.<tailnet>.ts.net/`). If Serve is already up, reuse it. Otherwise reply with one primary tappable LAN URL (`LAN IP:port`) when the bind can actually serve that address. Never use `127.0.0.1` or `0.0.0.0` as the phone link. If bind is loopback-only and Serve is not available, do not invent a LAN URL; use the existing ADR-0007 hint. Tappable means official Telegram `text_link` (correct parse_mode / message entities) or an inline URL button — a plain-text URL does not count. Prefer one primary link; do not dump a list that includes localhost. Settings / Network enroll card remains per this ADR; `/portal` does not run interactive login. Settings may still show the Serve MagicDNS HTTPS URL when Serve is up.
8. **device_ws (ADR-0056).** Same Axum bind/port — Serve covers SPA **and** `/api/device/v1/ws` (prefer WSS via Serve HTTPS). No protocol change.
9. **Cardinality.** One Tailscale node per RustFox process host in Must. Multi-bot-in-one-process shares one portal bind → one Serve target. Document “one fox Serve owner per host” vs ADR-0046 flock edge.
10. **ADR-0007 amend.** Remote path remains OS Tailscale; add “Portal Settings can enroll + Serve” as the productized path. Keep “no ACL/posture editor / no Funnel for Portal”. Replace the hard “we do not integrate the Tailscale API” MVP clause with: **Settings may wrap CLI/LocalAPI for enroll + Serve only** — still not a full Tailscale admin UI.

### Should (non-blocking)

- Interactive login URL / open-link flow when auth key absent.
- Outbound dial to other tailnet hosts (status-only in Must).
- Windows / macOS install-detection messaging (Linux + WSL first).
- Path B (`libtailscale` embed) called out only under **Consequences** as a later option when “no OS Tailscale app” becomes a hard Must.

### Out of scope

- ACL / posture policy editor
- Full OS Tailscale VPN features (exit nodes, subnet router UI, …)
- Funnel public Portal
- Cursor company Integrations / `custom:grokbot` posture / multi-tailnet slots
- Blocking v1.0.9 Soft tip Final Soft/QA

## Consequences

- New Settings section + SecretStore key + optional `[tailscale]` config.
- Runbook `docs/portal-access.md` gains Settings enroll + Serve steps; keep manual install path.
- `/portal` prefers Serve HTTPS MagicDNS when available.
- Path B remains a documented escape hatch (Go build cost + community crate risk) — not Must.
- Single-user self-host: Tailscale is **transport trust**; Portal token auth (ADR-0006) still required.

## Rejected

- Funnel for Portal admin UI
- Embedding libtailscale/tsnet as Must
- Binding Portal to `0.0.0.0` as the Tailscale product path (loopback + Serve is the path)
- Copying Cursor-hosted node / company posture as product surface
