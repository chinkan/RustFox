# Post-Merge Cleanup — Bucket C: Security & Correctness

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the `blocking_lock()` footgun in `CancelRegistry` and restore path traversal protection in `skill_tools.rs`.

**Architecture:** Convert `CancelRegistry` methods to async with `.lock().await`, move `validate_skill_name`/`validate_skill_path` from agent.rs to skill_tools.rs, add canonicalize containment checks.

**Depends on:** Bucket A (dead code removed), Bucket B (SkillTools gains registry fields)

---

## File Structure

| File | Action |
|------|--------|
| `src/cancel_registry.rs` | Convert to async methods |
| `src/command_tool.rs` | Update callers to `.await` |
| `src/platform/telegram.rs` | Update callback handler to `.await` |
| `src/skill_tools.rs` | Add path validation functions |
| `src/agent.rs` | Remove `validate_skill_name`/`validate_skill_path` (moved to skill_tools.rs) |

---

### Task 1: Convert CancelRegistry to async

**Files:**
- Modify: `src/cancel_registry.rs`
- Modify: `src/command_tool.rs`
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Make CancelRegistry methods async**

Replace `blocking_lock()` with `.lock().await`:
```rust
impl CancelRegistry {
    pub async fn register(&self, id: String, tx: oneshot::Sender<()>) {
        let mut map = self.inner.lock().await;
        map.insert(id, tx);
    }

    pub async fn cancel(&self, id: &str) -> bool {
        let mut map = self.inner.lock().await;
        if let Some(tx) = map.remove(id) {
            let _ = tx.send(());
            true
        } else {
            false
        }
    }

    pub async fn unregister(&self, id: &str) {
        let mut map = self.inner.lock().await;
        map.remove(id);
    }
}
```

- [ ] **Step 2: Update tests in cancel_registry.rs**

Change `#[test]` to `#[tokio::test]` and add `.await` to each call:
```rust
#[tokio::test]
async fn test_register_and_cancel() {
    let reg = CancelRegistry::new();
    let (tx, mut rx) = oneshot::channel();
    reg.register("cmd_1".to_string(), tx).await;
    assert!(reg.cancel("cmd_1").await);
    assert!(rx.try_recv().is_ok());
}
// Same for all 4 tests
```

- [ ] **Step 3: Update command_tool.rs callers**

Add `.await` to:
- `self.cancel_registry.register(cmd_id.clone(), cancel_tx).await;`
- `self.cancel_registry.unregister(&cmd_id).await;`

- [ ] **Step 4: Update telegram.rs callback handler**

Add `.await` to:
```rust
let text = if agent.cancel_registry.cancel(cmd_id).await {
```

- [ ] **Step 5: Run cargo check and tests**

Run: `cargo check && cargo test`
Expected: all pass (4 CancelRegistry tests updated to async)

- [ ] **Step 6: Commit**

```bash
git add src/cancel_registry.rs src/command_tool.rs src/platform/telegram.rs
git commit -m "fix: convert CancelRegistry to async methods, removing blocking_lock footgun"
```

---

### Task 2: Move path validation to skill_tools.rs

**Files:**
- Modify: `src/skill_tools.rs`
- Modify: `src/agent.rs`

- [ ] **Step 1: Add validate_skill_name and validate_skill_path to skill_tools.rs**

```rust
fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Skill name cannot be empty".to_string());
    }
    if name.len() > 64 {
        return Err("Skill name too long (max 64 chars)".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Skill name contains invalid characters".to_string());
    }
    Ok(())
}

fn validate_skill_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("Path cannot be empty".to_string());
    }
    if path.contains("..") {
        return Err("Path cannot contain '..'".to_string());
    }
    if path.starts_with('/') {
        return Err("Path cannot be absolute".to_string());
    }
    Ok(())
}
```

- [ ] **Step 2: Add validation to read_skill_file, write_skill_file, read_agent_file, write_agent_file**

Before each file operation in `SkillTools::execute()`, add:
```rust
validate_skill_name(skill_name).map_err(|e| anyhow::anyhow!(e))?;
validate_skill_path(relative_path).map_err(|e| anyhow::anyhow!(e))?;
```

- [ ] **Step 3: Remove validate_skill_name and validate_skill_path from agent.rs**

Delete these two functions. They are no longer called from agent.rs (all callers were in the old `write_skill_file`/`read_skill_file` handlers which are now in `skill_tools.rs`).

- [ ] **Step 4: Run cargo check and tests**

Run: `cargo check && cargo test`
Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add src/skill_tools.rs src/agent.rs
git commit -m "fix: move path validation to skill_tools.rs, restore path traversal protection"
```

---

## Self-Review

### Spec Coverage
- ✓ `CancelRegistry` methods are async with `.lock().await` (Task 1)
- ✓ All callers updated to `.await` (Task 1)
- ✓ `validate_skill_name`/`validate_skill_path` moved to skill_tools.rs (Task 2)
- ✓ Path validation applied to all 4 skill/agent file tools (Task 2)

### Placeholder Scan
No placeholders.

### Type Consistency
- `CancelRegistry::register()` returns `Future<Output = ()>` (was `()`)
- `CancelRegistry::cancel()` returns `Future<Output = bool>` (was `bool`)
- `CancelRegistry::unregister()` returns `Future<Output = ()>` (was `()`)

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-15-post-merge-cleanup-bucket-c.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?