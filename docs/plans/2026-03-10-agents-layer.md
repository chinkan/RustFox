# Agents Layer Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a dedicated `agents/` directory and generalised agent tools (`invoke_agent`, `read/write_agent_file`, `reload_agents`) so the bot distinguishes instruction skills from isolated subagent runners, and migrate soul-keeper to the new agents layer.

**Architecture:** Two parallel registries — `skills/` (instruction skills, no `model` field) and `agents/` (isolated agentic loops with `model` + tool whitelist in frontmatter). Both use the same `SkillRegistry`/loader infrastructure. Four new tools handle the agents layer; `invoke_subagent` is kept as a backward-compat alias. `AgentKind` enum in `run_subagent` drives which registry and which bootstrap read-tool to use.

**Tech Stack:** Rust 2021, Tokio, `serde`/`toml` for config, existing `SkillRegistry` + loader, `tokio::sync::RwLock` for hot-reload.

---

## Task 1: Add `AgentsConfig` to `src/config.rs`

**Files:**
- Modify: `src/config.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn test_agents_config_defaults_to_agents_dir() {
    let toml = r#"
        [telegram]
        bot_token = "tok"
        allowed_user_ids = [1]
        [openrouter]
        api_key = "key"
        [sandbox]
        allowed_directory = "/tmp"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert_eq!(cfg.agents.directory, PathBuf::from("agents"));
}
```

Place inside `#[cfg(test)] mod tests` at the bottom of `src/config.rs`.

**Step 2: Run test to verify it fails**

```bash
cargo test config::tests::test_agents_config_defaults_to_agents_dir
```

Expected: FAIL — `no field 'agents'`

**Step 3: Add `AgentsConfig` struct and wire it into `Config`**

After the existing `SkillsConfig` block, add:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct AgentsConfig {
    #[serde(default = "default_agents_dir")]
    pub directory: PathBuf,
}

fn default_agents_dir() -> PathBuf {
    PathBuf::from("agents")
}

fn default_agents_config() -> AgentsConfig {
    AgentsConfig {
        directory: default_agents_dir(),
    }
}
```

Add to `Config` struct (after `skills` field):

```rust
#[serde(default = "default_agents_config")]
pub agents: AgentsConfig,
```

**Step 4: Run test to verify it passes**

```bash
cargo test config::tests::test_agents_config_defaults_to_agents_dir
```

Expected: PASS

**Step 5: Run full test suite**

```bash
cargo test
```

Expected: all pass

**Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add AgentsConfig with default directory ./agents"
```

---

## Task 2: Update `src/skills/loader.rs` to handle `AGENT.md`

**Files:**
- Modify: `src/skills/loader.rs`

**Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/skills/loader.rs`:

```rust
#[test]
fn test_name_from_path_agent_md_uses_dir_name() {
    let path = std::path::Path::new("agents/soul-keeper/AGENT.md");
    let name = name_from_path(path);
    assert_eq!(name, "soul-keeper");
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test skills::loader::tests::test_name_from_path_agent_md_uses_dir_name
```

Expected: FAIL — returns `"AGENT"` not `"soul-keeper"`

**Step 3: Update `name_from_path` to match `AGENT.md`**

Replace:

```rust
if path.file_name().and_then(|f| f.to_str()) == Some("SKILL.md") {
```

With:

```rust
let filename = path.file_name().and_then(|f| f.to_str());
if matches!(filename, Some("SKILL.md") | Some("AGENT.md")) {
```

**Step 4: Update directory-scan to also check `AGENT.md`**

In `load_skills_from_dir`, replace the `if path.is_dir()` branch:

```rust
let skill_path = if path.is_dir() {
    let skill_file = path.join("SKILL.md");
    let agent_file = path.join("AGENT.md");
    if skill_file.exists() {
        skill_file
    } else if agent_file.exists() {
        agent_file
    } else {
        continue;
    }
} else if path.extension().and_then(|e| e.to_str()) == Some("md") {
    path.clone()
} else {
    continue;
};
```

**Step 5: Run test to verify it passes**

```bash
cargo test skills::loader::tests::test_name_from_path_agent_md_uses_dir_name
```

Expected: PASS

**Step 6: Run full test suite**

```bash
cargo test
```

Expected: all pass

**Step 7: Commit**

```bash
git add src/skills/loader.rs
git commit -m "feat(loader): recognise AGENT.md alongside SKILL.md in directory entries"
```

---

## Task 3: Update `src/skills/mod.rs` — `build_context` and `build_agents_context`

**Files:**
- Modify: `src/skills/mod.rs`

**Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests`:

```rust
#[test]
fn test_build_context_subagent_uses_invoke_agent() {
    let mut registry = SkillRegistry::new();
    registry.register(make_skill("my-agent", "Does work", "body", Some("some/model")));
    let ctx = registry.build_context();
    assert!(ctx.contains("invoke_agent"));
    assert!(!ctx.contains("invoke_subagent"));
}

#[test]
fn test_build_agents_context_lists_agents() {
    let mut registry = SkillRegistry::new();
    registry.register(make_skill("news-fetcher", "Fetches AI news", "body", None));
    let ctx = registry.build_agents_context();
    assert!(ctx.contains("invoke_agent"));
    assert!(ctx.contains("news-fetcher"));
    assert!(ctx.contains("Fetches AI news"));
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test skills::tests::test_build_context_subagent_uses_invoke_agent
cargo test skills::tests::test_build_agents_context_lists_agents
```

Expected: FAIL

**Step 3: Update `build_context` — change `invoke_subagent` to `invoke_agent`**

In `build_context()`, replace all occurrences of `invoke_subagent` with `invoke_agent` in the generated strings and header text.

**Step 4: Add `build_agents_context` method**

After `build_context`, add:

```rust
/// Build context string for the agents directory.
/// All agents are invoked via `invoke_agent`.
pub fn build_agents_context(&self) -> String {
    if self.skills.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    for agent in self.skills.values() {
        lines.push(format!(
            "- **{}**: {}\n  Invoke via: `invoke_agent(agent=\"{}\", prompt=\"<task>\")`",
            agent.name, agent.description, agent.name
        ));
    }
    let mut context =
        String::from("Delegate these tasks to specialized agents using `invoke_agent`:\n\n");
    context.push_str(&lines.join("\n"));
    context.push('\n');
    context
}
```

**Step 5: Update existing tests that assert `invoke_subagent`**

Replace every `assert!(ctx.contains("invoke_subagent"))` with `assert!(ctx.contains("invoke_agent"))` in the test block.

**Step 6: Run full test suite**

```bash
cargo test
```

Expected: all pass

**Step 7: Commit**

```bash
git add src/skills/mod.rs
git commit -m "feat(skills): build_context emits invoke_agent; add build_agents_context for agents registry"
```

---

## Task 4: Add `AgentKind`, `agents` field, new tools, and updated `run_subagent` in `src/agent.rs`

This is the largest task. Break into four sub-steps but commit once at the end.

**Files:**
- Modify: `src/agent.rs`

### 4a — Add `AgentKind` enum and `agents` field to `Agent`

**Step 1: Write a failing test for `effective_subagent_tools` including `read_agent_file`**

In `#[cfg(test)] mod tests`:

```rust
#[test]
fn test_effective_tools_always_includes_read_agent_file() {
    let declared = vec!["mcp_threads_post".to_string()];
    let effective = effective_subagent_tools(&declared);
    assert!(effective.contains(&"read_agent_file".to_string()));
}
```

**Step 2: Run to verify it fails**

```bash
cargo test agent::tests::test_effective_tools_always_includes_read_agent_file
```

**Step 3: Add `AgentKind` enum before `impl Agent`**

```rust
/// Which registry/directory an agent invocation targets.
#[derive(Clone, Copy)]
enum AgentKind {
    /// Look up in the skills registry; bootstrap uses `read_skill_file` / SKILL.md
    Skill,
    /// Look up in agents registry first, fall back to skills; bootstrap uses `read_agent_file` / AGENT.md
    Agent,
}
```

**Step 4: Add `agents` field to `Agent` struct**

After `pub skills: tokio::sync::RwLock<SkillRegistry>,`:

```rust
pub agents: tokio::sync::RwLock<SkillRegistry>,
```

**Step 5: Update `Agent::new` to accept `agents: SkillRegistry`**

Add `agents: SkillRegistry` parameter after `skills`. Initialise as `agents: tokio::sync::RwLock::new(agents)`.

**Step 6: Update `effective_subagent_tools` to include `read_agent_file`**

```rust
fn effective_subagent_tools(declared: &[String]) -> Vec<String> {
    let mut tools = vec![
        "read_skill_file".to_string(),
        "read_agent_file".to_string(),
    ];
    for t in declared {
        if t != "read_skill_file" && t != "read_agent_file" {
            tools.push(t.clone());
        }
    }
    tools
}
```

**Step 7: Run test to verify it passes**

```bash
cargo test agent::tests::test_effective_tools_always_includes_read_agent_file
```

### 4b — Add four new agent tools to `skill_tool_definitions()`

Replace the `invoke_subagent` `ToolDefinition` entry with:

1. **`invoke_subagent`** — kept but marked deprecated in description, calls `AgentKind::Skill`
2. **`invoke_agent`** — primary tool, accepts `agent` param, `AgentKind::Agent`
3. **`read_agent_file`** — reads from `config.agents.directory`
4. **`write_agent_file`** — writes to `config.agents.directory`
5. **`reload_agents`** — hot-reloads `agents/` registry

Schema for `invoke_agent`:

```json
{
  "type": "object",
  "properties": {
    "agent":  { "type": "string", "description": "Agent name (e.g. 'soul-keeper')" },
    "prompt": { "type": "string", "description": "Task for the agent" },
    "model":  { "type": "string", "description": "Optional model override" },
    "tools":  { "type": "array", "items": { "type": "string" }, "description": "Optional tool whitelist override" }
  },
  "required": ["agent", "prompt"]
}
```

Schema for `read_agent_file` / `write_agent_file`: same shape as `read_skill_file` / `write_skill_file` but with `agent_name` parameter.

### 4c — Update `run_subagent` to accept `AgentKind`

Change signature:

```rust
async fn run_subagent(
    &self,
    skill_name: &str,
    prompt: &str,
    model_override: Option<&str>,
    tools_override: Option<Vec<String>>,
    kind: AgentKind,   // NEW
) -> String {
```

Registry lookup logic:

```rust
let skill_opt = match kind {
    AgentKind::Agent => {
        // check agents registry first, fall back to skills
        let from_agents = { self.agents.read().await.get(skill_name).cloned() };
        if from_agents.is_some() { from_agents }
        else { self.skills.read().await.get(skill_name).cloned() }
    }
    AgentKind::Skill => {
        self.skills.read().await.get(skill_name).cloned()
    }
};
```

Bootstrap message selection:

```rust
let system_content = match kind {
    AgentKind::Agent => format!(
        "You are the '{}' agent. Your first action MUST be to call \
         read_agent_file with agent_name='{}' and relative_path='AGENT.md' to load your instructions.",
        skill_name, skill_name
    ),
    AgentKind::Skill => format!(
        "You are the '{}' subagent. Your first action MUST be to call \
         read_skill_file with skill_name='{}' and relative_path='SKILL.md' to load your instructions.",
        skill_name, skill_name
    ),
};
```

### 4d — Add execute_tool handlers for four new tools

Add cases in `execute_tool()` match:

- **`invoke_agent`** — extract `agent` arg (fallback `skill` for compat), call `run_subagent(..., AgentKind::Agent)`
- **`invoke_subagent`** — extract `skill` arg, call `run_subagent(..., AgentKind::Skill)` (backward compat)
- **`read_agent_file`** — read from `self.config.agents.directory.join(agent_name).join(relative_path)`, with symlink escape check
- **`write_agent_file`** — write to same path, `create_dir_all` for parent
- **`reload_agents`** — `load_skills_from_dir(&self.config.agents.directory)`, write-lock `self.agents`

Also update `build_system_prompt` to include agents context:

```rust
let agents = self.agents.read().await;
let agent_context = agents.build_agents_context();
if !agent_context.is_empty() {
    prompt.push_str("\n\n# Available Agents\n\n");
    prompt.push_str(&agent_context);
}
drop(agents);
```

**Step (compile check):**

```bash
cargo check
```

Fix any type errors before proceeding.

**Step (run full tests):**

```bash
cargo test
```

Expected: all pass (update any tests asserting old `effective_subagent_tools` output)

**Step: Commit**

```bash
git add src/agent.rs
git commit -m "feat(agent): add agents registry, invoke_agent/read_agent_file/write_agent_file/reload_agents tools, AgentKind-driven run_subagent"
```

---

## Task 5: Update `src/main.rs` — load agents at startup

**Files:**
- Modify: `src/main.rs`

**Step 1: Load agents after skills**

After the existing skills loading block:

```rust
// Load agents from the agents directory
let agents = load_skills_from_dir(&config.agents.directory).await?;
info!("  Agents: {}", agents.len());
```

**Step 2: Pass `agents` to `Agent::new`**

```rust
let agent = Arc::new_cyclic(|weak| {
    Agent::new(
        config.clone(),
        mcp_manager,
        memory.clone(),
        skills,
        agents,       // NEW
        task_store.clone(),
        Arc::clone(&scheduler),
        Arc::clone(&bot),
        weak.clone(),
        job_tx,
        Arc::clone(&langsmith),
    )
});
```

**Step 3: Run full tests**

```bash
cargo test
```

Expected: all pass

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(main): load agents/ directory at startup alongside skills/"
```

---

## Task 6: Create `agents/` directory

**Files:**
- Create: `agents/` (git-tracked via `.gitkeep` or first agent file)

**Step 1: Create the directory**

```bash
mkdir -p agents
```

The directory is picked up at runtime. No `.gitkeep` needed once an agent file is added.

**Step 2: Commit**

```bash
git add agents/
git commit -m "chore: create agents/ directory for agent definitions"
```

---

## Task 7: Migrate soul-keeper to `agents/`

soul-keeper currently lives in `skills/soul-keeper/SKILL.md`. It has a `model` field making it a subagent — it belongs in `agents/`.

**Files:**
- Create: `agents/soul-keeper/AGENT.md`
- Delete: `skills/soul-keeper/SKILL.md` (and the directory)
- Keep: `skills/soul/SOUL.md` — this is the identity file, not the agent definition

**Step 1: Create `agents/soul-keeper/AGENT.md`**

Content is identical to `skills/soul-keeper/SKILL.md`. The body references `read_skill_file`/`write_skill_file`/`reload_skills` for soul — **keep these unchanged** because `skills/soul/SOUL.md` still lives in `skills/`.

```markdown
---
name: soul-keeper
description: Updates the soul file when the user gives personality coaching or style preferences, or when you have learned something significant about the user that should permanently shape how you interact with them.
model: qwen/qwen3.5-122b-a10b
tools: [read_skill_file, write_skill_file, reload_skills]
max_iterations: 3
tags: [soul, identity, meta]
---

# Soul Keeper

[... full body content from skills/soul-keeper/SKILL.md ...]
```

**Step 2: Delete `skills/soul-keeper/`**

```bash
rm -rf skills/soul-keeper/
```

**Step 3: Verify `skills/soul/SOUL.md` is untouched**

```bash
ls skills/soul/
```

Expected: `SOUL.md`

**Step 4: Run full tests**

```bash
cargo test
```

Expected: all pass

**Step 5: Commit**

```bash
git add agents/soul-keeper/AGENT.md
git rm -r skills/soul-keeper/
git commit -m "feat(agents): migrate soul-keeper from skills/ to agents/ — identity files unchanged"
```

---

## Task 8: Create `skills/creating-agents/SKILL.md`

**Files:**
- Create: `skills/creating-agents/SKILL.md`

**Step 1: Create the skill directory and file**

```bash
mkdir -p skills/creating-agents
```

Content for `skills/creating-agents/SKILL.md`:

```markdown
---
name: creating-agents
description: Teaches how to create new agents in the agents/ directory — isolated agentic loops with their own model, tool whitelist, and AGENT.md instructions.
tags: [meta, agents, creation]
---

# Creating Agents

[... full content covering: when to use agents vs skills, AGENT.md format, frontmatter schema,
tool name table (built-in / skill tools / agent tools / MCP), step-by-step creation workflow,
invoke_agent usage, example minimal agent, backward compat note ...]
```

Key sections:

| Section | Content |
|---------|---------|
| Skill vs Agent table | When to use each (instruction/no-model vs isolated/model) |
| AGENT.md frontmatter | `name`, `description`, `model`, `tools`, `max_iterations`, `tags` |
| Tool name table | Exact runtime names: `mcp_{server}_{tool}`, built-in names |
| Creation workflow | `write_agent_file` → `reload_agents` → `invoke_agent` test |
| Example | Minimal summariser agent |
| Backward compat | agents registry first, then skills fallback |

**Step 2: Run full tests**

```bash
cargo test
```

Expected: all pass (no Rust changes, pure markdown)

**Step 3: Commit**

```bash
git add skills/creating-agents/
git commit -m "docs(skills): add creating-agents instruction skill for agent creation workflow"
```

---

## Final Verification

```bash
cargo clippy -- -D warnings
cargo fmt --all -- --check
cargo test
```

All must pass clean before pushing.

---

## Summary of New Tools

| Tool | Directory | Purpose |
|------|-----------|---------|
| `invoke_agent` | `agents/` first, fallback `skills/` | Primary: run an isolated agent loop |
| `read_agent_file` | `agents/` | Read AGENT.md or supporting files |
| `write_agent_file` | `agents/` | Create/update agent files |
| `reload_agents` | `agents/` | Hot-reload agents without restart |
| `invoke_subagent` | `skills/` only | Backward-compat alias |
| `read_skill_file` | `skills/` | Unchanged |
| `write_skill_file` | `skills/` | Unchanged |
| `reload_skills` | `skills/` | Unchanged |

## Key Invariants

- `read_skill_file` and `read_agent_file` are **always** in every agent's effective tool whitelist — never need declaring
- `skills/soul/SOUL.md` stays in `skills/` — soul-keeper reads/writes it via `read_skill_file`/`write_skill_file`
- `AgentKind::Agent` looks in agents registry first, then falls back to skills (backward compat)
- `AgentKind::Skill` only looks in skills registry
- `config.toml` may add `[agents] directory = "custom/path"` to override the default `./agents`
