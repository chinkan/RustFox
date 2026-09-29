# Changelog

All notable changes to RustFox are documented here.
This project adheres to [Semantic Versioning](https://semver.org/).

## [1.0.3] — 2026-09-29

Security hardening release. Clears all open Dependabot and CodeQL
code-scanning alerts, and upgrades the MCP client to a patched major.

### Security

- **MCP client (`rmcp`) upgraded `0.15` → `2.2`.** Resolves all four open
  Dependabot alerts (GHSA-9pj6-vhgr-3mwh, GHSA-33f5-2c5q-wgwj,
  GHSA-89vp-x53w-74fx, GHSA-9g45-5xwm-f3wc). Adapted the two call sites in
  `src/mcp.rs` to the 2.x API: `CallToolRequestParams` is now
  `#[non_exhaustive]` (built via `::new(..).with_arguments(..)`), and tool
  result content is matched through the `ContentBlock` enum instead of the
  removed `raw` field.
- **SecretStore file backend.** Key and nonce are now drawn directly from the
  OS CSPRNG (`rand::rngs::OsRng.gen()`) instead of filling a zero-initialised
  buffer, and the on-disk key is converted with `try_from` without
  intermediate constant-initialised arrays. No constant is ever used as key
  material. (`rust/hard-coded-cryptographic-value`)
- **Cleartext-logging hardening.** Removed sensitive identifiers and ambiguous
  credential wording from log/format sinks in the setup wizard, conversation
  tests, and secret-store notify tests. (`rust/cleartext-logging`)
- **CI least privilege.** Added explicit `permissions: contents: read` to the
  `ci`, `check-compile`, and `release` (build job) workflows.
  (`actions/missing-workflow-permissions`)

### Changed

- Workspace version bumped to `1.0.3`.

### Fixed

- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and the full test
  suite (792 tests across 18 suites) pass with the upgraded dependency set.

## [1.0.2] — 2026-09-28

- Web portal Control Plane: skills/agents CRUD, GitHub installer, task CRUD.
- 429 resilience: model fallback chain and dead-letter re-run queue.

[1.0.3]: https://github.com/chinkan/RustFox/releases/tag/v1.0.3
[1.0.2]: https://github.com/chinkan/RustFox/releases/tag/v1.0.2
