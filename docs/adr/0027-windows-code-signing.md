# ADR-0027: Code signing for Windows release binaries

- **Status:** Accepted (Option A locked 2026-10-18 — SignPath Foundation)
- **Date:** 2026-10-14
- **Accepted:** 2026-10-18
- **Target release:** v1.0.7
- **Depends on:** none
- **Issue:** #198

## Context

`.github/workflows/release.yml` (trigger: `v*` tag push or `workflow_dispatch`)
builds Windows in the `build` job matrix entry `windows-latest` /
`x86_64-pc-windows-msvc` with `cargo build --release`. The "Stage release
files" step copies `target/release/rustfox.exe`, `config.example.toml`,
`scripts/install.sh`, `scripts/services/` and the `install-service.bat` /
`uninstall-service.bat` templates into `staging/`; "Create archive (Windows)"
zips it with `Compress-Archive` as `rustfox-<tag>-x86_64-pc-windows-msvc.zip`;
"Upload artifact" (`actions/upload-artifact@v7`) uploads it. The `release` job
(`ubuntu-latest`, tag pushes only) downloads all artifacts and publishes them
with `softprops/action-gh-release@v3`. Today only `rustfox.exe` is shipped
(no DLLs, no installer).

Unsigned Windows binaries are blocked by Windows Smart App Control (SAC) with
no "Run anyway" option. This conflicts with the goal that non-technical users
can download and run RustFox. SmartScreen "unknown publisher" warnings are a
separate, softer problem. Local builds on SAC-enforcing machines can also
fail with `os error 4551` (community-sourced; Microsoft does not document
this code).

## Decision

1. Sign every Windows `.exe` and `.dll` shipped in a release (and any future
   installer) in CI, with an RFC 3161 timestamp, **before** it is zipped.
2. Once the gate in item 6 is enabled, a signing failure fails the release and
   unsigned Windows binaries are not published.
3. Signing runs only in the tag/release path of `release.yml`, not in `ci.yml`,
   `check-compile.yml` or pull requests. Manual `workflow_dispatch` builds may
   remain unsigned for testing.
4. Signing secrets live in GitHub Actions, scoped to a protected environment
   `release-signing` that only allows `v*` tags (maintainer creates this).
5. **Provider: Option A — SignPath Foundation** (locked 2026-10-18). Option B
   (Certum) remains a documented fallback only if SignPath rejects the project.
6. **Rollout gating.** The gate in item 2 is controlled by an explicit repo
   variable `REQUIRE_SIGNING`, not by whether a secret exists. While it is
   unset, Windows releases ship unsigned and the release notes must say so
   plainly. When `REQUIRE_SIGNING=true`, a missing or expired secret or any
   signing failure fails the release; unsigned Windows binaries are never
   published silently.
7. **SmartScreen.** Even signed binaries may show "Windows protected your PC"
   until the publisher builds reputation. The README must explain in plain,
   non-technical words: click "More info", then "Run anyway". This is
   distinct from Smart App Control, which offers no such option.

### Option A (locked): SignPath Foundation OSS program

- Upload the unsigned `rustfox.exe` with `actions/upload-artifact` v4+
  (step id `upload-unsigned-artifact`, prefer `archive: false`), then call
  `signpath/github-action-submit-signing-request@v3` with
  `github-artifact-id: ${{ steps.upload-unsigned-artifact.outputs.artifact-id }}`,
  `api-token: ${{ secrets.SIGNPATH_API_TOKEN }}`, and organization / project /
  signing-policy slugs from repo variables; download the signed result into
  staging, verify with `signtool`, then zip.
- All jobs before signing must run on GitHub-hosted runners (already true).
- Each release signing request needs manual approval in SignPath.
- The publisher shown to users is "SignPath Foundation".
- Requirements: a code signing policy line in the README and release page
  ("Free code signing provided by SignPath.io, certificate by SignPath
  Foundation"); named roles (Author, Reviewer, Approver); a privacy statement
  naming the external services RustFox talks to (OpenRouter, Telegram); MFA on
  GitHub and SignPath accounts; the CI workflow and build scripts are treated
  as source code and reviewed carefully.

Maintainer setup before enabling the gate:

| Item | Where |
|------|--------|
| SignPath org / project / signing policy | SignPath.io (OSS application) |
| `SIGNPATH_API_TOKEN` | Environment secret on `release-signing` |
| `SIGNPATH_ORGANIZATION_ID` | Repo or environment variable |
| `SIGNPATH_PROJECT_SLUG` | Repo or environment variable |
| `SIGNPATH_SIGNING_POLICY_SLUG` | Repo or environment variable |
| Environment `release-signing` | GitHub → Environments; restrict to `v*` tags |
| `REQUIRE_SIGNING=true` | Repo variable — set only after the rows above work |

### Option B (fallback only if SignPath rejects)

- Certum Open Source Code Signing in the cloud (SimplySign), from about
  EUR 49/year, issued to an individual, for open-source use only.
- Keys stay in Certum's cloud HSM; CI signs with `signtool` or `jsign` using
  cloud credentials stored in the protected environment's secrets.
- Certificate validity is at most 458 days from 2026-03, so renewal is
  roughly yearly. The publisher shown is the individual maintainer.

### Rejected

- **Azure Artifact Signing:** individual validation is limited to US/Canada
  and Hong Kong is not an eligible region for organizations.
- **EV certificates:** no SmartScreen reputation advantage since 2024, and
  they require a registered company.
- **Shipping unsigned forever:** keeps SAC blocking the target audience.

## Non-goals

- Signing or notarizing Linux and macOS artifacts is out of scope.

## Acceptance criteria

- A CI step runs `signtool verify /pa /v` on every signed binary and fails
  the job on any error.
- On the maintainer's Windows PC with SAC on, the downloaded release
  `rustfox.exe` (and any DLLs) launches, no `os error 4551` occurs, and
  `signtool verify /pa` passes.
- The README gains a section on local builds hitting `os error 4551`: use the
  signed release, or build on a machine without SAC enforcement. It must not
  casually advise turning SAC off, since re-enabling it may require a Windows
  reset when optional diagnostic data is off.
- The README explains, in plain non-technical words, the SmartScreen
  "Windows protected your PC" warning and the "More info" then "Run anyway"
  steps, and states that Smart App Control blocks have no such option.
- While signing is not yet configured, release notes for each release state
  that the Windows binaries are unsigned.
- While `REQUIRE_SIGNING` is not enabled, the release notes and README state
  that Windows with Smart App Control on cannot open the unsigned build;
  users should wait for a signed release or build it themselves, and are not
  told to turn SAC off. The SmartScreen "Run anyway" steps are described
  separately.
- With `REQUIRE_SIGNING=true`, a release run with the signing secret removed
  fails instead of publishing unsigned binaries.

## Consequences

- `release.yml` gains a conditional SignPath path gated by `REQUIRE_SIGNING`.
- Tag releases with the gate enabled wait for SignPath manual approval.
- Until the maintainer finishes SignPath setup and sets `REQUIRE_SIGNING=true`,
  Windows release zips remain unsigned (same as today).
- Target: v1.0.7.
