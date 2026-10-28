# Memory

Memory lets a user browse seeded knowledge, run hybrid search (FTS fallback in preview), filter by kind (conversation vs fact), and see a clear empty state when nothing matches.

## Sub-features

- `memory-browse` renders seeded knowledge on first load (`The Death of Portia`, Telegram AI assistant blurb).
- `memory-search` finds fact + conversation hits for a query like `death`.
- `memory-kind-filter` narrows to conversation-only results.
- `memory-empty` shows `Nothing matched` for a nonsense query.

## How to get to it (user POV)

- After login, open Memory in the nav.
- Deep-link `/memory` or `/memory?q=…`.

## Driving it with portal_preview harness

Preconditions:

- Doctor HEALTHY; preview fixtures loaded (default `portal_preview` seed).
- Authenticated session.

- **Browse.** `page.goto(BASE + '/memory')` (`e2e-verify.mjs` §6). Body includes `The Death of Portia` and `Telegram AI assistant`; no `internal_error`.
- **Search.** Fill `input[type=search]` with `death`. Body shows `fact:` and `[user]` hits including Portia.
- **Kind filter.** Click role button `Conversation` (exact). Conversation hits remain; fact row `favourite_author` is gone.
- **Empty.** Search `zzzznope`. Body includes `Nothing matched`.
- **API alternate.** `GET /api/memory/search?q=death` with auth → JSON array with scored hits (`docs/portal-api.md`).
- **Proof.** Screenshot `pw-05-memory.png` under `$EVIDENCE/memory/` and/or save the search JSON.

## Gotchas

- Preview has no embeddings — hybrid search falls back to FTS5; do not assert vector ranking.
- Empty `q` returns recent entries; that is browse mode, not a failed search.
- Wait ~900ms after search/filter input for debounce (as in `e2e-verify.mjs`).
