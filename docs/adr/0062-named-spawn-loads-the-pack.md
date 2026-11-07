# ADR-0062: Named spawn_agents loads the pack

- **Status:** Accepted
- **Date:** 2026-11-07
- **Issue:** #375
- **Related:** [ADR-0025](./0025-sandbox-confinement.md) (tool cap), [ADR-0050](./0050-portal-pixel-office-agent-seats.md) (seat←run). ADR-0061 is reserved for #336 provenance.

## Context

`spawn_agents` read `agent` / `name` only as a Portal seat tag and always ran the ad-hoc tool set (`read_file`, `write_file`, `list_files`, `execute_command`, or `[subagents].default_tools`). `invoke_agent` with the same name loaded the pack and its declared whitelist. A verifier spawned by name therefore received write and shell tools. The child stack was a synthetic `spawn_agents[<depth>.<index>]` frame, so the name was not on the peer stack.

## Decision

1. A `spawn_agents` task or shorthand with a resolvable `agent` or `name` uses the same resolve order as `invoke_agent` (agents → skills → bots), loads that pack, and applies its tool whitelist intersected with the parent cap (ADR-0025). `system_prompt` is not required and does not replace the pack.
2. An unknown name is an error. There is no fallback to the ad-hoc tool set.
3. Omitting `agent` and `name` keeps ad-hoc spawn: `system_prompt` is required and tools stay the ad-hoc default, still parent-capped. Ad-hoc children keep the depth-unique `spawn_agents[<depth>.<index>]` frame.
4. A named spawn uses the same depth, cycle, and stack key as `invoke_agent`. It is not a way around the peer guard.
5. Seat←run (ADR-0050) still records the real agent name for a named spawn.

## Consequences

- Parallel named tasks each keep their own whitelist.
- The `spawn_agents` schema documents `agent` and the named versus ad-hoc split. `# Available Agents` states that a spawn name loads the pack.
- Out of this decision: deleting `spawn_agents`, forcing every call through `invoke_agent`, widening the verifier whitelist, Portal seat animation, and a release tag.
