# Parallel Ad-Hoc Subagents — Design

**Date:** 2026-06-11
**Status:** Updated — v2 (added zero-trust verifier)

## Problem

The `invoke_agent` tool requires a predefined agent file (`agents/<name>/AGENT.md`) with frontmatter declaring `model` and `tools`. There is no way for the LLM to:

1. **Spawn an ad-hoc subagent** with an inline system prompt — no predefined file needed.
2. **Run multiple subagents in parallel** — all tool calls execute sequentially, `invoke_agent` blocks the main loop.
3. **Give subagents system context** (date/time, user model) without manually passing it in `prompt`.

The error `Failed to read directory: /home/kan/.rustfox/workspace/agents` occurred because the LLM tried to `list_files` in the sandbox to find agents — it had no way to create ad-hoc agents inline.

## Goals

1. **Ad-hoc subagents**: LLM can spawn a subagent with inline `system_prompt` + `prompt`, no AGENT.md required.
2. **Parallel execution**: Multiple subagent calls from one LLM response run concurrently.
3. **System context injection**: Every subagent automatically gets date/time, user model, and other ambient context.
4. **Backward compatibility**: Existing `invoke_agent(agent="name", prompt="...")` continues to work unchanged.
5. **Shared plans + memory**: Subagents can opt into shared `plan_view`/`plan_update` and `recall`/`search_memory` tools via whitelist or config.

## Non-Goals

- Forking conversation history into subagents (they stay isolated by default).
- Durable background subagents that outlive the parent turn.
- Nested subagent spawning (subagents cannot spawn further subagents).
- Changing the SKILL.md / AGENT.md file format.

## Design

### Tool: `spawn_agents`

New built-in tool (primary interface for ad-hoc + parallel):

```json
{
  "name": "spawn_agents",
  "description": "Spawn one or more isolated subagents. Each gets its own agentic loop. System context (date/time, user model) is auto-injected. When multiple tasks are provided, they run concurrently.",
  "parameters": {
    "type": "object",
    "properties": {
      "tasks": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "system_prompt": {
              "type": "string",
              "description": "Instructions for this subagent — its role, constraints, and behavior"
            },
            "prompt": {
              "type": "string",
              "description": "The task to execute"
            },
            "model": {
              "type": "string",
              "description": "Optional model override (e.g. 'google/gemini-flash-2.0' for cheap tasks)"
            },
            "tools": {
              "type": "array",
              "items": { "type": "string" },
              "description": "Optional tool whitelist. Default: built-in tools only."
            }
          },
          "required": ["system_prompt", "prompt"]
        }
      },
      "system_prompt": {
        "type": "string",
        "description": "Shorthand: system prompt for a single subagent (use instead of tasks for one)"
      },
      "prompt": {
        "type": "string",
        "description": "Shorthand: task for a single subagent"
      },
      "model": {
        "type": "string",
        "description": "Shorthand: model override for a single subagent"
      },
      "tools": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Shorthand: tool whitelist for a single subagent"
      }
    }
  }
}
```

### Ad-hoc subagent flow

When a single `{system_prompt, prompt}` is provided (no `tasks` array), it runs one subagent and returns its result as a plain string. When `tasks: [{...}, {...}]` is provided, all tasks run concurrently via `tokio::join_all` and results are returned as a structured JSON array:

```json
{
  "results": [
    "Task 0 completed: summarized file X...",
    "Task 1 completed: reviewed file Y..."
  ]
}
```

Results are ordered by task index regardless of completion order. Each entry is the subagent's final text response. Errors (e.g., subagent hit max iterations) are returned as error strings within the results array.

**Precedence:** If `tasks` is provided, shorthand fields (`system_prompt`, `prompt`, `model`, `tools` at the top level) are ignored. If neither `tasks` nor `system_prompt`+`prompt` are provided, the tool returns an error.

### System context injection

Every subagent's system message is constructed as:

```
[System Context]
Current date and time: 2026-06-11 14:30 UTC
User location: Asia/Hong_Kong
User model:
<user_model>
...user preferences, name, etc...
</user_model>

[Agent Instructions]
<system_prompt from tool call>
```

This is the same `build_system_prompt()` context minus skills/agent listings. The subagent gets ambient knowledge without the main agent having to manually include it.

### Changes to `run_subagent`

```rust
async fn run_subagent(
    &self,
    skill_name: Option<&str>,       // None for ad-hoc
    system_prompt: &str,            // inline instructions for ad-hoc
    user_prompt: &str,              // the task
    model_override: Option<&str>,
    tools_override: Option<Vec<String>>,
) -> String
```

**When `skill_name` is None (ad-hoc):**
- No registry lookup needed
- No `read_skill_file`/`read_agent_file` bootstrap step
- System message = system context + user's `system_prompt`
- Tool whitelist from `tools_override` (default: `["read_file", "write_file", "list_files", "execute_command"]` — sandbox tools only; `read_skill_file`/`read_agent_file` are excluded because there is no skill/agent file to read)
- Model from `model_override` or global default

**When `skill_name` is Some (predefined):**
- Existing behavior: registry lookup, model/tools from frontmatter
- Bootstrap reads AGENT.md or SKILL.md (unless `skip_bootstrap: true` in frontmatter)
- If `skip_bootstrap: true`, the AGENT.md body is used as the system message directly, skipping the "read your instructions" bootstrap step
- `system_prompt` can override the bootstrap message

**New frontmatter field:** `skip_bootstrap: bool` — when true, the AGENT.md/SKILL.md body content is injected directly as the system message. No "read your instructions" bootstrap step. Useful for simple evaluator agents like the verifier.

The `AgentKind` enum is kept for predefined agents (registry lookup for skills vs agents). For ad-hoc mode, the `skill_name: None` path bypasses the registry entirely.

### Parallel execution of subagent calls

When the LLM makes multiple `spawn_agents` or `invoke_agent` calls in one response, they run concurrently. Other tool calls (file I/O, plan tools) remain sequential to avoid races on shared state. `invoke_subagent` was removed — use `invoke_agent` for all predefined agent/skill invocations.

```rust
// Identify subagent-spawning tool calls (slowest ops, benefit most from parallel)
let is_agent_tool = |name: &str| -> bool {
    matches!(name, "spawn_agents" | "invoke_agent")
};

let (agent_indices, other_indices): (Vec<_>, Vec<_>) = tool_calls.iter().enumerate()
    .partition(|(_, tc)| is_agent_tool(&tc.function.name));

// Run subagent calls in parallel
let mut agent_results: Vec<(usize, String)> = Vec::new();
if !agent_indices.is_empty() {
    let futs: Vec<_> = agent_indices.iter().map(|(i, tc)| {
        let args = serde_json::from_str(&tc.function.arguments)
            .unwrap_or_default();
        async move {
            let result = self.execute_tool(&tc.function.name, &args, user_id, chat_id).await;
            (*i, result)
        }
    }).collect();
    agent_results = futures::future::join_all(futs).await;
}

// Non-agent tool calls run sequentially
let mut other_results: Vec<(usize, String)> = Vec::new();
for (i, tool_call) in &other_indices {
    let args = serde_json::from_str(&tool_call.function.arguments)
        .unwrap_or_default();
    let result = self.execute_tool(&tool_call.function.name, &args, user_id, chat_id).await;
    other_results.push((*i, result));
}

// Merge results in original order
let all_results: Vec<String> = [agent_results, other_results]
    .concat()
    .sorted_by_key(|(i, _)| *i)
    .map(|(_, r)| r)
    .collect();
```

Sequential non-agent calls prevent races on `.rustfox_plan.json` and other shared state. Subagent calls are the primary performance bottleneck (each runs a full LLM mini-loop) and benefit most from parallelization.

**Observability note:** LangSmith run tracking and tool event notifications (`tool_event_tx`) must be preserved around each tool call in both the parallel and sequential paths. Each subagent call gets its own LangSmith trace as a child of the current chain run.

### Predefined Verifier Agent: `agents/verifier/AGENT.md`

The verifier is implemented as a predefined agent — **no new tool needed**. It follows the existing `invoke_agent(agent="verifier", prompt="...")` pattern.

This approach is based on Anthropic's **Evaluator-Optimizer** pattern: one agent generates, another evaluates in a loop until PASS. The same pattern is used by LangGraph's evaluator-optimizer workflow with conditional routing back to the generator on rejection.

#### AGENT.md

```yaml
---
name: verifier
description: Zero-trust verifier. Evaluates work output against criteria. Use via: invoke_agent(agent="verifier", prompt="TASK: ...\\nCRITERIA: ...\\nEVIDENCE: ...")
tools:
  - read_file
  - list_files
  - plan_view
skip_bootstrap: true
---
You are a ZERO-TRUST VERIFIER. Your sole purpose is to critically evaluate
work output against strict criteria. You have NO incentive to approve bad work.

The main agent has done work and is asking you to verify it. You do NOT have
access to the agent's conversation history or reasoning — judge only the
evidence presented. You have READ-ONLY sandbox access: use `read_file` and
`list_files` to inspect actual output. Do NOT trust summaries — verify the
real files.

Your input will be in this format:
```
TASK: <original task description>
CRITERIA: <acceptance criteria>
EVIDENCE: <brief summary of what was done and key file paths>
```

Workflow:
1. Read the task and criteria from the input above
2. Use `read_file` to inspect files the worker created or modified
3. Use `list_files` to see what exists in the sandbox
4. Use `plan_view` to check plan state
5. Evaluate based on ACTUAL file contents, not the summary

Evaluate based on:
1. **COMPLETENESS**: Are ALL required files created? Are all requirements addressed?
2. **CORRECTNESS**: Read the actual files. Any errors, bugs, or hallucinations?
3. **CRITERIA FIT**: Does the implementation meet EVERY acceptance criterion?

Respond with exactly this structured format:

<evaluation>PASS, NEEDS_IMPROVEMENT, or FAIL</evaluation>
<feedback>
Be specific about what needs to improve and why. Reference specific files/lines.
For PASS, leave feedback empty.
</feedback>
```

**Three-tier outcome** (from Anthropic cookbook):
- **PASS** — all criteria met, work accepted
- **NEEDS_IMPROVEMENT** — work is on the right track but needs specific fixes
- **FAIL** — work is fundamentally wrong or incomplete

The three-tier system gives the worker more actionable feedback than a binary pass/fail. `NEEDS_IMPROVEMENT` guides refinement, while `FAIL` signals a restart is needed.

#### For complicated tasks

A single verifier works when criteria are comprehensive. For multi-faceted tasks, the main agent can:
1. Call `invoke_agent(agent="verifier", prompt="TASK: refactor auth module...\nCRITERIA: correctness, security\nEVIDENCE: ...")`
2. If rejected, iterate
3. For separate concerns (e.g., correctness + docs + performance), create specialized verifier agents: `agents/verifier-code/`, `agents/verifier-security/`, `agents/verifier-docs/`
4. Call them in sequence, requiring all to pass

#### System prompt update for main agent

The main agent's system prompt gains this section (injected by `build_system_prompt`):

```
# Work Verification Protocol

BEFORE ending your response, you MUST verify your work:

1. Call `invoke_agent(agent="verifier", prompt="TASK: ...\\nCRITERIA: ...\\nEVIDENCE: ...")`
   with the original task, your criteria, and a brief summary of what you did
   including key file paths.
2. The verifier has READ-ONLY sandbox access — it will use `read_file` and
   `list_files` to inspect the actual output. You do NOT need to dump file
   contents into the prompt. Just tell it which files to look at.
3. If the verifier returns NEEDS_IMPROVEMENT or FAIL, do NOT end. Instead,
   use the feedback to continue working. You will get another iteration.
4. Only if the verifier returns PASS may you end.
5. You may also verify intermediate results during multi-step tasks.
```

#### How it flows in the agentic loop

```
Iteration 1: Agent does work (file I/O, run commands, spawn_agents, etc.)
Iteration 2: Agent calls invoke_agent(agent="verifier", prompt="TASK: ...\nCRITERIA: ...\nEVIDENCE: ...")
             Verifier returns: <evaluation>NEEDS_IMPROVEMENT</evaluation><feedback>Missing error handling...</feedback>
Iteration 3: Agent sees feedback, fixes the issue, calls verifier again
             Verifier returns: <evaluation>PASS</evaluation><feedback></feedback>
Iteration 4: Agent can end (no tool calls)
```

No special loop code needed — the verifier result is just another tool message. The system prompt teaches the agent to iterate until accepted. A hard iteration cap (global `max_iterations`) prevents infinite loops.

**Note on enforcement:** Verification is system-prompt-guidance-only at this stage — the agent could skip calling the verifier. This is a conscious trade-off for simplicity. A future iteration could add a hard gate in the loop that checks whether the verifier was called before allowing a final response.

### Shared plans + memory

Plans live in `.rustfox_plan.json` in the sandbox — already shared at the filesystem level. If a subagent has `plan_view`/`plan_update` in its whitelist, it can read/update the same plan as the main agent.

Memory lives in `self.memory` (the Agent's knowledge store). Since subagent tool execution goes through the same `Agent::execute_tool`, whitelisting `recall`/`remember`/`search_memory` transparently shares the memory store.

Config options for default tool whitelist:

```toml
[subagents]
default_tools = ["read_file", "write_file", "list_files", "execute_command"]
# Add memory/plan tools:
# default_tools = ["read_file", "write_file", "list_files", "execute_command", "recall", "search_memory", "plan_view", "plan_update"]
```

When unspecified, defaults to sandbox tools only (safe default). Users opt into shared plans/memory via config.

#### Cleanup: Remove `invoke_subagent`

`invoke_subagent(skill="x", prompt="...")` is a deprecated alias for `invoke_agent(agent="x", prompt="...")` with a narrower registry scope (skills only, no agents fallback). Since `invoke_agent` already supports the `skill` parameter as a fallback, `invoke_subagent` provides zero additional value.

**Removal:**
- Remove the `invoke_subagent` tool definition from `skill_tool_definitions()`
- Remove the `"invoke_subagent"` handler arm from `execute_tool()`
- Remove `invoke_subagent` from the parallel execution classification in `process_message()`
- Update the system prompt to only reference `invoke_agent`
- The `skill` parameter fallback in `invoke_agent`'s handler (`arguments["agent"].as_str().or_else(|| arguments["skill"].as_str())`) ensures backward compatibility for any callers still using `skill` as the parameter name

**Skills that previously used `invoke_subagent`** (e.g., `news-fetcher`, `code-interpreter`) are automatically available via `invoke_agent` since it falls back to the skills registry.

### Agent discovery in system prompt

Previously, the LLM tried to `list_files` in the sandbox to find agents. All agents are already listed in the system prompt by `build_agents_context()` with names, descriptions, and `invoke_agent` hints. The system prompt section should be updated to explicitly tell the agent not to search for agents via file operations:

```
# Available Agents

All available agents are listed below with their descriptions. 
DO NOT try to list agent directories or files — everything you 
need to know about available agents is documented here.

- **verifier**: Zero-trust verifier...
  Invoke via: `invoke_agent(agent="verifier", prompt="...")`
- **soul-keeper**: ...
  Invoke via: `invoke_agent(agent="soul-keeper", prompt="...")`
```

The same treatment applies to the skills listing section. This reinforces the system prompt as the authoritative source and prevents the LLM from falling back to filesystem exploration.

### Backward compatibility

| Existing call | Continues to work? |
|---|---|---|
| `invoke_agent(agent="soul-keeper", prompt="...")` | ✅ Yes, unchanged |
| `invoke_agent(skill="news-fetcher", prompt="...")` | ✅ Yes — `invoke_agent` accepts `agent` with fallback to `skill` |
| `invoke_subagent(skill="x", prompt="...")` | ❌ **Removed** — replaced entirely by `invoke_agent` |
| `invoke_agent(agent="name", prompt="...", model="x")` | ✅ Yes, overrides still work |
| Predefined AGENT.md / SKILL.md files | ✅ Yes, unchanged |

New patterns enabled:

| New call | Behavior |
|---|---|
| `spawn_agents(system_prompt="...", prompt="...")` | Ad-hoc single subagent |
| `spawn_agents(tasks=[{...}, {...}])` | Parallel batch (join_all) |
| Multiple `spawn_agents` in one LLM response | Auto-parallel via join_all |

## Input Validation (Error Handling)

The `spawn_agents` handler must validate inputs and return clear error strings:

| Condition | Error |
|---|---|---|
| Neither `tasks` nor `system_prompt`+`prompt` provided | `"Missing tasks: provide either 'tasks' array or system_prompt+prompt"` |
| `tasks` is empty array | `"tasks array is empty"` |
| A task in `tasks` is missing `system_prompt` | `"Task at index N: missing system_prompt"` |
| A task in `tasks` is missing `prompt` | `"Task at index N: missing prompt"` |
| `system_prompt` is empty string | `"system_prompt cannot be empty"` |
| `prompt` is empty string (shorthand mode) | `"prompt cannot be empty"` |

## Implementation Plan

### Step 1: Extract system context builder
- File: `src/agent.rs`
- Extract system context portion from `build_system_prompt()` into a new `build_system_context()` method (date/time, user model, location — minus skills/agents listings)
- This is a pure refactor, no behavior change

### Step 2: Add `spawn_agents` tool definition
- File: `src/agent.rs` in `skill_tool_definitions()`
- JSON schema with `tasks` array + shorthand single-task fields

### Step 3: Refactor `run_subagent`
- Accept `Option<&str>` for `skill_name` (None = ad-hoc)
- Accept `&str` for inline `system_prompt`
- Inject system context via `build_system_context()` for ad-hoc mode
- Skip bootstrap read step for ad-hoc mode
- Return structured JSON for batch results

### Step 4: Add `spawn_agents` handler
- File: `src/agent.rs` in `execute_tool()`
- Parse `tasks` array (or shorthand single task)
- Validate inputs per error handling section above
- Dispatch to refactored `run_subagent` for each task
- Use `futures::future::join_all` for parallel execution of the task batch

### Step 5: Auto-parallel subagent tool calls
- File: `src/agent.rs` in `process_message()`
- Identify `spawn_agents` and `invoke_agent` calls in tool response
- Run them in parallel via `futures::future::join_all`
- Other tool calls remain sequential
- Merge results in original tool_call order

### Step 6: Config defaults
- File: `src/config.rs` — add `SubagentsConfig` struct
- `default_tools: Vec<String>`, `context_mode: String`

### Step 7: Remove `invoke_subagent`
- Remove tool definition from `skill_tool_definitions()`
- Remove handler from `execute_tool()`
- Remove from parallel execution classification
- `invoke_agent`'s `skill` parameter fallback ensures backward compat

### Step 8: Create verifier AGENT.md
- Create: `agents/verifier/AGENT.md`
- Frontmatter: `name: verifier`, `description: ...`, `tools: [read_file, list_files, plan_view]`, `skip_bootstrap: true`
- `plan_view` lets the verifier check plan completion status — did the worker mark all steps done?
- Read-only access means verifier cannot modify anything, only inspect
- Body: zero-trust verifier instructions with structured output format (PASS/NEEDS_IMPROVEMENT/FAIL)
- The verifier uses `invoke_agent(agent="verifier", prompt="...")` — no new tool needed

### Step 9: Update system prompt
- File: `src/agent.rs` in `build_system_prompt()`
- Add "Available Agents" header with note: "DO NOT try to list agent directories — all agents are listed here"
- Add "Work Verification Protocol" section teaching the agent to use `invoke_agent(agent="verifier", ...)` before ending
- Instruct to structure prompt as `TASK: ...\nCRITERIA: ...\nEVIDENCE: ...`
- Instruct to iterate on rejection and only end on PASS
- Update skills listing section with similar "DO NOT list files" note

## Deferred

1. **Context fork mode** (`context_mode: "fork"`): Optionally share conversation history with subagents. Deferred — the current `isolated` mode covers the primary use case. Implement when a concrete need arises.

2. **Subagent timeout behavior**: Subagents that hit max iterations currently return an error string. Acceptable for now — partial results could be returned in a future iteration if needed.
