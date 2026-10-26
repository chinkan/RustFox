# ADR-0044: xAI Active provider peer

- **Status:** Accepted (docs; impl after ADR on main + TL assign; this PR Refs #266 only)
- **Date:** 2026-10-26
- **Related:** ADR-0033 (Active vs provider model), ADR-0035 (`default_provider` / Active persist), ADR-0016 (SecretStore), ADR-0043 (parallel OpenCode — does not block), Issue #266
- **Issue:** #266

## Context

Operators want xAI (Grok) as a first-class Active provider peer alongside `openrouter` and `ollama`, selectable for chat and for the RustFox tool loop. Issue #266 tracks that work. Product grilling (R1/R2, all A — Accepted) locked the provider surface, peer semantics, credentials, UI first cut, fallback posture, and sequencing relative to OpenCode Active (ADR-0043).

xAI exposes an OpenAI-compatible Chat Completions API at `https://api.x.ai/v1`. This ADR locks RustFox to that direct surface (`type=xai`), not an OpenRouter alias and not the Responses API for this cut.

This document records those locks. It does not implement the provider peer, Settings/wizard chrome, Telegram `/models` wiring, fallback-chain edits, or a release tag.

## Decision

Product locks (grill R1/R2 all A) are authoritative for the first implementable slice after this ADR is on `main` and TL assigns:

1. **Provider type `xai` → direct xAI OpenAI-compat.** `type=xai` talks to `https://api.x.ai/v1` with OpenAI-compatible Chat Completions: `POST /v1/chat/completions`, Bearer auth. This is **not** an OpenRouter alias. **No Responses API** in this cut.

2. **Full Active peer.** xAI is a selectable Active provider: it may write `default_provider`, drive chat + the RustFox tool loop, and run the scheduler when Active = xAI. Peer semantics follow ADR-0033 / ADR-0035 for Active / `default_provider` persist.

3. **Default model:** `grok-4.7`.

4. **Credentials:** SecretStore + Settings/config, same pattern as the OpenRouter key (ADR-0016 spirit). The setup wizard does **not** force-ask for an xAI key.

5. **UI first cut:** backend peer + Telegram `/models` + Settings key. Portal / wizard large UI is a **separate follow-up**, not part of the first impl slice.

6. **Fallback chain unchanged.** Do **not** change the default fallback chain. Docs may suggest an optional example; the operator adds `xai/…` themselves if they want it. No forced xAI → OpenRouter leave hop.

7. **Parallel to OpenCode Active (ADR-0043).** Independent track; ADR-0043 does **not** block this work, and this work does not block ADR-0043.

8. **Parked issues:** #194 / #202 stay parked; this work does not touch them.

9. **Typed error, no silent hop:** If xAI is selected or hit while misconfigured (missing key, bad base URL, peer not yet implemented, etc.), return a **typed error**. **Forbid** silent fallback or hop to another provider type from that miss (spirit of ADR-0043 Decision §6).

10. **Sequencing:** this docs PR **Refs #266** only (do **not** Closes/Fixes). A later **implementation** PR lands only after this ADR is on `main`, the usual **four gates** pass, and TL assigns; that impl PR **Closes #266**. **No release tag** in this docs PR or implied by it. This docs PR does not implement.

## Non-goals

- Implementing the `xai` provider peer, chat path, tool loop, or scheduler wiring in this PR.
- OpenRouter alias for xAI / Grok.
- Responses API support in this cut.
- Changing the default fallback chain or forcing an xAI → OpenRouter leave hop.
- Portal / wizard large UI (separate follow-up).
- Forcing the setup wizard to ask for an xAI key.
- #194 / #202.
- Blocking or being blocked by OpenCode Active (ADR-0043).
- Silent hop to another provider type when xAI is misconfigured.
- A release tag from this docs change.

## Consequences

- After impl (post ADR on main + four gates + TL assign), operators can select xAI as Active with default model `grok-4.7`, chat and tool-loop against `https://api.x.ai/v1`, and run scheduled work when Active = xAI.
- Credentials follow SecretStore + Settings/config like the OpenRouter key; wizard stays non-forcing for xAI.
- First-cut UX is Telegram `/models` + Settings key; richer portal/wizard UI waits on a follow-up.
- Default fallback chain stays as today; optional `xai/…` entries are operator-owned.
- Misconfigured xAI fails loudly with a typed error rather than silently hopping to another provider type.
- OpenCode Active (ADR-0043) remains an independent parallel track.
- #194 / #202 remain parked.
- Tracking stays on #266: this PR Refs; later impl PR Closes after gates + TL assign. No tag.

## Related

- ADR-0016 — SecretStore pattern for provider keys (OpenRouter); xAI follows the same spirit.
- ADR-0033 — Active vs provider model; xAI becomes a full Active peer.
- ADR-0035 — `default_provider` / Active persist; xAI may write it when selected.
- ADR-0043 — OpenCode detect / peer Active direction; parallel, does not block.
- #266 — tracking. This PR Refs; later impl PR Closes after ADR on main + four gates + TL assign.
- #194 / #202 — parked; out of scope.
