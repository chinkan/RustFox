---
name: creating-agents
description: Teaches how to create new agents in the agents/ directory — isolated agentic loops with their own model, tool whitelist, and AGENT.md instructions.
tags: [meta, agents, creation]
---

# Creating Agents

Agents are isolated agentic mini-loops with their own model, tool whitelist, and instruction file (`AGENT.md`). Use agents when you need to delegate a self-contained task to a separate LLM context — typically work that benefits from a different model, a restricted tool set, or clean isolation from the main conversation.

## Skill vs Agent — When to Use Which

| Use a **skill** (`skills/`) | Use an **agent** (`agents/`) |
|-----------------------------|------------------------------|
| Instruction to load into the main context | Isolated task with its own model |
| No tool calls needed, or shares main tool set | Needs a restricted tool whitelist |
| Workflow guidance or persona adjustment | Sub-task that returns a single result |
| Lives in `skills/<name>/SKILL.md` | Lives in `agents/<name>/AGENT.md` |

## Agent File Format

```markdown
---
name: agent-name          # lowercase letters, numbers, hyphens only
description: One sentence — when to invoke this agent and what it returns.
model: provider/model-id  # required: the model this agent uses
tools: [tool1, tool2]     # required: exact tool names the agent may call
max_iterations: 5         # optional: default is global max (25)
tags: [tag1, tag2]        # optional
---

# Agent Name

(Instructions the agent follows. Starts with what it should do on first call.)

## Protocol

1. Step one
2. Step two
3. Return final answer
```

## Tool Names

Tools must be the **exact runtime names** visible to the agent:

| Category | Name format | Example |
|----------|-------------|---------|
| Built-in | plain name | `read_file`, `write_file`, `execute_command` |
| Skill tools | plain name | `read_skill_file`, `write_skill_file`, `reload_skills` |
| Agent tools | plain name | `read_agent_file`, `write_agent_file`, `reload_agents` |
| MCP tools | `mcp_{server}_{tool}` | `mcp_google-workspace_query_gmail_emails` |

`read_skill_file` and `read_agent_file` are always available to every agent — no need to list them.

## Step-by-Step: Create a New Agent

1. **Design** — What model? What tools? What does it return?
2. **Write** the `AGENT.md` using `write_agent_file`:
   ```
   write_agent_file(agent_name="my-agent", relative_path="AGENT.md", content="...")
   ```
3. **Reload** to activate immediately:
   ```
   reload_agents()
   ```
4. **Test** by invoking:
   ```
   invoke_agent(agent="my-agent", prompt="<test task>")
   ```

## Calling Agents

```
invoke_agent(agent="agent-name", prompt="Task description here")
```

Optional overrides for one-off invocations:
```
invoke_agent(agent="agent-name", prompt="...", model="anthropic/claude-sonnet-4-6", tools=["read_agent_file", "mcp_threads_post"])
```

## Example: Minimal Agent

```markdown
---
name: summariser
description: Summarises a block of text into 3 bullet points. Invoke when the user asks for a summary.
model: qwen/qwen3-235b-a22b
tools: []
max_iterations: 2
---

# Summariser

Read your instructions (already loaded), then summarise the text in the prompt into exactly 3 bullet points. Return only the bullets — no preamble.
```

## Existing Agents

Check `agents/` for current agents. Use `reload_agents` after any changes to activate them.

## Backward Compatibility

Skills with a `model:` field in `skills/` still work via `invoke_agent` — the agent registry is checked first, then the skills registry. New agents should go in `agents/`.
