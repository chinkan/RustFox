# Design: Subagent Model Selection — Skill-Scoped Subagents with Model Overrides

**Date:** 2026-02-23
**Status:** Approved

---

## Goal

Enable the RustFox agent to:

1. **Delegate tasks to a subagent** — a temporary, isolated mini-agentic loop that runs a skill's instructions with its own model, tool whitelist, and iteration budget
2. **Select the model per-skill** — each skill's SKILL.md frontmatter can declare which LLM to use, separate from the main agent's default model
3. **Give subagents only the tools they need** — tool whitelist declared in frontmatter; subagent cannot access memory, scheduling, or invoke further subagents
4. **Allow subagents to read their own skill files** — a new `read_skill_file` tool lets any agent read files from the skills directory without sandbox constraints

**Primary motivating use case:** A cron-triggered "post daily thread" workflow where the main agent (cheap/fast model) orchestrates — fetching Gmail content, invoking the subagent, posting to Threads — while the subagent (high-quality writing model, e.g. Claude Sonnet) runs the actual post-writing loop following detailed style instructions.

---

## Architecture Overview

```
Main Agent  (default model from config, e.g. kimi-k2.5)
│
│  system prompt = base prompt
│                + instruction skills: full body (unchanged)
│                + subagent skills: metadata only + invoke_subagent hint
│
│  tools = [existing tools] + read_skill_file + invoke_subagent
│
│  [cron fires] "Write daily thread post"
│    → mcp_gmail_fetch  → raw content
│    → invoke_subagent(skill="thread-writer", prompt="<raw content>")
│
└──► Subagent  (model from skill frontmatter, e.g. claude-sonnet-4-6)
         │
         │  system prompt = "You are the thread-writer subagent.
         │                   Start by calling read_skill_file."
         │  tools = [read_skill_file, ...declared in skill frontmatter]
         │  isolated message history — no shared conversation or memory
         │
         ├── iter 0: read_skill_file("thread-writer", "SKILL.md") → full instructions
         ├── iter 1: read_skill_file("thread-writer", "style-guide.md") → style doc
         ├── iter 2..N: composes post using instructions + style
         └── returns: polished post text
         ▲
Main Agent receives post as tool result
  → mcp_threads_post(post)
  → "Posted! Here's what was published: ..."
```

**Key principles:**
- **Isolated context** — subagent has no access to conversation history, memory, or scheduling
- **Progressive disclosure** — skill body only enters context when the subagent reads it at runtime
- **Composable** — any skill with a `model` field in frontmatter becomes a subagent skill
- **Safe** — subagent tool access is limited to its declared whitelist; no recursive `invoke_subagent`
- **Non-breaking** — existing instruction skills (no `model` field) continue to work exactly as before

---

## SKILL.md Frontmatter Extension

Three new optional fields:

```yaml
---
name: thread-writer
description: Use when writing daily Thread posts from fetched source content.
             Invoke via invoke_subagent, not directly.
model: anthropic/claude-sonnet-4-6   # optional — model for this subagent
tools: [read_skill_file, mcp_threads_post]  # optional — allowed tool whitelist
max_iterations: 8                    # optional — cap for this subagent's loop
---

# Thread Writer

You are a specialized subagent. Your full instructions are in SKILL.md which
you have already read. Write engaging daily Thread posts...
[full instructions — only loaded when subagent reads this at runtime]
```

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `model` | string | config default | If present, skill becomes a subagent skill |
| `tools` | list | `[read_skill_file]` | Tool whitelist for subagent; `read_skill_file` always added |
| `max_iterations` | integer | config `max_iterations` | Capped at global value |

Caller can override `model` and `tools` at `invoke_subagent` call time (per-invocation override).

---

## Skill Injection Change

Skills with a `model` field in frontmatter are treated as **subagent skills**. `build_context()` emits them differently:

**Before (instruction skill — unchanged):**
```
## Skill: creating-skills
Use when the user asks to create, write, or add a new bot skill...

[full body injected]
```

**After (subagent skill — metadata only):**
```
## Subagent skill: thread-writer
Use when writing daily Thread posts from fetched source content.
Invoke via: invoke_subagent(skill="thread-writer", prompt="<task content>")
```

This keeps the main agent's context lean and avoids injecting potentially long style guides into every conversation.

---

## New Tools

### `read_skill_file`

Reads a file from the skills directory. Available to both main agent and subagents.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `skill_name` | string | yes | Skill directory name (validated: `^[a-z0-9-]{1,64}$`) |
| `path` | string | yes | Relative path within skill dir, e.g. `SKILL.md`, `style-guide.md` |

- Resolves to `config.skills.directory / skill_name / path`
- Validates against traversal: no `..` components, not absolute
- Not sandbox-restricted (skills directory is separate from sandbox)

### `invoke_subagent`

Boots a subagent mini-loop and returns its final text response. Available to main agent only (not exposed to subagents — prevents infinite nesting).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `skill` | string | yes | Skill name to run as subagent |
| `prompt` | string | yes | Task content to pass to the subagent |
| `model` | string | no | Overrides skill's declared model |
| `tools` | list | no | Overrides skill's declared tool whitelist |

---

## Subagent Mini-Loop (`run_subagent`)

```
run_subagent(skill_name, prompt, model_override, tools_override)
  1. Load skill metadata (model, tools, max_iterations)
  2. Resolve final model: model_override → skill.model → config.model
  3. Resolve final tools: tools_override → skill.tools → [read_skill_file]
     Always prepend read_skill_file regardless
  4. Build subagent tool definitions (filtered to allowed list only)
  5. Bootstrap messages:
       system: "You are the <skill_name> subagent. Start by calling
                read_skill_file(skill_name='<name>', path='SKILL.md')."
       user:   <prompt>
  6. Mini agentic loop (up to resolved max_iterations):
       - call llm.chat_with_model(messages, tools, model)
       - if tool_calls: execute via execute_subagent_tool(), append, continue
       - else: return content
  7. On max iterations: return error message string
```

`execute_subagent_tool` handles only safe, stateless tools:
- `read_skill_file` — always available
- Built-in tools (`read_file`, `write_file`, `run_command`) — if in whitelist
- MCP tools — if in whitelist
- Memory/scheduling tools — **never** available to subagents

---

## LLM Client Change

Add model-override support to `LlmClient`:

```rust
// Existing (unchanged)
pub async fn chat(&self, messages: &[ChatMessage], tools: &[ToolDefinition]) -> Result<ChatMessage>

// New
pub async fn chat_with_model(
    &self,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    model: &str,
) -> Result<ChatMessage>
```

`chat()` becomes a thin wrapper calling `chat_with_model(&self.config.model, ...)`.

---

## Files Touched

| File | Change |
|------|--------|
| `src/llm.rs` | Add `chat_with_model(messages, tools, model: &str)` |
| `src/skills/mod.rs` | Add `model`, `tools`, `max_iterations` to `Skill` struct; update `build_context()` to separate instruction vs subagent skills |
| `src/skills/loader.rs` | Parse `model`, `tools`, `max_iterations` from frontmatter |
| `src/agent.rs` | Add `read_skill_file` + `invoke_subagent` tool definitions; implement `run_subagent()`; handle both in `execute_tool()` |
| `skills/thread-writer/SKILL.md` | New example subagent skill (demonstrates the feature) |

No new Cargo dependencies. No `config.toml` schema changes.

---

## Security Notes

- `read_skill_file` validates `skill_name` with strict regex and checks `path` for `..` traversal
- Subagents cannot call `invoke_subagent` — no recursive subagent nesting
- Subagents cannot call memory or scheduling tools — isolated and stateless
- The tool whitelist is enforced at `execute_subagent_tool` dispatch — unknown tools return an error string (not a crash)
- Subagent has no access to conversation history or persistent state

---

## Open Questions (resolved)

- **Model fallback**: subagent without `model` frontmatter gets `config.openrouter.model` — uses main model, but still isolated context
- **Subagent accessing sandbox**: allowed if `read_file`/`write_file`/`run_command` declared in `tools` whitelist — same sandbox as main agent
- **Max iterations for subagent**: resolved as `min(skill.max_iterations, config.max_iterations)` — cannot exceed global cap
- **Skill injection for subagent skills**: metadata-only in main system prompt; full body loaded lazily by subagent at runtime
