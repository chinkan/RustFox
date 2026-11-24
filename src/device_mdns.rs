//! LAN DNS-SD for Device WS (issue #436).
//!
//! Service `_rustfox-device._tcp`. TXT is `path`, `proto=1`, and `audio`.
//! The A record is one publishable IPv4 address. No device token, portal
//! token, or other secret is read or written into the instance name, SRV,
//! or TXT.
//!
//! Native macOS and Linux advertise in-process with the `mdns-sd` crate
//! (no Avahi and no Bonjour). WSL does not advertise; portproxy does not
//! carry multicast, so the Windows `rustfox-mdns` sidecar publishes the
//! Windows LAN address. This module does not change the portal bind.

use std::net::Ipv4Addr;

use anyhow::Context;

use crate::config::DeviceWsConfig;
use crate::device_ws::{AUDIO_PATH, WS_PATH};

/// DNS-SD type without the `.local` domain.
pub const SERVICE_TYPE: &str = "_rustfox-device._tcp";
/// Type plus domain, as mDNS registration expects it.
pub const SERVICE_DOMAIN: &str = "_rustfox-device._tcp.local.";
pub const DEFAULT_INSTANCE: &str = "rustfox";
pub const DEFAULT_PORT: u16 = 8090;
pub const TXT_PROTO: &str = "1";

/// Who is asking. The sidecar does not read `[device_ws].mdns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvertiseRole {
    Host,
    Sidecar,
}

/// One IPv4 address on a named interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NicAddr {
    pub name: String,
    pub addr: Ipv4Addr,
}

/// Inputs for [`decide`]. Tests pass these directly. The host and the
/// sidecar fill them from config or CLI plus the live interface list.
#[derive(Debug, Clone)]
pub struct AdvertiseQuery {
    pub device_ws_enabled: bool,
    pub mdns_enabled: bool,
    pub instance: String,
    pub bind: String,
    pub port: u16,
    pub in_wsl: bool,
    pub interfaces: Vec<NicAddr>,
    pub role: AdvertiseRole,
}

/// What to publish. TXT values are the fixed Device WS paths and `proto=1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisePlan {
    pub instance: String,
    pub service_type: String,
    pub host_name: String,
    pub addr: Ipv4Addr,
    pub nic_name: Option<String>,
    pub port: u16,
    pub txt: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvertiseOutcome {
    Advertise(AdvertisePlan),
    Skip(String),
}

/// CLI overrides for `rustfox-mdns`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarOpts {
    pub address: Option<Ipv4Addr>,
    pub port: u16,
    pub instance: String,
}

/// Running advertiser. Drop unregisters the service.
#[must_use = "dropping the advertiser unregisters the service"]
pub struct Advertiser {
    daemon: mdns_sd::ServiceDaemon,
    fullname: String,
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(self.fullname.as_str());
        let _ = self.daemon.shutdown();
    }
}

/// Fixed TXT. `audio` is included so the board does not have to rewrite it.
/// There is no token field.
pub fn txt_pairs() -> Vec<(String, String)> {
    vec![
        ("path".to_string(), WS_PATH.to_string()),
        ("proto".to_string(), TXT_PROTO.to_string()),
        ("audio".to_string(), AUDIO_PATH.to_string()),
    ]
}

/// Why this IPv4 address must not be the A record. `None` means the address
/// itself is allowed; the interface name is checked separately.
pub fn ipv4_skip_reason(addr: Ipv4Addr) -> Option<&'static str> {
    if addr.is_unspecified() {
        return Some("unspecified address");
    }
    if addr.is_loopback() {
        return Some("loopback");
    }
    if addr.is_link_local() {
        return Some("APIPA link-local");
    }
    if addr.is_multicast() || addr.is_broadcast() {
        return Some("multicast or broadcast");
    }
    // Tailscale CGNAT addresses are 100.64/10. The contract names them
    // "100.x", so the whole 100.0.0.0/8 is out of this slice.
    if addr.octets()[0] == 100 {
        return Some("Tailscale 100.x");
    }
    None
}

/// Why this interface is not a LAN the board can use.
pub fn nic_skip_reason(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    if n == "lo" || n == "lo0" || n.contains("loopback") {
        return Some("loopback interface");
    }
    if n.contains("wsl") || n.contains("hyper-v") || n.contains("hyperv") || n.contains("vethernet")
    {
        return Some("WSL/Hyper-V virtual NIC");
    }
    if n == "docker0"
        || n.starts_with("docker")
        || n.starts_with("br-")
        || n.starts_with("veth")
        || n.starts_with("virbr")
    {
        return Some("container or bridge NIC");
    }
    if n.contains("tailscale") {
        return Some("Tailscale interface");
    }
    None
}

/// `true` when the process is the Linux guest of WSL.
pub fn is_wsl(proc_version: Option<&str>, env_wsl_distro: bool, env_wsl_interop: bool) -> bool {
    if env_wsl_distro || env_wsl_interop {
        return true;
    }
    let Some(text) = proc_version else {
        return false;
    };
    let lower = text.to_ascii_lowercase();
    lower.contains("microsoft") || lower.contains("wsl")
}

pub fn detect_wsl() -> bool {
    let proc_version = std::fs::read_to_string("/proc/version").ok();
    let distro = std::env::var_os("WSL_DISTRO_NAME").is_some();
    let interop = std::env::var_os("WSL_INTEROP").is_some();
    is_wsl(proc_version.as_deref(), distro, interop)
}

/// Up IPv4 interfaces. IPv6 is ignored. A listing failure is an empty list.
pub fn local_ipv4_interfaces() -> Vec<NicAddr> {
    let list = match if_addrs::get_if_addrs() {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!("Device mDNS: could not list network interfaces: {e}");
            return Vec::new();
        }
    };
    list.into_iter()
        .filter(|iface| iface.oper_status == if_addrs::IfOperStatus::Up)
        .filter_map(|iface| match iface.addr {
            if_addrs::IfAddr::V4(v4) => Some(NicAddr {
                name: iface.name,
                addr: v4.ip,
            }),
            if_addrs::IfAddr::V6(_) => None,
        })
        .collect()
}

pub fn decide(query: &AdvertiseQuery) -> AdvertiseOutcome {
    if !query.device_ws_enabled {
        return AdvertiseOutcome::Skip("Device WS is disabled; not advertising.".to_string());
    }
    if query.in_wsl {
        return AdvertiseOutcome::Skip(wsl_skip_reason(query.role));
    }
    if query.role == AdvertiseRole::Host && !query.mdns_enabled {
        return AdvertiseOutcome::Skip("[device_ws].mdns is false; not advertising.".to_string());
    }
    let instance = match resolve_instance(&query.instance) {
        Ok(name) => name,
        Err(reason) => return AdvertiseOutcome::Skip(reason),
    };
    match classify_bind(&query.bind) {
        BindClass::Loopback => AdvertiseOutcome::Skip(format!(
            "portal bind is loopback only ({}); no LAN-reachable listen; not advertising.",
            query.bind.trim()
        )),
        BindClass::NoIpv4 => AdvertiseOutcome::Skip(format!(
            "portal bind {:?} is not a LAN-reachable IPv4 listen; bind 0.0.0.0 or a LAN IPv4; not advertising.",
            query.bind.trim()
        )),
        BindClass::Specific(ip) => decide_specific(query, instance, ip),
        BindClass::AllIpv4 => decide_auto(query, instance),
    }
}

fn wsl_skip_reason(role: AdvertiseRole) -> String {
    match role {
        AdvertiseRole::Host => concat!(
            "WSL detected; in-process mDNS is not advertised. ",
            "Portproxy does not carry multicast. ",
            "The Windows sidecar rustfox-mdns is required."
        )
        .to_string(),
        AdvertiseRole::Sidecar => concat!(
            "WSL detected; rustfox-mdns must run on the Windows host. ",
            "Portproxy does not carry multicast, so this process will not advertise."
        )
        .to_string(),
    }
}

fn decide_specific(query: &AdvertiseQuery, instance: String, ip: Ipv4Addr) -> AdvertiseOutcome {
    if let Some(reason) = ipv4_skip_reason(ip) {
        return AdvertiseOutcome::Skip(format!(
            "bind {ip} is not a publishable LAN IPv4 ({reason}); not advertising."
        ));
    }
    if let Some(nic) = query
        .interfaces
        .iter()
        .find(|nic| nic.addr == ip && nic_skip_reason(&nic.name).is_some())
    {
        let why = nic_skip_reason(&nic.name).unwrap_or("virtual NIC");
        return AdvertiseOutcome::Skip(format!(
            "bind {ip} is on {} ({why}); not advertising.",
            nic.name
        ));
    }
    let nic_name = query
        .interfaces
        .iter()
        .find(|nic| nic.addr == ip)
        .map(|nic| nic.name.clone());
    AdvertiseOutcome::Advertise(plan_for(instance, ip, nic_name, query.port))
}

fn decide_auto(query: &AdvertiseQuery, instance: String) -> AdvertiseOutcome {
    let mut rfc1918 = Vec::new();
    let mut other = Vec::new();
    for nic in &query.interfaces {
        if nic_skip_reason(&nic.name).is_some() || ipv4_skip_reason(nic.addr).is_some() {
            continue;
        }
        if is_rfc1918(nic.addr) {
            rfc1918.push(nic);
        } else {
            other.push(nic);
        }
    }
    let chosen = rfc1918.first().copied().or_else(|| other.first().copied());
    match chosen {
        Some(nic) => AdvertiseOutcome::Advertise(plan_for(
            instance,
            nic.addr,
            Some(nic.name.clone()),
            query.port,
        )),
        None => AdvertiseOutcome::Skip(
            "no publishable LAN IPv4; not advertising. Skipped loopback, WSL/Hyper-V virtual NICs, APIPA, and Tailscale 100.x."
                .to_string(),
        ),
    }
}

fn plan_for(
    instance: String,
    addr: Ipv4Addr,
    nic_name: Option<String>,
    port: u16,
) -> AdvertisePlan {
    AdvertisePlan {
        instance,
        service_type: SERVICE_DOMAIN.to_string(),
        host_name: host_name_for(addr),
        addr,
        nic_name,
        port,
        txt: txt_pairs(),
    }
}

fn host_name_for(addr: Ipv4Addr) -> String {
    let o = addr.octets();
    format!("rustfox-{}-{}-{}-{}.local.", o[0], o[1], o[2], o[3])
}

fn resolve_instance(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    let name = if name.is_empty() {
        DEFAULT_INSTANCE
    } else {
        name
    };
    if name.len() > 63 {
        return Err(format!(
            "instance name is {} bytes; DNS-SD instance names are at most 63 bytes; not advertising.",
            name.len()
        ));
    }
    if name.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err("instance name contains a control character; not advertising.".to_string());
    }
    Ok(name.to_string())
}

fn is_rfc1918(addr: Ipv4Addr) -> bool {
    let o = addr.octets();
    o[0] == 10 || (o[0] == 172 && (16..=31).contains(&o[1])) || (o[0] == 192 && o[1] == 168)
}

enum BindClass {
    Loopback,
    AllIpv4,
    Specific(Ipv4Addr),
    NoIpv4,
}

fn classify_bind(bind: &str) -> BindClass {
    let b = bind.trim();
    if b.eq_ignore_ascii_case("localhost") {
        return BindClass::Loopback;
    }
    let bare = b.trim_start_matches('[').trim_end_matches(']');
    if let Ok(v6) = bare.parse::<std::net::Ipv6Addr>() {
        return if v6.is_loopback() {
            BindClass::Loopback
        } else {
            BindClass::NoIpv4
        };
    }
    if let Ok(v4) = b.parse::<Ipv4Addr>() {
        if v4.is_loopback() {
            return BindClass::Loopback;
        }
        if v4.is_unspecified() {
            return BindClass::AllIpv4;
        }
        return BindClass::Specific(v4);
    }
    BindClass::NoIpv4
}

/// Build the DNS-SD record. Address auto-update stays off, so the daemon
/// cannot replace the chosen IPv4 with loopback, a virtual NIC, or IPv6.
pub fn service_info(plan: &AdvertisePlan) -> anyhow::Result<mdns_sd::ServiceInfo> {
    let info = mdns_sd::ServiceInfo::new(
        &plan.service_type,
        &plan.instance,
        &plan.host_name,
        std::net::IpAddr::V4(plan.addr),
        plan.port,
        &plan.txt[..],
    )
    .map_err(|e| anyhow::anyhow!("mDNS service record: {e}"))?;
    if info.is_addr_auto() {
        anyhow::bail!("mDNS service record must not auto-update addresses");
    }
    let v6 = info.get_addresses().iter().any(std::net::IpAddr::is_ipv6);
    if v6 {
        anyhow::bail!("mDNS service record must not include an IPv6 address");
    }
    Ok(info)
}

/// Bind UDP 5353 and publish `plan`. Failure is returned; callers log it
/// and leave the HTTP listener alone.
pub fn start(plan: &AdvertisePlan) -> anyhow::Result<Advertiser> {
    let info = service_info(plan)?;
    let fullname = info.get_fullname().to_string();
    let daemon = mdns_sd::ServiceDaemon::new().context("mDNS daemon")?;
    // IPv6/AAAA is out of this slice. Ignore a host that has no IPv6 socket.
    let _ = daemon.disable_interface(mdns_sd::IfKind::IPv6);
    daemon.register(info).context("mDNS register")?;
    Ok(Advertiser { daemon, fullname })
}

/// Decide and, when the plan says so, advertise. Skip reasons are info logs.
/// A daemon error is a warning. The portal bind is not modified.
pub fn spawn_host(device_ws: &DeviceWsConfig, bind: &str, port: u16) -> Option<Advertiser> {
    let query = AdvertiseQuery {
        device_ws_enabled: device_ws.enabled,
        mdns_enabled: device_ws.mdns,
        instance: device_ws.mdns_instance.clone(),
        bind: bind.to_string(),
        port,
        in_wsl: detect_wsl(),
        interfaces: local_ipv4_interfaces(),
        role: AdvertiseRole::Host,
    };
    match decide(&query) {
        AdvertiseOutcome::Skip(reason) => {
            tracing::info!("Device mDNS: {reason}");
            None
        }
        AdvertiseOutcome::Advertise(plan) => match start(&plan) {
            Ok(advertiser) => {
                tracing::info!(
                    instance = %plan.instance,
                    addr = %plan.addr,
                    port = plan.port,
                    "Device mDNS: advertising {SERVICE_TYPE}"
                );
                Some(advertiser)
            }
            Err(e) => {
                tracing::warn!("Device mDNS: failed to advertise: {e:#}");
                None
            }
        },
    }
}

pub fn parse_sidecar_args<S: AsRef<str>>(args: &[S]) -> Result<SidecarOpts, String> {
    let mut address = None;
    let mut port = DEFAULT_PORT;
    let mut instance = DEFAULT_INSTANCE.to_string();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_ref();
        if let Some((flag, value)) = split_equals(arg) {
            apply_flag(flag, value, &mut address, &mut port, &mut instance)?;
            i += 1;
            continue;
        }
        match arg {
            "--address" | "--addr" | "--port" | "--instance" => {
                let flag = arg;
                i += 1;
                let Some(value) = args.get(i) else {
                    return Err(format!("missing value for {flag}"));
                };
                apply_flag(flag, value.as_ref(), &mut address, &mut port, &mut instance)?;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown argument: {other}"));
            }
            other => return Err(format!("unexpected argument: {other}")),
        }
        i += 1;
    }
    Ok(SidecarOpts {
        address,
        port,
        instance,
    })
}

fn split_equals(arg: &str) -> Option<(&str, &str)> {
    const FLAGS: [&str; 4] = ["--address", "--addr", "--port", "--instance"];
    for flag in FLAGS {
        let prefix = format!("{flag}=");
        if let Some(value) = arg.strip_prefix(&prefix) {
            return Some((flag, value));
        }
    }
    None
}

fn apply_flag(
    flag: &str,
    value: &str,
    address: &mut Option<Ipv4Addr>,
    port: &mut u16,
    instance: &mut String,
) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("missing value for {flag}"));
    }
    match flag {
        "--address" | "--addr" => {
            let ip = value
                .parse::<Ipv4Addr>()
                .map_err(|_| format!("invalid IPv4 address: {value}"))?;
            *address = Some(ip);
        }
        "--port" => {
            let parsed = value
                .parse::<u16>()
                .map_err(|_| format!("invalid port: {value}"))?;
            if parsed == 0 {
                return Err("port must be 1-65535".to_string());
            }
            *port = parsed;
        }
        "--instance" => *instance = value.to_string(),
        other => return Err(format!("unknown argument: {other}")),
    }
    Ok(())
}

pub fn sidecar_query(opts: &SidecarOpts, interfaces: Vec<NicAddr>, in_wsl: bool) -> AdvertiseQuery {
    AdvertiseQuery {
        device_ws_enabled: true,
        mdns_enabled: true,
        instance: opts.instance.clone(),
        bind: opts
            .address
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "0.0.0.0".to_string()),
        port: opts.port,
        in_wsl,
        interfaces,
        role: AdvertiseRole::Sidecar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::device_ws;

    fn nic(name: &str, ip: &str) -> NicAddr {
        NicAddr {
            name: name.to_string(),
            addr: ip.parse().unwrap(),
        }
    }

    fn host_query(bind: &str, interfaces: Vec<NicAddr>) -> AdvertiseQuery {
        AdvertiseQuery {
            device_ws_enabled: true,
            mdns_enabled: true,
            instance: String::new(),
            bind: bind.to_string(),
            port: DEFAULT_PORT,
            in_wsl: false,
            interfaces,
            role: AdvertiseRole::Host,
        }
    }

    fn lan() -> Vec<NicAddr> {
        vec![nic("eth0", "192.168.1.20")]
    }

    fn assert_skip(outcome: &AdvertiseOutcome, needle: &str) {
        match outcome {
            AdvertiseOutcome::Skip(reason) => {
                let hay = reason.to_ascii_lowercase();
                let needle = needle.to_ascii_lowercase();
                assert!(
                    hay.contains(&needle),
                    "reason {reason:?} does not contain {needle:?}"
                );
            }
            AdvertiseOutcome::Advertise(plan) => {
                panic!(
                    "expected skip containing {needle:?}, advertised {}",
                    plan.addr
                );
            }
        }
    }

    fn advertised(outcome: AdvertiseOutcome) -> AdvertisePlan {
        match outcome {
            AdvertiseOutcome::Advertise(plan) => plan,
            AdvertiseOutcome::Skip(reason) => panic!("expected advertise, skipped: {reason}"),
        }
    }

    #[test]
    fn rejects_loopback_apipa_tailscale_100_and_non_unicast() {
        assert_eq!(
            ipv4_skip_reason("127.0.0.1".parse().unwrap()),
            Some("loopback")
        );
        assert_eq!(
            ipv4_skip_reason("127.0.0.2".parse().unwrap()),
            Some("loopback")
        );
        assert_eq!(
            ipv4_skip_reason("0.0.0.0".parse().unwrap()),
            Some("unspecified address")
        );
        assert_eq!(
            ipv4_skip_reason("169.254.1.1".parse().unwrap()),
            Some("APIPA link-local")
        );
        assert_eq!(
            ipv4_skip_reason("169.254.255.255".parse().unwrap()),
            Some("APIPA link-local")
        );
        for ip in ["100.64.0.1", "100.127.255.254", "100.88.1.1", "100.0.0.1"] {
            assert_eq!(
                ipv4_skip_reason(ip.parse().unwrap()),
                Some("Tailscale 100.x"),
                "{ip}"
            );
        }
        assert_eq!(ipv4_skip_reason("101.1.1.1".parse().unwrap()), None);
        assert_eq!(ipv4_skip_reason("99.1.1.1".parse().unwrap()), None);
        assert_eq!(
            ipv4_skip_reason("224.0.0.251".parse().unwrap()),
            Some("multicast or broadcast")
        );
        assert_eq!(
            ipv4_skip_reason("255.255.255.255".parse().unwrap()),
            Some("multicast or broadcast")
        );
        assert_eq!(ipv4_skip_reason("192.168.1.20".parse().unwrap()), None);
        assert_eq!(ipv4_skip_reason("10.1.2.3".parse().unwrap()), None);
        assert_eq!(ipv4_skip_reason("172.16.5.8".parse().unwrap()), None);
    }

    #[test]
    fn skips_wsl_hyperv_virtual_nics_and_keeps_a_172_16_lan() {
        assert_eq!(
            nic_skip_reason("vEthernet (WSL)"),
            Some("WSL/Hyper-V virtual NIC")
        );
        assert_eq!(
            nic_skip_reason("vEthernet (Default Switch)"),
            Some("WSL/Hyper-V virtual NIC")
        );
        assert_eq!(
            nic_skip_reason("Hyper-V Virtual Ethernet Adapter"),
            Some("WSL/Hyper-V virtual NIC")
        );
        assert_eq!(nic_skip_reason("loopback"), Some("loopback interface"));
        assert_eq!(nic_skip_reason("lo"), Some("loopback interface"));
        assert_eq!(nic_skip_reason("docker0"), Some("container or bridge NIC"));
        assert_eq!(
            nic_skip_reason("br-abcdef012345"),
            Some("container or bridge NIC")
        );
        assert_eq!(nic_skip_reason("veth0ab1"), Some("container or bridge NIC"));
        assert_eq!(nic_skip_reason("virbr0"), Some("container or bridge NIC"));
        assert_eq!(nic_skip_reason("tailscale0"), Some("Tailscale interface"));
        assert_eq!(nic_skip_reason("Ethernet"), None);
        assert_eq!(nic_skip_reason("eth0"), None);
        assert_eq!(nic_skip_reason("en0"), None);
        assert_eq!(nic_skip_reason("Wi-Fi"), None);
    }

    #[test]
    fn disabled_device_ws_does_not_advertise() {
        let mut q = host_query("0.0.0.0", lan());
        q.device_ws_enabled = false;
        assert_skip(&decide(&q), "disabled");
    }

    #[test]
    fn mdns_false_does_not_advertise_a_good_lan() {
        let mut q = host_query("0.0.0.0", lan());
        q.mdns_enabled = false;
        let outcome = decide(&q);
        assert_skip(&outcome, "mdns");
        let AdvertiseOutcome::Skip(reason) = outcome else {
            unreachable!();
        };
        assert!(
            !reason.to_ascii_lowercase().contains("sidecar"),
            "native mdns=false must not talk about the sidecar: {reason}"
        );
    }

    #[test]
    fn wsl_does_not_advertise_even_with_a_lan_address() {
        let mut q = host_query("0.0.0.0", lan());
        q.in_wsl = true;
        let outcome = decide(&q);
        assert_skip(&outcome, "wsl");
        assert_skip(&outcome, "sidecar");
        assert_skip(&outcome, "portproxy");
    }

    #[test]
    fn wsl_still_names_the_sidecar_when_mdns_is_false() {
        let mut q = host_query("0.0.0.0", lan());
        q.in_wsl = true;
        q.mdns_enabled = false;
        assert_skip(&decide(&q), "sidecar");
    }

    #[test]
    fn sidecar_inside_wsl_does_not_advertise() {
        let opts = SidecarOpts {
            address: None,
            port: DEFAULT_PORT,
            instance: DEFAULT_INSTANCE.to_string(),
        };
        let outcome = decide(&sidecar_query(&opts, lan(), true));
        assert_skip(&outcome, "windows host");
        assert_skip(&outcome, "portproxy");
    }

    #[test]
    fn loopback_bind_does_not_advertise_another_nic() {
        for bind in ["127.0.0.1", "127.0.0.2", "localhost", "::1", "[::1]"] {
            let outcome = decide(&host_query(bind, lan()));
            assert_skip(&outcome, "loopback");
        }
    }

    #[test]
    fn ipv6_unspecified_bind_is_not_a_lan_listen() {
        assert_skip(&decide(&host_query("::", lan())), "not a lan-reachable");
        assert_skip(&decide(&host_query("[::]", lan())), "not a lan-reachable");
    }

    #[test]
    fn no_publishable_ipv4_does_not_advertise() {
        let ifaces = vec![
            nic("lo", "127.0.0.1"),
            nic("eth0", "169.254.3.4"),
            nic("tailscale0", "100.64.1.2"),
            nic("vEthernet (WSL)", "172.22.32.1"),
            nic("docker0", "172.17.0.1"),
        ];
        let outcome = decide(&host_query("0.0.0.0", ifaces));
        assert_skip(&outcome, "no publishable");
        assert_skip(&outcome, "100.x");
    }

    #[test]
    fn auto_pick_prefers_rfc1918_and_skips_virtual_nics() {
        let ifaces = vec![
            nic("lo", "127.0.0.1"),
            nic("vEthernet (WSL)", "172.22.32.1"),
            nic("tailscale0", "100.88.1.1"),
            nic("docker0", "172.17.0.1"),
            nic("eth0", "203.0.113.10"),
            nic("wlan0", "192.168.1.9"),
        ];
        let plan = advertised(decide(&host_query("0.0.0.0", ifaces)));
        assert_eq!(plan.addr, "192.168.1.9".parse::<Ipv4Addr>().unwrap());
        assert_eq!(plan.nic_name.as_deref(), Some("wlan0"));
    }

    #[test]
    fn auto_pick_keeps_interface_order_inside_rfc1918() {
        let ifaces = vec![nic("eth0", "10.0.0.2"), nic("wlan0", "192.168.1.9")];
        let plan = advertised(decide(&host_query("0.0.0.0", ifaces)));
        assert_eq!(plan.addr, "10.0.0.2".parse::<Ipv4Addr>().unwrap());
        assert_eq!(plan.nic_name.as_deref(), Some("eth0"));
    }

    #[test]
    fn auto_pick_uses_a_public_address_when_nothing_else_is_publishable() {
        let plan = advertised(decide(&host_query(
            "0.0.0.0",
            vec![nic("eth0", "203.0.113.10")],
        )));
        assert_eq!(plan.addr, "203.0.113.10".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn specific_bind_advertises_that_address_only() {
        let ifaces = vec![nic("eth0", "10.1.2.3"), nic("wlan0", "192.168.1.9")];
        let plan = advertised(decide(&host_query("10.1.2.3", ifaces)));
        assert_eq!(plan.addr, "10.1.2.3".parse::<Ipv4Addr>().unwrap());
        assert_eq!(plan.nic_name.as_deref(), Some("eth0"));
        assert_eq!(plan.port, DEFAULT_PORT);
    }

    #[test]
    fn specific_tailscale_or_apipa_bind_does_not_advertise() {
        assert_skip(
            &decide(&host_query(
                "100.64.1.2",
                vec![nic("tailscale0", "100.64.1.2")],
            )),
            "100.x",
        );
        assert_skip(
            &decide(&host_query("169.254.1.5", vec![nic("eth0", "169.254.1.5")])),
            "apipa",
        );
    }

    #[test]
    fn specific_address_on_a_wsl_nic_does_not_advertise() {
        let ifaces = vec![
            nic("vEthernet (WSL)", "172.22.32.1"),
            nic("Ethernet", "192.168.1.40"),
        ];
        assert_skip(&decide(&host_query("172.22.32.1", ifaces)), "vethernet");
    }

    #[test]
    fn home_lan_172_16_on_ethernet_is_publishable() {
        let plan = advertised(decide(&host_query(
            "172.16.5.8",
            vec![nic("Ethernet", "172.16.5.8")],
        )));
        assert_eq!(plan.addr, "172.16.5.8".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn advertised_record_matches_the_txt_and_instance_contract() {
        let mut q = host_query("0.0.0.0", lan());
        q.instance = "kitchen".to_string();
        q.port = 9443;
        let plan = advertised(decide(&q));
        assert_eq!(plan.instance, "kitchen");
        assert_eq!(plan.port, 9443);
        assert_eq!(plan.service_type, SERVICE_DOMAIN);
        assert!(plan.service_type.starts_with(SERVICE_TYPE));
        assert_eq!(plan.host_name, "rustfox-192-168-1-20.local.");
        assert_eq!(
            plan.txt,
            vec![
                ("path".to_string(), WS_PATH.to_string()),
                ("proto".to_string(), "1".to_string()),
                ("audio".to_string(), AUDIO_PATH.to_string()),
            ]
        );
        assert_eq!(WS_PATH, "/api/device/v1/ws");
        assert_eq!(AUDIO_PATH, "/api/device/v1/audio");

        let info = service_info(&plan).expect("service info");
        assert!(!info.is_addr_auto());
        assert_eq!(info.get_port(), 9443);
        assert_eq!(info.get_hostname(), "rustfox-192-168-1-20.local.");
        assert_eq!(info.get_type(), SERVICE_DOMAIN);
        assert_eq!(info.get_property_val_str("path"), Some("/api/device/v1/ws"));
        assert_eq!(info.get_property_val_str("proto"), Some("1"));
        assert_eq!(
            info.get_property_val_str("audio"),
            Some("/api/device/v1/audio")
        );
        assert_eq!(info.get_properties().len(), 3);
        for key in [
            "token",
            "secret",
            "password",
            "api_key",
            "authorization",
            "bearer",
        ] {
            assert!(
                info.get_property(key).is_none(),
                "TXT must not contain {key}"
            );
        }
        let mut v4: Vec<Ipv4Addr> = info.get_addresses_v4().into_iter().copied().collect();
        v4.sort();
        assert_eq!(v4, vec!["192.168.1.20".parse::<Ipv4Addr>().unwrap()]);
        assert!(info.get_addresses().iter().all(|addr| addr.is_ipv4()));
        let rendered = format!("{plan:?}");
        assert!(!rendered.to_ascii_lowercase().contains("bearer"));
        assert!(!rendered.contains("token="));
    }

    #[test]
    fn empty_instance_defaults_to_rustfox_and_spaces_are_kept() {
        let plan = advertised(decide(&host_query("0.0.0.0", lan())));
        assert_eq!(plan.instance, DEFAULT_INSTANCE);

        let mut q = host_query("0.0.0.0", lan());
        q.instance = "  living room  ".to_string();
        let plan = advertised(decide(&q));
        assert_eq!(plan.instance, "living room");
        service_info(&plan).expect("spaces are a legal instance name");
    }

    #[test]
    fn oversized_or_control_instance_names_are_not_advertised() {
        let mut q = host_query("0.0.0.0", lan());
        q.instance = "a".repeat(64);
        assert_skip(&decide(&q), "63");

        q.instance = "rust\nfox".to_string();
        assert_skip(&decide(&q), "control");
    }

    #[test]
    fn default_portal_bind_does_not_advertise() {
        let cfg: Config = toml::from_str(
            r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [device_ws]
            enabled = true
            "#,
        )
        .unwrap();
        assert!(cfg.device_ws.mdns);
        assert_eq!(cfg.device_ws.mdns_instance, "rustfox");
        assert_eq!(cfg.portal.bind, "127.0.0.1");
        assert_eq!(cfg.portal.port, 8090);
        let outcome = decide(&AdvertiseQuery {
            device_ws_enabled: cfg.device_ws.enabled,
            mdns_enabled: cfg.device_ws.mdns,
            instance: cfg.device_ws.mdns_instance.clone(),
            bind: cfg.portal.bind.clone(),
            port: cfg.portal.port,
            in_wsl: false,
            interfaces: lan(),
            role: AdvertiseRole::Host,
        });
        assert_skip(&outcome, "loopback");
    }

    #[test]
    fn config_keys_default_on_and_can_be_turned_off() {
        let absent: Config = toml::from_str(
            r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            "#,
        )
        .unwrap();
        assert!(!absent.device_ws.enabled);
        assert!(absent.device_ws.mdns);
        assert_eq!(absent.device_ws.mdns_instance, DEFAULT_INSTANCE);

        let set: Config = toml::from_str(
            r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [device_ws]
            enabled = true
            mdns = false
            mdns_instance = "kitchen"
            "#,
        )
        .unwrap();
        assert!(set.device_ws.enabled);
        assert!(!set.device_ws.mdns);
        assert_eq!(set.device_ws.mdns_instance, "kitchen");
    }

    #[test]
    fn sidecar_auto_picks_the_windows_lan_and_honors_overrides() {
        let ifaces = vec![
            nic("Loopback Pseudo-Interface 1", "127.0.0.1"),
            nic("vEthernet (WSL)", "172.22.32.1"),
            nic("Tailscale", "100.64.8.8"),
            nic("Ethernet", "192.168.1.40"),
        ];
        let defaults = parse_sidecar_args::<&str>(&[]).unwrap();
        assert_eq!(defaults.address, None);
        assert_eq!(defaults.port, 8090);
        assert_eq!(defaults.instance, "rustfox");
        let plan = advertised(decide(&sidecar_query(&defaults, ifaces.clone(), false)));
        assert_eq!(plan.addr, "192.168.1.40".parse::<Ipv4Addr>().unwrap());
        assert_eq!(plan.port, 8090);
        assert_eq!(plan.instance, "rustfox");
        assert_eq!(plan.txt, txt_pairs());

        let opts =
            parse_sidecar_args(&["--address=10.2.3.4", "--port", "9000", "--instance", "den"])
                .unwrap();
        let plan = advertised(decide(&sidecar_query(
            &opts,
            vec![nic("Ethernet", "10.2.3.4"), nic("Wi-Fi", "192.168.1.40")],
            false,
        )));
        assert_eq!(plan.addr, "10.2.3.4".parse::<Ipv4Addr>().unwrap());
        assert_eq!(plan.port, 9000);
        assert_eq!(plan.instance, "den");

        let forced = parse_sidecar_args(&["--addr", "192.168.1.40"]).unwrap();
        let plan = advertised(decide(&sidecar_query(&forced, ifaces, false)));
        assert_eq!(plan.addr, "192.168.1.40".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn sidecar_explicit_address_is_used_when_it_is_not_in_the_list() {
        let opts = parse_sidecar_args(&[
            "--address",
            "192.168.50.8",
            "--port",
            "8090",
            "--instance",
            "rustfox",
        ])
        .unwrap();
        let plan = advertised(decide(&sidecar_query(
            &opts,
            vec![nic("Ethernet", "10.0.0.1")],
            false,
        )));
        assert_eq!(plan.addr, "192.168.50.8".parse::<Ipv4Addr>().unwrap());
        assert_eq!(plan.nic_name, None);
        assert_eq!(plan.port, 8090);
        assert_eq!(plan.instance, "rustfox");
    }

    #[test]
    fn sidecar_override_rejects_loopback_tailscale_apipa_and_wsl_nic() {
        let bad = [
            ("127.0.0.1", "loopback"),
            ("100.100.1.1", "100.x"),
            ("169.254.9.9", "apipa"),
        ];
        for (ip, needle) in bad {
            let opts = parse_sidecar_args(&["--address", ip]).unwrap();
            assert_skip(&decide(&sidecar_query(&opts, lan(), false)), needle);
        }
        let opts = parse_sidecar_args(&["--address", "172.22.32.1"]).unwrap();
        let outcome = decide(&sidecar_query(
            &opts,
            vec![nic("vEthernet (WSL)", "172.22.32.1")],
            false,
        ));
        assert_skip(&outcome, "wsl/hyper-v");
    }

    #[test]
    fn sidecar_parser_errors() {
        assert!(parse_sidecar_args(&["--port"]).is_err());
        assert!(parse_sidecar_args(&["--address"]).is_err());
        assert!(parse_sidecar_args(&["--address", "::1"]).is_err());
        assert!(parse_sidecar_args(&["--port", "0"]).is_err());
        assert!(parse_sidecar_args(&["--port=nope"]).is_err());
        assert!(parse_sidecar_args(&["--nope"]).is_err());
        assert!(parse_sidecar_args(&["extra"]).is_err());
    }

    #[test]
    fn wsl_detection_reads_proc_version_and_env() {
        assert!(is_wsl(
            Some("Linux version 5.15.0-microsoft-standard-WSL2"),
            false,
            false
        ));
        assert!(is_wsl(Some("Linux version 6.8.0-generic"), true, false));
        assert!(is_wsl(None, false, true));
        assert!(!is_wsl(Some("Linux version 6.8.0-generic"), false, false));
        assert!(!is_wsl(None, false, false));
    }

    #[test]
    fn txt_uses_the_collection_audio_path_not_the_id_route() {
        let pairs = txt_pairs();
        assert_eq!(pairs[0].1, device_ws::WS_PATH);
        assert_eq!(pairs[2].1, device_ws::AUDIO_PATH);
        assert!(!pairs[2].1.contains('{'));
        assert_ne!(pairs[2].1, device_ws::AUDIO_ID_PATH);
    }
}
