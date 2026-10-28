# Agents

Agents shows the main RustFox seat, preview subagents/skills from AgentOps, supports client-side filter via the search box, and can reload skills/agents with an on-page success hint.

## Sub-features

- `agents-main` renders the main agent row (`rustfox`, `telegram+web`).
- `agents-subagent` shows `preview-agent` from AgentOps.
- `agents-skills` shows `preview-skill` and a `Skills (N)` section.
- `agents-filter` hides rows for a non-matching query (`No agents match this filter.`).
- `agents-reload` runs Reload skills and shows `Reloaded N skills, M agents`.

## How to get to it (user POV)

- After login, open Agents in the nav.
- Deep-link `/agents` or `/agents?q=…`.

## Driving it with portal_preview harness

Preconditions:

- Doctor HEALTHY; preview AgentOps entries loaded.
- Authenticated session.

- **List.** `page.goto(BASE + '/agents')` (`e2e-verify.mjs` §5). Body includes `rustfox`, `telegram+web`, `preview-agent`, `preview-skill`.
- **Filter.** Fill `input[type=search]` with `nomatch-zzz`. Body includes `No agents match this filter.` and loses `telegram+web`. Clear the search to restore.
- **Reload.** Click role button matching `/Reload skills/i`. Body matches `/Reloaded \d+ skills, \d+ agents/`.
- **API alternate.** `GET /api/agents`, `GET /api/agents/skills`, `POST /api/agents/reload` with auth.
- **Proof.** Screenshot `pw-04-agents.png` under `$EVIDENCE/agents/` plus optional reload JSON.

## Gotchas

- Filter is client-side (TanStack search param); wait ~400ms after typing.
- Pixel office / WebGL visualize (ADR-0050) is separate from this list proof — do not require WebGL for agents-list verification.
- Full CRUD over skill/agent files is under `/skills` control-plane routes; this feature file covers the Agents status page only.
