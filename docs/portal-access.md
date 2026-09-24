# Portal Access Runbook (ADR 0007)

How to reach the RustFox web portal from each device you actually use.
The portal is an admin surface (chat as the bot, edit settings, restart-sensitive
config) — it must never face the public internet directly.

## TL;DR decision table

| Where you are | What to do |
|---|---|
| On the VPS itself | `http://127.0.0.1:8090/` — works with the default bind |
| Home Wi-Fi (laptop/phone on LAN) | Set `bind = "0.0.0.0"` → open `http://<vps-lan-ip>:8090/` |
| Out and about (phone) | Install Tailscale on VPS + phone → `http://<vps-tailnet-ip>:8090/` |
| Don't know your URLs | Message the bot `/portal` in Telegram — it replies with working URLs for the current network |

## 1. LAN use

Default config (loopback only):

```toml
[portal]
enabled = true
port = 8090
bind = "127.0.0.1"
```

To use it from other devices on your home network, bind all interfaces:

```toml
bind = "0.0.0.0"     # then open http://<vps-ip-on-lan>:8090/
```

…or bind the LAN address only (recommended if the box has several interfaces
and you don't want the portal answering on any of them):

```toml
bind = "192.168.1.50"   # the exact address your router gave the VPS
```

Find candidates quickly: send the bot `/portal` — it probes the host and
lists LAN + tailnet URLs (best-effort, never fabricates URLs the bind
couldn't serve).

**Firewall note:** keep port 8090 blocked at any router-level firewall —
LAN binding is for trusted networks only. Auth is a token, but the channel
is plain HTTP: don't pretend otherwise. On untrusted Wi-Fi, use Tailscale.

## 2. Remote access = Tailscale (supported path)

No public port, real crypto, 5-minute setup:

1. Install Tailscale on the VPS (`curl -fsSL https://tailscale.com/install.sh | sh`) and `sudo tailscale up`.
2. Install the Tailscale app on your phone/laptop, same tailnet.
3. Get the VPS's tailnet address: `tailscale ip -4` (e.g. `100.101.102.103`).
4. Bind the portal to it (or to all interfaces, since tailnet is private):

```toml
bind = "100.101.102.103"   # tailnet IP — precise
# or
bind = "0.0.0.0"           # LAN + tailnet + loopback all work
```

5. On your phone open `http://100.101.102.103:8090/` (or tap the URL from `/portal`).

The `100.64.0.0/10` range is CGNAT — Tailscale's allocation. The `/portal`
command labels those addresses as tailnet URLs automatically.

**Rebind caveat:** a tailnet IP can drift after key expiry/reinstall. If
`/portal` stops showing your tailnet URL, recheck `tailscale ip -4` or bind
`0.0.0.0`.

## 3. Logging in

- You pinned `[portal] token_sha256 = "<hash>"` in config.toml → type the
  token itself in the login screen. Generate both:

```bash
python3 -c "import secrets,hashlib;t=secrets.token_hex(16);print('token:',t);print('hash :',hashlib.sha256(t.encode()).hexdigest())"
```

- Nothing configured? The startup log prints a temporary token once
  (`journalctl --user -u rustfox | grep "temporary login"`). Log in with it,
  then pin the hash — the temporary token dies with the process.

Sessions last 30 days and **survive restarts** (ADR 0008A): the cookie is
HMAC-signed with a per-install secret in `~/.rustfox/portal_secret.key`.
"Log out everywhere" bumps a generation counter and invalidates every
cookie instantly.

## 4. Telegram WebView tips

Opening a `/portal` URL inside Telegram's in-app browser is the intended
phone journey (chat history is server-side — picking up a Telegram
conversation in the portal works after one login).

- WebView password managers are awkward; that's why login persists 30 days.
- "Add to Home Screen" (Android Chrome / iOS Safari share sheet) gets you a
  standalone app feel — PWA manifest is on the M3 list.
- Auto-login via Telegram initData HMAC (no token typing at all in WebView)
  is the first M3 item.

## 5. Not supported (deliberately)

| Option | Why not |
|---|---|
| Self-signed HTTPS baked in | mobile trust flows are miserable; wrong layer |
| cloudflared/frp public tunnel | exposes an admin UI to internet scanners; token auth over TLS-but-public is a worse security posture than tailnet |
| Port-forward + DDNS | same scanner problem, plus NAT pain |

**Escalation path** if a genuinely public deployment is ever wanted: put
Caddy (or any TLS-terminating reverse proxy) in front with HTTPS + its own
auth, and keep `bind` on loopback. That's a supported-by-nobody-but-us
configuration; test the token cookie flow end-to-end before trusting it.

## Troubleshooting

- **`/portal` says disabled** → set `[portal] enabled = true`, restart the
  bot (`systemctl --user restart rustfox`).
- **Login rejected** → `token_sha256` must be the sha256 of the exact token
  string you type (no trailing spaces/newlines in config.toml). The token in
  the startup log is random per boot.
- **URL refuses to connect but log says `serving on http://0.0.0.0:8090`**
  → you're hitting an address the bind can't answer (e.g. bind = 127.0.0.1
  while using the LAN IP) — `/portal` tells you which addresses are
  configured.
- **Portal died but bot is fine** → by design: portal bind failure logs
  `Portal: failed to start — …` and the Telegram bot keeps running
  (best-effort, ADR 0004). Check the port isn't taken: `ss -tlnp | grep 8090`.
- **Blank page at `/`** → this binary was built without the frontend
  (`web/dist` stub). The API still works (`/api/health`). Fix:
  `cd web && npm ci && npm run build` then rebuild RustFox — CI does this
  automatically since M1, so it only bites source builds.
