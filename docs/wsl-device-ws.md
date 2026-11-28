# Device WS on the LAN (WSL, Windows, macOS, Linux)

How a board on the trusted home LAN reaches RustFox Device WS, and how
DNS-SD `_rustfox-device._tcp` is published. This does not change Device WS
auth, paths, or the default bind (`127.0.0.1:8090`).

The default WSL path is **portproxy + firewall + the Windows sidecar**.
Mirrored WSL networking is not the supported path. Portproxy forwards TCP.
It does not carry multicast, so rustfox inside WSL does not advertise.

Companion firmware browse, NVS cache, and compiled-URI fallback live in
[rustfox-esp32-c6#5](https://github.com/chinkan/rustfox-esp32-c6/issues/5).
A companion firmware PR will be opened there. This host slice does not
put firmware in this repo.

## What is advertised

| Record | Value |
|---|---|
| Type | `_rustfox-device._tcp` |
| Instance | `rustfox`, or `[device_ws].mdns_instance` |
| SRV port | `[portal].port` (default 8090) |
| TXT `path` | `/api/device/v1/ws` |
| TXT `proto` | `1` |
| TXT `audio` | `/api/device/v1/audio` |
| A | One LAN IPv4 the board can open |

If `audio` were omitted, the board would rewrite `ws://` to `http://` on
the same host and port. This slice includes it.

No device token, portal token, or other secret is placed in TXT, SRV, or
the instance name. Do not set `mdns_instance` to a token.

These addresses are never advertised: `127.0.0.1` / other loopback, `::1`,
an APIPA `169.254.x.x` address, a Tailscale `100.x` address, or an address
on a WSL / Hyper-V virtual NIC (including the WSL NAT `172.x` address on
`vEthernet`). IPv6 is not advertised. Docker bridge addresses (`docker0`,
`br-*`, `veth*`) are skipped for the same reason: the board is not on that
network.

`[device_ws].mdns` and `mdns_instance` are `config.toml` keys. Portal
Settings does not edit them.

## 1. Portal bind and port

Device WS uses the portal bind and port. The default bind is loopback.
Advertising does not change how rustfox listens. If the listener is only
on loopback, rustfox does not advertise and logs that there is no
LAN-reachable listen.

For a board (and for Windows portproxy) set:

```toml
[portal]
enabled = true          # optional; Device WS can run with the UI off
port = 8090
bind = "0.0.0.0"        # or the LAN IPv4, not 127.0.0.1

[device_ws]
enabled = true
# mdns = true           # absent means true. false skips in-process advertise
# mdns_instance = "rustfox"
```

`bind = "0.0.0.0"` listens on every IPv4 interface. Prefer the LAN IPv4
when the host has extra interfaces and you do not want the socket on all
of them. The address you bind must be one the board can route to.

Inside WSL, `127.0.0.1` is the guest loopback. Windows portproxy connects
to the WSL eth0 address, so the guest must listen on `0.0.0.0` or that
eth0 address. rustfox still will not advertise from inside the guest.

## 2. WSL: Windows portproxy for TCP 8090

On the Windows host, in an elevated prompt:

```bat
wsl.exe -e hostname -I
netsh interface portproxy add v4tov4 listenaddress=0.0.0.0 listenport=8090 connectaddress=WSL_IPV4 connectport=8090
netsh interface portproxy show v4tov4
```

`WSL_IPV4` is the guest address from `hostname -I` (not a `127.x` address,
and not the Windows `vEthernet (WSL)` address). The WSL address can change
across restarts. When it does, delete and add the proxy again:

```bat
netsh interface portproxy delete v4tov4 listenaddress=0.0.0.0 listenport=8090
```

The board never needs that WSL address. It connects to the Windows LAN
IPv4 that the sidecar advertises, on port 8090. Portproxy forwards that
TCP connection into the guest.

From the Windows host, `http://127.0.0.1:8090/api/device/v1/ws` should reach
the guest after the proxy is in place (the WebSocket still requires the
device token). If it does not, the guest is not listening on a
portproxy-reachable address.

## 3. Windows firewall, including Public

Home PCs are often on the Public profile. A rule that only allows Private
does not match. Allow inbound TCP 8090 on Domain, Private, and Public:

```bat
netsh advfirewall firewall add rule name="RustFox Device WS TCP 8090" dir=in action=allow protocol=TCP localport=8090 profile=domain,private,public
```

The sidecar also has to receive mDNS queries (UDP 5353):

```bat
netsh advfirewall firewall add rule name="RustFox mDNS UDP 5353" dir=in action=allow protocol=UDP localport=5353 profile=domain,private,public
```

Outbound multicast to `224.0.0.251:5353` is allowed by the default Windows
outbound policy. If outbound UDP is restricted, allow `rustfox-mdns.exe`
to send UDP 5353.

## 4. Windows sidecar

Build on Windows from this repo (no Bonjour install):

```bat
cargo build --release --bin rustfox-mdns
```

The binary is `target\release\rustfox-mdns.exe`.

### Manual start

```bat
rustfox-mdns.exe
rustfox-mdns.exe --address 192.168.1.40 --port 8090 --instance rustfox
```

With no `--address`, it picks the first publishable Windows LAN IPv4.
It skips loopback, WSL/Hyper-V virtual NICs (`vEthernet`, Hyper-V),
APIPA, and Tailscale `100.x`. When several LAN addresses remain, it
prefers an RFC1918 address and otherwise keeps interface order. `--address`,
`--port`, and `--instance` override that. `--port` is the portproxy listen
port, not the WSL guest address. An override that is loopback, APIPA,
Tailscale `100.x`, or currently assigned to a WSL/Hyper-V NIC is refused.

Leave the process running. Ctrl+C stops it. It prints the address it
published. rustfox inside WSL logs that this sidecar is required and does
not publish a `172.x` (or other guest) address.

### Login / startup (Task Scheduler)

A Windows service is not required. An at-logon scheduled task is enough.
Adjust the path. Run this in a prompt that can create tasks for your user:

```bat
schtasks /Create /TN "RustFox Device mDNS" /SC ONLOGON /RL LIMITED /F /TR "\"C:\src\RustFox\target\release\rustfox-mdns.exe\" --port 8090"
```

`/SC ONLOGON` starts it when you sign in. `/RL LIMITED` does not request
elevation. Confirm in Task Scheduler that the task's "Run only when user
is logged on" setting matches how you use the PC, and that the history
shows the sidecar still running. To stop advertising, end that task.

The same command with `--address` and `--instance` belongs inside `/TR`
when you do not want the auto-picked address.

## 5. Native macOS and native Linux

No portproxy. rustfox advertises in-process when `[device_ws] enabled` is
true and `[device_ws].mdns` is not false. Avahi and Apple Bonjour do not
need to be installed.

The bind still has to be a LAN-reachable address. `bind = "0.0.0.0"` or
`bind = "<lan-ipv4>"` is required. `bind = "127.0.0.1"` does not advertise
and the board cannot connect, on macOS and on Linux, the same as anywhere
else. A missing publishable LAN IPv4 is logged and not advertised.

On Linux, the host firewall must allow the board to open TCP 8090 and must
allow mDNS. mDNS is UDP 5353 to and from `224.0.0.251`. For example, with
ufw:

```bash
sudo ufw allow 8090/tcp
sudo ufw allow 5353/udp
```

A rule that drops multicast, or an outbound filter that drops UDP 5353,
hides the advertisement even when rustfox is bound correctly. macOS may
prompt to allow incoming connections for the rustfox process.

## 6. Docker and other NAT VMs

A container or VM behind NAT has the same multicast and address limit as
WSL. The guest address is not a LAN address the board can use, and
multicast from the guest does not reach the LAN. This slice does not add
a second sidecar for Docker. Publish `_rustfox-device._tcp` from a process
that is actually on the LAN (native macOS or Linux, or the Windows sidecar
in front of WSL). A container using host networking is native Linux for
this purpose: in-process advertise, LAN bind, UDP 5353.

## What rustfox logs instead of advertising

- Device WS is off.
- `[device_ws].mdns` is false (native hosts only).
- WSL: in-process mDNS is off, and the Windows sidecar is required.
- The bind is loopback, or not an IPv4 LAN listen.
- The chosen address is not publishable, or there is no publishable LAN IPv4.

A failure to bind UDP 5353 is logged and does not stop the bot. Fix the
firewall or the port conflict and restart.
