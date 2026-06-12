# Multi-Platform Service Setup — Design

**Date:** 2026-06-12
**Status:** Draft

## Problem

RustFox runs as a foreground process. Users must manually keep it running (tmux, screen, nohup). There's no standard install path, build artifacts for other platforms, or `--setup` command in the main binary. Currently setup is a separate binary (`src/bin/setup.rs`) with no service integration.

## Goals

1. **`rustfox --setup` subcommand** — integrated into the main binary, launches web wizard (browser on `:8719`) or TUI (`--cli`)
2. **`rustfox --service` subcommand** — manage background service: install, remove, status, start, stop
3. **Cross-platform background service** — systemd (Linux), launchd (macOS), Windows Service
4. **Build scripts** — `.deb`, `.rpm`, `.tar.gz`, `.zip` packages
5. **GitHub Actions release workflow** — builds all targets, attaches to GitHub Releases
6. **Install scripts** — wraps binary placement + service setup per platform

## Non-Goals

- Docker images (separate work)
- Changing the existing TOML config format
- Removing manual `config.toml` editing
- Building for non-host targets in CI (native cross-compilation is a follow-up)

## Architecture

### Module structure

```
src/
├── setup/
│   ├── mod.rs              # SetupMode enum, CLI dispatch
│   ├── wizard.rs           # Web + CLI wizard (extracted from bin/setup.rs)
│   └── service.rs          # Service install/remove/status/start/stop
├── bin/
│   └── setup.rs            # Thin wrapper → rustfox::setup::run()
└── main.rs                 # Parse --setup / --service before normal bot start
```

### CLI interface

```
rustfox                          # Normal bot start (existing)
rustfox --setup                  # Opens web wizard on http://localhost:8719
rustfox --setup --cli            # Terminal wizard
rustfox --service install        # Install + enable + start background service
rustfox --service remove         # Stop + disable + remove service
rustfox --service status         # Show service status
rustfox --service start          # Start service (no install)
rustfox --service stop           # Stop service (no remove)
```

### Arg parsing migration

Current `main.rs` treats the first positional arg as a config file path:
```rust
let config_path = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| ...);
```

This conflicts with `--setup` and `--service` subcommands. The fix:

1. Add a `--config <PATH>` flag to `main.rs` to explicitly set the config path
2. The first positional arg is deprecated but still supported for backwards compat
3. If no `--config` flag and no positional arg, fall back to existing discovery logic (CWD → `~/.rustfox/config.toml`)

Dispatch logic:
```
parse CLI args →
  if --setup        → run setup::run() which dispatches to web or cli wizard
  if --service      → run setup::service::handle_action()
  if --config <P>   → use P as config_path, then normal bot start
  if positional arg → use it as config_path (deprecated), normal bot start
  else              → auto-discover config_path, normal bot start
```

After the wizard saves config, it asks "Install as background service? [Y/n]" and if yes, delegates to `service::install()`.

### Service management per platform

| Platform | Mechanism | Template location | Config | Run as |
|----------|-----------|-------------------|--------|--------|
| Linux | systemd --user | `~/.config/systemd/user/rustfox.service` | `~/.rustfox/config.toml` | Current user |
| macOS | launchd agent | `~/Library/LaunchAgents/com.rustfox.bot.plist` | `~/.rustfox/config.toml` | Current user |
| Windows | sc.exe or Win32 API | SCM database | `%USERPROFILE%\.rustfox\config.toml` | Current user |

Service templates are NOT embedded with hardcoded paths. At install time, `service::install()` calls `std::env::current_exe()`, renders the binary path and config path into the template, and writes the rendered file. This guarantees the service always runs the correct binary regardless of where it was installed (cargo, .deb, tarball, etc.).

#### systemd user service (Linux)

Template (`scripts/services/rustfox.service.template`):
```ini
[Unit]
Description=RustFox Telegram AI Assistant
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={{RUSTFOX_BIN}} --config {{RUSTFOX_CONFIG}}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

At install time, `{{RUSTFOX_BIN}}` is replaced with the actual binary path and `{{RUSTFOX_CONFIG}}` with `~/.rustfox/config.toml`. The rendered file is written to `~/.config/systemd/user/rustfox.service`, then `systemctl --user daemon-reload && systemctl --user enable --now rustfox.service` is executed.

#### launchd agent (macOS)

Template (`scripts/services/com.rustfox.bot.plist.template`):
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.rustfox.bot</string>
    <key>ProgramArguments</key>
    <array>
        <string>{{RUSTFOX_BIN}}</string>
        <string>--config</string>
        <string>{{RUSTFOX_CONFIG}}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{{RUSTFOX_HOME}}/Library/Logs/rustfox.log</string>
    <key>StandardErrorPath</key>
    <string>{{RUSTFOX_HOME}}/Library/Logs/rustfox.log</string>
</dict>
</plist>
```

NOTE: launchd does NOT support `%h` or `~` expansion in `StandardOutPath`. The template uses `{{RUSTFOX_HOME}}` which is replaced with the absolute home directory path at render time.

#### Windows Service

Install script (`scripts/install-service.bat.template`):
```batch
@echo off
sc create RustFox binPath="{{RUSTFOX_BIN}} --config {{RUSTFOX_CONFIG}}" start=auto
sc description RustFox "RustFox Telegram AI Assistant"
sc failure RustFox reset=86400 actions=restart/5000/restart/10000
sc start RustFox
```

## Rust implementation

### src/setup/mod.rs

```rust
pub mod service;
pub mod wizard;

#[derive(clap::Subcommand)]
pub enum SetupCommand {
    /// Open the setup wizard (default: web browser)
    #[clap(name = "setup")]
    Setup {
        /// Use terminal-based wizard instead of browser
        #[clap(long)]
        cli: bool,
    },
    /// Manage the background service
    #[clap(name = "service")]
    Service {
        #[clap(subcommand)]
        action: ServiceAction,
    },
}

#[derive(clap::Subcommand)]
pub enum ServiceAction {
    Install,
    Remove,
    Status,
    Start,
    Stop,
}
```

NOTE: `clap` is not yet a dependency. Two options: (A) add clap with derive, (B) manual arg parsing. Option A is recommended — clap is standard in Rust CLI apps.

### src/setup/service.rs

```rust
/// Install the bot as a background service for the current platform.
pub fn install() -> Result<()> {
    let exe_path = std::env::current_exe()?;

    #[cfg(target_os = "linux")]
    { install_systemd(&exe_path) }
    #[cfg(target_os = "macos")]
    { install_launchd(&exe_path) }
    #[cfg(target_os = "windows")]
    { install_windows_service(&exe_path) }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    { anyhow::bail!("Service installation not supported on this platform") }
}
```

Service templates use `{{MUSTACHE}}`-style placeholders. At install time, `service::install()` calls `std::env::current_exe()`, reads the template `.toml` file (or an embedded string if using `include_str!`), replaces placeholders, and writes the rendered file. The Rust `service.rs` module implements a simple string replacement function — no templating library dependency needed.

Placeholder substitution:
- `{{RUSTFOX_BIN}}` → `std::env::current_exe()` (the binary being run)
- `{{RUSTFOX_CONFIG}}` → `~/.rustfox/config.toml` (resolved via `dirs::home_dir()`)
- `{{RUSTFOX_HOME}}` → `~/.rustfox` (resolved via `dirs::home_dir()`)

### Code extraction approach

Rather than duplicating the wizard from `src/bin/setup.rs`, the plan is to:

1. Move the shared wizard logic into `src/setup/wizard.rs`
2. Keep `src/bin/setup.rs` as a thin CLI wrapper that calls `rustfox::setup::wizard::run()`
3. The main binary's `--setup` flag calls the same function
4. This avoids any behavior change for existing users of `./setup.sh` or `cargo run --bin setup`

## Build and packaging

### Build scripts

| Script | Produces | Contents |
|--------|----------|----------|
| `scripts/build-deb.sh` | `.deb` | Binary + systemd service postinst/prerm |
| `scripts/build-rpm.sh` | `.rpm` | Binary + systemd service %post/%preun |
| `scripts/build-macos.sh` | `.tar.gz` | Binary + launchd plist + install.sh |
| `scripts/build-windows.ps1` | `.zip` | Binary + install-service.bat + uninstall-service.bat |
| `scripts/install.sh` | — | Universal: detect platform → cargo install → service setup |

### Target triple to package arch mapping

Build scripts must translate Rust target triples to package manager architecture names:

| Rust target triple | Debian arch | RPM arch |
|--------------------|-------------|----------|
| `x86_64-unknown-linux-gnu` | `amd64` | `x86_64` |
| `aarch64-unknown-linux-gnu` | `arm64` | `aarch64` |

The `scripts/build-deb.sh` script receives the target triple as an argument and derives the Debian arch name internally. The same approach applies to `scripts/build-rpm.sh`.

### Package layout (Linux .deb example)

```
rustfox_1.0.0_amd64.deb
├── DEBIAN/
│   ├── control
│   ├── postinst    # install service: systemctl --user enable --now rustfox
│   └── prerm       # remove service: systemctl --user disable --now rustfox
└── usr/
    └── bin/
        └── rustfox
```

Service templates shipped in `/usr/share/rustfox/` for the postinst script to copy to `~/.config/systemd/user/`.

### GitHub Actions release workflow

`.github/workflows/release.yml`:

```yaml
name: Release

on:
  push:
    tags: ['v*']

jobs:
  build:
    strategy:
      matrix:
        include:
          - os: ubuntu-latest
            target: x86_64-unknown-linux-gnu
            ext: tar.gz
          - os: ubuntu-latest
            target: aarch64-unknown-linux-gnu
            ext: tar.gz
          - os: macos-latest
            target: x86_64-apple-darwin
            ext: tar.gz
          - os: macos-latest
            target: aarch64-apple-darwin
            ext: tar.gz
          - os: windows-latest
            target: x86_64-pc-windows-msvc
            ext: zip

    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - uses: Swatinem/rust-cache@v2
      - run: cargo build --release --target ${{ matrix.target }}
      - uses: actions/upload-artifact@v4
        with:
          name: binary-${{ matrix.target }}
          path: target/${{ matrix.target }}/release/rustfox${{ matrix.ext == 'zip' && '.exe' || '' }}
          if-no-files-found: error

  package:
    needs: build
    runs-on: ubuntu-latest
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            script: scripts/build-deb.sh
          - target: aarch64-unknown-linux-gnu
            script: scripts/build-deb.sh
          - target: x86_64-apple-darwin
            script: scripts/build-macos.sh
          - target: aarch64-apple-darwin
            script: scripts/build-macos.sh
          - target: x86_64-pc-windows-msvc
            script: scripts/build-windows.ps1
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
        with:
          name: binary-${{ matrix.target }}
          path: dist/
      - run: ${{ matrix.script }}
        shell: ${{ matrix.target == 'x86_64-pc-windows-msvc' && 'pwsh' || 'bash' }}
      - uses: softprops/action-gh-release@v2
        with:
          files: dist/*
```

### Generated release artifacts

Artifact filenames use the Rust target triple (not shortened arch names):

```
rustfox-v1.0.0-x86_64-unknown-linux-gnu.tar.gz
rustfox-v1.0.0-aarch64-unknown-linux-gnu.tar.gz
rustfox-v1.0.0-x86_64-apple-darwin.tar.gz
rustfox-v1.0.0-aarch64-apple-darwin.tar.gz
rustfox-v1.0.0-x86_64-pc-windows-msvc.zip
```

Each archive contains:
- `rustfox` binary (or `rustfox.exe` on Windows)
- `config.example.toml`
- `install.sh` or `install-service.bat`
- Service template files for the platform

## Data flow

```
User runs: rustfox --setup
  ↓
main.rs parses --setup flag
  ↓
setup::wizard::run()
  ├── Web mode: starts Axum on :8719, opens browser
  │   └── User fills form → POST /api/save-config → writes config.toml
  └── CLI mode: interactive prompts → writes config.toml
  ↓
Config saved. Ask: "Install as background service? [Y/n]"
  ├── No  → exit
  └── Yes → setup::service::install()
              ↓
              ┌─ Linux:   write systemd unit → systemctl --user daemon-reload
              │           → systemctl --user enable --now rustfox
              ├─ macOS:   write launchd plist → launchctl load -w
              └─ Windows: sc create + sc failure + sc start
              ↓
              Print: "RustFox installed as a background service."
```

## Error handling

- Service install errors are **non-fatal** — config is already written to disk first
- After a failed `service::install()`, the wizard prints the error and instructions, then exits. The user can retry with `rustfox --service install` later
- `--service` subcommand validates the platform is supported before doing anything (returns clear error on unsupported OS)
- Each service action prints user-friendly error if a platform tool is missing (e.g., "systemctl not found — is systemd installed?")
- The wizard's Axum server gracefully shuts down after config save (existing pattern) regardless of service install result

## Deferred

- Docker image (separate work item)
- Cross-compilation for all targets from a single CI runner
- Auto-update mechanism (`rustfox --update`)
- Integration tests for service scripts
- Homebrew formula for macOS
