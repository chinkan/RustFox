//! Portal URL discovery for the Telegram `/portal` entry point (ADR 0007).
//!
//! The portal binds loopback by default, so "which URL do I open on my
//! phone?" is not answerable from config alone: we probe the host's own
//! network addresses for LAN + tailnet (100.64.0.0/10 — CGNAT, the range
//! Tailscale assigns from) candidates. Everything here is best-effort with
//! hard timeouts; probe failures degrade to the loopback URL, never to an
//! error, and never fabricate a URL the bind could not actually serve.

use std::net::{IpAddr, UdpSocket};
use std::time::Duration;

use tokio::process::Command;

use crate::config::PortalConfig;

/// Tailscale's control socket path (used only for the "detected" hint).
const TS_SOCKET: &str = "/var/run/tailscale/tailscaled.sock";
/// Hard cap for every external probe so `/portal` can never hang the bot.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// CGNAT range Tailscale allocates interface addresses from (100.64/10).
fn is_tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 0x40,
        IpAddr::V6(_) => false,
    }
}

/// Best-effort primary outbound IP via the connected-UDP-socket trick
/// (no packets are sent; reveals the default-route source IP).
fn guess_lan_ip() -> Option<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    // connect() on UDP only records the peer address; works offline.
    sock.connect(("1.1.1.1", 80)).ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

/// Run a short CLI probe with a timeout, returning stdout on success.
/// `kill_on_drop` + the timeout together guarantee a wedged daemon can't
/// leave the child (or the reply) hanging.
async fn probe(name: &str, args: &[&str]) -> Option<String> {
    let child = Command::new(name)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let out = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        None
    }
}

/// Parse the first IPv4 address out of free text (CLI output).
fn parse_first_ipv4(text: &str) -> Option<IpAddr> {
    text.split_whitespace()
        .find(|t| t.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv4()))
        .and_then(|t| t.parse().ok())
}

/// Enumerate candidate machine addresses for wildcard binds:
/// LAN (UDP trick / `ip -4 addr`) + tailnet (`tailscale ip -4`, falling
/// back to scanning `ip -4 addr show` for CGNAT-range addresses).
/// Loopback is excluded; the caller adds it. May return empty.
async fn candidate_ips() -> Vec<IpAddr> {
    let mut lan = guess_lan_ip();
    let mut tails: Vec<IpAddr> = Vec::new();

    if let Some(text) = probe("tailscale", &["ip", "-4"]).await {
        if let Some(ip) = parse_first_ipv4(&text) {
            if is_tailnet(ip) {
                tails.push(ip);
            } else {
                lan.get_or_insert(ip);
            }
        }
    }
    if let Some(text) = probe("ip", &["-4", "addr", "show"]).await {
        for tok in text.split_whitespace() {
            let Some(rest) = tok.strip_prefix("inet") else {
                continue;
            };
            let Ok(ip) = rest.split('/').next().unwrap_or("").parse::<IpAddr>() else {
                continue;
            };
            if ip.is_loopback() {
                continue;
            }
            if is_tailnet(ip) {
                if !tails.contains(&ip) {
                    tails.push(ip);
                }
            } else if lan.is_none() {
                lan = Some(ip);
            }
        }
    }

    let mut out = Vec::new();
    out.extend(lan);
    out.extend(tails.into_iter().take(3));
    out
}

/// Whether a Tailscale install is visible on this host (for the hint line).
async fn tailscale_present() -> bool {
    probe("tailscale", &["ip", "-4"]).await.is_some() || std::path::Path::new(TS_SOCKET).exists()
}

/// Portal URLs the operator can actually open, honouring the bind.
///
/// - loopback bind → loopback only + a `hint` that remote use needs a bind
///   change (never fabricate LAN/tailnet URLs the socket couldn't serve).
/// - wildcard bind (`0.0.0.0`/empty) → discovered LAN + tailnet candidates,
///   loopback last.
/// - explicit bind address → that address verbatim.
pub async fn portal_urls(cfg: &PortalConfig) -> (Vec<String>, Option<String>) {
    let port = cfg.port;
    let loopback_url = format!("http://127.0.0.1:{port}/");

    let is_loopback_bind = cfg.bind.is_empty()
        || cfg.bind == "127.0.0.1"
        || cfg.bind.eq_ignore_ascii_case("localhost");
    let is_wildcard =
        cfg.bind.is_empty() || cfg.bind == "0.0.0.0" || cfg.bind == "::" || cfg.bind == "*";

    if is_loopback_bind && !is_wildcard {
        return (
            vec![loopback_url],
            Some(
                "Only this machine can reach the portal while bind = 127.0.0.1. \
                 For your phone: set `bind = \"0.0.0.0\"` (LAN) or the tailnet IP, then restart."
                    .to_string(),
            ),
        );
    }
    if is_wildcard {
        let mut urls: Vec<String> = candidate_ips()
            .await
            .into_iter()
            .map(|ip| format!("http://{ip}:{port}/"))
            .collect();
        urls.push(loopback_url);
        urls.dedup();
        return (urls, None);
    }
    (vec![format!("http://{}:{port}/", cfg.bind)], None)
}

/// The `/portal` reply body for Telegram (markdown-flavoured, no tables).
pub async fn portal_reply(cfg: &PortalConfig) -> String {
    if !cfg.enabled {
        return "🌐 Portal is **disabled** — set `[portal] enabled = true` in `config.toml` and restart.".to_string();
    }
    let (urls, hint) = portal_urls(cfg).await;
    let mut lines = vec!["🌐 **RustFox Portal**".to_string()];
    for u in &urls {
        lines.push(format!("  • {u}"));
    }
    if let Some(h) = hint {
        lines.push(format!("⚠️ {h}"));
    }
    if tailscale_present().await {
        lines.push(
            "✓ Tailscale detected — the 100.x URL works anywhere your tailnet does.".to_string(),
        );
    } else {
        lines.push("No Tailscale here — remote access from your phone: install it, see `docs/portal-access.md`.".to_string());
    }
    lines.push("Login token: the `[portal] token_sha256` you pinned; if none was set, the startup log prints a temporary one.".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(bind: &str, port: u16) -> PortalConfig {
        PortalConfig {
            enabled: true,
            port,
            bind: bind.into(),
            ..Default::default()
        }
    }

    #[test]
    fn tailnet_range_detection() {
        assert!(is_tailnet("100.64.0.1".parse().unwrap()));
        assert!(is_tailnet("100.127.255.255".parse().unwrap()));
        assert!(!is_tailnet("100.63.255.255".parse().unwrap()));
        assert!(!is_tailnet("100.128.0.1".parse().unwrap()));
        assert!(!is_tailnet("192.168.1.5".parse().unwrap()));
        assert!(!is_tailnet("::1".parse().unwrap()));
    }

    #[test]
    fn parses_first_ipv4_from_cli_text() {
        assert_eq!(
            parse_first_ipv4("100.82.14.23\nfd7a:115c:a1e0::9"),
            Some("100.82.14.23".parse::<IpAddr>().unwrap())
        );
        assert_eq!(parse_first_ipv4("only v6 fd7a::1"), None);
        assert_eq!(parse_first_ipv4("no ips here"), None);
        assert_eq!(parse_first_ipv4(""), None);
    }

    #[tokio::test]
    async fn explicit_bind_yields_only_that_url() {
        let (urls, hint) = portal_urls(&cfg("10.0.0.7", 9000)).await;
        assert_eq!(urls, vec!["http://10.0.0.7:9000/"]);
        assert!(hint.is_none());
    }

    #[tokio::test]
    async fn loopback_bind_never_fabricates_remote_urls() {
        let (urls, hint) = portal_urls(&cfg("127.0.0.1", 8090)).await;
        assert_eq!(urls, vec!["http://127.0.0.1:8090/"]);
        assert!(
            hint.is_some(),
            "loopback bind must explain remote limitation"
        );
    }

    #[tokio::test]
    async fn wildcard_bind_always_includes_loopback_fallback() {
        let (urls, hint) = portal_urls(&cfg("0.0.0.0", 8090)).await;
        assert!(
            urls.iter().any(|u| u == "http://127.0.0.1:8090/"),
            "loopback URL missing: {urls:?}"
        );
        assert!(hint.is_none());
    }

    #[tokio::test]
    async fn disabled_config_says_so() {
        let r = portal_reply(&PortalConfig {
            enabled: false,
            ..Default::default()
        })
        .await;
        assert!(r.contains("disabled"));
    }

    #[tokio::test]
    async fn reply_contains_urls_and_token_note() {
        let r = portal_reply(&cfg("0.0.0.0", 8090)).await;
        assert!(r.contains("http://"), "no URL in reply: {r}");
        assert!(r.contains("8090"));
        assert!(r.contains("token"));
    }
}
