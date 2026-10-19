# ADR-0035: `/models` Active persist + top-level `default_provider`

- **Status:** Accepted
- **Date:** 2026-10-19
- **Target release:** v1.0.7+ (does not block v1.0.6)
- **Depends on:** ADR-0033 (Active ≠ `[openrouter].model`; #223 / #225), #217 (banner shows Active / `current_model`)
- **Related:** #217 (display-only; do not reopen), `config_edit::persist_model_edit` / bak path
- **Issue:** #233

## Context

Kan on v1.0.6 (2026-10-19 ~14:09 HKT):

1. `/models` → openrouter → `deepseek/deepseek-v4.1-flash`
2. Bot: `✅ Model changed to openrouter/deepseek/deepseek-v4.1-flash`
3. `/restart`
4. Online banner: `Model: ollama/qwen3.8:27b` (reverted)

So live Active (`current_model`) changes, but boot Active does not follow the
choice across `/restart`.

**Root cause (tip `761e464` / v1.0.6):**

- `Agent::set_model` already calls `persist_model_edit`, which updates **that
  provider’s** `model` field only (matching `[[provider]]` or legacy
  `[openrouter].model`).
- Boot default Active comes from `Config::build_providers()` →
  **`providers[0].name`** (first `[[provider]]`, or legacy synthesized
  `[openrouter]` when no explicit openrouter provider). There is **no**
  disk key for “which provider is boot Active”.
- Kan’s first `[[provider]]` is `ollama`; switching Active to openrouter /
  deepseek therefore updates openrouter’s model on disk but leaves boot
  default as ollama → `/restart` restores `ollama/…`.

Related closed #217 fixed banner **display** vs Active — **not** config
persistence of boot default. ADR-0033 already locks Active ≠
`[openrouter].model` for agent invoke; this ADR locks how Active becomes the
**boot** default.

Product lock (PO on #233, 2026-10-19 HKT; Q1–Q5 A) is authoritative for the
Decision below.

## Decision

1. **Top-level key `default_provider = "<name>"` (PO name).** New optional
   config key at the root of `config.toml`. Name must match a configured
   provider (`[[provider]]` `name`, or legacy synthesized `"openrouter"`).

2. **Boot Active resolution** (`build_providers` / registry default):
   - **Key present and name exists** → that provider is boot default; Active
     model = that provider’s `model`.
   - **Key missing** → **compat**: keep today’s `providers[0]` behaviour
     (including legacy `[openrouter]` synthesized into the list).
   - **Key set but unknown name** → **fail boot** with a clear error naming
     the bad value. Do **not** silently fall back to `providers[0]`.

3. **`/models` Active change persist (Q1 A / Q5 A):** when the operator
   selects a provider + model via `/models` (or equivalent `set_model`
   path):
   - (a) Write **that** provider’s `model` (not only `[openrouter].model`
     when Active is another provider).
   - (b) Write **`default_provider`** to that provider’s name.
   - Other providers’ sections remain untouched.
   - Same slice must make boot Active survive `/restart` / cold start.

4. **Shared persist helper (Q3 A):** extend
   `config_edit::persist_model_edit` (same validate → `.bak` → atomic write
   → restore-on-fail path used by `/config set`). Portal UI for picking
   default provider is **not** required for this slice.

5. **Telegram confirmation must say saved (Q2 A / Q4 A):**
   - **Success (disk + live):** reply **must** include the word **saved**.
     Impl wording (lock):
     `✅ Model changed to \`{provider}/{model}\` (saved)`.
   - **Disk write fails:** still apply **live** in-memory Active update;
     Telegram **must** say live applied but **not saved** (no silent false
     “saved”). Impl wording (lock):
     `⚠️ Model changed to \`{provider}/{model}\` live, but not saved: {error}`.
   - Today’s `✅ Model changed to …` without “saved”, and failing the whole
     change when disk fails without live apply, do **not** meet this lock.

6. **Align with ADR-0033:** Active / `current_model` / banner remain the
   operator-facing source of truth and stay distinct from
   `[openrouter].model`. This ADR does **not** reopen #217 or change
   ADR-0032 / ADR-0033 / ADR-0034 shaping.

7. **Release:** target **v1.0.7+**. Must not block v1.0.6. GitHub English
   only. Four gates on the **impl** PR; this ADR PR is docs-only. Does
   **not** close #233 — leave open for impl.

## Non-goals

- Fallback Telegram view/edit UI (Q2 A follow-up; not this slice).
- Portal default-provider picker / Settings UI (Q3 A; not blocking).
- Reopening #217 (banner display-only work).
- Changing ADR-0032 / ADR-0033 / ADR-0034 shaping or fallback rewrite.
- Blocking v1.0.6 tagging (Kan-owned). No tag from this issue.
- Reordering `[[provider]]` arrays as the persistence mechanism (use
  `default_provider` instead of rewriting file order).

## Acceptance criteria

- Docs state: boot follows `default_provider` when set and valid; missing key
  → `providers[0]` compat; unknown name → fail boot (no silent `[0]`).
- Docs state: `/models` persist writes (a) that provider’s `model` and
  (b) `default_provider`; other providers untouched.
- Docs state: success Telegram copy includes **saved**; disk-fail path still
  updates live and tells the user it was **not saved**.
- After `/models` change + `/restart`, banner and `/models` Active match the
  chosen provider/model (impl acceptance; regression test on impl PR).
- CHANGELOG Unreleased note for this docs slice; shipping release note on
  impl.
- This docs PR does **not** close #233.


## Cross-reference (#236)

Live primary chat when `LoopConfig.model` is `None` must follow live
`current_model` (not boot-frozen registry default) — locked in **ADR-0036**.
This ADR’s boot/`default_provider` persist and Telegram saved / not-saved
copy locks are unchanged; #237 fallback UI stays out.

## Consequences

- Docs-only first (this ADR + optional `config.example.toml` comment + short
  cross-ref on ADR-0033). Tech lead assigns RustFox Rust Developer for impl
  after merge (`fix/233-default-provider` or similar). Leave #233 open for
  impl — this PR is a docs slice only.
- Operators can switch Active away from the first `[[provider]]` and keep it
  across restart without hand-editing TOML order.
- Mis-typed `default_provider` becomes a loud boot failure instead of a
  silent wrong Active — intentional for trust.
