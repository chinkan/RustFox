# ADR-0043: OpenCode detect-only (Phase 1) and peer Active direction

- **Status:** Accepted (Direction D Active peer: OpenCode Go cloud OpenAI-compat; Phase 1 detect-only remains for unconfigured peer; #267 ships D; #271 amends Go session header)
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
- An earlier draft of this ADR locked Phase 2 Direction **C** (OpenCode as agent / delegate). Product HOLD + grill **R3** **parks C**. The authoritative target is **peer Active + RustFox tools** (OpenCode Go DeepSeek as Active with the full RustFox tool loop). Chat path **D** is locked to **OpenCode Go cloud** OpenAI-compat; **F** interim has no technical blocker; see Direction.
- Phase 3 picker parity remains out of scope and separate until peer Active is validated.

This document records those locks. It does not implement detect, wizard/settings chrome, peer Active chat, fallback hops, or a release tag.

## Decision

Product locks (Phase 1) are authoritative for the first implementable slice after this ADR is on `main`:

1. **Detect + register + list:** Phase 1 discovers whether `opencode` is present, registers it as a known provider type for status surfaces, and runs `opencode models` (with `--pure` — see §5) to obtain the model id list when credentials allow.

2. **Three UI states:** Settings and the setup wizard show exactly three OpenCode states: **absent** (binary not found), **no-creds** (present but no usable credentials), **ready+N** (present, creds OK, N models from `opencode models`).

3. **Detect-only when peer not configured:** Without Go `base_url` + `api_key`, OpenCode stays detect-only (status/list; typed error on chat/Active; no silent hop). With peer configured (#267), OpenCode Go DeepSeek is selectable as Active like other peers (D = Go cloud OpenAI-compat).

4. **Model id matching:** Resolve model ids with **longest-prefix match**. The later Phase 1 implementation PR **must** add a `resolve_model` regression test covering that rule (name the requirement in the impl PR; this docs change does not write the test).

5. **`--pure` always:** Phase 1 probes **always** force `--pure`. Any later OpenCode invoke / probe paths that call the binary also always force `--pure`.

6. **Typed error, no silent hop:** If chat or Active selection hits OpenCode while the peer path is not configured (detect-only), return a **typed error** (`detect-only` / `peer not configured` / equivalent). **Forbid** silent fallback or hop to the default provider from that miss. (The Direction **leave-Go** chain — Active OpenCode Go DeepSeek → 402 or 429+GoUsageLimitError/Policy → OpenRouter DeepSeek — is a separate, explicit peer-path behavior; it is not a silent hop from detect-only.)

7. **Parked issues:** #194 / #202 stay parked; this work does not touch them.

8. **Sequencing:** this docs PR **Refs #259**. A later **Phase 1 implementation** PR lands only after this ADR is merged and the usual **four gates** pass; that impl PR **Closes #259** for the detect-only slice (or a follow-up issue if Product splits). **No release tag** in this docs PR or in the Phase 1 impl PR. Do **not** open peer Active / Phase 3 implementation from this PR until Product **re-gate** after this D lock. Product **HOLD** until re-gate (no merge/squash of this docs PR by Dev without four gates + re-gate).

## Direction

**Direction = peer Active + RustFox tools.** Target: OpenCode Go DeepSeek as Active with the full RustFox tool loop. Direction **C** (delegate / peer_invoke-like) is **parked / not the goal**.

**D is locked** (Researcher D-spike + Product TL). Active peer ships under #267 (four gates). Phase 1 detect-only remains when the peer path is not configured.

1. **Peer Active target:** OpenCode Go DeepSeek becomes a peer of `openrouter` / `ollama` for Active selection and chat, with RustFox owning the tool loop. Implementation follows this **peer path**, **not** a Phase 1-shaped “detect then somehow chat” shortcut, and **not** parked Direction C.

2. **Chat path — D locked (preferred):** **D = OpenCode Go cloud.** Completion surface: `https://opencode.ai/inference/go` with OpenAI-compatible `/openai/v1/chat/completions`. RustFox owns the tool loop against this completion surface (single-turn completion + tools). This is the authoritative peer Active chat path. **Every Go OpenAI-compat request** (chat completions and models list) **MUST** send a stable `x-opencode-session` header (#271); prefer a non-generic `User-Agent` (e.g. `RustFox/<version>`).

3. **Local `opencode serve` / `opencode run` are NOT D:** Those entry points use OpenCode’s own agent loop / tools. Do **not** treat local serve/run as the peer Active chat path.

4. **Chat path — F interim (no technical blocker):** **F interim** has **no technical blocker** (detect / `opencode models` / `--pure` already cover Phase 1 posture). If Product chooses F interim instead of shipping D: Active stays OpenRouter DeepSeek; OpenCode remains detect/catalog only. F is an interim product lock, not a permanent peer.

5. **Oppose B as peer:** Do **not** treat **B** (MCP bridge / inverted loop — OpenCode owns the loop and RustFox tools are bridged in) as the peer Active path.

6. **A never default:** A tool-less prompt / subprocess shim (**A**) is **never** the default path and **must not** ship as the default. If any experimental A path exists for research, it stays non-default and out of normal Active/chat selection.

7. **Fallback (leave-Go; peer path):** After Active peer ships under D (not under Phase 1 detect-only): Active = OpenCode Go DeepSeek; leave-Go → fall back to **OpenRouter DeepSeek** when the Go response is **HTTP 402** (balance/credits) **OR** **HTTP 429** whose body indicates Go usage-limit / policy budget (`GoUsageLimitError`, `go_usage_limit`, or Policy+budget). Do **not** auto-fallback on a plain short 429 (rate-limit / retry). **`MissingSessionID` / HTTP 400 is NOT leave-Go** — fix the session header (#271); leave remains 402 or 429+GoUsageLimitError/Policy only. This is explicit peer-path fallback, not a silent hop from detect-only errors (see Decision §6).

8. **Sandbox (ADR-0025):** Any ship that runs OpenCode-side work under confinement **MUST** meet ADR-0025 (or equivalent). If that cannot be met for the chosen chat path, do not ship that path.

9. **Scheduler:** Until a separate decision, peer Active OpenCode for scheduled runs is **not** assumed. Prefer interactive-first; document scheduler policy in a follow-up if Product expands it.

10. **Phase 3 out of scope:** Broader picker parity polish beyond the peer Active lock is a **separate** later decision. Not part of Phase 1 or of shipping D/F.

11. **C parked:** Direction C (delegate / peer_invoke-like, interactive-only, scheduler disables delegate) from the prior revision of this ADR is **parked**. Do not implement C as the goal. Do not reopen C from this PR.

12. **Open items:** Leave for follow-up:
    - Go live tools smoke: PASS (`deepseek-v4.1-flash` tool_calls) per #267 research.
    - Leave-Go triggers locked: 402 **or** 429+GoUsageLimitError/Policy (not plain short 429).
    - HK Privacy if relevant to Go cloud traffic.

## Non-goals

- Treating local `opencode serve` / `opencode run` as the Active chat path.
- Silent hop from detect-only / peer-not-configured errors into the default provider.
- Shipping Direction **C** (delegate) — parked; not the goal.
- xAI peer (#266 / #268) — parallel track; not this ADR.
- Shipping Option **B** (MCP bridge / inverted loop) as the peer Active path.
- Shipping Option **A** (tool-less shim) as the default path.
- Phase 3 picker parity beyond the peer Active direction lock.
- Silent fallback / hop to the default provider when OpenCode is hit while detect-only or peer-not-implemented.
- #194 / #202.
- Implementation code, glossary edits, or a release tag in this docs change.

## Consequences

- Operators can see whether OpenCode is absent, missing creds, or ready with N models, without RustFox routing chat through it yet (Phase 1).
- Product avoids a false “provider peer” that would drop RustFox tools via a tool-less shim (A never default; B opposed as peer).
- **D is locked** to OpenCode Go cloud OpenAI-compat (`https://opencode.ai/inference/go` + `/openai/v1/chat/completions`); local serve/run ≠ D. Do not ship Active peer until Product re-gate; HOLD. F interim has no technical blocker. Open items (Go tools smoke, quota status, HK Privacy) do not block Phase 1 docs.
- After D ships (#267), leave-Go (402 or 429+GoUsageLimitError/Policy) on Active OpenCode Go DeepSeek falls back to OpenRouter DeepSeek along the peer path (not Phase 1); plain short 429 does not leave. Go requests carry stable `x-opencode-session` (#271); MissingSessionID/400 is not leave-Go.
- Active / `default_provider` semantics from ADR-0033 / ADR-0035 stay unchanged for openrouter and ollama until peer Active is validated and implemented.
- Later Phase 1 impl must include longest-prefix `resolve_model` regression coverage and typed detect-only errors (no silent hop).
- Prior Direction C text in this ADR is superseded; filename renamed to `0043-opencode-detect-and-peer.md` to match.

## Related

- ADR-0025 — sandbox confinement; dependency for confined OpenCode-side work on the peer path.
- ADR-0033 — Active vs provider model; OpenCode stays out of Active in Phase 1; peer Active is Direction (D locked to Go cloud; ship waits re-gate).
- ADR-0035 — `default_provider` / Active persist; OpenCode not a peer there in Phase 1.
- #259 — tracking. This PR Refs; later Phase 1 impl closes the detect-only slice (or a Product-split follow-up).
- #267 — Direction D Active peer (Go OpenAI-compat). Soft Active follow-ups #269 / #271.
- #271 — Go Active must send stable `x-opencode-session`; MissingSessionID ≠ leave-Go.
- #194 / #202 — parked; out of scope.
