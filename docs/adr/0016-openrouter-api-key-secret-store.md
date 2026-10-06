# ADR 0016: OpenRouter API key lives in SecretStore

## Status
Accepted

## Date
2026-10-05

## Context
BotFather tokens already use `secret:NAME` in config and SecretStore (seal on wizard write, startup migrate+scrub, `set_verified`). `[openrouter].api_key` is still a plain string on disk. Kan’s `config.toml` had a plaintext OpenRouter key. Runtime does not resolve `secret:` for that field yet. Product locked sealing it the same way as bot tokens (2026-10-05). Implementation order after Bug B (ADR 0015), before typed model ids (ADR 0017).

## Decision
- Canonical vault name: `openrouter.api_key`. Config holds only `api_key = "secret:openrouter.api_key"`.
- Wizard / `--setup` seal: write the key with `set_verified` (or shared helper with bot tokens), then write the secret ref — never plaintext in `config.toml`.
- Startup migrate+scrub: if `[openrouter].api_key` is plaintext, store it (`set_verified`), replace with the secret ref, persist via the shared config write path (`.bak` safety net), matching bot-token migrate.
- Resolve the ref wherever `[openrouter].api_key` is read for LLM calls (including the legacy provider copy from that field). Reuse / generalize `parse_secret_ref` and seal helpers from `secret_store/bot_token.rs` rather than a one-off path.
- **Out of scope this ADR:** `[embedding].api_key` and other `[[provider]].api_key` values stay as they are (may remain plaintext). Document that deliberately; a later requirement can seal them.
- GUIDE / example config: show the secret ref, never echo key material in `/config` or chat.

## Consequences
After upgrade, existing plaintext OpenRouter keys move into the vault (Linux file vault per ADR/Bug A; macOS/Windows keyring-first). A `secret:` string in config before this ships does nothing until resolve exists — ship seal+resolve together. Rotating a key that was pasted in chat remains the operator’s job outside the product.
