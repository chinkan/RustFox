//! Windows-side DNS-SD advertiser for Device WS.
//!
//! WSL portproxy does not carry multicast, so rustfox inside WSL does not
//! advertise. Run this binary on the Windows host. It publishes
//! `_rustfox-device._tcp` with the same TXT and port contract as the
//! in-process advertiser. No Bonjour install.

use std::process::ExitCode;

use rustfox::device_mdns::{self, AdvertiseOutcome};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return ExitCode::SUCCESS;
    }
    let opts = match device_mdns::parse_sidecar_args(&args) {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("rustfox-mdns: {err}");
            eprintln!("Try rustfox-mdns --help");
            return ExitCode::from(2);
        }
    };
    let query = device_mdns::sidecar_query(
        &opts,
        device_mdns::local_ipv4_interfaces(),
        device_mdns::detect_wsl(),
    );
    let plan = match device_mdns::decide(&query) {
        AdvertiseOutcome::Advertise(plan) => plan,
        AdvertiseOutcome::Skip(reason) => {
            eprintln!("rustfox-mdns: {reason}");
            return ExitCode::from(1);
        }
    };
    println!(
        "rustfox-mdns: advertising {} instance {} at {}:{} TXT path=/api/device/v1/ws proto=1 audio=/api/device/v1/audio",
        device_mdns::SERVICE_TYPE,
        plan.instance,
        plan.addr,
        plan.port
    );
    let advertiser = match device_mdns::start(&plan) {
        Ok(advertiser) => advertiser,
        Err(err) => {
            eprintln!("rustfox-mdns: {err:#}");
            return ExitCode::from(1);
        }
    };
    println!("rustfox-mdns: running until Ctrl+C");
    wait_for_stop();
    drop(advertiser);
    ExitCode::SUCCESS
}

fn print_help() {
    println!(
        "\
rustfox-mdns — advertise RustFox Device WS on the LAN

Publishes DNS-SD _rustfox-device._tcp (pure Rust, no Bonjour).
TXT is path=/api/device/v1/ws, proto=1, audio=/api/device/v1/audio.
No device token or portal token is written into the record.

With no --address, it picks a publishable LAN IPv4, preferring an
RFC1918 address and otherwise keeping interface order. Loopback,
WSL/Hyper-V virtual NICs, APIPA, and Tailscale 100.x are skipped.
Run this on Windows, not inside WSL. The port is the portproxy listen
port (default 8090).

Usage:
  rustfox-mdns [--address <ipv4>] [--port <port>] [--instance <name>]

Examples:
  rustfox-mdns
  rustfox-mdns --address 192.168.1.40 --port 8090 --instance rustfox
"
    );
}

fn wait_for_stop() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        #[cfg(unix)]
        {
            let mut sigterm =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(sigterm) => sigterm,
                    Err(err) => {
                        eprintln!("rustfox-mdns: SIGTERM handler failed: {err}");
                        let _ = tokio::signal::ctrl_c().await;
                        return;
                    }
                };
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if let Err(err) = result {
                        eprintln!("rustfox-mdns: Ctrl+C wait failed: {err}");
                    }
                }
                _ = sigterm.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            if let Err(err) = tokio::signal::ctrl_c().await {
                eprintln!("rustfox-mdns: Ctrl+C wait failed: {err}");
            }
        }
    });
}
