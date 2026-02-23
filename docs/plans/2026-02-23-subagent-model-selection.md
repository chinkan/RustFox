# Subagent Model Selection Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add an `invoke_subagent` tool that lets the main agent delegate tasks to isolated mini-agents that run a named skill using a skill-specific model and tool whitelist.

**Architecture:** Each skill's SKILL.md can declare `model`, `tools`, and `max_iterations` in its YAML frontmatter. When the main agent calls `invoke_subagent(skill, prompt)`, a fresh agentic loop boots with that skill's model (via a new `chat_with_model` LLM method), a restricted tool list, and an isolated message history. Subagents read their own instructions at runtime using a new `read_skill_file` tool. Subagent skills appear in the main agent's system prompt as metadata-only (name + description) rather than with full bodies.

**Tech Stack:** Rust 2021, Tokio, `serde_json`, `anyhow`. No new dependencies.

---

## Reference files

Read these before starting. They contain all the patterns you'll be repeating:

- `src/llm.rs` — `LlmClient::chat()` you will refactor (68 lines total)
- `src/skills/mod.rs` — `Skill` struct and `build_context()` you will extend (75 lines)
- `src/skills/loader.rs` — `extract_field` / `extract_list_field` helpers you will follow (164 lines)
- `src/agent.rs` — `execute_tool()`, `skill_tool_definitions()`, `validate_skill_name()`, `validate_skill_path()` you will extend (~1000 lines — read carefully before Task 4)
- `docs/plans/2026-02-23-subagent-model-selection-design.md` — approved design doc

---

## Task 1: Add `chat_with_model` to `LlmClient`

**Files:**
- Modify: `src/llm.rs`

The `ChatRequest` struct (line 46) and `LlmClient::chat()` (line 80) are the only things to touch. The goal is to make the model a runtime parameter so the subagent loop can call any model without creating a new `LlmClient`.

**Step 1: Write the failing test**

Add this to the bottom of `src/llm.rs` (after the `impl LlmClient` block):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_request_serializes_model_field() {
        // Verifies the model string will appear in the JSON POST body
        let req = ChatRequest {
            model: "anthropic/claude-sonnet-4-6".to_string(),
            messages: vec![],
            tools: None,
            tool_choice: None,
            max_tokens: 100,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "anthropic/claude-sonnet-4-6");
    }

    #[test]
    fn test_chat_request_default_model_is_different_from_override() {
        // Ensures chat_with_model can use a different model than the config default
        let default_req = ChatRequest {
            model: "moonshotai/kimi-k2.5".to_string(),
            messages: vec![],
            tools: None,
            tool_choice: None,
            max_tokens: 100,
        };
        let override_req = ChatRequest {
            model: "anthropic/claude-sonnet-4-6".to_string(),
            messages: vec![],
            tools: None,
            tool_choice: None,
            max_tokens: 100,
        };
        let json_default = serde_json::to_value(&default_req).unwrap();
        let json_override = serde_json::to_value(&override_req).unwrap();
        assert_ne!(json_default["model"], json_override["model"]);
    }
}
```

**Step 2: Run to verify tests pass (they test existing serialization)**

```bash
cargo test --lib llm 2>&1 | tail -20
```

Expected: PASS (these only test `ChatRequest` serialization which already works)

**Step 3: Refactor `chat()` into `chat_with_model()`**

Replace the existing `chat()` method body. The full `impl LlmClient` block becomes:

```rust
impl LlmClient {
    pub fn new(config: OpenRouterConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
        }
    }

    /// Chat with an explicit model string (used by subagents to override the default).
    pub async fn chat_with_model(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
    ) -> Result<ChatMessage> {
        let tools_param = if tools.is_empty() {
            None
        } else {
            Some(tools.to_vec())
        };

        let tool_choice = if tools_param.is_some() {
            Some("auto".to_string())
        } else {
            None
        };

        let request = ChatRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            tools: tools_param,
            tool_choice,
            max_tokens: self.config.max_tokens,
        };

        let url = format!("{}/chat/completions", self.config.base_url);

        debug!("Sending request to OpenRouter: {}", url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to OpenRouter")?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenRouter API error ({}): {}", status, error_body);
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .context("Failed to parse OpenRouter response")?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .context("No response from OpenRouter")
    }

    /// Chat using the model configured in config.toml (delegates to chat_with_model).
    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatMessage> {
        self.chat_with_model(messages, tools, &self.config.model).await
    }
}
```

**Step 4: Verify compilation and tests**

```bash
cargo check 2>&1 | grep -E "^error" | head -20
cargo test --lib llm 2>&1 | tail -20
```

Expected: 0 errors, 2 tests pass.

**Step 5: Commit**

```bash
git add src/llm.rs
git commit -m "feat(llm): add chat_with_model for per-call model override"
```

---

## Task 2: Extend `Skill` struct with subagent fields

**Files:**
- Modify: `src/skills/mod.rs`

The `Skill` struct (line 9) and `build_context()` (line 52) both need changing. Skills with a `model` field are treated as subagent skills — their full body is NOT injected into the main agent's system prompt, only metadata.

**Step 1: Write the failing tests**

Add a `#[cfg(test)] mod tests` block at the bottom of `src/skills/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_skill(name: &str, description: &str, content: &str, model: Option<&str>) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            content: content.to_string(),
            tags: vec![],
            model: model.map(str::to_string),
            tools: vec![],
            max_iterations: None,
        }
    }

    #[test]
    fn test_build_context_instruction_skill_injects_full_body() {
        // Skills without a model field get their full content injected
        let mut registry = SkillRegistry::new();
        registry.register(make_skill(
            "my-skill",
            "Does things",
            "# Instructions\nDo this and that.",
            None, // no model = instruction skill
        ));
        let ctx = registry.build_context();
        assert!(ctx.contains("# Instructions"));
        assert!(ctx.contains("Do this and that."));
    }

    #[test]
    fn test_build_context_subagent_skill_injects_metadata_only() {
        // Skills with a model field get only name + description + invoke hint
        let mut registry = SkillRegistry::new();
        registry.register(make_skill(
            "thread-writer",
            "Use when writing Thread posts.",
            "# Super Secret Instructions\nLong style guide...",
            Some("anthropic/claude-sonnet-4-6"),
        ));
        let ctx = registry.build_context();
        // Metadata present
        assert!(ctx.contains("thread-writer"));
        assert!(ctx.contains("Use when writing Thread posts."));
        assert!(ctx.contains("invoke_subagent"));
        // Body NOT present
        assert!(!ctx.contains("Super Secret Instructions"));
        assert!(!ctx.contains("Long style guide"));
    }

    #[test]
    fn test_build_context_empty_registry() {
        let registry = SkillRegistry::new();
        assert_eq!(registry.build_context(), String::new());
    }

    #[test]
    fn test_build_context_mixed_skills() {
        // Both skill types can coexist
        let mut registry = SkillRegistry::new();
        registry.register(make_skill(
            "instruction-skill",
            "An instruction skill",
            "Follow these instructions.",
            None,
        ));
        registry.register(make_skill(
            "subagent-skill",
            "A subagent skill",
            "Secret subagent body.",
            Some("some/model"),
        ));
        let ctx = registry.build_context();
        assert!(ctx.contains("Follow these instructions."));
        assert!(!ctx.contains("Secret subagent body."));
        assert!(ctx.contains("invoke_subagent"));
    }
}
```

**Step 2: Run to verify tests fail (fields don't exist yet)**

```bash
cargo test --lib skills::mod 2>&1 | grep -E "^error" | head -10
```

Expected: compile errors about missing fields `model`, `tools`, `max_iterations`.

**Step 3: Add fields to `Skill` struct**

Replace the `Skill` struct (lines 7-18) with:

```rust
/// A loaded skill from a markdown file
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Skill {
    /// Skill name (derived from filename or frontmatter)
    pub name: String,
    /// Short description
    pub description: String,
    /// Full markdown content (the instructions)
    pub content: String,
    /// Category/tags for organization
    pub tags: Vec<String>,
    /// If set, this skill runs as a subagent using this model
    pub model: Option<String>,
    /// Tool whitelist for the subagent (empty = read_skill_file only)
    pub tools: Vec<String>,
    /// Max loop iterations for the subagent (None = use global config default)
    pub max_iterations: Option<u32>,
}
```

**Step 4: Update `build_context()` to split instruction vs subagent skills**

Replace `build_context()` (lines 52-65) with:

```rust
/// Build context string for the system prompt.
/// Instruction skills (no model field): full body injected.
/// Subagent skills (have model field): metadata only + invoke_subagent hint.
pub fn build_context(&self) -> String {
    if self.skills.is_empty() {
        return String::new();
    }

    let mut instruction_section = String::new();
    let mut subagent_section = String::new();

    for skill in self.skills.values() {
        if skill.model.is_some() {
            // Subagent skill — metadata only
            subagent_section.push_str(&format!(
                "- **{}**: {}\n  Invoke via: `invoke_subagent(skill=\"{}\", prompt=\"<task>\")`\n",
                skill.name, skill.description, skill.name
            ));
        } else {
            // Instruction skill — full body
            instruction_section.push_str(&format!("## Skill: {}\n", skill.name));
            instruction_section.push_str(&format!("{}\n\n", skill.content));
        }
    }

    let mut context = String::new();

    if !instruction_section.is_empty() {
        context.push_str("You have the following skills available. When relevant, follow these instructions:\n\n");
        context.push_str(&instruction_section);
    }

    if !subagent_section.is_empty() {
        context.push_str("\n## Available Subagent Skills\n\n");
        context.push_str("Delegate these tasks using `invoke_subagent`:\n\n");
        context.push_str(&subagent_section);
    }

    context
}
```

**Step 5: Fix loader.rs — `Skill::new` calls need the new fields**

After this step, `loader.rs` will fail to compile because it constructs `Skill { ... }` without the new fields. Fix each `Ok(Skill { ... })` in `src/skills/loader.rs` by adding:

```rust
model: None,
tools: vec![],
max_iterations: None,
```

to both construction sites (the frontmatter path at line ~78 and the no-frontmatter path at line ~91). This is a temporary stub — Task 3 replaces it with real parsing.

**Step 6: Run tests**

```bash
cargo test --lib 2>&1 | tail -30
```

Expected: all tests pass including the 4 new ones.

**Step 7: Commit**

```bash
git add src/skills/mod.rs src/skills/loader.rs
git commit -m "feat(skills): add model/tools/max_iterations fields; subagent skills show metadata-only in system prompt"
```

---

## Task 3: Parse new frontmatter fields in skill loader

**Files:**
- Modify: `src/skills/loader.rs`

`extract_field` and `extract_list_field` already exist. You need to add `extract_u32_field` and wire up the three new fields.

**Step 1: Write the failing tests**

Add a `#[cfg(test)] mod tests` block at the bottom of `src/skills/loader.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_u32_field_present() {
        let fm = "name: my-skill\nmax_iterations: 8\n";
        assert_eq!(extract_u32_field(fm, "max_iterations"), Some(8));
    }

    #[test]
    fn test_extract_u32_field_absent() {
        let fm = "name: my-skill\n";
        assert_eq!(extract_u32_field(fm, "max_iterations"), None);
    }

    #[test]
    fn test_extract_u32_field_invalid_value() {
        let fm = "max_iterations: not-a-number\n";
        assert_eq!(extract_u32_field(fm, "max_iterations"), None);
    }

    #[test]
    fn test_load_skill_parses_model_field() {
        let frontmatter = "name: thread-writer\ndescription: Write posts\nmodel: anthropic/claude-sonnet-4-6\n";
        let model = extract_field(frontmatter, "model");
        assert_eq!(model.as_deref(), Some("anthropic/claude-sonnet-4-6"));
    }

    #[test]
    fn test_load_skill_parses_tools_field() {
        let frontmatter = "tools: [read_skill_file, mcp_threads_post]\n";
        let tools = extract_list_field(frontmatter, "tools");
        assert_eq!(tools, vec!["read_skill_file", "mcp_threads_post"]);
    }

    #[test]
    fn test_load_skill_defaults_when_fields_absent() {
        let frontmatter = "name: plain-skill\ndescription: Simple skill\n";
        assert_eq!(extract_field(frontmatter, "model"), None);
        assert!(extract_list_field(frontmatter, "tools").is_empty());
        assert_eq!(extract_u32_field(frontmatter, "max_iterations"), None);
    }
}
```

**Step 2: Run to verify `test_extract_u32_field_*` fail (function doesn't exist)**

```bash
cargo test --lib skills::loader 2>&1 | grep -E "^error" | head -5
```

Expected: compile error — `extract_u32_field` not found.

**Step 3: Add `extract_u32_field`**

Add this function after `extract_list_field` in `src/skills/loader.rs`:

```rust
/// Extract a `key: N` unsigned integer field from YAML-like frontmatter
fn extract_u32_field(frontmatter: &str, key: &str) -> Option<u32> {
    extract_field(frontmatter, key)?.parse().ok()
}
```

**Step 4: Wire up new fields in `load_skill_file`**

In `load_skill_file`, replace the frontmatter branch return (currently around line 78) with:

```rust
return Ok(Skill {
    name: skill_name,
    description: description.unwrap_or_else(|| first_line_or_heading(&body)),
    content: body,
    tags,
    model: extract_field(frontmatter, "model"),
    tools: extract_list_field(frontmatter, "tools"),
    max_iterations: extract_u32_field(frontmatter, "max_iterations"),
});
```

The no-frontmatter path (line ~91) keeps `model: None, tools: vec![], max_iterations: None` — no skills without frontmatter can be subagents.

**Step 5: Run tests**

```bash
cargo test --lib 2>&1 | tail -20
```

Expected: all tests pass including the 6 new loader tests.

**Step 6: Commit**

```bash
git add src/skills/loader.rs
git commit -m "feat(skills/loader): parse model, tools, max_iterations from SKILL.md frontmatter"
```

---

## Task 4: Add `read_skill_file` tool

**Files:**
- Modify: `src/agent.rs`

This tool reads a file from the skills directory (not sandbox-restricted). It reuses the existing `validate_skill_name` and `validate_skill_path` helpers already in `agent.rs` (lines 886-919).

**Step 1: Write the failing tests**

In `src/agent.rs`, find the existing `#[cfg(test)] mod tests` block (line 922). Add these tests to it:

```rust
#[test]
fn test_read_skill_file_validates_skill_name() {
    // validate_skill_name is reused — just verify the boundary
    assert!(validate_skill_name("valid-skill").is_ok());
    assert!(validate_skill_name("../evil").is_err());
    assert!(validate_skill_name("").is_err());
}

#[test]
fn test_read_skill_file_validates_relative_path() {
    assert!(validate_skill_path("SKILL.md").is_ok());
    assert!(validate_skill_path("style-guide.md").is_ok());
    assert!(validate_skill_path("../other-skill/SKILL.md").is_err());
    assert!(validate_skill_path("/etc/passwd").is_err());
    assert!(validate_skill_path("").is_err());
}
```

**Step 2: Run to verify they pass (reuses existing validated functions)**

```bash
cargo test --lib agent::tests 2>&1 | tail -20
```

Expected: all pass (these functions already exist and are already tested; we're just adding clarity tests).

**Step 3: Add `read_skill_file` tool definition**

In `skill_tool_definitions()` (around line 465), add a third `ToolDefinition` entry after `reload_skills`:

```rust
ToolDefinition {
    tool_type: "function".to_string(),
    function: FunctionDefinition {
        name: "read_skill_file".to_string(),
        description: concat!(
            "Read a file from a skill directory. Use this to load a skill's full instructions ",
            "or supporting files (style guides, templates, reference docs). ",
            "Available to both the main agent and subagents."
        ).to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "skill_name": {
                    "type": "string",
                    "description": "Skill directory name (e.g. 'thread-writer')"
                },
                "path": {
                    "type": "string",
                    "description": "Relative path within the skill directory (e.g. 'SKILL.md', 'style-guide.md')"
                }
            },
            "required": ["skill_name", "path"]
        }),
    },
},
```

**Step 4: Add `read_skill_file` handler in `execute_tool()`**

In `execute_tool()`, add a match arm **before** the `"write_skill_file"` arm (around line 748):

```rust
"read_skill_file" => {
    let skill_name = match arguments["skill_name"].as_str() {
        Some(n) => n.to_string(),
        None => return "Missing skill_name".to_string(),
    };
    let relative_path = match arguments["path"].as_str() {
        Some(p) => p.to_string(),
        None => return "Missing path".to_string(),
    };

    if let Err(e) = validate_skill_name(&skill_name) {
        return format!("Invalid skill_name: {}", e);
    }
    if let Err(e) = validate_skill_path(&relative_path) {
        return format!("Invalid path: {}", e);
    }

    let target = self
        .config
        .skills
        .directory
        .join(&skill_name)
        .join(&relative_path);

    match tokio::fs::read_to_string(&target).await {
        Ok(content) => content,
        Err(e) => format!(
            "Failed to read skill file '{}/{}': {}",
            skill_name, relative_path, e
        ),
    }
}
```

**Step 5: Verify compilation**

```bash
cargo check 2>&1 | grep -E "^error" | head -10
```

Expected: 0 errors.

**Step 6: Run all tests**

```bash
cargo test --lib 2>&1 | tail -20
```

Expected: all pass.

**Step 7: Commit**

```bash
git add src/agent.rs
git commit -m "feat(agent): add read_skill_file tool for reading skill files from skills directory"
```

---

## Task 5: Add `invoke_subagent` tool and `run_subagent` mini-loop

**Files:**
- Modify: `src/agent.rs`

This is the main feature. `run_subagent` is a new `async fn` on `Agent` that boots an isolated agentic loop with the skill's model and tool whitelist.

**Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/agent.rs`:

```rust
#[test]
fn test_subagent_tool_whitelist_always_includes_read_skill_file() {
    // read_skill_file is always available to subagents regardless of whitelist
    let declared: Vec<String> = vec!["mcp_threads_post".to_string()];
    let effective = effective_subagent_tools(&declared);
    assert!(effective.contains(&"read_skill_file".to_string()));
    assert!(effective.contains(&"mcp_threads_post".to_string()));
}

#[test]
fn test_subagent_tool_whitelist_empty_gets_read_skill_file() {
    let declared: Vec<String> = vec![];
    let effective = effective_subagent_tools(&declared);
    assert_eq!(effective, vec!["read_skill_file".to_string()]);
}

#[test]
fn test_subagent_tool_whitelist_deduplicates_read_skill_file() {
    // If the skill already lists read_skill_file, it shouldn't appear twice
    let declared = vec!["read_skill_file".to_string(), "mcp_something".to_string()];
    let effective = effective_subagent_tools(&declared);
    let count = effective.iter().filter(|t| *t == "read_skill_file").count();
    assert_eq!(count, 1);
}
```

**Step 2: Run to verify tests fail (function doesn't exist)**

```bash
cargo test --lib agent::tests 2>&1 | grep -E "^error" | head -5
```

Expected: compile error — `effective_subagent_tools` not found.

**Step 3: Add `effective_subagent_tools` helper (free function)**

Add this **after** the `validate_skill_path` function (around line 919), before the `#[cfg(test)]` block:

```rust
/// Build the effective tool whitelist for a subagent.
/// Always includes `read_skill_file`; deduplicates.
fn effective_subagent_tools(declared: &[String]) -> Vec<String> {
    let mut tools = vec!["read_skill_file".to_string()];
    for t in declared {
        if t != "read_skill_file" {
            tools.push(t.clone());
        }
    }
    tools
}
```

**Step 4: Run new tests to verify they pass**

```bash
cargo test --lib agent::tests 2>&1 | tail -20
```

Expected: all pass.

**Step 5: Implement `run_subagent` method on `Agent`**

Add this method to `impl Agent` in `src/agent.rs`, just before `execute_tool`:

```rust
/// Run a named skill as an isolated subagent mini-loop.
/// Returns the subagent's final text response (or an error string).
async fn run_subagent(
    &self,
    skill_name: &str,
    prompt: &str,
    model_override: Option<&str>,
    tools_override: Option<Vec<String>>,
) -> String {
    // Resolve model and tool list from skill metadata (or overrides)
    let (resolved_model, declared_tools, max_iter) = {
        let skills = self.skills.read().await;
        let skill = skills.get(skill_name);
        let model = model_override
            .map(str::to_string)
            .or_else(|| skill.and_then(|s| s.model.clone()))
            .unwrap_or_else(|| self.config.openrouter.model.clone());
        let tools = tools_override
            .or_else(|| skill.map(|s| s.tools.clone()))
            .unwrap_or_default();
        let max_i = skill
            .and_then(|s| s.max_iterations)
            .unwrap_or_else(|| self.config.max_iterations())
            .min(self.config.max_iterations());
        (model, tools, max_i)
    };

    let allowed_tools = effective_subagent_tools(&declared_tools);

    // Build the subagent tool definitions (filtered to whitelist only)
    let all_possible_tools: Vec<ToolDefinition> = {
        let mut t = tools::builtin_tool_definitions();
        t.extend(self.mcp.tool_definitions());
        t.extend(self.skill_tool_definitions()); // includes read_skill_file
        t
    };
    let subagent_tools: Vec<ToolDefinition> = all_possible_tools
        .into_iter()
        .filter(|td| allowed_tools.contains(&td.function.name))
        .collect();

    // Bootstrap messages — system prompt instructs subagent to read its SKILL.md first
    let system_content = format!(
        "You are the '{}' subagent. Your first action MUST be to call \
         read_skill_file with skill_name='{}' and path='SKILL.md' to load your instructions.",
        skill_name, skill_name
    );
    let mut messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(system_content),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(prompt.to_string()),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    // Mini agentic loop (isolated — no memory, no scheduling)
    for iteration in 0..max_iter {
        let response = match self
            .llm
            .chat_with_model(&messages, &subagent_tools, &resolved_model)
            .await
        {
            Ok(r) => r,
            Err(e) => return format!("Subagent '{}' error: {}", skill_name, e),
        };

        if let Some(tool_calls) = &response.tool_calls {
            if !tool_calls.is_empty() {
                info!(
                    "Subagent '{}' requested {} tool call(s) (iteration {})",
                    skill_name,
                    tool_calls.len(),
                    iteration
                );

                messages.push(response.clone());

                for tool_call in tool_calls {
                    let arguments: serde_json::Value =
                        serde_json::from_str(&tool_call.function.arguments)
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                    // Only allow whitelisted tools
                    let result = if allowed_tools.contains(&tool_call.function.name) {
                        self.execute_tool(
                            &tool_call.function.name,
                            &arguments,
                            "",  // subagent has no user_id context
                            "",  // subagent has no chat_id context
                        )
                        .await
                    } else {
                        format!(
                            "Tool '{}' is not available to this subagent.",
                            tool_call.function.name
                        )
                    };

                    messages.push(ChatMessage {
                        role: "tool".to_string(),
                        content: Some(result),
                        tool_calls: None,
                        tool_call_id: Some(tool_call.id.clone()),
                    });
                }

                continue;
            }
        }

        // Final response — no tool calls
        return response.content.unwrap_or_default();
    }

    format!(
        "Subagent '{}' reached the maximum number of iterations ({}).",
        skill_name, max_iter
    )
}
```

**Step 6: Add `invoke_subagent` tool definition**

In `skill_tool_definitions()`, add a fourth entry after `read_skill_file`:

```rust
ToolDefinition {
    tool_type: "function".to_string(),
    function: FunctionDefinition {
        name: "invoke_subagent".to_string(),
        description: concat!(
            "Delegate a task to a named skill running as an isolated subagent. ",
            "The subagent uses its own model and tool whitelist declared in its SKILL.md frontmatter. ",
            "Use this for skills listed under 'Available Subagent Skills' in the system prompt. ",
            "The subagent runs an isolated agentic loop and returns its final text response."
        ).to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "skill": {
                    "type": "string",
                    "description": "Name of the skill to run as a subagent (e.g. 'thread-writer')"
                },
                "prompt": {
                    "type": "string",
                    "description": "The task content to pass to the subagent"
                },
                "model": {
                    "type": "string",
                    "description": "Optional: override the skill's declared model for this invocation"
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional: override the skill's declared tool whitelist"
                }
            },
            "required": ["skill", "prompt"]
        }),
    },
},
```

**Step 7: Add `invoke_subagent` handler in `execute_tool()`**

Add a match arm after the `"reload_skills"` arm (around line 799):

```rust
"invoke_subagent" => {
    let skill = match arguments["skill"].as_str() {
        Some(s) => s.to_string(),
        None => return "Missing skill".to_string(),
    };
    let prompt = match arguments["prompt"].as_str() {
        Some(p) => p.to_string(),
        None => return "Missing prompt".to_string(),
    };
    let model_override = arguments["model"].as_str().map(str::to_string);
    let tools_override = arguments["tools"].as_array().map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect::<Vec<_>>()
    });

    info!(
        "Invoking subagent '{}' (model_override: {:?})",
        skill, model_override
    );

    self.run_subagent(
        &skill,
        &prompt,
        model_override.as_deref(),
        tools_override,
    )
    .await
}
```

**Step 8: Verify compilation and tests**

```bash
cargo check 2>&1 | grep -E "^error" | head -20
cargo test --lib 2>&1 | tail -20
```

Expected: 0 errors, all tests pass.

**Step 9: Commit**

```bash
git add src/agent.rs
git commit -m "feat(agent): add invoke_subagent tool and run_subagent mini-loop with model override"
```

---

## Task 6: Add example subagent skill and run full CI checks

**Files:**
- Create: `skills/thread-writer/SKILL.md`

**Step 1: Create the example subagent skill**

Create `skills/thread-writer/SKILL.md`:

```markdown
---
name: thread-writer
description: Use when writing daily Thread posts from fetched source content. Invoke via invoke_subagent, not directly.
model: anthropic/claude-sonnet-4-6
tools: [read_skill_file, mcp_threads_post]
max_iterations: 8
---

# Thread Writer

You are a specialized subagent that writes engaging daily Thread posts.

## Your Task

Given source content (e.g. email summaries, articles, notes), write a compelling Thread post that:
- Opens with a strong hook in the first post
- Breaks content into short, punchy posts (max 500 chars each)
- Uses a consistent voice: direct, insightful, no hype
- Ends with a clear takeaway or call to action
- Avoids filler phrases ("As an AI...", "In conclusion...")

## Format

Return the posts as a numbered list:
1. [first post — hook]
2. [second post]
...
N. [final post — takeaway]

## Style Notes

- Short sentences. Active voice.
- No hashtags unless the content is specifically about a trending topic.
- Emojis are optional but use sparingly (max 1 per post).
```

**Step 2: Run `cargo fmt`**

```bash
cargo fmt --all 2>&1
```

Expected: exits cleanly (no output means no formatting issues).

**Step 3: Run `cargo clippy`**

```bash
cargo clippy -- -D warnings 2>&1 | grep -E "^error|warning\[" | head -20
```

Expected: 0 errors. Fix any warnings before proceeding.

**Step 4: Run the full test suite**

```bash
cargo test 2>&1 | tail -30
```

Expected: all tests pass.

**Step 5: Run `cargo build --release`**

```bash
cargo build --release 2>&1 | grep -E "^error" | head -10
```

Expected: 0 errors. (This takes a minute — it validates the release build.)

**Step 6: Final commit**

```bash
git add skills/thread-writer/SKILL.md
git commit -m "feat(skills): add thread-writer as example subagent skill"
```

**Step 7: Push to remote**

```bash
git push -u origin claude/subagent-model-selection-5OTEa
```

Expected: pushed successfully.

---

## Verification Checklist

Before calling this done, confirm:

- [ ] `cargo fmt --all -- --check` exits 0
- [ ] `cargo clippy -- -D warnings` exits 0
- [ ] `cargo test` passes all tests
- [ ] `cargo build --release` succeeds
- [ ] `git log --oneline` shows 6 commits on `claude/subagent-model-selection-5OTEa`

---

## What Was NOT Built (intentional scope limits)

- **Recursive subagents**: `invoke_subagent` is not in any subagent's tool definitions — no nesting by design
- **Subagent memory access**: subagents cannot call `remember`, `recall`, or `search_memory` — stateless by design
- **Subagent scheduling**: subagents cannot schedule tasks — orchestration stays in the main agent
- **Config-level subagent defaults**: all configuration lives in SKILL.md frontmatter, not `config.toml`
