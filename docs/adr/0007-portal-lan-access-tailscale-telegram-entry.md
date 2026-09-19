# ADR 0007: Portal Access — LAN Default, Tailscale for Remote, Telegram Link as Entry

## Status
Accepted

## Date
2026-09-19

## Context
Portal usage is mobile-first for the operator (Telegram on the phone all day). The default bind `127.0.0.1` (ADR 0004) makes the portal reachable only from the VPS itself; binding to the LAN address works on the home Wi-Fi but is useless when out and about, and port-forwarding a plain-HTTP admin UI to the public internet is unacceptable (token auth over cleartext + internet scanners).

Options considered:
- (a) LAN-only, acknowledge phone-remote is out of scope
- (b) LAN bind + user-level overlay network (Tailscale/ZeroTier) for remote; document as the supported remote-access path
- (c) Built-in HTTPS self-signed cert + mobile trust flow
- (d) Public reverse tunnel (cloudflared/frp)

## Decision
Go with **(b)**:

1. `portal.bind` becomes operator-configurable (`127.0.0.1` default, `0.0.0.0`/LAN IP for home Wi-Fi use). No auto-detection of interfaces in MVP.
2. **Remote access = Tailscale** (already installed pattern on home servers). The VPS joins a tailnet; the phone's Tailscale app gives reach anywhere. The portal never needs a public port. We ship a short runbook (`docs/portal-access.md`), we do not integrate the Tailscale API.
3. **Telegram as the entry point**: a `/portal` bot command replies with the portal URL(s) for the current network (LAN URL + tailnet URL when detectable via `tailscale ip -4` at startup, best-effort). Since the chat session history lives server-side (SQLite), opening the portal in Telegram's in-app WebView and logging in once is enough to pick up any conversation — Telegram WebView auto-login (initData HMAC, ADR 0006 follow-up) then removes even the token typing for Telegram-launched sessions.
4. Self-signed HTTPS (c) and tunnels (d) are documented as *possible but unsupported* for MVP. TLS-terminating reverse proxy (Caddy) is the escalation path if a public deployment is ever wanted.

## Rationale
- Tailscale turns "portal anywhere" into a 5-minute account-level setup instead of portal code; wireguard on the tailnet is stronger crypto than anything we would embed, and keeps `bind` defaults safe.
- Telegram deep-link entry matches the operator's actual journey (lives in Telegram → taps link → portal), and its WebView is a real mobile browser — the responsive layout + PWA manifest cover "add to home screen" without extra code.
- Auto-login in WebView is the only piece that feels native; it is isolated to the auth layer so nothing else waits on it.

## Consequences
- `docs/portal-access.md` runbook (LAN bind, Tailscale, `/portal` command, WebView tips) becomes a deliverable of M2.
- `/portal` command handler in the Telegram platform layer (small: reads config + optional `tailscale ip` probe, sends link).
- Telegram initData auto-login moves from "deferred backlog" to **M3 first item** because out-and-about use makes the manual token paste the daily friction point (mobile keyboards + password managers in WebViews are painful).
- If tailnet probe fails or Tailscale absent, `/portal` degrades to showing only the LAN URL — zero hard dependency.
