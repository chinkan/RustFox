# Post-Merge Cleanup — Bucket A: Strip Legacy Shim & Remove Dead Code

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the parallel code paths created by the M1 extraction. Ensure `ToolRegistry` is the single source of truth for tool definitions and dispatch.

**Architecture:** Delete the `builtin_tool_definitions()`/`execute_builtin_tool()` shim in `tools.rs`, remove dead functions and `#[allow(dead_code)]` from `agent.rs`, and deduplicate tool definitions by routing `all_tool_definitions()` through `ToolRegistry`.

**Notes:**
- `bot` stays in Agent through this bucket (needed by `restore_scheduled_tasks()` — removed in Bucket D)
- `is_compacted_regurgitation()` stays (called by `run_subagent_loop()` — removed in Bucket D)
- `validate_skill_name()`/`validate_skill_path()` stay (tests reference them — moved in Bucket C)
- `skills_rw` stays in main.rs (needed by Bucket B)

**Depends on:** Buckets B–D for remaining cleanup

---

## File Structure

| File | Action |
|------|--------|
| `src/tools.rs` | Delete shim functions, keep validators + tests |
| `src/agent.rs` | Remove dead functions, `#[allow(dead_code)]`, `running_commands`, duplicate tool defs, update `all_tool_definitions` |
| `src/main.rs` | Remove `skills_rw` removal (keep it) |
| `src/lib.rs` | No changes |

---

### Task 1: Strip tools.rs to path validators only

**Files:**
- Modify: `src/tools.rs`

- [ ] **Step 1: Delete the shim functions and unused imports**

Remove from `src/tools.rs`:
- `use serde_json::{json, Value};`
- `use crate::llm::{FunctionDefinition, ToolDefinition};`
- Everything from the `// Backward-compatible shims` comment through the `#[cfg(test)]` block
- The test module (only `validate_home_path` tests remain — they're already preserved in the new smaller test block below)

The file should end up with only:
```rust
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Validates that a path is within the allowed sandbox directory.
pub fn validate_sandbox_path(/* ... */) -> Result<PathBuf> { /* ... */ }

/// Validates that a path is within the RustFox home directory.
pub fn validate_home_path(/* ... */) -> Result<PathBuf> { /* ... */ }

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    // validate_home_path tests only
}
```

- [ ] **Step 2: Run cargo check**

Run: `cargo check`
Expected: compiles cleanly. If it doesn't, check that no code outside tools.rs still calls `tools::builtin_tool_definitions()` or `tools::execute_builtin_tool()`.

- [ ] **Step 3: Run tests**

Run: `cargo test tools::tests`
Expected: 2 tests pass (path validator tests)

- [ ] **Step 4: Commit**

```bash
git add src/tools.rs
git commit -m "cleanup: strip tools.rs to path validators only, remove legacy shim"
```

---

### Task 2: Remove duplicate tool definitions from agent.rs

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Delete `memory_tool_definitions()` method**

Find and delete the `memory_tool_definitions()` method (around line 1322). It's a private method that duplicates `MemoryTools::define()`.

- [ ] **Step 2: Delete `scheduling_tool_definitions()` method**

Find and delete the `scheduling_tool_definitions()` method (around line 1376). Duplicates `SchedulingTools::define()`.

- [ ] **Step 3: Delete `skill_tool_definitions()` method**

Find and delete the `skill_tool_definitions()` method (around line 1467). Duplicates `SkillTools::define()`.

- [ ] **Step 4: Update `all_tool_definitions()` to use ToolRegistry**

Replace:
```rust
pub fn all_tool_definitions(&self) -> Vec<ToolDefinition> {
    let mut all_tools: Vec<ToolDefinition> = tools::builtin_tool_definitions();
    all_tools.extend(self.memory_tool_definitions());
    all_tools.extend(self.scheduling_tool_definitions());
    all_tools.extend(self.skill_tool_definitions());
    all_tools.extend(self.mcp.tool_definitions());
    all_tools
}
```

With:
```rust
pub fn all_tool_definitions(&self) -> Vec<ToolDefinition> {
    let mut all = self.tool_registry.all_definitions();
    all.extend(self.mcp.tool_definitions());
    all
}
```

- [ ] **Step 5: Run cargo check**

Run: `cargo check`
Expected: compiles cleanly

- [ ] **Step 6: Run cargo test**

Run: `cargo test`
Expected: all 462 tests pass

- [ ] **Step 7: Commit**

```bash
git add src/agent.rs
git commit -m "cleanup: remove duplicate tool definitions from Agent, route all_tool_definitions through ToolRegistry"
```

---

### Task 3: Remove dead functions and #[allow(dead_code)] from agent.rs

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Remove `RunningCommand` struct**

Delete the struct (around line 84):
```rust
pub struct RunningCommand {
    pub cancel_tx: oneshot::Sender<()>,
}
```

- [ ] **Step 2: Remove `COMPACTION_RAG_LIMIT` constant**

Delete `const COMPACTION_RAG_LIMIT: usize = 5;` (around line 72).

- [ ] **Step 3: Remove `running_commands` field from Agent struct**

Delete from the struct (around line 115):
```rust
pub running_commands: Arc<tokio::sync::Mutex<HashMap<String, RunningCommand>>>,
```

And from the constructor (around line 210):
```rust
running_commands: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
```

- [ ] **Step 4: Remove dead functions (safe to delete, no callers remain)**

Delete these functions:
- `execute_command_interactive()` (around line 2135)
- `auto_compact_conversation()` (around line 1773)
- `reactive_compact()` (around line 1837)
- `summarize_and_replace()` (around line 1887)
- `resolve_skill_base_dir()` (around line 368)
- `soul_file_path()` (around line 2951)
- `validate_soul_file_path()` (around line 2965)

For each function:
1. Find the `fn` definition
2. Find the matching closing `}` (same indentation)
3. Delete the entire function body

- [ ] **Step 5: Remove all `#[allow(dead_code)]` annotations**

Search for `#[allow(dead_code)]` in `src/agent.rs`. There should be approximately 12-15 of them. For each:
1. Check if the annotated item is actually dead
2. If yes, delete the annotation
3. The compiler will tell you if something is genuinely unused (with `#![deny(dead_code)]` in lib.rs)

- [ ] **Step 6: Run cargo check**

Run: `cargo check`
Expected: compiles cleanly

- [ ] **Step 7: Run cargo test**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 8: Commit**

```bash
git add src/agent.rs
git commit -m "cleanup: remove RunningCommand, dead functions, and #[allow(dead_code)] from Agent"
```

---

### Task 4: Update run_subagent tool definitions to use ToolRegistry

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Update `run_subagent()` tool list construction**

In `run_subagent()` (around line 1745), replace:
```rust
let mut t = tools::builtin_tool_definitions();
t.extend(self.memory_tool_definitions());
t.extend(self.scheduling_tool_definitions());
t.extend(self.skill_tool_definitions());
```

With:
```rust
let mut t = self.tool_registry.all_definitions();
```

And in the `run_subagent_loop` fallback path (around line 1838), do the same replacement.

- [ ] **Step 2: Run cargo check**

Run: `cargo check`
Expected: compiles cleanly

- [ ] **Step 3: Run cargo test**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add src/agent.rs
git commit -m "cleanup: subagent tool list uses ToolRegistry instead of old inline methods"
```

---

## Self-Review

### Spec Coverage
- ✓ Strip `tools.rs` shim (Task 1)
- ✓ Remove duplicate tool definitions from agent.rs (Task 2)
- ✓ Remove dead functions, `running_commands`, `#[allow(dead_code)]` (Task 3)
- ✓ Update subagent tool list to use ToolRegistry (Task 4)
- ✓ `bot` kept in Agent (deferred to Bucket D)
- ✓ `is_compacted_regurgitation` kept (deferred to Bucket D)
- ✓ `validate_skill_name`/`validate_skill_path` kept (deferred to Bucket C)
- ✓ `skills_rw` kept in main.rs (needed by Bucket B)

### Placeholder Scan
No placeholders.

### Type Consistency
- `ToolRegistry::all_definitions()` returns `Vec<ToolDefinition>` — consistent with old return type
- Tool names and parameter names unchanged (using same registry as before)

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-15-post-merge-cleanup-bucket-a.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?
