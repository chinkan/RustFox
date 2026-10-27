# ADR-0047: Starlight docs site `rustfox-docs`

- **Status:** Accepted
- **Date:** 2026-10-27
- **Related:** ADR-0030 (self-upgrade; Upgrade page links/ingests, does not re-lock), ADR-0046 / #289 (single-instance flock + safe `--version`; Upgrade/Install must document kill-all + probe ban), Product-lock GO 2026-10-27
- **Issue:** none (Product-lock GO 2026-10-27; no tracking issue)

## Context

RustFox user-facing docs today live mainly as GUIDE material and ADRs under the main repo `docs/`. Operators and agents need a dedicated, versionable public docs site (Getting started / Install / Build / Usage / Upgrade) with machine-readable llms.txt surfaces, without turning the main-repo `docs/` tree into a Starlight project.

Product-lock GO landed 2026-10-27: create a separate docs repo `chinkan/rustfox-docs`, publish on GitHub Pages, stack Astro 7 + Starlight 0.42.x + community versioning + `starlight-llms-txt`, and keep ADRs only in RustFox `docs/adr/`.

This document records those locks as **Accepted**. It does **not** create the docs repo, implement the Astro/Starlight site, change runtime, or mix with the parallel #289 / ADR-0046 flock track. Main README short pointer lands later when the site URL is live (separate step).

## Decision

Product locks (Product-lock GO 2026-10-27) are authoritative:

1. **Separate docs repo + GH Pages.** Public site lives in `chinkan/rustfox-docs`. GitHub Pages at `https://chinkan.github.io/rustfox-docs/` — set that as the Astro/Starlight `site` URL. **No custom domain.** The main RustFox repo `docs/` stays GUIDE / ADR and does **not** become the Starlight site.

2. **Stack.** Astro **7** + Starlight **0.42.x** + `starlight-llms-txt` (peer ≥0.41). Emit `llms.txt`, `llms-full`, and `llms-small`.

3. **Versioning.** Use community **`starlight-versions`** (Starlight has no built-in versions). Folder-archive model; **current / Latest** = docs-repo `main`. After a RustFox GitHub Release tag, archive one docs version. Docs lag is OK; docs versioning is **not** a release gate. **Risk:** the plugin is early-development — **pin** the dependency and record breakage risk in the docs-repo notes.

4. **MVP EN pages.** Getting started · Install · Build from source · Usage · Upgrade & selfupgrade, plus llms.txt. Cantonese / zh-HK / MCP / portal come later.

5. **Upgrade page content locks (no engine re-lock).** The Upgrade page must link/ingest **ADR-0030** and **ADR-0046 / #289**. Do **not** re-lock or change self-upgrade engine semantics. Install and Upgrade must document **kill-all strays sharing the same fox-home** before install/upgrade, and **ban bare / `--version` as a probe** until #289 lands; after #289 impl, document flock + safe `--version` as the belt (kill-all discipline remains).

6. **Main README pointer (separate step).** When the site is live, add a short pointer in the main RustFox README. That is a later PR; do **not** wait on #289 — the flock track is parallel.

7. **ADRs stay in RustFox.** Architecture Decision Records live only in RustFox `docs/adr/`. The docs site may link or ingest them; it does not become the ADR source of truth.

8. **Staffing (post-merge; this PR does not implement).** UI owns the docs site. Dev adds the main README pointer when the URL is live. **This PR does not create `chinkan/rustfox-docs` and does not implement the site.**

9. **Sequencing:** this docs-only PR → four gates → TL squash onto `main`. **No tag.** Soft test / #194 / #202 out of scope. Parallel to #289 / ADR-0046 / PR #290 — do not wait for the site, and do not mix branches.

## Non-goals

- Creating the `chinkan/rustfox-docs` repository.
- Implementing the Astro / Starlight site, Pages workflow, or versioning archive process in this PR.
- Mixing with #289 / PR #290 / ADR-0046 / branch `docs/289-adr-0046-single-instance-flock`.
- Turning main-repo `docs/` into Starlight.
- Re-locking or changing ADR-0030 self-upgrade engine semantics.
- Soft test / tag / #194 / #202.
- Any Rust / runtime / test change.
- Cantonese / zh-HK / MCP / portal docs in the MVP cut.
- A custom domain for the docs site.

## Consequences

- Operators and agents get a dedicated GH Pages docs site with EN MVP + llms.txt surfaces once UI stands up `chinkan/rustfox-docs`.
- Main-repo GUIDE/ADR layout is unchanged; ADRs remain the SoT under `docs/adr/`.
- Version archives lag releases by design and never gate a RustFox tag.
- Install/Upgrade docs stay aligned with ADR-0030 and ADR-0046/#289 (kill-all + probe ban → flock + safe `--version`) without rewriting upgrade engine locks.
- #289 flock work proceeds in parallel; README pointer waits only on a live site URL, not on flock merge.
- Early `starlight-versions` breakage risk is accepted via pin + recorded risk note in the docs repo.

## Related

- ADR-0030 — `/selfupgrade` main|release|bare-auto; Upgrade page links/ingests; engine semantics not re-locked here.
- ADR-0046 / #289 — start-time flock + safe `--version`; Install/Upgrade must document kill-all + probe ban (post-impl: flock + safe `--version`). Parallel track; do not mix.
- Product-lock GO 2026-10-27 — Accepted locks for separate `rustfox-docs` Starlight site.
- Live tip `7ddd3db` — docs PR base (`origin/main` at lock time).
- #194 / #202 — parked; out of scope.
