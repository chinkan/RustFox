# ADR-0049: rustfox-docs end-user content IA

- **Status:** Proposed
- **Date:** 2026-10-27
- **Related:** ADR-0047 (Starlight stack / repo / MVP ship record — **do not amend**), ADR-0030 (self-upgrade; Update page links/ingests), ADR-0046 (flock + safe `--version`; Update/Setup kill-stray notes), Product grill 2026-10-27 (PO r3 Q11–Q15 all ACCEPT)
- **Issue:** none (Product grill 2026-10-27; no tracking issue)

## Context

ADR-0047 locked the **stack**, separate repo `chinkan/rustfox-docs`, GH Pages, Astro 7 + Starlight + llms-txt + versions, and an EN MVP page set (Getting started · Install · Build · Usage · Upgrade). That ADR remains the ship/stack record.

Operators now need a second lock: **end-user / self-hoster content information architecture** for the public sidebar — audience, tone, full tree, migration from MVP slugs, page boundaries, Commands inventory, Contribute paths, and phased ship batches — without rewriting ADR-0047 or implementing Starlight pages in this PR.

Product grill 2026-10-27 (PO r3 Q11–Q15 all ACCEPT) locked the tree and boundaries below. This document records them as **Proposed** until Product Accept flips Status to Accepted. Soft test is N/A. **No tag.** This PR does **not** implement Starlight pages in `rustfox-docs`; **RustFox UI** owns Phase 1 on the docs repo after Accept.

## Decision

Product locks (PO grill 2026-10-27; Q11–Q15 ACCEPT) are authoritative:

1. **New ADR-0049 — do not amend ADR-0047.** ADR-0047 stays the stack / repo / MVP ship record. Content IA lives here.

2. **Audience / tone.** End-user and self-hoster. Not an operator GUIDE dump. Keep GUIDE / ADR list / Architecture deep pages **out of the sidebar** (link-out OK). **Advanced** is out of scope for this cut.

3. **Language.** English-only this cut. zh-HK / Cantonese later.

4. **Sidebar IA (full tree locked; ship phased).**

   - **Getting started** — keep; short path overview that **links into** How to use. Do **not** merge or deprecate Getting started into How to use.
   - **How to use** (group): Setup · Config · Telegram · Portal · Update
   - **Commands** — one page; detail each CLI in the locked inventory (Decision 10)
   - **How to contribute** — one page; RustFox code PR path + `rustfox-docs` content PR path (not an ADR dump)
   - **Features** — one page; H2: tools · MCP · schedule · memory (capability catalog)
   - **Providers** — one page; H2: OpenRouter · OpenCode · xAI · Ollama (choose / credential / Active·peer)
   - **Troubleshooting** — one page

5. **Ship batches (ADR locks the full tree; UI ships phased).**

   - **Phase 1** (UI after Accept; not this PR): How to use ×5 (Setup · Config · Telegram · Portal · Update) + Commands + How to contribute. **Fold Build from source into Setup (advanced).** Drop top-level Build.
   - **Phase 2** (later UI PR): Features + Providers + Troubleshooting

6. **MVP migration (from ADR-0047 MVP slugs).**

   - Install + Build → **Setup**
   - Usage → **Telegram** / **Portal** / **Config**
   - Upgrade → **Update**
   - Old slugs keep redirects / bookmarks

7. **Page boundaries.**

   - **Telegram** = daily how-to
   - **Features** = what / limits + links (no long duplicate of Telegram how-to)
   - **Config** = how to fill wizard / TOML
   - **Providers** = per-provider choose / credential / Active·peer (no knob-dump duplicate of Config)

8. **Out of sidebar this cut.** GUIDE · ADR list · Architecture deep pages (link-out OK). **Advanced** OOS.

9. **Update page.** Link / ingest **ADR-0030** and **ADR-0046**. Do **not** re-lock the self-upgrade engine (per ADR-0047). Document kill-stray / same fox-home notes and safe `--version` alignment with ADR-0046 as Setup/Update require; engine semantics stay in ADR-0030 / ADR-0046.

10. **Commands inventory (locked — do not invent extras).** One Commands page covers:

    - `rustfox` daemon
    - `--setup` / wizard
    - `install` / `status` / `stop` / `restart`
    - `--version` / `-V`
    - `/selfupgrade` (cross-link **Update**)
    - kill-stray / fox-home notes

    If a command appears missing during impl, note it for Product; **do not** add without PO.

11. **Contribute.** Paths for RustFox **code** PRs and `rustfox-docs` **content** PRs. ADRs remain SoT in RustFox `docs/adr/` (unchanged from ADR-0047).

12. **Unchanged from ADR-0047.** Stack Astro 7 + Starlight + llms-txt + versions; GH Pages; content repo `chinkan/rustfox-docs`; ADRs SoT in RustFox. This ADR does not re-open those locks.

13. **Staffing after Accept.** Assign **RustFox UI** Phase 1 on `rustfox-docs` (not this PR). This docs-only PR records locks only.

14. **Sequencing.** This docs-only PR → Product Accept (flip Status to Accepted) → four gates as usual for follow-on work. Soft N/A. **No tag.** No Rust / portal / package / `Cargo.lock` / runtime / tests in this PR.

## Non-goals

- Amending or replacing ADR-0047.
- Implementing Starlight pages, sidebar config, redirects, or content in `chinkan/rustfox-docs` in this PR.
- Phase 1 / Phase 2 UI work (UI after Accept).
- zh-HK / Cantonese docs this cut.
- Putting GUIDE / ADR list / Architecture / Advanced in the sidebar this cut.
- Re-locking ADR-0030 self-upgrade engine or ADR-0046 flock / `--version` semantics.
- Inventing Commands beyond Decision 10.
- Any Rust / portal / package / `Cargo.lock` / runtime / test change.
- Soft test or a release tag from this docs PR.

## Consequences

- End-user sidebar IA is locked in full; UI can ship Phase 1 then Phase 2 without re-grilling the tree.
- ADR-0047 remains the authoritative stack / repo / MVP record; MVP slugs migrate with redirects.
- Update stays aligned with ADR-0030 + ADR-0046 without engine re-lock.
- Commands page stays a bounded inventory; extras need Product.
- Contribute stays two PR paths (code vs docs content); ADRs stay in RustFox.
- Soft N/A on this docs PR; no tag; Status stays **Proposed** until Product Accept.

## Related

- ADR-0047 — Starlight `rustfox-docs` stack / repo / MVP; **do not amend**.
- ADR-0030 — `/selfupgrade`; Update page links/ingests; engine not re-locked here.
- ADR-0046 — start-time flock + safe `--version`; Setup/Update kill-stray / fox-home notes.
- Product grill 2026-10-27 — PO r3 Q11–Q15 ACCEPT; locks recorded above.
- Live tip `acbb112` — docs PR base (`origin/main` at lock time).
