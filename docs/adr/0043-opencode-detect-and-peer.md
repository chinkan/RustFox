# ADR-0043: OpenCode detect-only (Phase 1) and peer Active direction

- **Status:** Accepted (Direction caveats: D under verification; do not ship Active peer until D or F interim is locked/validated)
- **Date:** 2026-10-24
- **Amends / supersedes:** prior Direction **C** (delegate) on this same ADR (commit `9bdceb1` / filename `0043-opencode-detect-and-delegate.md`). Product HOLD + grill R3 parks C; target is peer Active + RustFox tools.
- **Target release:** post-v1.0.7 / TBD (v1.0.7 tag is Kan-owned; this ADR does not ship a tag)
- **Depends on:** ADR-0025 (sandbox confinement — required for any peer Active / tool-loop ship that runs OpenCode-side work under confinement)
- **Related:** #259 (tracking; this docs PR Refs; later Phase 1 impl PR Closes), ADR-0033 (Active vs provider model), ADR-0035 (`default_provider` / Active persist)
- **Issue:** #259

## Context

OpenCode (`opencode`) fronts a large local model catalogue and credentials many operators already hold. Issue #259 asked to treat it as a first-class provider alongside `openrouter` and `ollama`. Product grilling locked a staged path:

- OpenCode is **not** an OpenAI-compatible chat-completions peer by default. Its real surface is session/prompt + SSE (or `opencode run` with its own agent loop and tools). A thin tool-less subprocess shim would silently disable RustFox tools — that path must never become the default.
- Phase 1 therefore **detects and surfaces readiness only**. It must not make OpenCode selectable as the active provider or usable for chat.
- An earlier draft of this ADR locked Phase 2 Direction **C** (OpenCode as agent / delegate). Product HOLD + grill **R3** **parks C**. The authoritative target is **peer Active + RustFox tools** (OpenCode Go DeepSeek as Active with the full RustFox tool loop). Chat-path options under that target are **D** (preferred) and **F** (interim); see Direction.
- Phase 3 picker parity remains out of scope and separate until peer Active is validated.

This document records those locks. It does not implement detect, wizard/settings chrome, peer Active chat, fallback hops, or a release tag.

## Decision

Product locks (Phase 1) are authoritative for the first implementable slice after this ADR is on `main`:

1. **Detect + register + list:** Phase 1 discovers whether `opencode` is present, registers it as a known provider type for status surfaces, and runs `opencode models` (with `--pure` — see §5) to obtain the model id list when credentials allow.

2. **Three UI states:** Settings and the setup wizard show exactly three OpenCode states: **absent** (binary not found), **no-creds** (present but no usable credentials), **ready+N** (present, creds OK, N models from `opencode models`).

3. **Not selectable / not for chat:** In Phase 1, OpenCode **must not** be selectable as the active provider and **must not** be used for chat. Status and list only. Peer Active is Direction only until D (or F interim) is locked/validated.

4. **Model id matching:** Resolve model ids with **longest-prefix match**. The later Phase 1 implementation PR **must** add a `resolve_model` regression test covering that rule (name the requirement in the impl PR; this docs change does not write the test).

5. **`--pure` always:** Phase 1 probes **always** force `--pure`. Any later OpenCode invoke / probe paths that call the binary also always force `--pure`.

6. **Typed error, no silent hop:** If chat or Active selection hits OpenCode while it is still detect-only (or while peer Active is not yet implemented / not yet validated), return a **typed error** (`detect-only` / `peer not implemented` / equivalent). **Forbid** silent fallback or hop to the default provider from that detect-only miss. (The Direction **fallback** chain — Active OpenCode Go DeepSeek → quota/402 → OpenRouter DeepSeek — is a separate, explicit peer-path behavior after Active peer ships; it is not a silent hop from detect-only.)

7. **Parked issues:** #194 / #202 stay parked; this work does not touch them.

8. **Sequencing:** this docs PR **Refs #259**. A later **Phase 1 implementation** PR lands only after this ADR is merged and the usual **four gates** pass; that impl PR **Closes #259** for the detect-only slice (or a follow-up issue if Product splits). **No release tag** in this docs PR or in the Phase 1 impl PR. Do **not** open peer Active / Phase 3 implementation from this PR until Direction D (or F interim) is locked/validated. Product **HOLD** until re-gate after R3.

## Direction

**Direction = peer Active + RustFox tools.** Target: OpenCode Go DeepSeek as Active with the full RustFox tool loop. Direction **C** (delegate / peer_invoke-like) is **parked / not the goal**.

**D is under verification; do not ship Active peer until D (or F interim) is locked/validated.** A Researcher spike on D may run in parallel; a small follow-up amend to this ADR is OK after that brief.

1. **Peer Active target:** OpenCode Go DeepSeek becomes a peer of `openrouter` / `ollama` for Active selection and chat, with RustFox owning the tool loop. Implementation follows this **peer path**, **not** a Phase 1-shaped “detect then somehow chat” shortcut, and **not** parked Direction C.

2. **Chat path — D preferred:** Prefer **D**: single-turn completion API + tools (RustFox drives the tool loop against OpenCode’s completion surface). Verify D first before any Active peer ship.

3. **Chat path — F interim if D not viable short-term:** If D is not viable in the short term, **F interim**: Active stays OpenRouter DeepSeek; OpenCode remains detect/catalog only (Phase 1 posture). F is an interim product lock, not a permanent peer.

4. **Oppose B as peer:** Do **not** treat **B** (MCP bridge / inverted loop — OpenCode owns the loop and RustFox tools are bridged in) as the peer Active path.

5. **A never default:** A tool-less prompt / subprocess shim (**A**) is **never** the default path and **must not** ship as the default. If any experimental A path exists for research, it stays non-default and out of normal Active/chat selection.

6. **Fallback (write into ADR; impl follows peer path):** After Active peer ships under D (not under Phase 1 detect-only): Active = OpenCode Go DeepSeek; on **quota / 402** → fall back to **OpenRouter DeepSeek**. This is explicit peer-path fallback, not a silent hop from detect-only errors (see Decision §6). Implementation of this fallback follows the peer path, **not** Phase 1.

7. **Sandbox (ADR-0025):** Any ship that runs OpenCode-side work under confinement **MUST** meet ADR-0025 (or equivalent). If that cannot be met for the chosen chat path, do not ship that path.

8. **Scheduler:** Until a separate decision, peer Active OpenCode for scheduled runs is **not** assumed. Prefer interactive-first; document scheduler policy in a follow-up if Product expands it.

9. **Phase 3 out of scope:** Broader picker parity polish beyond the peer Active lock is a **separate** later decision. Not part of Phase 1 or of shipping D/F.

10. **C parked:** Direction C (delegate / peer_invoke-like, interactive-only, scheduler disables delegate) from the prior revision of this ADR is **parked**. Do not implement C as the goal. Do not reopen C from this PR.

## Non-goals

- Making OpenCode selectable as Active or usable for chat in Phase 1.
- Shipping peer Active / D / F / fallback in this docs change or in the Phase 1 impl PR.
- Shipping Direction **C** (delegate) — parked; not the goal.
- Shipping Option **B** (MCP bridge / inverted loop) as the peer Active path.
- Shipping Option **A** (tool-less shim) as the default path.
- Phase 3 picker parity beyond the peer Active direction lock.
- Silent fallback / hop to the default provider when OpenCode is hit while detect-only or peer-not-implemented.
- #194 / #202.
- Implementation code, glossary edits, or a release tag in this docs change.

## Consequences

- Operators can see whether OpenCode is absent, missing creds, or ready with N models, without RustFox routing chat through it yet (Phase 1).
- Product avoids a false “provider peer” that would drop RustFox tools via a tool-less shim (A never default; B opposed as peer).
- Peer Active is gated on validating **D** (or locking **F** interim). Until then, do not ship Active peer; HOLD / re-gate after R3.
- After D ships, quota/402 on Active OpenCode Go DeepSeek falls back to OpenRouter DeepSeek along the peer path (not Phase 1).
- Active / `default_provider` semantics from ADR-0033 / ADR-0035 stay unchanged for openrouter and ollama until peer Active is validated and implemented.
- Later Phase 1 impl must include longest-prefix `resolve_model` regression coverage and typed detect-only errors (no silent hop).
- Prior Direction C text in this ADR is superseded; filename renamed to `0043-opencode-detect-and-peer.md` to match.

## Related

- ADR-0025 — sandbox confinement; dependency for confined OpenCode-side work on the peer path.
- ADR-0033 — Active vs provider model; OpenCode stays out of Active in Phase 1; peer Active is Direction until D/F locked.
- ADR-0035 — `default_provider` / Active persist; OpenCode not a peer there in Phase 1.
- #259 — tracking. This PR Refs; later Phase 1 impl closes the detect-only slice (or a Product-split follow-up).
- #194 / #202 — parked; out of scope.
