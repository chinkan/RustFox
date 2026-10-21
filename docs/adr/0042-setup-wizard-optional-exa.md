# ADR-0042: Setup wizard optional Exa one-tap

- **Status:** Accepted
- **Date:** 2026-10-21
- **Target release:** v1.0.7
- **Amends:** ADR-0041 (wizard Exa-only exception to §3 / Non-goals), ADR-0023 (wizard may offer optional Exa; pin/consent/stub/soft 429 stand)
- **Depends on:** ADR-0041 (presets + Custom; Settings MCP form), ADR-0023 (Exa pin, consent, stub, soft 429)
- **Related:** #256 (tracking; this docs PR Refs; later impl PR Closes), #172 (Settings MCP shipped; out of scope)
- **Issue:** #256

## Context

Settings MCP chrome already shipped on main (#172 / #252+#253 under ADR-0041): Exa and Jina presets plus Custom HTTP form, hot reload, Exa privacy consent and optional `x-api-key`, Jina Bearer, header map. That surface stays the full MCP form.

ADR-0041 Q3 / §3 said portal Settings only and **no wizard MCP step**. Product locks for #256 (PO grill 2026-10-21; Q1–Q13 = A) carve a narrow exception: the **web setup wizard** may offer an **optional, skippable Exa-only** page (keyless + one-time privacy consent). Custom, Jina, and Google stay out of the wizard. Settings remains the place for keys and the full form.

This document records that decision. It does not implement the wizard page, stub wiring, or a release tag.

## Decision

Product locks (Q1–Q13 = A) are authoritative:

1. **Amends ADR-0041 §3 / Non-goals:** the wizard may offer **Exa only**. Custom, Jina, and Google stay out of the wizard. Settings keeps the full MCP form (presets + Custom).

2. **Optional skippable page after core fields:** after the existing core wizard fields, show one optional Exa page. Default is **off**. **Skip** leaves Exa off (no consent, no `[[mcp_servers]]` Exa row written for enable).

3. **Keyless + one-time consent:** enable requires privacy consent (same meaning as ADR-0023 / Settings). There is **no API key field** in the wizard. An optional Exa API key remains **Settings only** (SecretStore / `x-api-key` as today).

4. **Same shared state as Settings:** enabling writes the same Exa `[[mcp_servers]]` row and `privacy_accepted_at` that Settings uses. Portal and wizard share one on/off. Disabling in Settings turns Exa off for the instance.

5. **Persist only during setup:** the wizard does **not** live-connect to Exa. It only persists config/consent. Connect happens on normal startup or Settings hot-reload (ADR-0041 §9).

6. **Ollama path:** the Ollama setup path gets the **same** optional page. Consent is still required to enable.

7. **Thin CLI setup:** the thin CLI setup path does **not** ask about MCP (no Exa page, no MCP form).

8. **No backfill:** existing installs are not migrated into showing this page. Only a **new** install or a **re-run** of the web setup wizard sees it.

9. **Stub + Telegram offer (later impl):** the later implementation PR includes ADR-0023 §7 behavior: Exa off → `web_search` stub + once-per-conversation Telegram offer; Exa on → no stub. Soft 429 behavior is unchanged (ADR-0023 §8). This docs PR only documents that; it does not ship the stub.

10. **Settings MCP (#172) stays:** do not rework the Settings form or presets chrome already on main.

11. **Parked issues:** #194 / #202 stay parked; this work does not touch them.

12. **Sequencing:** this docs PR **Refs #256**. A later **one** implementation PR (wizard UI page + Dev setup persist + stub wiring) **Closes #256**. No release tag in either PR.

### Soft checks (later implementation PR)

Acceptance from #256 (for the impl PR, not this docs change):

1. After core wizard fields, optional skippable Exa page; default off; Skip leaves Exa off.
2. Enable requires privacy consent; writes the same Exa `[[mcp_servers]]` + `privacy_accepted_at` as Settings; no API key field in the wizard.
3. Ollama setup path gets the same page.
4. No Jina / Custom / Google on the wizard.
5. Exa off → `web_search` stub + once-per-conversation Telegram offer; Exa on → no stub; soft 429 unchanged (ADR-0023).
6. Docs ADR-0042 merged before or with the impl; four gates; no release tag in the impl PR.

## Non-goals

- Jina, Custom, or Google on the setup wizard.
- A full MCP form (name, URL, Bearer, header map) in the wizard.
- An Exa API key field in the wizard.
- Live Exa connect during setup.
- Thin CLI MCP / Exa ask.
- Backfill or migration UI for existing installs.
- Reworking Settings MCP (#172) already on main.
- #194 / #202.
- Implementation code or a release tag in this docs change.

## Consequences

- New and re-run web wizard installs can opt into keyless Exa with one consent tap, without leaving setup for Settings.
- ADR-0041’s “no wizard MCP step” becomes “no full MCP form / no Jina/Custom/Google in wizard”; Exa optional page is the documented exception.
- ADR-0023 pin, consent, stub, soft 429, and Settings key path stand; wizard shares the same Exa on/off.
- Operators who skip keep Exa off and can still enable later in Settings or via Telegram stub offer (once impl lands).
- Thin CLI and existing installs without a wizard re-run are unchanged.

## Related

- ADR-0041 — presets + Custom; Settings form. Amended here for the wizard Exa-only exception.
- ADR-0023 — Exa pin, consent, stub, soft 429. Amended here for optional wizard Exa.
- #256 — tracking. This PR Refs; later impl PR Closes.
- #172 — Settings MCP shipped; out of scope here.
