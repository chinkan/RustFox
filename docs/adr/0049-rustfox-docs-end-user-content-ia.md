# ADR-0049: rustfox-docs end-user content IA

- **Status:** Accepted
- **Date:** 2026-10-27
- **Related:** ADR-0047 (Starlight stack / repo / MVP ship record — **do not amend** except its own Accepted Mermaid stack delta), ADR-0030 (self-upgrade; Update page documents locks in plain language), ADR-0043 (OpenCode peer / MissingSessionID), ADR-0044 (xAI Active; bare `xai` refuse), ADR-0046 (flock + safe `--version`; Update/Setup kill-stray notes), Product grill 2026-10-27 (PO r3 Q11–Q15 all ACCEPT), Product delta 2026-10-27 (Commands expand + Providers/Troubleshooting soft locks), **Accepted delta 2026-10-27 (Kan/Product lock — end-user pages do not link ADRs)**
- **Issue:** none (Product grill 2026-10-27; no tracking issue)

## Context

ADR-0047 locked the **stack**, separate repo `chinkan/rustfox-docs`, GH Pages, Astro 7 + Starlight + llms-txt + versions, and an EN MVP page set (Getting started · Install · Build · Usage · Upgrade). That ADR remains the ship/stack record.

Operators now need a second lock: **end-user / self-hoster content information architecture** for the public sidebar — audience, tone, full tree, migration from MVP slugs, page boundaries, Commands inventory, Contribute paths, and phased ship batches — without rewriting ADR-0047 or implementing Starlight pages in this PR.

Product grill 2026-10-27 (PO r3 Q11–Q15 all ACCEPT) locked the tree and boundaries below. Product delta 2026-10-27 expands the Commands inventory (OS CLI + Telegram slash) and soft-locks Providers / Troubleshooting symptoms in plain language (plus GUIDE / `portal-access.md`; ADRs remain SoT in RustFox, not hyperlinked from public end-user pages). Product Accept landed; Status is **Accepted**. Soft test is N/A. **No tag.** This PR does **not** implement Starlight pages in `rustfox-docs`; **RustFox UI** owns Phase 1 on the docs repo after Accept.

**Accepted delta 2026-10-27 (Kan/Product lock):** End-user rustfox-docs / public sidebar pages must **not** require or place ADR hyperlinks (ADRs are developer-only). Soft-lock symptoms on Providers / Troubleshooting stay in **plain language**, with GUIDE / `portal-access.md` / Config / Providers / Commands as the user-facing deep links. ADRs remain SoT in RustFox `docs/adr/`; link-out OK from GUIDE / developer docs, **not** from public sidebar end-user pages. Do **not** open a new ADR; Status stays **Accepted**. UI strips any ADR links in a parallel rustfox-docs PR (do not wait on that merge).

## Decision

Product locks (PO grill 2026-10-27; Q11–Q15 ACCEPT + Product delta 2026-10-27 + Accepted delta 2026-10-27 no end-user ADR links) are authoritative:

1. **New ADR-0049 — do not amend ADR-0047 for content IA.** ADR-0047 stays the stack / repo / MVP ship record (its own Accepted Mermaid stack delta is separate). Content IA lives here.

2. **Audience / tone.** End-user and self-hoster. Not an operator GUIDE dump. Keep GUIDE / ADR list / Architecture deep pages **out of the sidebar**. Link-out to GUIDE / developer docs is OK; **do not** place ADR hyperlinks on public sidebar end-user pages. **Advanced** is out of scope for this cut.

3. **Language.** English-only this cut. zh-HK / Cantonese later.

4. **Sidebar IA (full tree locked; ship phased).**

   - **Getting started** — keep; short path overview that **links into** How to use. Do **not** merge or deprecate Getting started into How to use.
   - **How to use** (group): Setup · Config · Telegram · Portal · Update
   - **Commands** — one page; two H2 sections — **OS CLI** and **Telegram bot commands** (Decision 10)
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

8. **Out of sidebar this cut.** GUIDE · ADR list · Architecture deep pages. Link-out to GUIDE / developer docs OK; **no ADR hyperlinks on public sidebar end-user pages.** **Advanced** OOS.

9. **Update page.** Document the locks from **ADR-0030** and **ADR-0046** in **plain language** on the end-user Update page (kill-stray / same fox-home notes and safe `--version` as Setup/Update require). **Do not** require or place ADR hyperlinks on that public page; ADR link-out remains OK from GUIDE / developer docs. Do **not** re-lock the self-upgrade engine (per ADR-0047). **Docs lag:** ADR-0046 Context may still say `--version` is unimplemented — Commands / Update SoT for the shipped safe probe is **code + GUIDE + site**; do **not** re-lock engine semantics here. Engine locks stay in ADR-0030 / ADR-0046.

10. **Commands inventory (locked — do not invent extras).** One Commands page with **two H2 sections**. Cross-link **Update** for `/selfupgrade`. If a command appears missing during impl, note it for Product; **do not** add without PO.

    ### OS CLI

    - Bare `rustfox` / `run` (foreground start; `run` same as bare)
    - `--setup` [ `--cli` ]
    - `--config` / positional config path
    - `install` / `uninstall` (`remove` deprecated alias)
    - `status` / `start` / `stop` / `restart`
    - `--version` / `-V` (safe probe; see Decision 9 docs-lag note)
    - Note: **no `--help` / `-h`** in this cut
    - Deprecated `--service <verb>` wrapper (prefer bare verbs)
    - Kill-stray / fox-home notes

    ### Telegram bot commands (Active only)

    - `/start` · `/clear` · `/tools` · `/skills` · `/verbose` · `/queryrewrite` · `/update-skills` · `/models` · `/format` · `/portal` · `/config` · `/selfupgrade` · `/restart` · `/agents` (plus subforms as needed)
    - **Planned** slash (`/supervise` · `/tasks` · …): one-liner “not shipped” (or omit); prefer one-liner OOS + deep-link GUIDE. Do **not** invent slash beyond this inventory.

11. **Providers / Troubleshooting soft locks (plain language — not GUIDE-only; no ADR hyperlinks on end-user pages).** Phase 2 pages **still required** to carry these end-user symptoms. Write them in **plain language**. Cross-link GUIDE / `portal-access.md` / Config / Providers / Commands as relevant. **Do not** require and **do not** place ADR hyperlinks on rustfox-docs end-user / sidebar pages. ADRs remain SoT in RustFox `docs/adr/`; link-out OK from GUIDE / developer docs only.

    - **OpenCode:** session / MissingSessionID ≠ leave-Go (plain language; OpenCode session header / peer session behavior — do not require an ADR hyperlink on the public page)
    - **xAI:** bare `xai` refuse / no silent hop (plain language)
    - **401 / 402 / credits** = user-facing symptoms (Providers and/or Troubleshooting)
    - **Portal:** link `portal-access.md` (from Portal how-to and Troubleshooting as relevant)

12. **Contribute.** Paths for RustFox **code** PRs and `rustfox-docs` **content** PRs. ADRs remain SoT in RustFox `docs/adr/` (unchanged from ADR-0047).

13. **Unchanged from ADR-0047.** Stack Astro 7 + Starlight + llms-txt + versions; GH Pages; content repo `chinkan/rustfox-docs`; ADRs SoT in RustFox. This ADR does not re-open those locks (Mermaid additive stack delta lives on ADR-0047).

14. **Staffing after Accept.** Assign **RustFox UI** Phase 1 on `rustfox-docs` (not this PR). This docs-only PR records locks only.

15. **Sequencing.** This docs-only PR records locks; Product Accept landed (Status **Accepted**). Follow-on work uses four gates as usual. Soft N/A. **No tag.** No Rust / portal / package / `Cargo.lock` / runtime / tests in this PR.

## Non-goals

- Amending or replacing ADR-0047 for content IA (Mermaid stack delta on ADR-0047 is a separate Accepted note).
- Implementing Starlight pages, sidebar config, redirects, or content in `chinkan/rustfox-docs` in this PR.
- Phase 1 / Phase 2 UI work (UI after Accept).
- zh-HK / Cantonese docs this cut.
- Putting GUIDE / ADR list / Architecture / Advanced in the sidebar this cut.
- Requiring or placing ADR hyperlinks on rustfox-docs end-user / public sidebar pages.
- Re-locking ADR-0030 self-upgrade engine or ADR-0046 flock / `--version` semantics.
- Inventing OS CLI or Telegram slash Commands beyond Decision 10.
- Turning Providers / Troubleshooting into GUIDE-only dumps (soft locks in Decision 11 stay required in plain language; no ADR hyperlink requirement on those public pages).
- Any Rust / portal / package / `Cargo.lock` / runtime / test change.
- Soft test or a release tag from this docs PR.

## Consequences

- End-user sidebar IA is locked in full; UI can ship Phase 1 then Phase 2 without re-grilling the tree.
- ADR-0047 remains the authoritative stack / repo / MVP record; MVP slugs migrate with redirects.
- Update documents ADR-0030 + ADR-0046 locks in plain language without engine re-lock and without ADR hyperlinks on the public page.
- Commands page stays a bounded two-section inventory (OS CLI + Telegram Active); extras need Product.
- Providers / Troubleshooting carry soft locks in plain language (OpenCode session / MissingSessionID ≠ leave-Go; bare `xai` refuse; 401/402/credits; Portal → `portal-access.md`), not GUIDE-only dumps and **not** via ADR hyperlinks on end-user pages.
- ADRs remain SoT in RustFox `docs/adr/`; GUIDE / developer docs may link out to them; public sidebar end-user pages must not.
- Commands/Update document the shipped safe `--version` probe without amending ADR-0046 engine text.
- Contribute stays two PR paths (code vs docs content); ADRs stay in RustFox.
- Soft N/A on this docs PR; no tag; Status is **Accepted** (Product Accept landed; Accepted delta 2026-10-27 recorded).

## Related

- ADR-0047 — Starlight `rustfox-docs` stack / repo / MVP; **do not amend** for content IA (Accepted Mermaid stack delta is separate on that ADR).
- ADR-0030 — `/selfupgrade`; Update page documents locks in plain language; engine not re-locked here.
- ADR-0043 — OpenCode peer / session / MissingSessionID ≠ leave-Go; Providers soft lock (plain language on end-user pages; SoT here).
- ADR-0044 — xAI Active; bare `xai` refuse / no silent hop; Providers soft lock (plain language on end-user pages; SoT here).
- ADR-0046 — start-time flock + safe `--version`; Setup/Update kill-stray / fox-home notes. Docs lag on Context “unimplemented” — Commands/Update SoT = code+GUIDE+site for shipped probe; do not re-lock engine here.
- Product grill 2026-10-27 — PO r3 Q11–Q15 ACCEPT; base IA locks.
- Product delta 2026-10-27 — Commands two-H2 expand + Providers/Troubleshooting soft locks.
- **Accepted delta 2026-10-27 (Kan/Product)** — end-user rustfox-docs pages do not link ADRs; soft locks stay plain language + GUIDE / `portal-access.md` / Config / Providers / Commands; ADRs SoT in RustFox `docs/adr/`.
- Live tip `1704d84` — docs PR base (`origin/main` at this amend).
