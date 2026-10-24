# ADR-0043: OpenCode detect-only (Phase 1) and Phase 2 C direction

- **Status:** Accepted
- **Date:** 2026-10-24
- **Target release:** post-v1.0.7 / TBD (v1.0.7 tag is Kan-owned; this ADR does not ship a tag)
- **Depends on:** ADR-0025 (sandbox confinement — required for any Phase 2 ship)
- **Related:** #259 (tracking; this docs PR Refs; later Phase 1 impl PR Closes), ADR-0033 (Active vs provider model), ADR-0035 (`default_provider` / Active persist)
- **Issue:** #259

## Context

OpenCode (`opencode`) fronts a large local model catalogue and credentials many operators already hold. Issue #259 asked to treat it as a first-class provider alongside `openrouter` and `ollama`. Product grilling locked a staged path instead:

- OpenCode is **not** an OpenAI-compatible chat-completions peer. Its real surface is session/prompt + SSE (or `opencode run` with its own agent loop and tools). A thin tool-less subprocess shim would silently disable RustFox tools — that path must never become the default.
- Phase 1 therefore **detects and surfaces readiness only**. It must not make OpenCode selectable as the active provider or usable for chat.
- Phase 2 Direction **C** treats OpenCode as an **agent / delegate** (peer_invoke-like), not as a peer of `openrouter`/`ollama`. Phase 2 is direction only in this document; it does not ship here.
- Phase 3 picker parity is out of scope and separate.

This document records those locks. It does not implement detect, wizard/settings chrome, delegate invoke, or a release tag.

## Decision

Product locks (Phase 1) are authoritative for the first implementable slice after this ADR is on `main`:

1. **Detect + register + list:** Phase 1 discovers whether `opencode` is present, registers it as a known provider type for status surfaces, and runs `opencode models` (with `--pure` — see §5) to obtain the model id list when credentials allow.

2. **Three UI states:** Settings and the setup wizard show exactly three OpenCode states: **absent** (binary not found), **no-creds** (present but no usable credentials), **ready+N** (present, creds OK, N models from `opencode models`).

3. **Not selectable / not for chat:** In Phase 1, OpenCode **must not** be selectable as the active provider and **must not** be used for chat. Status and list only.

4. **Model id matching:** Resolve model ids with **longest-prefix match**. The later Phase 1 implementation PR **must** add a `resolve_model` regression test covering that rule (name the requirement in the impl PR; this docs change does not write the test).

5. **`--pure` always:** Phase 1 probes **always** force `--pure`. Future Phase 2 Direction C invoke paths also always force `--pure`.

6. **Typed error, no silent hop:** If chat or Active selection hits OpenCode while it is still detect-only (or while Phase 2 delegate is not implemented), return a **typed error** (`detect-only` / `delegate not implemented`). **Forbid** silent fallback or hop to the default provider.

7. **Parked issues:** #194 / #202 stay parked; this work does not touch them.

8. **Sequencing:** this docs PR **Refs #259**. A later **Phase 1 implementation** PR lands only after this ADR is merged and the usual **four gates** pass; that impl PR **Closes #259** for the detect-only slice (or a follow-up issue if Product splits). **No release tag** in this docs PR or in the Phase 1 impl PR. Do **not** open Phase 2/3 implementation from this PR.

## Direction

Phase 2 is **direction only** here (Direction = **C**). It is not shipped in this ADR or in the Phase 1 impl.

1. **OpenCode = agent / delegate:** Phase 2 C treats OpenCode as an agent / delegate (peer_invoke-like), **not** as a peer provider of `openrouter` / `ollama`. Chat routing does not promote OpenCode into the same Active/`default_provider` slot as those peers until a later, separate decision (Phase 3).

2. **Option A never default:** A tool-less subprocess shim (Option A) is **never** the default path and **must not** ship as the default. If any experimental A path exists for research, it stays non-default and out of normal Active/chat selection.

3. **Sandbox required (ADR-0025):** Phase 2 **MUST** run OpenCode delegate work under the RustFox sandbox / equivalent confinement (ADR-0025). If that cannot be met, **do not ship Phase 2**.

4. **Scheduler:** Phase 2 C is **interactive-only**. The scheduler **disables** OpenCode delegate; scheduled runs must not invoke it.

5. **`--pure`:** Phase 2 C continue to always force `--pure` on OpenCode invoke (same as Phase 1 probes).

6. **Phase 3 out of scope:** Picker parity (treating OpenCode like openrouter/ollama in the model picker / Active flow) is a **separate** later decision. Not part of Phase 1 or Phase 2 C as locked here.

## Non-goals

- Making OpenCode selectable as Active or usable for chat in Phase 1.
- Shipping Phase 2 delegate / peer_invoke in this docs change or in the Phase 1 impl PR.
- Shipping Option A (tool-less subprocess) as the default path.
- Phase 3 picker parity with openrouter/ollama.
- Treating OpenCode as an OpenAI-compatible `chat_completions` peer.
- Silent fallback / hop to the default provider when OpenCode is hit while detect-only or unimplemented.
- Scheduler use of OpenCode delegate.
- #194 / #202.
- Implementation code, glossary edits, or a release tag in this docs change.

## Consequences

- Operators can see whether OpenCode is absent, missing creds, or ready with N models, without RustFox routing chat through it yet.
- Product avoids a false “provider peer” that would drop RustFox tools via a tool-less shim.
- Phase 2 work is gated on sandbox (ADR-0025) and stays interactive-only; without sandbox, Phase 2 does not ship.
- Active / `default_provider` semantics from ADR-0033 / ADR-0035 stay unchanged for openrouter and ollama until a separate Phase 3 decision.
- Later Phase 1 impl must include longest-prefix `resolve_model` regression coverage and typed detect-only errors.

## Related

- ADR-0025 — sandbox confinement; hard dependency for any Phase 2 ship.
- ADR-0033 — Active vs provider model; OpenCode stays out of Active in Phase 1.
- ADR-0035 — `default_provider` / Active persist; OpenCode not a peer there in Phase 1 / Direction C.
- #259 — tracking. This PR Refs; later Phase 1 impl closes the detect-only slice (or a Product-split follow-up).
- #194 / #202 — parked; out of scope.
