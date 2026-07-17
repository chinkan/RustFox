# Architecture Deepening — Post-Merge Cleanup Design

## Status
Draft

## Date
2026-07-15

## Context
The Architecture Deepening refactoring (M1–M4) extracted ToolRegistry, PlatformSender, CancelRegistry, ConversationManager, and AgenticLoop from the Agent god module. Post-merge code review identified ~20 issues across four buckets. This document designs the cleanup.

## Approach
Sequential per-bucket cleanup, each bucket independently mergeable and testable.

---

## Bucket A: Strip Legacy Shim + Remove Dead Code

### Goal
Eliminate the parallel code paths created by the M1 extraction. Ensure `ToolRegistry` is the single source of truth for tool definitions and dispatch.

### Changes

**`src/tools.rs`**
- Delete `builtin_tool_definitions()` (lines 88–187)
- Delete `execute_builtin_tool()` (lines 189–273)
- Keep only: `validate_sandbox_path()`, `validate_home_path()`, and their tests
- Remove the `// Backward-compatible shims` comment block

**`src/agent.rs` — Struct**
- Remove `pub bot: Arc<Bot>` from the struct (it is a duplicate — `SchedulingTools` holds its own `Arc<Bot>`)
- EXCEPTION: `restore_scheduled_tasks()` still uses `self.bot` to rebuild fire closures for tasks loaded from DB. Keep `bot` in Agent until Bucket D migrates restore logic, OR pass `bot` as a parameter to `restore_scheduled_tasks()` from `main.rs`. **Recommendation:** Keep `bot` in Agent through Bucket A, remove it in Bucket D when `restore_scheduled_tasks()` is refactored.
- Remove `pub running_commands: Arc<Mutex<HashMap<String, RunningCommand>>>` (replaced by CancelRegistry)
- Remove `RunningCommand` struct
- Remove `COMPACTION_RAG_LIMIT` constant

**`src/agent.rs` — Dead functions to delete**
- `memory_tool_definitions()` — duplicate of `MemoryTools::define()`
- `scheduling_tool_definitions()` — duplicate of `SchedulingTools::define()`
- `skill_tool_definitions()` — duplicate of `SkillTools::define()`
- `execute_command_interactive()` — duplicate of `CommandTool::exec_command()`
- `execute_tool()` (the old inline dispatch) — replaced by M1's ToolRegistry dispatch
- `auto_compact_conversation()` — replaced by `ConversationManager::compact_tier3/4`
- `reactive_compact()` — replaced by `ConversationManager::compact_tier4`
- `summarize_and_replace()` — replaced by `ConversationManager::compact_tier4`
- `validate_skill_name()` — moved to `skill_tools.rs` (in Bucket C)
- `validate_skill_path()` — moved to `skill_tools.rs` (in Bucket C)
- NOTE: Keep these functions in `agent.rs` through Bucket A (their tests reference them). Move to `skill_tools.rs` in Bucket C.
- `soul_file_path()` — replaced by inline path construction in BuiltinTools
- `validate_soul_file_path()` — replaced by inline path validation in BuiltinTools
- `resolve_skill_base_dir()` — unused after extraction
- `is_compacted_regurgitation()` — still called by `run_subagent_loop()` (line 2063). Defer deletion to Bucket D when `run_subagent_loop` is migrated to AgenticLoop.

**`src/agent.rs` — Updates**
- `all_tool_definitions()`: change to `self.tool_registry.all_definitions()` + `self.mcp.tool_definitions()`
- `run_subagent()` / `run_subagent_loop()`: update tool list construction to use `self.tool_registry.all_definitions()`
- Remove all `#[allow(dead_code)]` annotations

**`src/main.rs`**
- Remove `bot` from `Agent::new()` call
- Keep `skills_rw: Arc<RwLock<SkillRegistry>>` in main.rs (needed by both BuiltinTools and SkillTools in Bucket B)
- Do NOT remove `skills_rw` in this bucket; verify it is still used after cleaning up

**`src/agent_prompt.rs`**
- Remove `estimate_prompt_bytes()` if unused
- Remove `should_auto_compact()` if unused

### Risk
- `run_subagent()` and `run_subagent_loop()` currently use `tools::builtin_tool_definitions()` for tool enumeration. Switching to `tool_registry.all_definitions()` will change which tools subagents see. Must verify that all tool names are identical (including `execute_command` which is now registered by `CommandTool`).
- The `bot` field removal may break `ScheduledJobRequest` closure captures. Verify that `SchedulingTools` (which holds its own `Arc<Bot>`) is the only path through which fire closures access the bot.

### Test Strategy
- `cargo test` must pass with zero changes to test output
- `cargo clippy -- -D warnings` must pass
- Verify no new `#[allow(dead_code)]` annotations are needed

---

## Bucket B: Fix Behavioral Regressions

### Goal
Restore behaviors that were lost during extraction. The spec promised "zero behavioral change" — this bucket honors that promise.

### Changes

**`src/skill_tools.rs` — reload_skills and reload_agents**
- Restore actual `load_skills_from_dir` calls instead of returning hardcoded strings
- This requires passing `skills_dir`, `agents_dir` through the handler (already done)
- Requires a `SkillRegistry` to reload into — but `SkillTools` doesn't own one. Two options:
  - Add `Arc<RwLock<SkillRegistry>>` and `Arc<RwLock<SkillRegistry>>` for skills/agents to `SkillTools`
  - Add a callback/reload channel
  - **Recommendation:** Add `skills: Arc<RwLock<SkillRegistry>>` and `agents: Arc<RwLock<SkillRegistry>>` to `SkillTools` struct, similar to how `BuiltinTools` holds `skills: Arc<RwLock<SkillRegistry>>`

**`src/builtin_tools.rs` — self_upgrade side effect**
- After a successful `learning::self_upgrade()` call in the `"self_upgrade"` handler, set `self.restart_pending.store(true, Ordering::SeqCst)` to signal the bot to restart
- Add `restart_pending: Arc<AtomicBool>` to `BuiltinTools`
- The `Arc<AtomicBool>` is shared with `Agent` (which holds the same `Arc`). `telegram.rs` reads `agent.restart_pending` — it can read the same `Arc` via the Agent struct. BuiltinTools receives the `Arc` at construction time.

**`src/builtin_tools.rs` — update_soul_file side effect**
- After a successful soul file write (in the `"update_soul_file"` handler, after the write-verification passes), set `self.soul_updated.store(true, Ordering::SeqCst)` to prevent redundant post-task soul reflection
- Add `soul_updated: Arc<AtomicBool>` to `BuiltinTools`
- Shared with `Agent` — same pattern as `restart_pending`
- NOTE: the post-loop soul reflection code that reads `soul_updated` must also be restored in `process_message`. This is the block that checks `soul_updated` after the loop and conditionally fires `update_soul_file` with a session-end reflection. This code was lost during M2/M3. Restore it in this bucket.

**`src/builtin_tools.rs` — tool parameter descriptions**
- Restore original tool descriptions from `tools.rs` (e.g., `read_file` description changed from `"Read the contents of a file within the sandbox directory"` to `"Read the contents of a file from the sandbox."`)
- Review ALL tool descriptions in `builtin_tools.rs` against the originals in `tools.rs` (pre-cleanup) and restore verbatim

**`src/builtin_tools.rs` — try_new_tech logging**
- Restore the `info!("Running experiment '{}'", ...)` log line

### Risk
- Adding `Arc<RwLock<SkillRegistry>>` to `SkillTools` is safe — `SkillRegistry` is loaded from disk, not built from `SkillTools` definitions. The real circular dependency that kept `invoke_agent`/`spawn_agents` in `Agent` is `Agent` ↔ `AgenticLoop` (subagent spawning requires the Agent's loop infrastructure). The `SkillTools` reload path is a one-way dependency: `SkillTools` calls `load_skills_from_dir()` and swaps the registry, which is fine.
- Tool description changes affect LLM prompt caching. The original descriptions were what the LLM was trained on. Restoring them is the right thing to do.

### Test Strategy
- Write a test for `reload_skills` using a temp directory with a known skill that gets created after `SkillTools` construction
- Write a test for `self_upgrade` side effect (mock the upgrade, assert `restart_pending` is set)
- Write a test for `update_soul_file` side effect (assert `soul_updated` is set)
- Parameter description tests already exist in `builtin_tools.rs` — verify they pass and cover all tools

---

## Bucket C: Security & Correctness

### Goal
Fix the `blocking_lock()` footgun in `CancelRegistry` and restore path traversal protection in `skill_tools.rs`.

### Changes

**`src/cancel_registry.rs` — async methods**
- Change `register`, `cancel`, `unregister` from sync to `async fn`
- Replace `self.inner.blocking_lock()` with `self.inner.lock().await`
- Update all callers:
  - `src/command_tool.rs`: `self.cancel_registry.register(...)` → `.await`
  - `src/command_tool.rs`: `self.cancel_registry.unregister(...)` → `.await`
  - `src/platform/telegram.rs`: `agent.cancel_registry.cancel(cmd_id)` → `.await`
- Tests: change test methods to `#[tokio::test]` and add `.await` calls

**`src/skill_tools.rs` — path validation**
- Restore the `validate_skill_name()` and `validate_skill_path()` functions (moved from `agent.rs`)
- Add canonicalize containment check: `validate_sandbox_path` for skills/agents directory
- Each `read_skill_file`, `write_skill_file`, `read_agent_file`, `write_agent_file` must validate:
  1. Skill/agent name contains only valid characters (alphanumeric + hyphens)
  2. Relative path does not contain `..` traversal
  3. Resolved path is within the skills/agents directory

### Risk
- Making `CancelRegistry` methods async is a breaking change for all callers. Must update all call sites in a single atomic commit.
- Path validation in `skill_tools.rs` requires access to the skills/agents directory path (already in the struct) and the `validate_sandbox_path` function (import from `tools.rs`).

### Test Strategy
- `CancelRegistry` tests: update to async, 4 tests should still pass
- `skill_tools.rs` tests: add tests for path traversal attempts, valid paths, invalid characters
- `tools.rs` validator tests: already exist, should continue to pass

---

## Bucket D: Migrate Subagent Loop to AgenticLoop

### Goal
Eliminate the second copy of the agentic loop. `run_subagent_loop` should use `AgenticLoop` with `MessageContainer::Plain`.

### Changes

**`src/agent.rs` — run_subagent_loop**
- Replace the inline loop body with `AgenticLoop::new(...).run(&mut MessageContainer::Plain(messages), user_id, chat_id)`
- `LoopConfig` for subagent: `compaction_enabled: false`, `loop_detection_enabled: true`, `interactive_loop_callback: false`, `allowed_tools: Some(tool_whitelist)`
- `make_tool_ctx` closure: same pattern as `process_message`, captures `self.sender`, `self.cancel_registry`, `sandbox_dir`, `home_dir`
- Remove the old inline loop body (~170 lines)
- `run_subagent()`: update to pass `user_id` and `chat_id` through to `run_subagent_loop`

**`src/agent.rs` — execute_tool (old)**
- After migrating the subagent loop, the old `execute_tool` function is no longer called by any code path
- Delete it (~130 lines)
- `AdHocTask` struct — only used by the old `execute_tool()` for `spawn_agents`. Delete it when `execute_tool` is removed.

Hmm, this is a problem. The old `execute_tool` in `agent.rs` handles `invoke_agent` and `spawn_agents`. But the new `AgenticLoop` dispatches tools through `ToolRegistry`. The `invoke_agent` and `spawn_agents` tools are NOT in `ToolRegistry` — they're in the old `execute_tool`.

So before we can delete the old `execute_tool`, we need to either:
- Option A: Register `invoke_agent` and `spawn_agents` in `ToolRegistry` (requires breaking the circular dependency)
- Option B: Have `AgenticLoop` call back to `Agent::execute_tool` for agent-specific tools
- Option C: Keep `invoke_agent`/`spawn_agents` in `Agent::execute_tool` and have `AgenticLoop::run` accept a callback for "special" tools

**Recommendation: Option C** — Add an optional `special_tool_handler: Option<&'a dyn Fn(&str, &Value, &str, &str) -> Option<String>>` to `AgenticLoop`. The signature is `(tool_name, arguments, user_id, chat_id) -> Option<result>`. When set, the loop calls this handler first for each tool call. If it returns `Some(result)`, that result is used. If `None`, the loop falls through to `ToolRegistry`. Agent passes a closure that handles `invoke_agent` and `spawn_agents` by calling `self.run_subagent()`.

### Risk
- Option C adds indirection but avoids the circular dependency
- Must ensure `user_id` and `chat_id` are passed through to the special tool handler
- All subagent paths must be tested: `invoke_agent` with predefined agent, `invoke_agent` with skill fallback, `spawn_agents` with tasks array, `spawn_agents` with shorthand fields
- **Recovery nudge:** The old `run_subagent_loop` injects `recovery_nudge_for()` on empty response retries. `AgenticLoop` only counts empty responses and continues. Must add empty-response recovery nudge support to `AgenticLoop` before migrating the subagent loop, or the subagent loses nudge guidance on retry. **Recommendation:** Add a `recovery_nudge: Option<String>` config field to `LoopConfig` that is injected on empty response retry.

### Test Strategy
- Existing subagent tests in `agent.rs` tests module must pass
- Add a test for `AgenticLoop` with `special_tool_handler` that returns results for known tool names and `None` for unknown
- Integration test: subagent can `read_file` through the new path

---

## Execution Order

```
Bucket A ──→ Bucket B ──→ Bucket C ──→ Bucket D
  (clear      (fix          (secure      (unify
   dead code)  behavior)     async ops)   loops)
```

Each bucket is a separate PR with its own commit, tests, and review.

**Dependency notes:**
- Bucket A keeps `bot` in Agent (removed in Bucket D), keeps `is_compacted_regurgitation` (deleted in Bucket D), and keeps `validate_skill_name`/`validate_skill_path` (moved in Bucket C). This ensures each bucket is independently compilable and testable.
- Bucket A retains `skills_rw` in main.rs (needed by Bucket B).
- Bucket D redesigns the `restore_scheduled_tasks()` bot access, enabling `bot` removal from Agent.

---

## Future Work (not in scope)
- Add unit tests for `ConversationManager` (compaction, RAG injection, steer)
- Add unit tests for `AgenticLoop` (mock LLM, both `MessageContainer` modes)
- Add unit tests for `CommandTool`, `MemoryTools`, `SchedulingTools`, `SkillTools`
- `plan_create` file layout divergence between old shim and new handler (will be resolved by Bucket A)