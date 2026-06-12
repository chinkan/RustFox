//! Thin wrapper — delegates to `rustfox::setup::wizard`.
//!
//! Kept for backwards compat with `./setup.sh` and `cargo run --bin setup`.
//! New users should use `rustfox --setup` instead.

use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = std::env::args().any(|a| a == "--cli");
    let config_dir = match std::env::var("RUSTFOX_CONFIG_PATH") {
        Ok(p) => PathBuf::from(p)
            .parent()
            .map(|d| d.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".")),
        Err(_) => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    rustfox::setup::wizard::run(&config_dir, cli).await
}
