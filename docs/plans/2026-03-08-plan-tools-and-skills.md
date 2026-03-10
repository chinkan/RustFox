# Plan Tools + Skills Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `plan_create`, `plan_update`, `plan_view` built-in tools to `src/tools.rs`, update the default system prompt in `src/config.rs`, and create two new bot skills: `code-interpreter` (subagent) and `problem-solver` (orchestration subagent with plan tools).

**Architecture:** Plan state is stored as `.rustfox_plan.json` in the sandbox directory — no new shared state needed, fits cleanly into the existing stateless `execute_builtin_tool(tool_name, arguments, sandbox_dir)` signature. Skills are YAML-frontmatter markdown files dropped into `skills/` — no code changes needed for skill loading.

**Tech Stack:** Rust 2021, `serde_json` (already imported in `tools.rs`), `tokio::fs` (already used), `anyhow` for errors.

---

## Task 1: Add `plan_create` tool

**Files:**
- Modify: `src/tools.rs:47-126` (add to `builtin_tool_definitions()`)
- Modify: `src/tools.rs:128-237` (add match arm in `execute_builtin_tool()`)
- Test: `src/tools.rs` (add `#[cfg(test)] mod tests` at bottom)

**Step 1: Write the failing test**

Add to the bottom of `src/tools.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_plan_create_writes_json() {
        let dir = tempdir().unwrap();
        let args = serde_json::json!({
            "title": "My Plan",
            "steps": ["Step A", "Step B"]
        });
        let result = execute_builtin_tool("plan_create", &args, dir.path())
            .await
            .unwrap();
        assert!(result.contains("My Plan"));
        assert!(result.contains("Step A"));
        assert!(result.contains("Step B"));

        let plan_path = dir.path().join(".rustfox_plan.json");
        assert!(plan_path.exists());
        let content = std::fs::read_to_string(plan_path).unwrap();
        let plan: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(plan["title"].as_str().unwrap(), "My Plan");
        assert_eq!(plan["steps"][0]["status"].as_str().unwrap(), "todo");
        assert_eq!(plan["steps"][1]["description"].as_str().unwrap(), "Step B");
    }
}
```

Note: `tempfile` crate may need adding. Check `Cargo.toml` first. If missing, add:
```toml
[dev-dependencies]
tempfile = "3"
```

**Step 2: Run test to verify it fails**

```bash
cargo test test_plan_create_writes_json -- --nocapture
```
Expected: FAIL — `Unknown built-in tool: plan_create`

**Step 3: Add `plan_create` to `builtin_tool_definitions()`**

Insert after the `execute_command` block (before the closing `]`) in `builtin_tool_definitions()`:

```rust
ToolDefinition {
    tool_type: "function".to_string(),
    function: FunctionDefinition {
        name: "plan_create".to_string(),
        description: "Create a new execution plan with ordered steps. Call this BEFORE starting any multi-step task. Stores the plan in the sandbox for tracking.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short title describing the overall goal"
                },
                "steps": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Ordered list of step descriptions"
                }
            },
            "required": ["title", "steps"]
        }),
    },
},
```

**Step 4: Add `plan_create` match arm in `execute_builtin_tool()`**

Insert before the `_ =>` catch-all arm:

```rust
"plan_create" => {
    let title = arguments["title"]
        .as_str()
        .context("Missing 'title' argument")?;
    let steps = arguments["steps"]
        .as_array()
        .context("Missing 'steps' argument")?;

    let plan_steps: Vec<serde_json::Value> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "id": i,
                "description": s.as_str().unwrap_or(""),
                "status": "todo",
                "notes": ""
            })
        })
        .collect();

    let plan = json!({
        "title": title,
        "steps": plan_steps
    });

    let plan_path = sandbox_dir.join(".rustfox_plan.json");
    tokio::fs::write(&plan_path, serde_json::to_string_pretty(&plan)?)
        .await
        .context("Failed to write plan file")?;

    info!("Plan created: {} ({} steps)", title, plan_steps.len());

    let checklist: Vec<String> = plan_steps
        .iter()
        .map(|s| {
            format!(
                "[ ] {}: {}",
                s["id"].as_u64().unwrap_or(0),
                s["description"].as_str().unwrap_or("")
            )
        })
        .collect();

    Ok(format!(
        "Plan created: {}\n\n{}",
        title,
        checklist.join("\n")
    ))
}
```

**Step 5: Run test to verify it passes**

```bash
cargo test test_plan_create_writes_json -- --nocapture
```
Expected: PASS

**Step 6: Commit**

```bash
git add src/tools.rs Cargo.toml
git commit -m "feat: add plan_create built-in tool"
```

---

## Task 2: Add `plan_update` tool

**Files:**
- Modify: `src/tools.rs` (tool definition + match arm + test)

**Step 1: Write the failing test**

Add to `mod tests` in `src/tools.rs`:

```rust
#[tokio::test]
async fn test_plan_update_changes_step_status() {
    let dir = tempdir().unwrap();

    // Create a plan first
    let create_args = serde_json::json!({
        "title": "Test Plan",
        "steps": ["Step A", "Step B"]
    });
    execute_builtin_tool("plan_create", &create_args, dir.path())
        .await
        .unwrap();

    // Update step 0 to in_progress
    let update_args = serde_json::json!({
        "step_id": 0,
        "status": "in_progress"
    });
    let result = execute_builtin_tool("plan_update", &update_args, dir.path())
        .await
        .unwrap();
    assert!(result.contains("in_progress") || result.contains("→"));

    // Verify the JSON was updated
    let plan_path = dir.path().join(".rustfox_plan.json");
    let content = std::fs::read_to_string(plan_path).unwrap();
    let plan: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(plan["steps"][0]["status"].as_str().unwrap(), "in_progress");
    assert_eq!(plan["steps"][1]["status"].as_str().unwrap(), "todo");
}

#[tokio::test]
async fn test_plan_update_stores_notes() {
    let dir = tempdir().unwrap();

    let create_args = serde_json::json!({
        "title": "Test",
        "steps": ["Only step"]
    });
    execute_builtin_tool("plan_create", &create_args, dir.path())
        .await
        .unwrap();

    let update_args = serde_json::json!({
        "step_id": 0,
        "status": "done",
        "notes": "Completed successfully"
    });
    execute_builtin_tool("plan_update", &update_args, dir.path())
        .await
        .unwrap();

    let plan_path = dir.path().join(".rustfox_plan.json");
    let content = std::fs::read_to_string(plan_path).unwrap();
    let plan: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(plan["steps"][0]["notes"].as_str().unwrap(), "Completed successfully");
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test test_plan_update -- --nocapture
```
Expected: FAIL — `Unknown built-in tool: plan_update`

**Step 3: Add `plan_update` to `builtin_tool_definitions()`**

Insert after `plan_create` definition:

```rust
ToolDefinition {
    tool_type: "function".to_string(),
    function: FunctionDefinition {
        name: "plan_update".to_string(),
        description: "Update a step's status in the active plan. Call before starting a step (in_progress) and after finishing (done or failed).".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "step_id": {
                    "type": "integer",
                    "description": "Zero-based index of the step to update"
                },
                "status": {
                    "type": "string",
                    "enum": ["todo", "in_progress", "done", "failed"],
                    "description": "New status for the step"
                },
                "notes": {
                    "type": "string",
                    "description": "Optional notes — result summary, error message, etc."
                }
            },
            "required": ["step_id", "status"]
        }),
    },
},
```

**Step 4: Add `plan_update` match arm**

Insert before the `_ =>` catch-all:

```rust
"plan_update" => {
    let step_id = arguments["step_id"]
        .as_u64()
        .context("Missing 'step_id' argument")? as usize;
    let status = arguments["status"]
        .as_str()
        .context("Missing 'status' argument")?;
    let notes = arguments
        .get("notes")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let plan_path = sandbox_dir.join(".rustfox_plan.json");
    let content = tokio::fs::read_to_string(&plan_path)
        .await
        .context("No active plan found. Call plan_create first.")?;
    let mut plan: serde_json::Value = serde_json::from_str(&content)
        .context("Invalid plan file format")?;

    let steps = plan["steps"]
        .as_array_mut()
        .context("Invalid plan: missing steps array")?;
    let step = steps
        .get_mut(step_id)
        .with_context(|| format!("Step {} not found in plan", step_id))?;

    step["status"] = json!(status);
    step["notes"] = json!(notes);

    tokio::fs::write(&plan_path, serde_json::to_string_pretty(&plan)?)
        .await
        .context("Failed to update plan file")?;

    let icon = match status {
        "done" => "[x]",
        "failed" => "[!]",
        "in_progress" => "[>]",
        _ => "[ ]",
    };

    info!("Plan step {} → {}", step_id, status);
    Ok(format!(
        "{} Step {}: {} [{}]{}",
        icon,
        step_id,
        step["description"].as_str().unwrap_or(""),
        status,
        if notes.is_empty() {
            String::new()
        } else {
            format!(" — {}", notes)
        }
    ))
}
```

**Step 5: Run tests to verify they pass**

```bash
cargo test test_plan_update -- --nocapture
```
Expected: PASS (both tests)

**Step 6: Commit**

```bash
git add src/tools.rs
git commit -m "feat: add plan_update built-in tool"
```

---

## Task 3: Add `plan_view` tool

**Files:**
- Modify: `src/tools.rs` (tool definition + match arm + test)

**Step 1: Write the failing test**

Add to `mod tests`:

```rust
#[tokio::test]
async fn test_plan_view_renders_checklist() {
    let dir = tempdir().unwrap();

    // Create plan
    let create_args = serde_json::json!({
        "title": "My Plan",
        "steps": ["Alpha", "Beta", "Gamma"]
    });
    execute_builtin_tool("plan_create", &create_args, dir.path())
        .await
        .unwrap();

    // Mark step 0 done, step 1 in_progress
    execute_builtin_tool(
        "plan_update",
        &serde_json::json!({ "step_id": 0, "status": "done", "notes": "ok" }),
        dir.path(),
    )
    .await
    .unwrap();
    execute_builtin_tool(
        "plan_update",
        &serde_json::json!({ "step_id": 1, "status": "in_progress" }),
        dir.path(),
    )
    .await
    .unwrap();

    let result = execute_builtin_tool("plan_view", &serde_json::json!({}), dir.path())
        .await
        .unwrap();

    assert!(result.contains("My Plan"));
    assert!(result.contains("[x]")); // done
    assert!(result.contains("[>]")); // in_progress
    assert!(result.contains("[ ]")); // todo
    assert!(result.contains("Alpha"));
    assert!(result.contains("ok")); // notes shown
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test test_plan_view_renders_checklist -- --nocapture
```
Expected: FAIL — `Unknown built-in tool: plan_view`

**Step 3: Add `plan_view` to `builtin_tool_definitions()`**

Insert after `plan_update` definition:

```rust
ToolDefinition {
    tool_type: "function".to_string(),
    function: FunctionDefinition {
        name: "plan_view".to_string(),
        description: "View the current plan as a checklist. Call at the end of execution to review progress before synthesising the final answer.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
    },
},
```

**Step 4: Add `plan_view` match arm**

```rust
"plan_view" => {
    let plan_path = sandbox_dir.join(".rustfox_plan.json");
    let content = tokio::fs::read_to_string(&plan_path)
        .await
        .context("No active plan found. Call plan_create first.")?;
    let plan: serde_json::Value =
        serde_json::from_str(&content).context("Invalid plan file format")?;

    let title = plan["title"].as_str().unwrap_or("Untitled Plan");
    let steps = plan["steps"]
        .as_array()
        .context("Invalid plan: missing steps array")?;

    let lines: Vec<String> = steps
        .iter()
        .map(|s| {
            let icon = match s["status"].as_str().unwrap_or("todo") {
                "done" => "[x]",
                "failed" => "[!]",
                "in_progress" => "[>]",
                _ => "[ ]",
            };
            let desc = s["description"].as_str().unwrap_or("");
            let notes = s["notes"].as_str().unwrap_or("");
            if notes.is_empty() {
                format!("{} {}", icon, desc)
            } else {
                format!("{} {} — {}", icon, desc, notes)
            }
        })
        .collect();

    Ok(format!("# {}\n\n{}", title, lines.join("\n")))
}
```

**Step 5: Run test to verify it passes**

```bash
cargo test test_plan_view_renders_checklist -- --nocapture
```
Expected: PASS

**Step 6: Run all plan tests and clippy**

```bash
cargo test test_plan -- --nocapture
cargo clippy -- -D warnings
```
Expected: All pass, no warnings

**Step 7: Commit**

```bash
git add src/tools.rs
git commit -m "feat: add plan_view built-in tool"
```

---

## Task 4: Update default system prompt

**Files:**
- Modify: `src/config.rs:116-121`

**Step 1: Replace `default_system_prompt()`**

Replace the function body at `src/config.rs:116-121`:

```rust
fn default_system_prompt() -> String {
    "You are RustFox — an AI assistant with tools, memory, and skills.\n\
     \n\
     ## Identity\n\
     Your name is RustFox, but your soul (if loaded) overrides any default identity.\n\
     Soul takes precedence over everything.\n\
     \n\
     ## Priority Chain\n\
     When responding, apply context in this order:\n\
     1. SOUL — your loaded soul/identity defines who you are and how you speak\n\
     2. MEMORY — recalled user preferences, corrections, and context from past conversations\n\
     3. CONTEXT — the current conversation and user request\n\
     \n\
     ## Skills First\n\
     You have skills. For every user request:\n\
     - Check if a relevant skill exists (listed in your system context)\n\
     - If yes: load and follow it via read_skill_file before responding\n\
     - If no matching skill: reason directly, or use code-interpreter for computation/scripting tasks\n\
     - For complex multi-step problems: invoke the problem-solver subagent\n\
     \n\
     ## Sandbox\n\
     File and command tools operate only within the allowed sandbox directory."
        .to_string()
}
```

**Step 2: Verify existing config tests still pass**

```bash
cargo test --lib config -- --nocapture
```
Expected: All 3 existing config tests pass (they don't test the system prompt text, only config parsing)

**Step 3: Run cargo check**

```bash
cargo check
```
Expected: No errors

**Step 4: Commit**

```bash
git add src/config.rs
git commit -m "feat: update default system prompt with skills-first priority chain"
```

---

## Task 5: Create `code-interpreter` skill

**Files:**
- Create: `skills/code-interpreter/SKILL.md`

No Rust code changes needed — the skills loader auto-discovers files.

**Step 1: Create the directory and file**

Create `skills/code-interpreter/SKILL.md`:

```markdown
---
name: code-interpreter
description: Execute code snippets and scripts in the sandbox. Supports Python 3 and Node.js. Use for calculations, data processing, file generation, and scripting tasks.
tags: [code, execution, scripting]
model: qwen/qwen3-235b-a22b
tools:
  - read_file
  - write_file
  - execute_command
---

# Code Interpreter

You are a code execution agent. Your job is to run code and return results.

## Workflow

1. **Receive** a task prompt (code to run, or a problem to solve with code)
2. **Choose** the right runtime: Python 3 (`python3`) or Node.js (`node`)
3. **Write** the script to the sandbox: e.g. `tmp_script.py`
4. **Execute** it with `execute_command`
5. **Return** stdout/stderr output clearly, noting success or failure

## Rules

- Always write scripts to the sandbox directory
- Clean up temp files after execution with `execute_command("rm tmp_script.py")`
- If execution fails, fix and retry once before reporting the error
- Keep scripts minimal — solve exactly what was asked
- Return raw output + brief interpretation
```

**Step 2: Verify skill file is valid YAML frontmatter**

```bash
python3 -c "
import re, sys
content = open('skills/code-interpreter/SKILL.md').read()
m = re.match(r'^---\n(.*?)\n---', content, re.DOTALL)
print('Frontmatter OK' if m else 'MISSING frontmatter')
"
```
Expected: `Frontmatter OK`

**Step 3: Commit**

```bash
git add skills/code-interpreter/SKILL.md
git commit -m "feat: add code-interpreter subagent skill"
```

---

## Task 6: Create `problem-solver` skill

**Files:**
- Create: `skills/problem-solver/SKILL.md`

**Step 1: Create the directory and file**

Create `skills/problem-solver/SKILL.md`:

```markdown
---
name: problem-solver
description: Decompose and solve complex multi-step problems. Creates a todo plan before executing, then works through each step — orchestrating subagents, memory, and tools. Inspired by LangChain plan-and-execute agents.
tags: [orchestration, reasoning, planning]
model: qwen/qwen3-235b-a22b
tools:
  - plan_create
  - plan_update
  - plan_view
  - read_skill_file
  - invoke_subagent
  - read_file
  - write_file
  - execute_command
  - remember
  - recall
  - search_memory
---

# Problem Solver

You are a plan-and-execute orchestration agent. You ALWAYS plan before acting.

## Workflow

### Step 1 — Plan
Before doing anything else, call `plan_create` with a clear title and ordered steps.

### Step 2 — Execute
Work through each step in order:
1. Call `plan_update(step_id, "in_progress")` before starting each step
2. Execute the step using the best tool or subagent
3. Call `plan_update(step_id, "done", notes="result summary")` when complete
4. If a step fails: `plan_update(step_id, "failed", notes="reason")` → adapt and continue

### Step 3 — Replan (if needed)
If a step fails and the rest of the plan is no longer valid, call `plan_create` again with revised steps.

### Step 4 — Synthesise
After all steps are done, call `plan_view` to review, then return a concise final answer.

## Delegation Rules

- Code/scripting/computation → `invoke_subagent(skill="code-interpreter", ...)`
- Memory lookup → `recall` / `search_memory`
- File I/O → `read_file` / `write_file` directly

## Examples

**"What's the most expensive item in my budget CSV?"**
```
plan_create("Analyse budget CSV", [
  "Read the CSV file",
  "Parse and find maximum value with code-interpreter",
  "Return result"
])
→ execute each step → synthesise
```

**"Debug why my script crashes on large inputs"**
```
plan_create("Debug crash on large inputs", [
  "Read the script",
  "Reproduce crash with code-interpreter",
  "Identify root cause",
  "Propose fix"
])
→ execute each step → synthesise
```

**"Summarise my last 3 conversations with Alice"**
```
plan_create("Summarise Alice conversations", [
  "Search memory for Alice",
  "Extract last 3 conversation summaries",
  "Synthesise into readable summary"
])
→ execute each step → synthesise
```

## Rules

- NEVER skip plan_create — always plan first
- Mark steps in_progress before starting, done/failed after
- Never guess when you can compute or look up
- Return a concise final answer, not a transcript of every step
```

**Step 2: Verify frontmatter**

```bash
python3 -c "
import re
content = open('skills/problem-solver/SKILL.md').read()
m = re.match(r'^---\n(.*?)\n---', content, re.DOTALL)
print('Frontmatter OK' if m else 'MISSING frontmatter')
"
```
Expected: `Frontmatter OK`

**Step 3: Commit**

```bash
git add skills/problem-solver/SKILL.md
git commit -m "feat: add problem-solver orchestration subagent skill"
```

---

## Task 7: Final verification

**Step 1: Run all tests**

```bash
cargo test -- --nocapture
```
Expected: All tests pass (including the 3 new plan tool tests + existing config tests)

**Step 2: Run clippy**

```bash
cargo clippy -- -D warnings
```
Expected: No warnings

**Step 3: Run fmt check**

```bash
cargo fmt --all -- --check
```
Expected: No formatting issues (run `cargo fmt` to fix if needed, then recheck)

**Step 4: Check all new skill files are present**

```bash
ls skills/code-interpreter/ skills/problem-solver/
```
Expected:
```
skills/code-interpreter/:
SKILL.md

skills/problem-solver/:
SKILL.md
```

**Step 5: Push branch**

```bash
git push -u origin claude/add-programming-skills-SC8ph
```
