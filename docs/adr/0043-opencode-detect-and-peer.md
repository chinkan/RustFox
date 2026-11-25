# ADR-0043: OpenCode detect-only (Phase 1) and peer Active direction

- **Status:** Accepted (Direction D Active peer: OpenCode Go cloud OpenAI-compat; Phase 1 detect-only remains for unconfigured peer; #267 ships D; #271 amends Go session header; #277 / #279 ship continuity SoT + anti cold-intro; #280 / #282 ship matcher widen W1 ∪ W2; #283 amends W2 cold-intro phrase widen)
- **Date:** 2026-10-24
- **Amended:** 2026-11-25 (#438, Asia/Hong_Kong) — fallback full work parity; #433 residual (a configured `openrouter/…` chain entry is not a silent skip)
- **Amends / supersedes:** prior Direction **C** (delegate) on this same ADR (commit `9bdceb1` / filename `0043-opencode-detect-and-delegate.md`). Product HOLD + grill R3 parks C; target is peer Active + RustFox tools.
- **Target release:** post-v1.0.7 / TBD (v1.0.7 tag is Kan-owned; this ADR does not ship a tag)
- **Depends on:** ADR-0025 (sandbox confinement — required for any peer Active / tool-loop ship that runs OpenCode-side work under confinement)
- **Related:** #259 (Phase 1 detect-only), #267 (D Active peer), #271 / #272 (`x-opencode-session`), #277 / #279 (continuity SoT + anti cold-intro shipped), #280 / #282 (matcher widen W1 ∪ W2 shipped), #283 (W2+ phrase widen; this docs PR Refs; later impl PR Closes), ADR-0033 (Active vs provider model), ADR-0035 (`default_provider` / Active persist)
- **Issue:** #259 (Phase 1); #277 / #279 (anti cold-intro shipped); #280 / #282 (matcher widen shipped); #283 (W2 phrase widen amend); #438 (fallback full work parity — this docs amend is Related, not Closes)

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

## Decision — Continuity and anti cold-intro (#277)

Product locks for Active-peer continuity (grill / tip `d50eae8`). Amend in place; Phase 1 Decision above and leave-Go / MissingSessionID (#271) are unchanged. Docs PR #278 **Refs #277**; impl #279 **Closes #277**. Matcher shape further amended by #280 / #283 below.

1. **Continuity source of truth:** RustFox **SQLite → `messages[]`** via shared `ConversationManager` (OpenRouter parity). Applies to **all Active peers** (not OpenCode-only). Continuity is not derived from `x-opencode-session`.

2. **`x-opencode-session` role:** Go opaque API requirement only; **not** the transcript. #272 process-stable behavior stands. Per-conversation `(platform, bot_id, user_id)` persist-across-restart remains an **explicit follow-up** (out of this slice’s MUST).

3. **Anti cold-intro (MUST):** when continuity already has **≥1 prior user or assistant** turn (excluding pure system / summary markers), the final user-visible reply **must not** be a capability cold-intro (greeting + capability menu / “I can help you”-style, including the known Cantonese Fox intro pattern).

4. **Mechanism:** light prompt when history is non-empty, plus a **structural final-reply guard** on the **shared Active final-reply path** (all Active peers, not OpenCode-only). Prefer false-negatives over false-positives that kill normal short replies. On match → reject and **regenerate once**; if still cold-intro → short “continue prior thread / please clarify” sentence — **no** capability menu. Matcher shape = **W1 ∪ W2** per #280 / #283 (below); the #277-era help-offer AND is superseded.

5. **Unchanged / parked:** Leave-Go and MissingSessionID rules unchanged (#271). #194 / #202 stay parked. **No release tag** in this docs PR or the follow-on impl PR without owner.

## Decision — Anti cold-intro matcher widen (#280)

Product locks after live false-negative on tip `29e4461` (grill / #280). Amend in place; #277 continuity SoT, mechanism shape (light prompt + shared Active final-reply guard; regen once; else clarify, no menu), and leave-Go / MissingSessionID (#271) are unchanged. Docs PR #281 **Refs #280**; impl #282 **Closes #280**. W2 phrase set further amended by #283 below.

1. **Matcher = W1 ∪ W2** when prior continuity has ≥1 user/assistant turn (excluding pure system / summary markers):
   - **W1:** greeting + ≥2 menu lines — help-offer (“I can help you” / 「我可以幫你」) is **no longer required**.
   - **W2:** explicit lost-context / cannot-see-prior language (locale-aware, e.g. 斷 context、見唔到、lost context) + ≥2 menu lines.

2. **F1:** still prefer false-negatives over false-positives. Do **not** loosen greeting or menu-line definitions; only drop the help-offer AND for W1, and add the W2 class.

3. **C1:** lost-context alone (no ≥2 menu lines) is **not** a structural reject — avoid killing normal “I can’t see file X” replies.

4. **Mechanism unchanged:** light prompt when history is non-empty + **structural final-reply guard** on the **shared Active final-reply path** (all Active peers). On match → reject and **regenerate once**; if still cold-intro → short clarify rewrite — **no** capability menu. Soft #279 proved the reject→regen→clarify path when the matcher hits; the tip `29e4461` live miss is matcher coverage.

5. **Parked (explicit):** Leave-Go, MissingSessionID, per-conversation `x-opencode-session` persist-across-restart, mid-tool draft persist, long-turn focus loss, #194 / #202, and any release tag — out of this amend’s MUST.

## Decision — Anti cold-intro matcher W2+ phrase widen (#283)

Product locks after live false-negative on tip `9f58afb` (grill / #283). Amend in place; #277 continuity SoT, #280 W1 ∪ W2 matcher shape (except the W2 phrase set), mechanism (light prompt + shared Active final-reply guard; regen once; else clarify, no menu), and leave-Go / MissingSessionID (#271) are unchanged. Soft #282 @ `9f58afb`: **ACCEPT** claimed W1 ∪ W2; **HOLD** vs this live FN class. This docs PR **Refs #283**; a later **implementation** PR **Closes #283**.

1. **Live FN class @ tip `9f58afb`:** Cantonese lost-content claim + capability menu (e.g. 「我好似收唔到你今次嘅內容 — 只見到系統 context…」+ multi-option menu) does **not** hit current W1 (no greeting signal) or the tip W2 CJK list (斷 context / 見唔到 / 睇唔到先前 / 冇咗 context / …). Soft prove on match still ends on English `COLD_INTRO_CLARIFY_REWRITE` (single clarify sentence, no menu). Therefore this is a **new matcher coverage gap**, not a successful clarify path and not Continuity SoT failure.

2. **W2+ phrase widen (matcher-only):**
   - **P1 CJK:** `收唔到`、`收唔到你今次`、`只見到系統 context`、`只見到系統`、`今次嘅內容`、`冇見到`（≠ `見唔到`）。
   - **E1 EN:** `didn't get your`、`only see system context`、`only seeing system`.
   - **M1 / C1:** new phrase **+ ≥2 menu lines** → reject; phrase alone ≠ reject (avoid killing normal lost-content replies without a capability menu).
   - **F1:** still prefer false-negatives over false-positives; do **not** loosen greeting or menu-line definitions; **W1 unchanged**.

3. **Soft Continuity SoT unchanged:** SQLite → `messages[]` via `ConversationManager` (all Active peers). Continuity is not derived from `x-opencode-session`.

4. **Mechanism unchanged:** light prompt when history is non-empty + **structural final-reply guard** on the **shared Active final-reply path** (all Active peers). On match → reject and **regenerate once**; if still cold-intro → short clarify rewrite — **no** capability menu.

5. **Out of scope / park (explicit):** `agent.max_iterations`, OpenCode `context_window` / prepare compaction, history inject, session persist, Leave-Go, MissingSessionID, mid-tool draft persist, long-turn focus loss, #194 / #202, and any release tag — out of this amend’s MUST. Soft QA/QA2 only after later impl gates; never the operator main Telegram identity / home config.

## Decision — Fallback full work parity (#438)

Product lock (2026-11-25, Asia/Hong_Kong). **P1 / v1.0.10 Must.** Amend in place. Related to #438. This docs slice does not implement runtime, tests, or config behavior, and does not merge or tag. The same lock is ADR-0029 Amendment (#438). Direction §7 leave-Go triggers stay: plain short 429 does not leave; HTTP 402 or a quota-class 429 does. This decision locks reachability and work parity on that walk, and the #433 residual below.

This supersedes the #218 / #220 omit-tools-first fallback lock in ADR-0029. It does not reopen 401/403 hopping.

1. **Full work parity** between main and fallback for a turn: same tools (MCP, schedule, sandbox, spawn), same system/soul, same memory/history, same tool-loop, first turn and mid-loop, main chat and agent/subagent invoke.

2. **OpenCode→OpenRouter** (and any other configured chain entry) must be reachable when that backup is configured: the provider must be registered; `secret:` / Portal-saved keys resolve on both `[openrouter]` and `[[provider]] type=openrouter`; a successful leave-Go / 402 / quota walk sends tools, not text-only. If the backup is missing, fail in an operator-visible way that names it; no silent skip. A literal `secret:` value must never be sent as Bearer. Close secret resolve for every `[[provider]]` type that documents `secret:` keys, not only xAI and the legacy `[openrouter]` table.

3. **Local/Ollama fallback:** try the same work first (keep tools; guarantee a plain user message). Rewrite+omit is recovery only, after keep-tools fails with no user query found (or equivalent), or the model has no tool interface. After that keep-tools 500: one ADR-0034-style omit-retry on the same fallback model, then the next chain entry.

4. **Plain OpenCode 429:** keep leave-Go (stay on Go, ADR-0009 retry). Walk only on 402 or quota-class 429 (`GoUsageLimitError` / policy+budget).

5. **Vision/audio parts the fallback cannot accept:** skip that chain entry; if none can, fail clearly. Do not silently strip and answer as text.

**Out of scope**

- Models that cannot call tools pretending to
- Equal quality, speed, context, or `max_tokens` (use each provider’s cap; do not shrink tools to fit)
- Voice STT/TTS cloud fallback (#349)
- Embeddings, image-gen, or OpenRouter `models:[]` routing
- 401/403 hop on a correctly resolved bad key
- Changing Silent / tool-UI policy
- mDNS, tag, or flash

**Regression:** Detect-only OpenCode / misconfigured xAI still do not silent-hop. Decision §6 and Direction item 7’s “not a silent hop from detect-only” stay.

**Tests** (acceptance for the implementation issue; this docs PR does not write them):

- Replace the `fallback_ollama_first_turn_with_tools_rewrites_and_omits_tools` omit-first lock.
- Add leave-Go → registered OpenRouter with tools.
- Add secret-ref resolve on `type=openrouter`.

### Residual on tip `af0a8161` (#433 and call-time skip)

Independently verified on live main `af0a81617f0931a300beb8a61e90695092e462ce` (parent `cff2878e`, after #433). These are the gaps this lock closes. They are not extra scope.

1. After #433, an OpenCode-Active config with empty or absent `[openrouter]` does not synthesize an `openrouter` provider. A chain entry such as `openrouter/deepseek/deepseek-chat` is skipped at call time; boot only warns (`[fallback] chain entry … names unknown provider … it will be skipped at call time`). **Optional or empty `[openrouter]` must not silently skip a configured `openrouter/…` chain entry at call time.** When that backup is configured, the provider is registered and the walk can send tools. A missing backup is an operator-visible failure that names it. #432 / #433 still stand for boot: a config with no `[openrouter]` table parses, and Active OpenRouter with no key fails naming `[openrouter].api_key`. This amend does not require an unusable empty-key OpenRouter row when no chain entry names `openrouter`.

2. `resolve_openrouter_api_key` only rewrites legacy `[openrouter].api_key`. `resolve_xai_api_keys` covers only `type=xai`. `[[provider]] type=openrouter` `secret:` is not resolved. An unresolved `secret:` can be sent as Bearer and typically 401s, which stops the rest of the chain. A literal `secret:` must never be sent as Bearer.

3. `src/llm.rs` around lines 911–923 is still the #218 omit-tools-first path, including first turn with a plain user query, locked by `fallback_ollama_first_turn_with_tools_rewrites_and_omits_tools`.

## Direction

**Direction = peer Active + RustFox tools.** Target: OpenCode Go DeepSeek as Active with the full RustFox tool loop. Direction **C** (delegate / peer_invoke-like) is **parked / not the goal**.

**D is locked** (Researcher D-spike + Product TL). Active peer ships under #267 (four gates). Phase 1 detect-only remains when the peer path is not configured.

1. **Peer Active target:** OpenCode Go DeepSeek becomes a peer of `openrouter` / `ollama` for Active selection and chat, with RustFox owning the tool loop. Implementation follows this **peer path**, **not** a Phase 1-shaped “detect then somehow chat” shortcut, and **not** parked Direction C.

2. **Chat path — D locked (preferred):** **D = OpenCode Go cloud.** Completion surface: `https://opencode.ai/inference/go` with OpenAI-compatible `/openai/v1/chat/completions`. RustFox owns the tool loop against this completion surface (single-turn completion + tools). This is the authoritative peer Active chat path. **Every Go OpenAI-compat request** (chat completions and models list) **MUST** send a stable `x-opencode-session` header (#271 / #272); prefer a non-generic `User-Agent` (e.g. `RustFox/<version>`). That header is a **Go opaque API requirement**, **not** the conversation transcript store (#277). Process-stable mint (#272) stands. **Per-conversation** session keyed by `(platform, bot_id, user_id)` persist-across-restart is an **explicit follow-up**, **not** a MUST of the #277 docs slice.

3. **Local `opencode serve` / `opencode run` are NOT D:** Those entry points use OpenCode’s own agent loop / tools. Do **not** treat local serve/run as the peer Active chat path.

4. **Chat path — F interim (no technical blocker):** **F interim** has **no technical blocker** (detect / `opencode models` / `--pure` already cover Phase 1 posture). If Product chooses F interim instead of shipping D: Active stays OpenRouter DeepSeek; OpenCode remains detect/catalog only. F is an interim product lock, not a permanent peer.

5. **Oppose B as peer:** Do **not** treat **B** (MCP bridge / inverted loop — OpenCode owns the loop and RustFox tools are bridged in) as the peer Active path.

6. **A never default:** A tool-less prompt / subprocess shim (**A**) is **never** the default path and **must not** ship as the default. If any experimental A path exists for research, it stays non-default and out of normal Active/chat selection.

7. **Fallback (leave-Go; peer path):** After Active peer ships under D (not under Phase 1 detect-only): Active = OpenCode Go DeepSeek; leave-Go → fall back to **OpenRouter DeepSeek** when the Go response is **HTTP 402** (balance/credits) **OR** **HTTP 429** whose body indicates Go usage-limit / policy budget (`GoUsageLimitError`, `go_usage_limit`, or Policy+budget). Do **not** auto-fallback on a plain short 429 (rate-limit / retry). **`MissingSessionID` / HTTP 400 is NOT leave-Go** — fix the session header (#271); leave remains 402 or 429+GoUsageLimitError/Policy only. This is explicit peer-path fallback, not a silent hop from detect-only errors (see Decision §6). **Amended 2026-11-25 (#438):** a successful leave-Go / 402 / quota walk sends the same tools as the primary turn (full work parity, ADR-0029 Amendment #438). Optional or empty `[openrouter]` must not silently skip a configured `openrouter/…` entry. Plain OpenCode 429 stays on Go (ADR-0009 retry). See Decision — Fallback full work parity (#438).

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
- **#277 docs slice:** not a conversation-id / history-miss bugfix (continuity SoT is already SQLite → `messages[]`); no leave-Go / MissingSessionID change; no release tag; per-conversation `x-opencode-session` persist-across-restart is a follow-up, not a MUST of that amend.
- **#280 docs slice:** tip `29e4461` live FN = matcher coverage gap (not a clarify-rewrite or continuity-miss failure); soft #279 reject→regen→clarify path remains valid when the matcher hits; no leave-Go / MissingSessionID / per-conversation `x-opencode-session` persist / mid-tool draft persist / long-turn focus loss; no release tag; #194 / #202 stay parked. No Rust, tests, or CHANGELOG in this docs PR.
- **#283 docs slice:** tip `9f58afb` live FN = W2 phrase coverage gap (Cantonese lost-content + menu; not English `COLD_INTRO_CLARIFY_REWRITE`); Soft #282 ACCEPT claimed W1 ∪ W2 / HOLD vs this class; no `max_iterations` / `context_window` / compaction / history inject / session persist / Leave-Go / MissingSessionID / mid-tool draft / long-turn focus; no release tag; #194 / #202 stay parked. No Rust, tests, or CHANGELOG in this docs PR.

## Consequences

- Operators can see whether OpenCode is absent, missing creds, or ready with N models, without RustFox routing chat through it yet (Phase 1).
- Product avoids a false “provider peer” that would drop RustFox tools via a tool-less shim (A never default; B opposed as peer).
- **D is locked** to OpenCode Go cloud OpenAI-compat (`https://opencode.ai/inference/go` + `/openai/v1/chat/completions`); local serve/run ≠ D. Do not ship Active peer until Product re-gate; HOLD. F interim has no technical blocker. Open items (Go tools smoke, quota status, HK Privacy) do not block Phase 1 docs.
- After D ships (#267), leave-Go (402 or 429+GoUsageLimitError/Policy) on Active OpenCode Go DeepSeek falls back to OpenRouter DeepSeek along the peer path (not Phase 1); plain short 429 does not leave. Go requests carry stable `x-opencode-session` (#271 / #272); MissingSessionID/400 is not leave-Go. The session header is opaque Go API state, not the transcript (#277). **Amended 2026-11-25 (#438):** that walk has full work parity (tools, not text-only) and a configured OpenRouter backup is registered, including when `[openrouter]` is optional or empty. A missing backup is named. See Decision — Fallback full work parity (#438).
- Continuity for **all Active peers** is SQLite → `messages[]` via `ConversationManager` (OpenRouter parity). Anti cold-intro: with ≥1 prior user/assistant turn in continuity, the final reply must not be a capability cold-intro; mechanism = light prompt + structural final-reply guard (regenerate once; else short clarify, no menu) on the shared Active path (#277 / #279). Matcher = **W1 ∪ W2** (#280 / #282 / #283): greeting + ≥2 menu (help-offer not required) **or** lost-context signal + ≥2 menu; W2 phrase set widened under #283 (P1 CJK incl. `冇見到` ≠ `見唔到`; E1 EN); prefer FN (F1); lost-context alone ≠ reject (C1); W1 unchanged. Tip `9f58afb` live FN = W2 phrase coverage (not clarify rewrite / Continuity SoT). Soft #282 ACCEPT claimed W1 ∪ W2; HOLD vs that class. MissingSessionID / per-conversation session persist / mid-tool draft / long-turn / max_iterations / context_window / #194 / #202 / tag remain parked for that matcher slice. Leave-Go work parity is Amendment (#438).
- Active / `default_provider` semantics from ADR-0033 / ADR-0035 stay unchanged for openrouter and ollama until peer Active is validated and implemented.
- Later Phase 1 impl must include longest-prefix `resolve_model` regression coverage and typed detect-only errors (no silent hop).
- Prior Direction C text in this ADR is superseded; filename renamed to `0043-opencode-detect-and-peer.md` to match.

## Related

- ADR-0025 — sandbox confinement; dependency for confined OpenCode-side work on the peer path.
- ADR-0033 — Active vs provider model; OpenCode stays out of Active in Phase 1; peer Active is Direction (D locked to Go cloud; ship waits re-gate).
- ADR-0035 — `default_provider` / Active persist; OpenCode not a peer there in Phase 1.
- #259 — tracking. This PR Refs; later Phase 1 impl closes the detect-only slice (or a Product-split follow-up).
- #267 — Direction D Active peer (Go OpenAI-compat). Soft Active follow-ups #269 / #271.
- #271 / #272 — Go Active must send stable `x-opencode-session`; MissingSessionID ≠ leave-Go; process-stable mint stands.
- #277 / #279 — Continuity SoT (SQLite → `messages[]`) + anti cold-intro MUST for all Active peers (docs #278 Refs; impl #279 Closes). Per-conversation session persist = follow-up.
- #280 / #282 — Anti cold-intro matcher widen (W1 ∪ W2; F1; C1) shipped (docs #281 Refs; impl #282 Closes). Tip `29e4461` live FN was coverage gap.
- #283 — W2+ phrase widen (P1 CJK incl. `冇見到`; E1 EN; M1/C1; F1; W1 unchanged); this docs PR Refs; later impl Closes. Tip `9f58afb` live FN = phrase coverage gap.
- #438 — fallback full work parity (2026-11-25). This docs amend is Related. The implementation issue is separate. ADR-0029 Amendment (#438) is the same lock.
- #432 / #433 — `[openrouter]` optional at boot when another provider is Active. Residual: a configured `openrouter/…` chain entry is not a silent skip at call time.
- #194 / #202 — parked; out of scope.
