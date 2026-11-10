//! Portal phone link for the Telegram `/portal` command (ADR-0007, ADR-0065 §7).
//!
//! One primary link: MagicDNS Serve HTTPS when Tailscale can serve, otherwise
//! a LAN URL only when the bind can actually serve that address. Loopback with
//! no Serve keeps the ADR-0007 hint. Probes and `tailscale serve` share a hard
//! cap; failures fall through and never crash the bot. Funnel is never started.

use std::net::{IpAddr, UdpSocket};
use std::time::Duration;

use tokio::process::Command;

use crate::config::PortalConfig;

/// Hard cap for every external CLI call so `/portal` cannot hang the bot.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// Cap for the whole Serve probe + start sequence.
const PORTAL_BUDGET: Duration = Duration::from_secs(8);

const TOKEN_NOTE: &str = "Login token: the `[portal] token_sha256` you pinned; if none was set, the startup log prints a temporary one.";

const LOOPBACK_HINT: &str = "Only this machine can reach the portal while bind = 127.0.0.1. \
     For your phone: set `bind = \"0.0.0.0\"` (LAN) or the tailnet IP, then restart.";

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
        .stdin(std::process::Stdio::null())
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

/// Host facts `/portal` is allowed to ask for. Tests substitute a scripted probe.
trait HostProbe: Send + Sync {
    fn run<'a>(
        &'a self,
        program: &'a str,
        args: &'a [&'a str],
    ) -> impl std::future::Future<Output = Option<String>> + Send + 'a;

    fn lan_ip(&self) -> Option<IpAddr>;
}

struct SystemProbe;

impl HostProbe for SystemProbe {
    async fn run(&self, program: &str, args: &[&str]) -> Option<String> {
        probe(program, args).await
    }

    fn lan_ip(&self) -> Option<IpAddr> {
        guess_lan_ip()
    }
}

/// Telegram `/portal` reply. `telegram_parts` is plain text plus official entities.
pub struct PortalReply {
    markdown: String,
}

impl PortalReply {
    /// Plain text and message entities. A phone link is a `text_link` entity.
    pub fn telegram_parts(&self) -> (String, Vec<teloxide::types::MessageEntity>) {
        crate::utils::markdown_entities::markdown_to_entities(&self.markdown)
    }
}

fn link_reply(url: &str) -> PortalReply {
    PortalReply {
        markdown: format!("🌐 **RustFox Portal**\n\n[{url}]({url})\n\n{TOKEN_NOTE}"),
    }
}

fn hint_reply(loopback: bool) -> PortalReply {
    let warning = if loopback {
        LOOPBACK_HINT
    } else {
        "No phone address was found for this bind. Check the host network, then try again."
    };
    PortalReply {
        markdown: format!(
            "🌐 **RustFox Portal**\n\n⚠️ {warning}\n\n\
             No Tailscale here — remote access from your phone: install it, see `docs/portal-access.md`.\n\n\
             {TOKEN_NOTE}"
        ),
    }
}

fn disabled_reply() -> PortalReply {
    PortalReply {
        markdown: "🌐 Portal is **disabled** — set `[portal] enabled = true` in `config.toml` and restart.".to_string(),
    }
}

fn bind_text(cfg: &PortalConfig) -> &str {
    cfg.bind.trim()
}

fn is_wildcard(cfg: &PortalConfig) -> bool {
    matches!(bind_text(cfg), "" | "0.0.0.0" | "::" | "[::]" | "*")
}

fn is_loopback_only(cfg: &PortalConfig) -> bool {
    let bind = bind_text(cfg);
    bind == "127.0.0.1"
        || bind == "::1"
        || bind == "[::1]"
        || bind.eq_ignore_ascii_case("localhost")
}

fn usable_phone_ip(ip: IpAddr) -> bool {
    !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast()
}

fn explicit_phone_url(cfg: &PortalConfig) -> Option<String> {
    if is_wildcard(cfg) || is_loopback_only(cfg) {
        return None;
    }
    let bind = bind_text(cfg);
    match bind.parse::<IpAddr>() {
        Ok(ip) if usable_phone_ip(ip) => Some(format!("http://{ip}:{}/", cfg.port)),
        Ok(_) => None,
        Err(_) => Some(format!("http://{bind}:{}/", cfg.port)),
    }
}

/// Prefer a non-tailnet address from `ip -4 addr`. A tailnet address is used
/// only when it is the only address the wildcard bind can serve.
fn parse_ip_addr(text: &str) -> Option<IpAddr> {
    let mut tokens = text.split_whitespace();
    let mut lan = None;
    let mut tailnet = None;
    while let Some(tok) = tokens.next() {
        if tok != "inet" {
            continue;
        }
        let Some(addr) = tokens.next() else { break };
        let Ok(ip) = addr.split('/').next().unwrap_or("").parse::<IpAddr>() else {
            continue;
        };
        if !usable_phone_ip(ip) {
            continue;
        }
        if is_tailnet(ip) {
            tailnet.get_or_insert(ip);
        } else {
            lan.get_or_insert(ip);
        }
    }
    lan.or(tailnet)
}

async fn lan_phone_url(cfg: &PortalConfig, probe: &impl HostProbe) -> Option<String> {
    if let Some(url) = explicit_phone_url(cfg) {
        return Some(url);
    }
    if !is_wildcard(cfg) {
        return None;
    }
    let guessed = probe.lan_ip().filter(|ip| usable_phone_ip(*ip));
    let ip = match guessed {
        Some(ip) if !is_tailnet(ip) => Some(ip),
        other => {
            let scanned = probe
                .run("ip", &["-4", "addr", "show"])
                .await
                .as_deref()
                .and_then(parse_ip_addr);
            scanned.or(other)
        }
    };
    ip.map(|ip| format!("http://{ip}:{}/", cfg.port))
}

fn json_value(text: &str) -> Option<serde_json::Value> {
    serde_json::from_str(text).ok()
}

fn backend_running(status_json: &str) -> bool {
    json_value(status_json)
        .and_then(|v| {
            v.get("BackendState")
                .and_then(|s| s.as_str())
                .map(|s| s == "Running")
        })
        .unwrap_or(false)
}

fn magic_dns_name(status_json: &str) -> Option<String> {
    let name = json_value(status_json)?
        .get("Self")?
        .get("DNSName")?
        .as_str()?
        .trim()
        .trim_end_matches('.')
        .to_string();
    if name.is_empty() || !name.ends_with(".ts.net") {
        return None;
    }
    Some(name)
}

fn serve_is_up(serve_json: &str) -> bool {
    let Some(v) = json_value(serve_json) else {
        return false;
    };
    if v.get("Web")
        .and_then(|w| w.as_object())
        .is_some_and(|web| !web.is_empty())
    {
        return true;
    }
    v.get("TCP")
        .and_then(|tcp| tcp.as_object())
        .is_some_and(|tcp| {
            tcp.values()
                .any(|spec| spec.get("HTTPS").and_then(|flag| flag.as_bool()) == Some(true))
        })
}

fn https_url_for_host(host: &str, port: Option<&str>) -> String {
    let host = host.trim().trim_end_matches('.');
    match port {
        Some("443") | None => format!("https://{host}/"),
        Some(port) => format!("https://{host}:{port}/"),
    }
}

fn host_from_serve_key(key: &str) -> Option<String> {
    let (host, port) = match key.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && !host.is_empty() => (host, Some(port)),
        _ => (key, None),
    };
    if !host.trim_end_matches('.').ends_with(".ts.net") {
        return None;
    }
    Some(https_url_for_host(host, port))
}

fn serve_public_url(serve_json: &str, dns: &str) -> String {
    if let Some(web) = json_value(serve_json).and_then(|v| v.get("Web").cloned()) {
        if let Some(web) = web.as_object() {
            for key in web.keys() {
                if let Some(url) = host_from_serve_key(key) {
                    return url;
                }
            }
        }
    }
    format!("https://{dns}/")
}

/// Start HTTPS Serve to loopback when the node can serve and Serve is down.
/// `None` is the soft-fail (missing CLI, logged out, start failed).
async fn magicdns_serve_url(cfg: &PortalConfig, probe: &impl HostProbe) -> Option<String> {
    let status = probe.run("tailscale", &["status", "--json"]).await?;
    if !backend_running(&status) {
        return None;
    }
    let dns = magic_dns_name(&status)?;
    if let Some(serve) = probe.run("tailscale", &["serve", "status", "--json"]).await {
        if serve_is_up(&serve) {
            return Some(serve_public_url(&serve, &dns));
        }
    }
    let target = format!("http://127.0.0.1:{}", cfg.port);
    probe
        .run(
            "tailscale",
            &["serve", "--bg", "--https=443", "--yes", target.as_str()],
        )
        .await?;
    Some(format!("https://{dns}/"))
}

async fn build_reply(cfg: &PortalConfig, probe: &impl HostProbe) -> PortalReply {
    if let Some(url) = magicdns_serve_url(cfg, probe).await {
        return link_reply(&url);
    }
    if let Some(url) = lan_phone_url(cfg, probe).await {
        return link_reply(&url);
    }
    hint_reply(is_loopback_only(cfg))
}

fn fallback_reply(cfg: &PortalConfig, lan: Option<IpAddr>) -> PortalReply {
    if let Some(url) = explicit_phone_url(cfg) {
        return link_reply(&url);
    }
    if is_wildcard(cfg) {
        if let Some(ip) = lan.filter(|ip| usable_phone_ip(*ip)) {
            return link_reply(&format!("http://{ip}:{}/", cfg.port));
        }
    }
    hint_reply(is_loopback_only(cfg))
}

async fn portal_reply_bounded(
    cfg: &PortalConfig,
    probe: &impl HostProbe,
    budget: Duration,
) -> PortalReply {
    if !cfg.enabled {
        return disabled_reply();
    }
    match tokio::time::timeout(budget, build_reply(cfg, probe)).await {
        Ok(reply) => reply,
        Err(_) => fallback_reply(cfg, probe.lan_ip()),
    }
}

/// The `/portal` reply. Serve is started when Tailscale can serve and is not
/// already up. The result is always a reply — probe and Serve failures do not
/// surface as errors.
pub async fn portal_reply(cfg: &PortalConfig) -> PortalReply {
    portal_reply_bounded(cfg, &SystemProbe, PORTAL_BUDGET).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use teloxide::types::MessageEntityKind;

    fn cfg(bind: &str, port: u16) -> PortalConfig {
        PortalConfig {
            enabled: true,
            port,
            bind: bind.into(),
            ..Default::default()
        }
    }

    fn text_link_urls(reply: &PortalReply) -> Vec<String> {
        reply
            .telegram_parts()
            .1
            .into_iter()
            .filter_map(|entity| match entity.kind {
                MessageEntityKind::TextLink { url } => Some(url.to_string()),
                _ => None,
            })
            .collect()
    }

    fn plain(reply: &PortalReply) -> String {
        reply.telegram_parts().0
    }

    fn assert_one_tappable(reply: &PortalReply, url: &str) {
        let (body, entities) = reply.telegram_parts();
        let links: Vec<_> = entities
            .iter()
            .filter_map(|entity| match &entity.kind {
                MessageEntityKind::TextLink { url } => {
                    Some((entity.offset, entity.length, url.to_string()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            links.len(),
            1,
            "primary phone link must be one official text_link, not a plain URL list: {body}"
        );
        let (offset, length, link) = &links[0];
        assert_eq!(link, url);
        let utf16: Vec<u16> = body.encode_utf16().collect();
        let covered = String::from_utf16(&utf16[*offset..*offset + *length])
            .expect("text_link offset must cover valid UTF-16");
        assert_eq!(
            covered, *url,
            "text_link must cover the phone URL in the message text"
        );
        assert!(
            !body.contains("127.0.0.1") && !body.contains("0.0.0.0"),
            "phone link must not be loopback or wildcard: {body}"
        );
        assert_eq!(
            body.matches("http://").count() + body.matches("https://").count(),
            1,
            "reply must carry one URL, not a list: {body}"
        );
    }

    fn running_status(dns: &str) -> String {
        serde_json::json!({
            "BackendState": "Running",
            "Self": {"DNSName": dns, "Online": true}
        })
        .to_string()
    }

    fn serve_https(host: &str, port: u16) -> String {
        let key = format!("{host}:443");
        serde_json::json!({
            "TCP": {"443": {"HTTPS": true}},
            "Web": {
                key.clone(): {
                    "Handlers": {
                        "/": {"Proxy": format!("http://127.0.0.1:{port}")}
                    }
                }
            },
            "AllowFunnel": {key: false}
        })
        .to_string()
    }

    struct FakeHost {
        calls: Mutex<Vec<Vec<String>>>,
        lan: Option<IpAddr>,
        status: Option<String>,
        serve_before: Option<String>,
        serve_start_ok: bool,
        fail_all: bool,
    }

    impl FakeHost {
        fn offline(lan: Option<IpAddr>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                lan,
                status: None,
                serve_before: None,
                serve_start_ok: false,
                fail_all: true,
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("calls").clone()
        }

        fn assert_funnel_never_and_no_login(&self) {
            for args in self.calls() {
                let joined = args.join(" ");
                assert!(
                    !joined.contains("funnel"),
                    "Funnel must never be enabled: {args:?}"
                );
                assert_ne!(args.first().map(String::as_str), Some("login"));
                assert_ne!(args.first().map(String::as_str), Some("up"));
            }
        }
    }

    impl HostProbe for FakeHost {
        async fn run(&self, program: &str, args: &[&str]) -> Option<String> {
            let mut recorded = vec![program.to_string()];
            recorded.extend(args.iter().map(|s| (*s).to_string()));
            self.calls.lock().expect("calls").push(recorded);
            if self.fail_all || program != "tailscale" {
                return None;
            }
            match args {
                ["status", "--json"] => self.status.clone(),
                ["serve", "status", "--json"] => self.serve_before.clone(),
                ["serve", "--bg", "--https=443", "--yes", target] => {
                    assert!(
                        target.starts_with("http://127.0.0.1:"),
                        "serve target must be the portal loopback: {target}"
                    );
                    if self.serve_start_ok {
                        Some(String::new())
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }

        fn lan_ip(&self) -> Option<IpAddr> {
            self.lan
        }
    }

    struct HangProbe;

    impl HostProbe for HangProbe {
        async fn run(&self, _program: &str, _args: &[&str]) -> Option<String> {
            std::future::pending::<()>().await;
            None
        }

        fn lan_ip(&self) -> Option<IpAddr> {
            None
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
    fn parses_phone_address_from_ip_addr() {
        let text = "inet 127.0.0.1/8 inet 192.168.1.20/24 inet 100.82.14.23/32";
        assert_eq!(
            parse_ip_addr(text),
            Some("192.168.1.20".parse::<IpAddr>().unwrap())
        );
        assert_eq!(
            parse_ip_addr("inet 100.82.14.23/32"),
            Some("100.82.14.23".parse::<IpAddr>().unwrap())
        );
        assert_eq!(parse_ip_addr("inet 127.0.0.1/8 inet 0.0.0.0/0"), None);
        assert_eq!(parse_ip_addr("no ips here"), None);
    }

    #[tokio::test]
    async fn serve_already_up_replies_with_magicdns_text_link() {
        let host = FakeHost {
            calls: Mutex::new(Vec::new()),
            lan: Some("192.168.1.20".parse().unwrap()),
            status: Some(running_status("fox.tailnet.ts.net.")),
            serve_before: Some(serve_https("fox.tailnet.ts.net", 8090)),
            serve_start_ok: true,
            fail_all: false,
        };
        let reply = portal_reply_bounded(&cfg("127.0.0.1", 8090), &host, PORTAL_BUDGET).await;
        assert_one_tappable(&reply, "https://fox.tailnet.ts.net/");
        assert!(plain(&reply).contains("token"));
        let calls = host.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.as_slice() == ["tailscale", "serve", "status", "--json"]),
            "must read existing Serve status: {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.iter().any(|a| a == "--bg")),
            "already-up Serve must be reused, not restarted: {calls:?}"
        );
        host.assert_funnel_never_and_no_login();
    }

    #[tokio::test]
    async fn serve_start_then_magicdns_text_link() {
        let host = FakeHost {
            calls: Mutex::new(Vec::new()),
            lan: Some("192.168.1.20".parse().unwrap()),
            status: Some(running_status("fox.tailnet.ts.net.")),
            serve_before: Some("{}".to_string()),
            serve_start_ok: true,
            fail_all: false,
        };
        let reply = portal_reply_bounded(&cfg("127.0.0.1", 8090), &host, PORTAL_BUDGET).await;
        assert_one_tappable(&reply, "https://fox.tailnet.ts.net/");
        let calls = host.calls();
        assert!(
            calls.iter().any(|c| {
                c.as_slice()
                    == [
                        "tailscale",
                        "serve",
                        "--bg",
                        "--https=443",
                        "--yes",
                        "http://127.0.0.1:8090",
                    ]
            }),
            "must start HTTPS Serve at the portal loopback: {calls:?}"
        );
        host.assert_funnel_never_and_no_login();
    }

    #[tokio::test]
    async fn tailscale_unavailable_wildcard_bind_is_one_lan_text_link() {
        let host = FakeHost::offline(Some("192.168.1.20".parse().unwrap()));
        let reply = portal_reply_bounded(&cfg("0.0.0.0", 8090), &host, PORTAL_BUDGET).await;
        assert_one_tappable(&reply, "http://192.168.1.20:8090/");
        host.assert_funnel_never_and_no_login();
    }

    #[tokio::test]
    async fn explicit_lan_bind_without_serve_is_that_address() {
        let host = FakeHost::offline(None);
        let reply = portal_reply_bounded(&cfg("10.0.0.7", 9000), &host, PORTAL_BUDGET).await;
        assert_one_tappable(&reply, "http://10.0.0.7:9000/");
    }

    #[tokio::test]
    async fn loopback_without_serve_is_adr0007_hint_without_a_lan_url() {
        let host = FakeHost::offline(Some("192.168.1.20".parse().unwrap()));
        let reply = portal_reply_bounded(&cfg("127.0.0.1", 8090), &host, PORTAL_BUDGET).await;
        assert!(
            text_link_urls(&reply).is_empty(),
            "loopback without Serve must not invent a tappable phone link: {:?}",
            text_link_urls(&reply)
        );
        let body = plain(&reply);
        assert!(
            body.contains("Only this machine can reach the portal"),
            "missing ADR-0007 hint: {body}"
        );
        assert!(
            !body.contains("http://") && !body.contains("https://"),
            "must not fabricate a LAN or loopback URL: {body}"
        );
        assert!(
            !body.contains("192.168.1.20"),
            "must not invent a LAN URL the loopback socket cannot serve: {body}"
        );
    }

    #[tokio::test]
    async fn localhost_bind_without_serve_uses_the_same_hint() {
        let host = FakeHost::offline(Some("10.1.1.1".parse().unwrap()));
        let reply = portal_reply_bounded(&cfg("localhost", 8090), &host, PORTAL_BUDGET).await;
        assert!(text_link_urls(&reply).is_empty());
        assert!(!plain(&reply).contains("http://"));
        assert!(!plain(&reply).contains("10.1.1.1"));
    }

    #[tokio::test]
    async fn needs_login_does_not_start_serve_or_interactive_login() {
        let host = FakeHost {
            calls: Mutex::new(Vec::new()),
            lan: None,
            status: Some(
                serde_json::json!({"BackendState": "NeedsLogin", "Self": {"DNSName": ""}})
                    .to_string(),
            ),
            serve_before: None,
            serve_start_ok: true,
            fail_all: false,
        };
        let reply = portal_reply_bounded(&cfg("127.0.0.1", 8090), &host, PORTAL_BUDGET).await;
        assert!(text_link_urls(&reply).is_empty());
        let calls = host.calls();
        assert!(
            !calls
                .iter()
                .any(|c| c.iter().any(|a| a == "--bg" || a == "login" || a == "up")),
            "NeedsLogin must not start Serve or interactive login: {calls:?}"
        );
        host.assert_funnel_never_and_no_login();
    }

    #[tokio::test]
    async fn serve_start_failure_on_wildcard_falls_through_to_lan() {
        let host = FakeHost {
            calls: Mutex::new(Vec::new()),
            lan: Some("192.168.1.50".parse().unwrap()),
            status: Some(running_status("fox.tailnet.ts.net.")),
            serve_before: Some("{}".to_string()),
            serve_start_ok: false,
            fail_all: false,
        };
        let reply = portal_reply_bounded(&cfg("0.0.0.0", 8090), &host, PORTAL_BUDGET).await;
        assert_one_tappable(&reply, "http://192.168.1.50:8090/");
        host.assert_funnel_never_and_no_login();
    }

    #[tokio::test]
    async fn soft_fail_garbage_status_does_not_panic() {
        let host = FakeHost {
            calls: Mutex::new(Vec::new()),
            lan: None,
            status: Some("not-json".to_string()),
            serve_before: Some("not-json".to_string()),
            serve_start_ok: false,
            fail_all: false,
        };
        let reply = portal_reply_bounded(&cfg("127.0.0.1", 8090), &host, PORTAL_BUDGET).await;
        assert!(text_link_urls(&reply).is_empty());
        assert!(plain(&reply).contains("Only this machine can reach the portal"));
    }

    #[tokio::test]
    async fn disabled_portal_does_not_probe_or_link() {
        let host = FakeHost::offline(Some("192.168.1.20".parse().unwrap()));
        let reply = portal_reply_bounded(
            &PortalConfig {
                enabled: false,
                ..cfg("0.0.0.0", 8090)
            },
            &host,
            PORTAL_BUDGET,
        )
        .await;
        assert!(plain(&reply).to_ascii_lowercase().contains("disabled"));
        assert!(text_link_urls(&reply).is_empty());
        assert!(
            host.calls().is_empty(),
            "disabled portal must not start Serve: {:?}",
            host.calls()
        );
    }

    #[tokio::test]
    async fn wedged_probe_returns_within_the_hard_cap() {
        let started = Instant::now();
        let reply = portal_reply_bounded(
            &cfg("127.0.0.1", 8090),
            &HangProbe,
            Duration::from_millis(200),
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "probe hung for {:?}",
            started.elapsed()
        );
        assert!(text_link_urls(&reply).is_empty());
        assert!(plain(&reply).contains("Only this machine can reach the portal"));
    }

    #[test]
    fn probe_budget_constants_stay_bounded() {
        let budget = std::hint::black_box(PORTAL_BUDGET);
        let probe_timeout = std::hint::black_box(PROBE_TIMEOUT);
        assert!(budget <= Duration::from_secs(8));
        assert!(probe_timeout <= Duration::from_secs(2));
        assert!(probe_timeout < budget);
    }

    #[tokio::test]
    async fn cli_probe_times_out_instead_of_hanging() {
        let started = Instant::now();
        let out = probe("sleep", &["30"]).await;
        assert!(out.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "sleep probe ran for {:?}",
            started.elapsed()
        );
        assert!(started.elapsed() >= PROBE_TIMEOUT);
    }
}
