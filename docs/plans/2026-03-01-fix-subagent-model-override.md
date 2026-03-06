# Fix Subagent Model Override Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix three bugs that together cause the "subagent not working with overridden model" symptom: a `build_context()` regression that hides orchestration skills, silent API errors in `run_subagent`, and no warning when declared subagent tools aren't available at runtime.

**Architecture:** Three focused patches to `src/skills/mod.rs` and `src/agent.rs`. No new dependencies. All changes are additive or small reversions. TDD throughout.

**Tech Stack:** Rust 2021, Tokio, `tracing`, `anyhow`. No new crates.

---

## Root Cause Summary

Investigation found three independent bugs (confirmed by reading git diff and source):

### Bug 1 — `build_context()` regression (primary cause)

Commit `2706d91` changed instruction skills (no `model` field in frontmatter) from **full body injection** to **metadata-only with `read_skill_file` hint**. This contradicts the approved design doc and the CLAUDE.md documentation, which both say:

> Skills can be **instruction skills** (no `model` in frontmatter; **full body injected** into the system prompt)

Impact: `daily-news-to-threads` is an instruction skill. Before commit `2706d91`, the agent automatically knew the orchestration steps (including `invoke_subagent(skill="thread-writer-hk", model="anthropic/claude-sonnet-4-6")`). After the commit, the agent only sees a metadata hint and must proactively call `read_skill_file` first. If it skips that step — or calls `invoke_subagent` with wrong parameters — the model override never fires.

**Fix:** Revert `build_context()` instruction skill handling to full body injection. Subagent skills (with `model`) stay metadata-only (unchanged).

### Bug 2 — Silent API errors in `run_subagent`

In `src/agent.rs:655`:
```rust
Err(e) => return format!("Subagent '{}' error: {}", skill_name, e),
```
When OpenRouter rejects the model ID (e.g., `anthropic/claude-sonnet-4-6` not found) there is **no `error!()` log**. The only trace is the returned string, which the main agent receives as a tool result and may try to parse as content. Users see confusing output instead of a clear error.

**Fix:** Add `error!()` log before returning the error string.

### Bug 3 — Silent tool mismatch

In `run_subagent`, `subagent_tools` is filtered from `all_possible_tools`. If a skill declares `tools: [read_skill_file, mcp_fetch_fetch]` but the `fetch` MCP server is not configured, `mcp_fetch_fetch` is silently absent from `subagent_tools`. The subagent's system prompt says to verify URLs using fetch, but the tool is not available — causing the subagent to fail or skip mandatory steps without any log explaining why.

**Fix:** Add `warn!()` when any declared tool is not available at subagent launch time.

---

## Reference Files

Read before starting:
- `src/skills/mod.rs` — `build_context()` you will fix (~113 lines)
- `src/agent.rs` — `run_subagent()` (lines 580-716) and `effective_subagent_tools()` (lines 1201-1211)
- `docs/plans/2026-02-23-subagent-model-selection.md` — original spec (instruction skills inject full body)

---

## Task 1: Fix `build_context()` regression — restore instruction skill full body injection

**Files:**
- Modify: `src/skills/mod.rs`

### Step 1: Write the failing test

The current test `test_build_context_instruction_skill_metadata_only` asserts the wrong behavior (metadata-only). We need to replace it with a test that asserts the correct behavior (full body injected), and add a regression test that ensures the behavior split is correct.

Find the `#[cfg(test)] mod tests` block at the bottom of `src/skills/mod.rs`. Replace `test_build_context_instruction_skill_metadata_only` with:

```rust
#[test]
fn test_build_context_instruction_skill_injects_full_body() {
    // Instruction skills (no model): full body is injected into system prompt.
    // This is the spec behavior per design doc and CLAUDE.md.
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
    // metadata is also present (as a section header)
    assert!(ctx.contains("my-skill"));
}
```

Also replace `test_build_context_mixed_skills` with:

```rust
#[test]
fn test_build_context_mixed_skills() {
    // Instruction skill body is injected; subagent skill body is NOT.
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
    // Instruction skill: full body present
    assert!(ctx.contains("Follow these instructions."));
    // Subagent skill: body NOT present
    assert!(!ctx.contains("Secret subagent body."));
    // Both have invoke/load hints
    assert!(ctx.contains("invoke_subagent"));
}
```

### Step 2: Run to verify tests fail (wrong behavior currently asserted)

```bash
cargo test -- skills::mod 2>&1 | tail -20
```

Expected: 2 test failures (the tests assert the correct behavior but the code does the opposite).

### Step 3: Fix `build_context()` in `src/skills/mod.rs`

Replace the entire `build_context()` method (lines 56–103) with:

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
        context.push_str(
            "You have the following skills available. When relevant, follow these instructions:\n\n",
        );
        context.push_str(&instruction_section);
    }

    if !subagent_section.is_empty() {
        if !instruction_section.is_empty() {
            context.push('\n');
        }
        context.push_str("## Available Subagent Skills\n\n");
        context.push_str("Delegate these tasks using `invoke_subagent`:\n\n");
        context.push_str(&subagent_section);
    }

    context
}
```

### Step 4: Run tests to verify they pass

```bash
cargo test -- skills 2>&1 | tail -20
```

Expected: all skills tests pass.

### Step 5: Run the full test suite

```bash
cargo test 2>&1 | tail -10
```

Expected: all tests pass.

### Step 6: Commit

```bash
git add src/skills/mod.rs
git commit -m "fix(skills): restore instruction skill full body injection in build_context"
```

---

## Task 2: Add error logging and tool-availability warning to `run_subagent`

**Files:**
- Modify: `src/agent.rs`

### Step 1: Write failing test for tool-mismatch warning helper

Add to the `#[cfg(test)] mod tests` block in `src/agent.rs`:

```rust
#[test]
fn test_missing_subagent_tools_detected() {
    // If a declared tool is not in all_possible, it should be detectable.
    // This tests the helper that warns at launch time.
    let declared = vec!["read_skill_file".to_string(), "mcp_nonexistent_tool".to_string()];
    let available: Vec<String> = vec!["read_skill_file".to_string()]; // mcp_nonexistent_tool missing
    let missing = missing_subagent_tools(&declared, &available);
    assert_eq!(missing, vec!["mcp_nonexistent_tool".to_string()]);
}

#[test]
fn test_missing_subagent_tools_empty_when_all_present() {
    let declared = vec!["read_skill_file".to_string()];
    let available = vec!["read_skill_file".to_string(), "write_file".to_string()];
    let missing = missing_subagent_tools(&declared, &available);
    assert!(missing.is_empty());
}
```

### Step 2: Run to verify tests fail (function doesn't exist)

```bash
cargo test -- agent::tests 2>&1 | grep "^error" | head -5
```

Expected: compile error — `missing_subagent_tools` not found.

### Step 3: Add `missing_subagent_tools` helper function

Add this function right after `effective_subagent_tools` (around line 1211):

```rust
/// Return declared tools that are not present in the set of all available tool names.
/// Used to warn at subagent launch when the whitelist references unavailable tools.
fn missing_subagent_tools(declared: &[String], available_names: &[String]) -> Vec<String> {
    declared
        .iter()
        .filter(|t| !available_names.contains(t))
        .cloned()
        .collect()
}
```

### Step 4: Run tests to verify helper tests pass

```bash
cargo test -- agent::tests 2>&1 | tail -20
```

Expected: all agent tests pass including the two new ones.

### Step 5: Wire in the warning and error logging in `run_subagent`

In `run_subagent`, after `let subagent_tools: Vec<ToolDefinition> = ...` (around line 624), add the warning block:

```rust
// Warn if any declared tool is not available at runtime (e.g. MCP server not configured).
let available_names: Vec<String> = all_possible_tools
    .iter()
    .map(|td| td.function.name.clone())
    .collect();
// Re-use all_possible_tools before it's consumed by the filter above — we need names only.
// NOTE: the filter above consumes all_possible_tools; rebuild names from subagent_tools + declared.
let actually_available_names: Vec<String> = subagent_tools
    .iter()
    .map(|td| td.function.name.clone())
    .collect();
let missing = missing_subagent_tools(&allowed_tools, &actually_available_names);
if !missing.is_empty() {
    warn!(
        "Subagent '{}': declared tools not available at runtime (MCP server not configured?): {:?}",
        skill_name, missing
    );
}
```

**Important**: The code above references `all_possible_tools` AFTER it's been moved into the iterator. Restructure the tool building to avoid the move issue. Replace the current tool-building block (lines 614–624) with:

```rust
// Build the subagent tool definitions (filtered to whitelist only)
let all_possible_tools: Vec<ToolDefinition> = {
    let mut t = tools::builtin_tool_definitions();
    t.extend(self.mcp.tool_definitions());
    t.extend(self.skill_tool_definitions()); // includes read_skill_file
    t
};

// Warn if any declared tool is not available at runtime
let available_names: Vec<String> = all_possible_tools
    .iter()
    .map(|td| td.function.name.clone())
    .collect();
let missing = missing_subagent_tools(&allowed_tools, &available_names);
if !missing.is_empty() {
    warn!(
        "Subagent '{}': declared tools not available at runtime \
         (MCP server not configured?): {:?}",
        skill_name, missing
    );
}

let subagent_tools: Vec<ToolDefinition> = all_possible_tools
    .into_iter()
    .filter(|td| allowed_tools.contains(&td.function.name))
    .collect();
```

Then add `error!()` logging to the API failure arm (line 655). Change:

```rust
Err(e) => return format!("Subagent '{}' error: {}", skill_name, e),
```

To:

```rust
Err(e) => {
    error!(
        "Subagent '{}' API call failed (model: '{}'): {}",
        skill_name, resolved_model, e
    );
    return format!("Subagent '{}' error: {}", skill_name, e);
}
```

You'll need `use tracing::error;` if not already imported. Check the top of agent.rs — if only `info` and `debug` are imported, add `error` and `warn`:

```rust
use tracing::{debug, error, info, warn};
```

### Step 6: Verify compilation

```bash
cargo check 2>&1 | grep "^error" | head -10
```

Expected: 0 errors.

### Step 7: Run all tests

```bash
cargo test 2>&1 | tail -10
```

Expected: all tests pass.

### Step 8: Commit

```bash
git add src/agent.rs
git commit -m "fix(agent): add error logging on subagent API failure; warn on missing tools at launch"
```

---

## Task 3: Run full CI checks and push

**Files:** none (verification only)

### Step 1: Run `cargo fmt`

```bash
cargo fmt --all 2>&1
```

Expected: no output (no formatting issues).

### Step 2: Run `cargo clippy`

```bash
cargo clippy -- -D warnings 2>&1 | grep -E "^error|warning\[" | head -20
```

Expected: 0 errors. Fix any warnings that appear before continuing.

### Step 3: Run full test suite

```bash
cargo test 2>&1 | tail -10
```

Expected: all tests pass.

### Step 4: Run `cargo build --release`

```bash
cargo build --release 2>&1 | grep "^error" | head -10
```

Expected: 0 errors.

### Step 5: Final commit (if any fmt/clippy fixes were needed)

```bash
git add -p  # stage only the fmt/clippy changes
git commit -m "style: apply fmt and clippy fixes"
```

### Step 6: Push branch

```bash
git push -u origin claude/subagent-model-selection-5OTEa
```

---

## Verification Checklist

Before calling this done:

- [ ] `cargo fmt --all -- --check` exits 0
- [ ] `cargo clippy -- -D warnings` exits 0
- [ ] `cargo test` — all tests pass (was 39 before this fix; should be 41+ after adding 2 new agent tests)
- [ ] `cargo build --release` succeeds
- [ ] Server log shows `error!` when subagent API fails (manually verifiable if you have a test OpenRouter call)
- [ ] Server log shows `warn!` if an MCP tool declared in skill frontmatter is not available at runtime
- [ ] `daily-news-to-threads/SKILL.md` full body is visible in system prompt again (check log output at bot startup)

---

## What Was NOT Changed (intentional scope limits)

- **Model ID validation**: the model string in SKILL.md frontmatter (`anthropic/claude-sonnet-4-6`) is passed directly to OpenRouter unchanged. Validation would require an API call. If this model ID is wrong, the new `error!()` log will now surface the OpenRouter error clearly.
- **Instruction skill auto-loading from disk**: skills are still loaded once at startup (or reload_skills). No change there.
- **Subagent MCP tool availability**: if a required MCP server (e.g., `fetch`) is not configured, the subagent still won't have the tool. The warning added in Task 2 will surface this in logs, but the fix for missing servers is in `config.toml`, not in code.
